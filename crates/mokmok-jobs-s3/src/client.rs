//! The signed HTTP transport every operation goes through.

use std::fmt;
use std::time::{Duration, SystemTime};

use aws_credential_types::Credentials;
use aws_sigv4::http_request::{
    PayloadChecksumKind, SignableBody, SignableRequest, SigningSettings, UriPathNormalizationMode,
    sign,
};
use aws_smithy_runtime_api::client::identity::Identity;
use mokmok_events::StoreError;
use ureq::http::Uri;

use crate::listing::{self, Page};
use crate::{S3Config, S3Credentials};

pub struct Client {
    agent: ureq::Agent,
    endpoint: String,
    bucket: String,
    prefix: String,
    listing_prefix: String,
    region: String,
    credentials: S3Credentials,
    timeout: Duration,
}

impl fmt::Debug for Client {
    fn fmt(
        &self,
        formatter: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        formatter
            .debug_struct("Client")
            .field("endpoint", &self.endpoint)
            .field("bucket", &self.bucket)
            .field("prefix", &self.prefix)
            .field("region", &self.region)
            .field("credentials", &self.credentials)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

pub enum GetOutcome {
    Missing,
    Found { body: Vec<u8>, etag: Option<String> },
}

pub struct Response {
    pub status: u16,
    pub etag: Option<String>,
    pub body: Vec<u8>,
}

#[derive(Clone, Copy)]
pub enum Condition<'a> {
    Absent,
    Match(&'a str),
}

#[derive(Clone, Copy)]
enum Method {
    Get,
    Put,
    Delete,
}

impl Method {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Put => "PUT",
            Self::Delete => "DELETE",
        }
    }
}

impl Client {
    pub fn open(config: S3Config) -> Result<Self, StoreError> {
        let S3Config {
            endpoint,
            bucket,
            region,
            prefix,
            credentials,
            timeout,
        } = config;
        if timeout.is_zero() {
            return Err(StoreError::Invalid(
                "the request timeout must be greater than zero".to_owned(),
            ));
        }
        let endpoint = normalize_endpoint(&endpoint)?;
        if !is_plain_component(&bucket) {
            return Err(StoreError::Invalid(format!(
                "the bucket {bucket:?} is not a plain path component"
            )));
        }
        validate_prefix(&prefix)?;
        if region.is_empty() {
            return Err(StoreError::Invalid("the region is empty".to_owned()));
        }
        let listing_prefix = if prefix.is_empty() {
            String::new()
        } else {
            format!("{prefix}/")
        };
        let agent = ureq::Agent::new_with_config(
            ureq::Agent::config_builder()
                .http_status_as_error(false)
                .timeout_global(Some(timeout))
                .build(),
        );
        Ok(Self {
            agent,
            endpoint,
            bucket,
            prefix,
            listing_prefix,
            region,
            credentials,
            timeout,
        })
    }

    pub fn bucket(&self) -> &str {
        &self.bucket
    }

    pub fn listing_prefix(&self) -> &str {
        &self.listing_prefix
    }

    pub fn label(
        &self,
        name: &str,
    ) -> String {
        if self.prefix.is_empty() {
            format!("/{}/{name}", self.bucket)
        } else {
            format!("/{}/{}/{name}", self.bucket, self.prefix)
        }
    }

    pub fn probe(&self) -> Result<(), StoreError> {
        let name = probe_name();
        let label = self.label(&name);
        let first = self.put(&name, b"", Condition::Absent)?;
        if !(200..=299).contains(&first.status) {
            let _ = self.delete(&name);
            return Err(StoreError::Io(format!(
                "{label}: the capability probe write returned {}{}",
                first.status,
                code_note(&first.body)
            )));
        }
        let second = self.put(&name, b"", Condition::Match("\"mokmok-probe-mismatch\""))?;
        // A store that cannot clean up must still open; the name carries no `.jsonl`.
        let _ = self.delete(&name);
        match second.status {
            409 | 412 => Ok(()),
            200..=299 => Err(StoreError::Invalid(format!(
                "{label}: the store accepted an If-Match it had to refuse, so a concurrent \
                 append could overwrite another writer; this adapter needs a store that \
                 honours a conditional write"
            ))),
            status => Err(StoreError::Io(format!(
                "{label}: the capability probe returned {status}{}",
                code_note(&second.body)
            ))),
        }
    }

