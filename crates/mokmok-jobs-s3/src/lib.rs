//! `S3`-compatible object storage for the event store port.
//!
//! One stream is one object named `<key>.jsonl` under a configured prefix, with the
//! same `JSONL` record format every other adapter writes. An append rewrites the whole
//! object: it reads the committed bytes, composes the next version, and writes them
//! back under a conditional header.
//!
//! Calls block on network I/O. A caller on a `tokio` runtime must run them in a
//! blocking context, and the daemon uses `spawn_blocking`.
//!
//! The conditional write makes a concurrent append fail with
//! [`StoreError::Conflict`] instead of losing an update, but one daemon per prefix is
//! still the supported deployment: two writers that each retry on conflict will churn
//! rather than coordinate.

mod client;
mod listing;

use std::fmt;
use std::marker::PhantomData;
use std::time::Duration;

use mokmok_events::{
    CloudEvent, EnumerableStore, EventStore, EventType, StoreError, StreamKey, Version,
    canonical_keys, check_append, complete_prefix, encode_records, object_name, parse_records,
};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::client::{Client, Condition, GetOutcome};

/// Everything [`S3EventStore::open`] needs to reach one bucket.
#[derive(Clone, Debug)]
pub struct S3Config {
    /// The base URL, `http(s)://host[:port]`, with or without a trailing slash.
    pub endpoint: String,
    /// The bucket that holds the streams.
    pub bucket: String,
    /// The signing region, such as `us-east-1`.
    pub region: String,
    /// The `/`-joined path inside the bucket that holds the streams, or `""`.
    pub prefix: String,
    /// The key pair every request is signed with.
    pub credentials: S3Credentials,
    /// The whole-request deadline.
    pub timeout: Duration,
}

/// The static key pair an [`S3Config`] signs with.
#[derive(Clone)]
pub struct S3Credentials {
    access_key: String,
    secret_key: String,
    session_token: Option<String>,
}

impl S3Credentials {
    #[must_use]
    pub fn new(
        access_key: impl Into<String>,
        secret_key: impl Into<String>,
        session_token: Option<String>,
    ) -> Self {
        Self {
            access_key: access_key.into(),
            secret_key: secret_key.into(),
            session_token,
        }
    }

    /// Reads `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, and optional
    /// `AWS_SESSION_TOKEN`.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Invalid`] when a required variable is missing or is not
    /// valid Unicode.
    pub fn from_env() -> Result<Self, StoreError> {
        let access_key = required_env("AWS_ACCESS_KEY_ID")?;
        let secret_key = required_env("AWS_SECRET_ACCESS_KEY")?;
        let session_token = std::env::var("AWS_SESSION_TOKEN").ok();
        Ok(Self::new(access_key, secret_key, session_token))
    }

    pub(crate) fn access_key(&self) -> &str {
        &self.access_key
    }

    pub(crate) fn secret_key(&self) -> &str {
        &self.secret_key
    }

    pub(crate) fn session_token(&self) -> Option<&str> {
        self.session_token.as_deref()
    }
}

impl fmt::Debug for S3Credentials {
    fn fmt(
        &self,
        formatter: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        let visible: String = self.access_key.chars().take(4).collect();
        let token = self.session_token.as_ref().map(|_| "[redacted]");
        formatter
            .debug_struct("S3Credentials")
            .field("access_key", &visible)
            .field("secret_key", &"[redacted]")
            .field("session_token", &token)
            .finish()
    }
}

fn required_env(name: &str) -> Result<String, StoreError> {
    std::env::var(name)
        .map_err(|error| StoreError::Invalid(format!("{name} is not usable: {error}")))
}

/// An [`EventStore`] whose streams are objects in an `S3`-compatible bucket.
///
/// One object holds one stream, and every append rewrites that object whole. Reads and
/// writes block on the network; run them off a `tokio` runtime's worker threads.
pub struct S3EventStore<K, E> {
    client: Client,
    _kind: PhantomData<fn() -> K>,
    _event: PhantomData<fn() -> E>,
}

impl<K, E> fmt::Debug for S3EventStore<K, E> {
    fn fmt(
        &self,
        formatter: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        formatter
            .debug_struct("S3EventStore")
            .field("client", &self.client)
            .finish()
    }
}

impl<K: StreamKey, E> S3EventStore<K, E> {
    /// Validates `config`, builds one connection agent, and probes the bucket for
    /// conditional-write support.
    ///
    /// The probe writes a throwaway object and requires the store to refuse an
    /// `If-Match` it cannot satisfy. A store that ignores conditional writes would
    /// silently drop a concurrent append, so such a store is refused here rather than
    /// at the first conflict.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Invalid`] when the endpoint, bucket, region, prefix, or
    /// timeout is malformed, or when the store ignores conditional writes. Returns
    /// [`StoreError::Io`] when the probe cannot reach the store.
    pub fn open(config: S3Config) -> Result<Self, StoreError> {
        let client = Client::open(config)?;
        client.probe()?;
        Ok(Self {
            client,
            _kind: PhantomData,
            _event: PhantomData,
        })
    }
}

