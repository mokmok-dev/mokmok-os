#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

mod common;

use std::num::NonZeroU32;
use std::time::Duration;

use common::{ACCESS_KEY, FakeS3, SECRET_KEY, SESSION_TOKEN};
use mokmok_events::{
    CloudEvent, EnumerableStore, EventStore, StoreError, StreamKey, Version, encode_records,
};
use mokmok_jobs_core::{JobEvent, JobId};
use mokmok_jobs_jsonl::JsonlEventStore;
use mokmok_jobs_s3::{S3Config, S3Credentials, S3EventStore};

const BUCKET: &str = "mokmok";
const PREFIX: &str = "jobs";

fn id(value: u128) -> JobId {
    JobId::from_u128(value)
}

fn submitted() -> JobEvent {
    JobEvent::Submitted {
        input: b"in".to_vec(),
        max_attempts: NonZeroU32::new(1).expect("literal is nonzero"),
    }
}

fn at(
    id: &JobId,
    version: u64,
    event: JobEvent,
) -> CloudEvent<JobEvent> {
    CloudEvent::new(id.as_str(), Version::new(version), event)
}

fn first(id: &JobId) -> CloudEvent<JobEvent> {
    at(id, 1, submitted())
}

fn started(id: &JobId) -> CloudEvent<JobEvent> {
    at(id, 2, JobEvent::Started { attempt: 1 })
}

fn name(id: &JobId) -> String {
    format!("{}.jsonl", id.as_str())
}

fn path(id: &JobId) -> String {
    format!("/{BUCKET}/{PREFIX}/{}", name(id))
}

fn object(
    fake: &FakeS3,
    id: &JobId,
) -> Option<Vec<u8>> {
    fake.object(BUCKET, &format!("{PREFIX}/{}", name(id)))
}

fn store(fake: &FakeS3) -> S3EventStore<JobId, JobEvent> {
    S3EventStore::open(fake.config(BUCKET, PREFIX)).expect("the store opens")
}

fn encoded(events: &[CloudEvent<JobEvent>]) -> Vec<u8> {
    let mut bytes = Vec::new();
    encode_records(events, &mut bytes).expect("records encode");
    bytes
}

#[test]
fn two_events_round_trip_through_append_load_and_keys() -> Result<(), Box<dyn std::error::Error>> {
    let fake = FakeS3::start();
    let store = store(&fake);
    let id = id(1);
    let events = [first(&id), started(&id)];
    assert_eq!(store.append(&id, Version::ZERO, &events)?, Version::new(2));
    assert_eq!(store.load(&id)?, events);
    assert_eq!(store.keys()?, vec![id]);
    Ok(())
}

#[test]
fn a_stale_append_reports_the_stored_version() -> Result<(), Box<dyn std::error::Error>> {
    let fake = FakeS3::start();
    let store = store(&fake);
    let id = id(2);
    store.append(&id, Version::ZERO, &[first(&id)])?;
    let error = store
        .append(&id, Version::ZERO, &[first(&id)])
        .expect_err("a stale append is refused");
    assert!(matches!(
        error,
        StoreError::Conflict {
            expected,
            actual,
        } if expected == Version::ZERO && actual == Version::new(1)
    ));
    assert_eq!(store.load(&id)?.len(), 1);
    Ok(())
}

#[test]
fn a_record_for_another_stream_is_refused_and_leaves_the_object_untouched()
-> Result<(), Box<dyn std::error::Error>> {
    let fake = FakeS3::start();
    let store = store(&fake);
    let id = id(3);
    let other = JobId::from_u128(4);
    let error = store
        .append(&id, Version::ZERO, &[first(&other)])
        .expect_err("a record from another stream is refused");
    assert!(matches!(error, StoreError::Invalid(_)));
    assert!(object(&fake, &id).is_none());
    assert_eq!(store.load(&id)?.len(), 0);
    Ok(())
}

#[test]
fn a_skipped_version_is_refused() {
    let fake = FakeS3::start();
    let store = store(&fake);
    let id = id(5);
    let error = store
        .append(&id, Version::ZERO, &[started(&id)])
        .expect_err("a skipped version is refused");
    assert!(matches!(error, StoreError::Invalid(_)));
    assert!(object(&fake, &id).is_none());
}

#[test]
fn an_empty_append_creates_no_object() -> Result<(), Box<dyn std::error::Error>> {
    let fake = FakeS3::start();
    let store = store(&fake);
    let id = id(6);
    assert_eq!(store.append(&id, Version::ZERO, &[])?, Version::ZERO);
    assert!(object(&fake, &id).is_none());
    store.append(&id, Version::ZERO, &[first(&id)])?;
    assert_eq!(store.append(&id, Version::new(1), &[])?, Version::new(1));
    Ok(())
}