    pub fn get(
        &self,
        name: &str,
    ) -> Result<GetOutcome, StoreError> {
        let url = self.url(Some(name), None);
        let response = self.send(Method::Get, &url, &[], &[])?;
        match response.status {
            200 => Ok(GetOutcome::Found {
                body: response.body,
                etag: response.etag,
            }),
            404 => match error_code(&response.body) {
                Some(code) if code == "NoSuchKey" => Ok(GetOutcome::Missing),
                Some(code) => Err(StoreError::Io(format!("{}: 404 {code}", self.label(name)))),
                None => Err(StoreError::Io(format!(
                    "{}: 404 with no error code",
                    self.label(name)
                ))),
            },
            status => Err(StoreError::Io(format!(
                "{}: GET returned {status}{}",
                self.label(name),
                code_note(&response.body)
            ))),
        }
    }

    pub fn put(
        &self,
        name: &str,
        body: &[u8],
        condition: Condition<'_>,
    ) -> Result<Response, StoreError> {
        let url = self.url(Some(name), None);
        let headers = match condition {
            Condition::Absent => vec![("if-none-match", "*")],
            Condition::Match(etag) => vec![("if-match", etag)],
        };
        self.send(Method::Put, &url, &headers, body)
    }

    pub fn delete(
        &self,
        name: &str,
    ) -> Result<(), StoreError> {
        let url = self.url(Some(name), None);
        let response = self.send(Method::Delete, &url, &[], &[])?;
        match response.status {
            200..=299 | 404 => Ok(()),
            status => Err(StoreError::Io(format!(
                "{}: DELETE returned {status}{}",
                self.label(name),
                code_note(&response.body)
            ))),
        }
    }

    pub fn list(
        &self,
        prefix: &str,
        token: Option<&str>,
    ) -> Result<Page, StoreError> {
        let mut pairs = vec![("list-type", "2"), ("max-keys", "1000"), ("prefix", prefix)];
        if let Some(token) = token {
            pairs.push(("continuation-token", token));
        }
        let query = encode_pairs(&pairs);
        let url = self.url(None, Some(&query));
        let response = self.send(Method::Get, &url, &[], &[])?;
        if !(200..=299).contains(&response.status) {
            return Err(StoreError::Io(format!(
                "/{}: the listing returned {}{}",
                self.bucket,
                response.status,
                code_note(&response.body)
            )));
        }
        listing::parse_page(&format!("/{}?{query}", self.bucket), &response.body)
    }

    fn url(
        &self,
        name: Option<&str>,
        query: Option<&str>,
    ) -> String {
        let mut url = format!("{}/{}", self.endpoint, self.bucket);
        if let Some(name) = name {
            url.push('/');
            if !self.prefix.is_empty() {
                url.push_str(&self.prefix);
                url.push('/');
            }
            url.push_str(name);
        }
        if let Some(query) = query {
            url.push('?');
            url.push_str(query);
        }
        url
    }