impl<K, E> EventStore<K, E> for S3EventStore<K, E>
where
    K: StreamKey,
    E: EventType + Serialize + DeserializeOwned,
{
    fn load(
        &self,
        key: &K,
    ) -> Result<Vec<CloudEvent<E>>, StoreError> {
        let name = object_name(key.as_str());
        let label = self.client.label(&name);
        match self.client.get(&name)? {
            GetOutcome::Missing => Ok(Vec::new()),
            GetOutcome::Found { body, etag } => {
                if etag.is_none() {
                    return Err(StoreError::Io(format!(
                        "{label}: the object carries no ETag, so a concurrent append cannot be fenced"
                    )));
                }
                let (_, events) = parse_records::<E>(&label, key.as_str(), complete_prefix(&body))?;
                Ok(events)
            },
        }
    }

    fn append(
        &self,
        key: &K,
        expected: Version,
        events: &[CloudEvent<E>],
    ) -> Result<Version, StoreError> {
        let name = object_name(key.as_str());
        let label = self.client.label(&name);
        let (committed, current, etag) = match self.client.get(&name)? {
            GetOutcome::Missing => (Vec::new(), Version::ZERO, None),
            GetOutcome::Found { body, etag } => {
                let Some(etag) = etag else {
                    return Err(StoreError::Io(format!(
                        "{label}: the object carries no ETag, so a concurrent append cannot be fenced"
                    )));
                };
                let committed = complete_prefix(&body).to_vec();
                let (current, _) = parse_records::<E>(&label, key.as_str(), &committed)?;
                (committed, current, Some(etag))
            },
        };
        if current != expected {
            return Err(StoreError::Conflict {
                expected,
                actual: current,
            });
        }
        if events.is_empty() {
            return Ok(current);
        }
        let last = check_append(key.as_str(), expected, events)?;
        let mut body = committed;
        encode_records(events, &mut body)?;
        let condition = etag
            .as_ref()
            .map_or(Condition::Absent, |tag| Condition::Match(tag.as_str()));
        let response = self.client.put(&name, &body, condition)?;
        match response.status {
            200..=299 => Ok(last),
            409 | 412 => {
                let actual = self.stored_version(key).map_err(|error| {
                    StoreError::Io(format!(
                        "{label}: re-read after a refused write failed: {error}"
                    ))
                })?;
                Err(StoreError::Conflict { expected, actual })
            },
            status => Err(StoreError::Io(format!(
                "{label}: PUT returned {status}{}",
                client::code_note(&response.body)
            ))),
        }
    }
}

impl<K, E> EnumerableStore<K, E> for S3EventStore<K, E>
where
    K: StreamKey,
    E: EventType + Serialize + DeserializeOwned,
{
    fn keys(&self) -> Result<Vec<K>, StoreError> {
        let listing_prefix = self.client.listing_prefix();
        let mut names = Vec::new();
        let mut token: Option<String> = None;
        let mut seen: Vec<String> = Vec::new();
        loop {
            let page = self.client.list(listing_prefix, token.as_deref())?;
            for listed in page.keys {
                if let Some(rest) = listed.strip_prefix(listing_prefix) {
                    names.push(rest.to_owned());
                }
            }
            if !page.truncated {
                break;
            }
            let next = page.next_token.ok_or_else(|| {
                StoreError::Io(format!(
                    "/{}: the listing is truncated without a continuation token",
                    self.client.bucket()
                ))
            })?;
            if seen.contains(&next) {
                return Err(StoreError::Io(format!(
                    "/{}: the listing repeated the continuation token {next:?}",
                    self.client.bucket()
                )));
            }
            seen.push(next.clone());
            token = Some(next);
        }
        Ok(canonical_keys(names))
    }
}

impl<K: StreamKey, E: EventType + DeserializeOwned> S3EventStore<K, E> {
    fn stored_version(
        &self,
        key: &K,
    ) -> Result<Version, StoreError> {
        let name = object_name(key.as_str());
        let label = self.client.label(&name);
        match self.client.get(&name)? {
            GetOutcome::Missing => Ok(Version::ZERO),
            GetOutcome::Found { body, .. } => {
                let (version, _) =
                    parse_records::<E>(&label, key.as_str(), complete_prefix(&body))?;
                Ok(version)
            },
        }
    }
}