#[test]
fn a_create_that_races_a_committed_object_is_a_conflict() -> Result<(), Box<dyn std::error::Error>>
{
    let fake = FakeS3::start();
    let store = store(&fake);
    let id = id(7);
    fake.race_next_put(
        BUCKET,
        &format!("{PREFIX}/{}", name(&id)),
        &encoded(&[first(&id)]),
    );
    let error = store
        .append(&id, Version::ZERO, &[first(&id)])
        .expect_err("the refused create is a conflict");
    assert!(matches!(
        error,
        StoreError::Conflict {
            expected,
            actual,
        } if expected == Version::ZERO && actual == Version::new(1)
    ));
    assert_eq!(store.load(&id)?.len(), 1);
    let put = fake
        .log()
        .into_iter()
        .find(|request| request.method == "PUT" && request.path == path(&id))
        .expect("the refused create is logged");
    assert_eq!(put.if_none_match.as_deref(), Some("*"));
    Ok(())
}

#[test]
fn a_refused_put_is_reported_as_a_conflict_with_the_stored_version()
-> Result<(), Box<dyn std::error::Error>> {
    let fake = FakeS3::start();
    let store = store(&fake);
    let id = id(8);
    store.append(&id, Version::ZERO, &[first(&id)])?;
    fake.refuse_next_put();
    let error = store
        .append(&id, Version::new(1), &[started(&id)])
        .expect_err("the refused append is a conflict");
    assert!(matches!(
        error,
        StoreError::Conflict {
            expected,
            actual,
        } if expected == Version::new(1) && actual == Version::new(1)
    ));
    let put = fake
        .log()
        .into_iter()
        .find(|request| {
            request.method == "PUT" && request.path == path(&id) && request.if_match.is_some()
        })
        .expect("the refused append is logged");
    assert_eq!(put.status, 412);
    Ok(())
}

#[test]
fn a_missing_stream_loads_empty() -> Result<(), Box<dyn std::error::Error>> {
    let fake = FakeS3::start();
    let store = store(&fake);
    let id = id(9);
    assert_eq!(store.load(&id)?, Vec::<CloudEvent<JobEvent>>::new());
    assert_eq!(store.keys()?, Vec::<JobId>::new());
    Ok(())
}

#[test]
fn a_torn_tail_is_ignored_and_dropped_by_the_next_append() -> Result<(), Box<dyn std::error::Error>>
{
    let fake = FakeS3::start();
    let store = store(&fake);
    let id = id(10);
    let mut torn = encoded(&[first(&id)]);
    torn.extend_from_slice(b"{\"specversion\"");
    fake.put_object(BUCKET, &format!("{PREFIX}/{}", name(&id)), &torn);
    assert_eq!(store.load(&id)?, vec![first(&id)]);
    assert_eq!(
        store.append(&id, Version::new(1), &[started(&id)])?,
        Version::new(2)
    );
    assert_eq!(
        object(&fake, &id).expect("the object exists"),
        encoded(&[first(&id), started(&id)])
    );
    assert_eq!(store.load(&id)?.len(), 2);
    Ok(())
}

#[test]
fn keys_walks_every_page_and_skips_stray_objects() -> Result<(), Box<dyn std::error::Error>> {
    let fake = FakeS3::start();
    let store = store(&fake);
    fake.page_size(2);
    let ids: Vec<JobId> = (11..=15).map(id).collect();
    for id in &ids {
        store.append(id, Version::ZERO, &[first(id)])?;
    }
    fake.put_object(BUCKET, &format!("{PREFIX}/notes.txt"), b"stray");
    fake.put_object(
        BUCKET,
        &format!("{PREFIX}/{}.jsonl", ids[0].as_str().to_uppercase()),
        b"",
    );
    assert_eq!(store.keys()?, ids);
    let listings = fake
        .log()
        .iter()
        .filter(|request| request.query.contains("list-type=2"))
        .count();
    assert!(listings > 1, "the store made {listings} listing calls");
    Ok(())
}

#[test]
fn a_missing_bucket_is_an_error_not_an_empty_stream() {
    let fake = FakeS3::start();
    let store = store(&fake);
    let id = id(16);
    fake.force_response(&path(&id), 404, "<Error><Code>NoSuchBucket</Code></Error>");
    let error = store
        .load(&id)
        .expect_err("a missing bucket is not an empty stream");
    assert!(matches!(error, StoreError::Io(_)));
    assert!(error.to_string().contains("NoSuchBucket"), "{error}");
}