    fn send(
        &self,
        method: Method,
        url: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> Result<Response, StoreError> {
        let mut applied: Vec<(String, String)> = headers
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        applied.extend(self.sign(method.as_str(), url, headers, body)?);

        let result = match method {
            Method::Get => with_headers(self.agent.get(url), &applied).call(),
            Method::Delete => with_headers(self.agent.delete(url), &applied).call(),
            Method::Put => with_headers(self.agent.put(url), &applied).send(body),
        };
        let response = result.map_err(|error| StoreError::Io(format!("{url}: {error}")))?;
        let status = response.status().as_u16();
        let etag = response
            .headers()
            .get("etag")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let response_body = response
            .into_body()
            .read_to_vec()
            .map_err(|error| StoreError::Io(format!("{url}: cannot read the response: {error}")))?;
        Ok(Response {
            status,
            etag,
            body: response_body,
        })
    }

    fn sign(
        &self,
        method: &str,
        url: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> Result<Vec<(String, String)>, StoreError> {
        let identity: Identity = Credentials::new(
            self.credentials.access_key(),
            self.credentials.secret_key(),
            self.credentials.session_token().map(str::to_owned),
            None,
            "mokmok",
        )
        .into();
        let mut settings = SigningSettings::default();
        settings.payload_checksum_kind = PayloadChecksumKind::XAmzSha256;
        settings.uri_path_normalization_mode = UriPathNormalizationMode::Disabled;
        let params = aws_sigv4::sign::v4::SigningParams::builder()
            .identity(&identity)
            .region(self.region.as_str())
            .name("s3")
            .time(SystemTime::now())
            .settings(settings)
            .build()
            .map_err(|error| StoreError::Io(format!("{url}: cannot build the signer: {error}")))?
            .into();
        let request = SignableRequest::new(
            method,
            url,
            headers.iter().map(|(name, value)| (*name, *value)),
            SignableBody::Bytes(body),
        )
        .map_err(|error| StoreError::Io(format!("{url}: cannot sign the request: {error}")))?;
        let (instructions, _signature) = sign(request, &params)
            .map_err(|error| StoreError::Io(format!("{url}: cannot sign the request: {error}")))?
            .into_parts();
        Ok(instructions
            .headers()
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
            .collect())
    }
}

fn with_headers<T>(
    mut request: ureq::RequestBuilder<T>,
    headers: &[(String, String)],
) -> ureq::RequestBuilder<T> {
    for (name, value) in headers {
        request = request.header(name.as_str(), value.as_str());
    }
    request
}

fn normalize_endpoint(raw: &str) -> Result<String, StoreError> {
    let uri: Uri = raw.parse().map_err(|_| {
        StoreError::Invalid(format!(
            "the endpoint {raw:?} is not a scheme, host, and optional port"
        ))
    })?;
    let Some(scheme) = uri.scheme_str() else {
        return Err(StoreError::Invalid(format!(
            "the endpoint {raw:?} has no scheme"
        )));
    };
    if !matches!(scheme, "http" | "https") {
        return Err(StoreError::Invalid(format!(
            "the endpoint {raw:?} is not http or https"
        )));
    }
    let Some(authority) = uri.authority() else {
        return Err(StoreError::Invalid(format!(
            "the endpoint {raw:?} has no host"
        )));
    };
    if authority.host().is_empty() || authority.as_str().contains('@') {
        return Err(StoreError::Invalid(format!(
            "the endpoint {raw:?} has no usable host"
        )));
    }
    if !matches!(uri.path(), "" | "/") || uri.query().is_some() {
        return Err(StoreError::Invalid(format!(
            "the endpoint {raw:?} must end at the optional port"
        )));
    }
    Ok(format!("{scheme}://{authority}"))
}

fn validate_prefix(prefix: &str) -> Result<(), StoreError> {
    if prefix.is_empty() || prefix.split('/').all(is_plain_component) {
        return Ok(());
    }
    Err(StoreError::Invalid(format!(
        "the prefix {prefix:?} is not a set of plain path components"
    )))
}

fn is_plain_component(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~'))
}

fn encode_pairs(pairs: &[(&str, &str)]) -> String {
    let mut encoded: Vec<(String, String)> = pairs
        .iter()
        .map(|(name, value)| (percent_encode(name), percent_encode(value)))
        .collect();
    encoded.sort();
    encoded
        .into_iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("&")
}

fn percent_encode(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
    encoded
}

pub fn code_note(body: &[u8]) -> String {
    error_code(body).map_or_else(String::new, |code| format!(" ({code})"))
}

fn error_code(body: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(body);
    let start = text.find("<Code>")? + "<Code>".len();
    let rest = &text[start..];
    let end = rest.find("</Code>")?;
    Some(rest[..end].to_owned())
}

fn probe_name() -> String {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    format!(".mokmok-probe-{}-{nanos}", std::process::id())
}