#[test]
fn an_object_without_an_etag_cannot_be_read() -> Result<(), Box<dyn std::error::Error>> {
    let fake = FakeS3::start();
    let store = store(&fake);
    let id = id(17);
    store.append(&id, Version::ZERO, &[first(&id)])?;
    fake.omit_etag();
    let error = store
        .load(&id)
        .expect_err("an unfenceable object is refused");
    assert!(matches!(error, StoreError::Io(_)));
    assert!(error.to_string().contains("ETag"), "{error}");
    Ok(())
}

#[test]
fn open_refuses_a_store_that_ignores_conditional_writes() {
    let fake = FakeS3::start();
    fake.ignore_conditionals();
    let error = S3EventStore::<JobId, JobEvent>::open(fake.config(BUCKET, PREFIX))
        .expect_err("a store without conditional writes is refused");
    assert!(matches!(error, StoreError::Invalid(_)));
    assert!(error.to_string().contains("conditional"), "{error}");
}

#[test]
fn open_deletes_its_probe_object() -> Result<(), Box<dyn std::error::Error>> {
    let fake = FakeS3::start();
    let store = store(&fake);
    assert_eq!(store.keys()?, Vec::<JobId>::new());
    assert!(
        fake.log()
            .iter()
            .any(|request| request.method == "DELETE" && request.path.contains(".mokmok-probe-"))
    );
    assert!(
        fake.object_names(BUCKET)
            .iter()
            .all(|name| !name.contains(".mokmok-probe-"))
    );
    Ok(())
}

#[test]
fn a_wrong_secret_cannot_open_but_a_signed_request_can() -> Result<(), Box<dyn std::error::Error>> {
    let fake = FakeS3::start();
    let id = id(18);
    store(&fake).append(&id, Version::ZERO, &[first(&id)])?;
    let mut config = fake.config(BUCKET, PREFIX);
    config.credentials = S3Credentials::new(ACCESS_KEY, "wrong-secret", None);
    let error = S3EventStore::<JobId, JobEvent>::open(config)
        .expect_err("the fake rejects the wrong signature");
    assert!(matches!(error, StoreError::Io(_)));
    assert!(fake.log().iter().any(|request| request.status == 403));
    Ok(())
}

#[test]
fn a_server_error_is_reported_as_io() {
    let fake = FakeS3::start();
    let store = store(&fake);
    let id = id(19);
    fake.force_response(&path(&id), 500, "<Error><Code>InternalError</Code></Error>");
    let error = store.load(&id).expect_err("a 500 is an error");
    assert!(matches!(error, StoreError::Io(_)));
    assert!(error.to_string().contains("500"), "{error}");
    assert!(error.to_string().contains("InternalError"), "{error}");
    assert!(error.to_string().contains(&path(&id)), "{error}");
}

#[test]
fn a_slow_server_times_out_as_io() -> Result<(), Box<dyn std::error::Error>> {
    let fake = FakeS3::start();
    let mut config = fake.config(BUCKET, PREFIX);
    config.timeout = Duration::from_millis(100);
    let store = S3EventStore::<JobId, JobEvent>::open(config)?;
    fake.sleep(Duration::from_millis(300));
    let error = store.load(&id(20)).expect_err("a slow server times out");
    assert!(matches!(error, StoreError::Io(_)));
    Ok(())
}

#[test]
fn credentials_debug_redacts_the_secret_and_token() {
    let credentials = S3Credentials::new(
        "AKIAIOSFODNN7EXAMPLE",
        "secret-key-value",
        Some("session-token-value".to_owned()),
    );
    let debug = format!("{credentials:?}");
    assert!(debug.contains("AKIA"), "{debug}");
    assert!(!debug.contains("EXAMPLE"), "{debug}");
    assert!(!debug.contains("secret-key-value"), "{debug}");
    assert!(!debug.contains("session-token-value"), "{debug}");
}

#[test]
fn store_debug_names_the_store_and_never_prints_the_secret() {
    let fake = FakeS3::start();
    let store = store(&fake);
    let debug = format!("{store:?}");
    assert!(debug.contains("S3EventStore"), "{debug}");
    assert!(debug.contains(BUCKET), "{debug}");
    assert!(!debug.contains(SECRET_KEY), "{debug}");
}

#[test]
fn requests_use_the_path_style_object_path_and_sign_the_session_token()
-> Result<(), Box<dyn std::error::Error>> {
    let fake = FakeS3::with_credentials(ACCESS_KEY, SECRET_KEY, Some(SESSION_TOKEN));
    let prefix = "jobs/2026";
    let store = S3EventStore::<JobId, JobEvent>::open(fake.config(BUCKET, prefix))?;
    let id = id(21);
    store.append(&id, Version::ZERO, &[first(&id)])?;
    assert_eq!(store.load(&id)?.len(), 1);
    let expected = format!("/{BUCKET}/{prefix}/{}", name(&id));
    let put = fake
        .log()
        .into_iter()
        .find(|request| request.method == "PUT" && request.path == expected)
        .expect("the append is logged");
    assert_eq!(put.session_token.as_deref(), Some(SESSION_TOKEN));
    assert!(
        put.signed_headers
            .iter()
            .any(|name| name == "x-amz-security-token")
    );
    Ok(())
}

#[test]
fn a_listing_that_repeats_a_token_is_refused() {
    let fake = FakeS3::start();
    let store = store(&fake);
    fake.stuck_token();
    let error = store.keys().expect_err("a repeated token is refused");
    assert!(matches!(error, StoreError::Io(_)));
    assert!(error.to_string().contains("repeated"), "{error}");
    assert!(error.to_string().contains(BUCKET), "{error}");
}

#[test]
fn a_listing_with_no_names_in_the_first_page_still_finishes()
-> Result<(), Box<dyn std::error::Error>> {
    let fake = FakeS3::start();
    let store = store(&fake);
    let id = id(22);
    store.append(&id, Version::ZERO, &[first(&id)])?;
    fake.empty_first_page();
    assert_eq!(store.keys()?, vec![id]);
    Ok(())
}

#[test]
fn a_listing_decodes_an_xml_entity_in_a_key() -> Result<(), Box<dyn std::error::Error>> {
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    struct AnyKey(String);

    impl StreamKey for AnyKey {
        fn as_str(&self) -> &str {
            &self.0
        }

        fn parse_key(raw: &str) -> Result<Self, StoreError> {
            Ok(Self(raw.to_owned()))
        }
    }

    let fake = FakeS3::start();
    let store = S3EventStore::<AnyKey, JobEvent>::open(fake.config(BUCKET, PREFIX))?;
    fake.put_object(BUCKET, &format!("{PREFIX}/a&b.jsonl"), b"");
    assert_eq!(store.keys()?, vec![AnyKey("a&b".to_owned())]);
    Ok(())
}

#[test]
fn open_rejects_a_malformed_config() {
    let fake = FakeS3::start();
    let valid = fake.config(BUCKET, PREFIX);
    let cases: Vec<(S3Config, &str)> = vec![
        (
            {
                let mut config = valid.clone();
                config.endpoint = "ftp://host".to_owned();
                config
            },
            "http",
        ),
        (
            {
                let mut config = valid.clone();
                config.endpoint = "http://".to_owned();
                config
            },
            "endpoint",
        ),
        (
            {
                let mut config = valid.clone();
                config.endpoint = "http://host/path".to_owned();
                config
            },
            "endpoint",
        ),
        (
            {
                let mut config = valid.clone();
                config.endpoint = "http://user:pass@host".to_owned();
                config
            },
            "endpoint",
        ),
        (
            {
                let mut config = valid.clone();
                config.bucket = "bad/bucket".to_owned();
                config
            },
            "bucket",
        ),
        (
            {
                let mut config = valid.clone();
                config.prefix = "jobs/../x".to_owned();
                config
            },
            "prefix",
        ),
        (
            {
                let mut config = valid.clone();
                config.region = String::new();
                config
            },
            "region",
        ),
        (
            {
                let mut config = valid;
                config.timeout = Duration::ZERO;
                config
            },
            "timeout",
        ),
    ];
    for (config, needle) in cases {
        let error = S3EventStore::<JobId, JobEvent>::open(config)
            .expect_err("the malformed config is refused");
        assert!(matches!(error, StoreError::Invalid(_)), "{error}");
        assert!(error.to_string().contains(needle), "{error}");
    }
}

fn scenario<S>(store: &S) -> Result<(Version, Vec<Version>, Vec<JobId>), StoreError>
where
    S: EventStore<JobId, JobEvent> + EnumerableStore<JobId, JobEvent>,
{
    let id = id(23);
    let version = store.append(&id, Version::ZERO, &[first(&id), started(&id)])?;
    let loaded: Vec<Version> = store.load(&id)?.iter().map(|event| event.version).collect();
    store.append(
        &id,
        version,
        &[at(
            &id,
            3,
            JobEvent::Succeeded {
                output: b"out".to_vec(),
            },
        )],
    )?;
    Ok((version, loaded, store.keys()?))
}

#[test]
fn both_backends_agree_on_the_same_scenario() -> Result<(), Box<dyn std::error::Error>> {
    let fake = FakeS3::start();
    let directory = tempfile::tempdir()?;
    let jsonl = JsonlEventStore::<JobId, JobEvent>::open(directory.path().join("jobs"))?;
    assert_eq!(scenario(&jsonl)?, scenario(&store(&fake))?);
    Ok(())
}
