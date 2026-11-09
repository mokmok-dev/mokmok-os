#![allow(dead_code)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

//! A fake `S3` that verifies `SigV4` on its own, so every store test also proves the
//! wire signature.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::Response;
use axum::routing::any;
use hmac::{Hmac, Mac};
use mokmok_jobs_s3::{S3Config, S3Credentials};
use sha2::{Digest, Sha256};
use tokio::sync::watch;

pub const ACCESS_KEY: &str = "AKIAFAKEACCESSKEY";
pub const SECRET_KEY: &str = "fake-secret-key-do-not-log";
pub const SESSION_TOKEN: &str = "fake-session-token";
pub const REGION: &str = "us-east-1";

const MAX_BODY: usize = 1 << 20;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Knob {
    IgnoreConditionals,
    OmitEtag,
    RefuseNextPut,
    EmptyFirstPage,
    StuckToken,
}

struct Object {
    bytes: Vec<u8>,
    etag: String,
}

struct ForcedResponse {
    path: String,
    status: u16,
    body: String,
}

struct Credentials {
    access_key: String,
    secret_key: String,
    session_token: Option<String>,
    region: String,
}

struct Knobs {
    flags: BTreeSet<Knob>,
    page_size: usize,
    race_next_put: Option<(String, Vec<u8>)>,
    sleep: Option<Duration>,
    forced: Option<ForcedResponse>,
}

struct FakeState {
    objects: BTreeMap<String, Object>,
    log: Vec<LoggedRequest>,
    credentials: Credentials,
    knobs: Knobs,
    etag_counter: u64,
}

#[derive(Clone, Debug)]
pub struct LoggedRequest {
    pub method: String,
    pub path: String,
    pub query: String,
    pub status: u16,
    pub signed_headers: Vec<String>,
    pub session_token: Option<String>,
    pub if_match: Option<String>,
    pub if_none_match: Option<String>,
}

pub struct FakeS3 {
    base_url: String,
    state: Arc<Mutex<FakeState>>,
    access_key: String,
    secret_key: String,
    session_token: Option<String>,
    stop: Option<watch::Sender<bool>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl FakeS3 {
    pub fn start() -> Self {
        Self::with_credentials(ACCESS_KEY, SECRET_KEY, None)
    }

    pub fn with_credentials(
        access_key: &str,
        secret_key: &str,
        session_token: Option<&str>,
    ) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("the fake binds");
        listener
            .set_nonblocking(true)
            .expect("the fake listener is nonblocking");
        let address = listener.local_addr().expect("the fake has an address");
        let base_url = format!("http://{address}");
        let state = Arc::new(Mutex::new(FakeState {
            objects: BTreeMap::new(),
            log: Vec::new(),
            credentials: Credentials {
                access_key: access_key.to_owned(),
                secret_key: secret_key.to_owned(),
                session_token: session_token.map(str::to_owned),
                region: REGION.to_owned(),
            },
            knobs: Knobs {
                flags: BTreeSet::new(),
                page_size: 2,
                race_next_put: None,
                sleep: None,
                forced: None,
            },
            etag_counter: 0,
        }));
        let app = Router::new()
            .fallback(any(handle))
            .with_state(Arc::clone(&state));
        let (stop, mut stopped) = watch::channel(false);
        let thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("the fake runtime builds");
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener)
                    .expect("the fake listener converts");
                let _ = axum::serve(listener, app)
                    .with_graceful_shutdown(async move {
                        let _ = stopped.changed().await;
                    })
                    .await;
            });
        });
        Self {
            base_url,
            state,
            access_key: access_key.to_owned(),
            secret_key: secret_key.to_owned(),
            session_token: session_token.map(str::to_owned),
            stop: Some(stop),
            thread: Some(thread),
        }
    }

    /// A config for `bucket` and `prefix` that carries this fake's credentials.
    #[must_use]
    pub fn config(
        &self,
        bucket: &str,
        prefix: &str,
    ) -> S3Config {
        S3Config {
            endpoint: self.base_url.clone(),
            bucket: bucket.to_owned(),
            region: REGION.to_owned(),
            prefix: prefix.to_owned(),
            credentials: S3Credentials::new(
                self.access_key.clone(),
                self.secret_key.clone(),
                self.session_token.clone(),
            ),
            timeout: Duration::from_secs(5),
        }
    }

    pub fn put_object(
        &self,
        bucket: &str,
        name: &str,
        bytes: &[u8],
    ) {
        let mut state = self.state();
        let etag = state.next_etag();
        state.objects.insert(
            format!("{bucket}/{name}"),
            Object {
                bytes: bytes.to_vec(),
                etag,
            },
        );
        drop(state);
    }

    #[must_use]
    pub fn object(
        &self,
        bucket: &str,
        name: &str,
    ) -> Option<Vec<u8>> {
        self.state()
            .objects
            .get(&format!("{bucket}/{name}"))
            .map(|object| object.bytes.clone())
    }

    #[must_use]
    pub fn object_names(
        &self,
        bucket: &str,
    ) -> Vec<String> {
        let prefix = format!("{bucket}/");
        self.state()
            .objects
            .keys()
            .filter_map(|key| key.strip_prefix(&prefix).map(str::to_owned))
            .collect()
    }

    #[must_use]
    pub fn log(&self) -> Vec<LoggedRequest> {
        self.state().log.clone()
    }

    pub fn ignore_conditionals(&self) {
        self.state().knobs.flags.insert(Knob::IgnoreConditionals);
    }

    pub fn omit_etag(&self) {
        self.state().knobs.flags.insert(Knob::OmitEtag);
    }

    pub fn refuse_next_put(&self) {
        self.state().knobs.flags.insert(Knob::RefuseNextPut);
    }

    pub fn empty_first_page(&self) {
        self.state().knobs.flags.insert(Knob::EmptyFirstPage);
    }

    pub fn stuck_token(&self) {
        self.state().knobs.flags.insert(Knob::StuckToken);
    }

    pub fn race_next_put(
        &self,
        bucket: &str,
        name: &str,
        bytes: &[u8],
    ) {
        self.state().knobs.race_next_put = Some((format!("{bucket}/{name}"), bytes.to_vec()));
    }

    pub fn sleep(
        &self,
        duration: Duration,
    ) {
        self.state().knobs.sleep = Some(duration);
    }

    pub fn force_response(
        &self,
        path: &str,
        status: u16,
        body: &str,
    ) {
        self.state().knobs.forced = Some(ForcedResponse {
            path: path.to_owned(),
            status,
            body: body.to_owned(),
        });
    }

    pub fn page_size(
        &self,
        size: usize,
    ) {
        self.state().knobs.page_size = size;
    }

    fn state(&self) -> MutexGuard<'_, FakeState> {
        self.state.lock().expect("the fake state is not poisoned")
    }
}

impl Drop for FakeS3 {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(true);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl FakeState {
    fn flag(
        &self,
        knob: Knob,
    ) -> bool {
        self.knobs.flags.contains(&knob)
    }

    fn set_flag(
        &mut self,
        knob: Knob,
        value: bool,
    ) {
        if value {
            self.knobs.flags.insert(knob);
        } else {
            self.knobs.flags.remove(&knob);
        }
    }

    fn next_etag(&mut self) -> String {
        self.etag_counter += 1;
        format!("\"{:016x}\"", self.etag_counter)
    }
}

struct FakeResponse {
    status: u16,
    body: Vec<u8>,
    content_type: &'static str,
    etag: Option<String>,
}

fn error_response(
    status: u16,
    code: &str,
) -> FakeResponse {
    FakeResponse {
        status,
        body: format!("<Error><Code>{code}</Code></Error>").into_bytes(),
        content_type: "application/xml",
        etag: None,
    }
}

async fn handle(
    State(shared): State<Arc<Mutex<FakeState>>>,
    request: Request,
) -> Response {
    let method = request.method().as_str().to_owned();
    let uri = request.uri().clone();
    let headers = request.headers().clone();
    let body = axum::body::to_bytes(request.into_body(), MAX_BODY)
        .await
        .map_or_else(|_| Vec::new(), |bytes| bytes.to_vec());

    let delay = shared
        .lock()
        .expect("the fake state is not poisoned")
        .knobs
        .sleep;
    if let Some(delay) = delay {
        tokio::time::sleep(delay).await;
    }

    let mut state = shared.lock().expect("the fake state is not poisoned");
    let parsed = parse_authorization(&headers);
    let response = if verify_signature(&state, &method, &uri, &headers, parsed.as_ref()).is_ok() {
        dispatch(&mut state, &method, &uri, &headers, &body)
    } else {
        error_response(403, "SignatureDoesNotMatch")
    };
    state.log.push(LoggedRequest {
        method,
        path: uri.path().to_owned(),
        query: uri.query().unwrap_or_default().to_owned(),
        status: response.status,
        signed_headers: parsed
            .as_ref()
            .map(|parsed| parsed.signed_headers.clone())
            .unwrap_or_default(),
        session_token: header_text(&headers, "x-amz-security-token"),
        if_match: header_text(&headers, "if-match"),
        if_none_match: header_text(&headers, "if-none-match"),
    });
    drop(state);

    let mut builder = Response::builder()
        .status(StatusCode::from_u16(response.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR))
        .header(header::CONTENT_TYPE, response.content_type);
    if let Some(etag) = response.etag {
        builder = builder.header(header::ETAG, etag);
    }
    builder
        .body(Body::from(response.body))
        .expect("the fake response builds")
}

fn dispatch(
    state: &mut FakeState,
    method: &str,
    uri: &Uri,
    headers: &HeaderMap,
    body: &[u8],
) -> FakeResponse {
    let path = uri.path().to_owned();
    if let Some(forced) = state.knobs.forced.take_if(|forced| forced.path == path) {
        return FakeResponse {
            status: forced.status,
            body: forced.body.into_bytes(),
            content_type: "application/xml",
            etag: None,
        };
    }
    let Some((bucket, name)) = split_path(uri.path()) else {
        return error_response(400, "InvalidRequest");
    };
    match method {
        "GET"
            if query_param(uri.query().unwrap_or_default(), "list-type").as_deref()
                == Some("2") =>
        {
            list_objects(state, bucket, uri.query().unwrap_or_default())
        },
        "GET" => get_object(state, bucket, name),
        "PUT" => put_object(state, bucket, name, headers, body),
        "DELETE" => {
            state.objects.remove(&format!("{bucket}/{name}"));
            FakeResponse {
                status: 204,
                body: Vec::new(),
                content_type: "application/xml",
                etag: None,
            }
        },
        _ => error_response(405, "MethodNotAllowed"),
    }
}

fn split_path(path: &str) -> Option<(&str, &str)> {
    let rest = path.strip_prefix('/')?;
    let (bucket, name) = rest
        .split_once('/')
        .map_or((rest, ""), |(bucket, name)| (bucket, name));
    (!bucket.is_empty()).then_some((bucket, name))
}

fn get_object(
    state: &FakeState,
    bucket: &str,
    name: &str,
) -> FakeResponse {
    state.objects.get(&format!("{bucket}/{name}")).map_or_else(
        || error_response(404, "NoSuchKey"),
        |object| FakeResponse {
            status: 200,
            body: object.bytes.clone(),
            content_type: "application/octet-stream",
            etag: (!state.flag(Knob::OmitEtag)).then(|| object.etag.clone()),
        },
    )
}

fn put_object(
    state: &mut FakeState,
    bucket: &str,
    name: &str,
    headers: &HeaderMap,
    body: &[u8],
) -> FakeResponse {
    let key = format!("{bucket}/{name}");
    if state.flag(Knob::RefuseNextPut) {
        state.set_flag(Knob::RefuseNextPut, false);
        return error_response(412, "PreconditionFailed");
    }
    if let Some((race_key, bytes)) = state.knobs.race_next_put.take_if(|(race, _)| *race == key) {
        let etag = state.next_etag();
        state.objects.insert(race_key, Object { bytes, etag });
    }
    if !state.flag(Knob::IgnoreConditionals) {
        let absent = header_text(headers, "if-none-match").as_deref() == Some("*");
        if absent && state.objects.contains_key(&key) {
            return error_response(412, "PreconditionFailed");
        }
        if let Some(expected) = header_text(headers, "if-match") {
            let matches = state
                .objects
                .get(&key)
                .is_some_and(|object| object.etag == expected);
            if !matches {
                return error_response(412, "PreconditionFailed");
            }
        }
    }
    let etag = state.next_etag();
    state.objects.insert(
        key,
        Object {
            bytes: body.to_vec(),
            etag: etag.clone(),
        },
    );
    FakeResponse {
        status: 200,
        body: Vec::new(),
        content_type: "application/xml",
        etag: Some(etag),
    }
}

fn list_objects(
    state: &mut FakeState,
    bucket: &str,
    query: &str,
) -> FakeResponse {
    let prefix = query_param(query, "prefix").unwrap_or_default();
    if state.flag(Knob::StuckToken) {
        return listing_page(
            bucket,
            &prefix,
            state.knobs.page_size,
            &[],
            true,
            Some("stuck"),
        );
    }
    let bucket_prefix = format!("{bucket}/");
    let mut names: Vec<String> = state
        .objects
        .keys()
        .filter_map(|key| key.strip_prefix(&bucket_prefix))
        .filter(|name| name.starts_with(&prefix))
        .map(str::to_owned)
        .collect();
    names.sort();
    let page_size = state.knobs.page_size.max(1);
    let token = query_param(query, "continuation-token");
    if token.is_none() && state.flag(Knob::EmptyFirstPage) && !names.is_empty() {
        state.set_flag(Knob::EmptyFirstPage, false);
        return listing_page(bucket, &prefix, page_size, &[], true, Some("0"));
    }
    let start = token
        .and_then(|token| token.parse::<usize>().ok())
        .unwrap_or(0)
        .min(names.len());
    let end = (start + page_size).min(names.len());
    let truncated = end < names.len();
    let next = truncated.then(|| end.to_string());
    listing_page(
        bucket,
        &prefix,
        page_size,
        &names[start..end],
        truncated,
        next.as_deref(),
    )
}

fn listing_page(
    bucket: &str,
    prefix: &str,
    page_size: usize,
    names: &[String],
    truncated: bool,
    token: Option<&str>,
) -> FakeResponse {
    let mut xml = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?><ListBucketResult>");
    let _ = write!(
        xml,
        "<Name>{}</Name><Prefix>{}</Prefix><KeyCount>{}</KeyCount><MaxKeys>{page_size}</MaxKeys><IsTruncated>{truncated}</IsTruncated>",
        escape(bucket),
        escape(prefix),
        names.len()
    );
    if let Some(token) = token {
        let _ = write!(
            xml,
            "<NextContinuationToken>{}</NextContinuationToken>",
            escape(token)
        );
    }
    for name in names {
        let _ = write!(xml, "<Contents><Key>{}</Key></Contents>", escape(name));
    }
    xml.push_str("</ListBucketResult>");
    FakeResponse {
        status: 200,
        body: xml.into_bytes(),
        content_type: "application/xml",
        etag: None,
    }
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

struct ParsedAuthorization {
    access_key: String,
    scope: String,
    signed_headers: Vec<String>,
    signature: String,
}

fn parse_authorization(headers: &HeaderMap) -> Option<ParsedAuthorization> {
    let value = header_text(headers, "authorization")?;
    let rest = value.strip_prefix("AWS4-HMAC-SHA256 ")?;
    let mut access_key = None;
    let mut scope = None;
    let mut signed_headers = None;
    let mut signature = None;
    for part in rest.split(',') {
        let part = part.trim();
        if let Some(credential) = part.strip_prefix("Credential=") {
            let (key, scope_text) = credential.split_once('/')?;
            access_key = Some(key.to_owned());
            scope = Some(scope_text.to_owned());
        } else if let Some(list) = part.strip_prefix("SignedHeaders=") {
            signed_headers = Some(list.split(';').map(str::to_owned).collect());
        } else if let Some(hex) = part.strip_prefix("Signature=") {
            signature = Some(hex.to_owned());
        }
    }
    Some(ParsedAuthorization {
        access_key: access_key?,
        scope: scope?,
        signed_headers: signed_headers?,
        signature: signature?,
    })
}

fn verify_signature(
    state: &FakeState,
    method: &str,
    uri: &Uri,
    headers: &HeaderMap,
    parsed: Option<&ParsedAuthorization>,
) -> Result<(), String> {
    let parsed = parsed.ok_or("the request carries no signature")?;
    if parsed.access_key != state.credentials.access_key {
        return Err("unknown access key".to_owned());
    }
    let scope: Vec<&str> = parsed.scope.split('/').collect();
    let [date, region, service, terminal] = scope.as_slice() else {
        return Err("malformed credential scope".to_owned());
    };
    if *region != state.credentials.region.as_str()
        || *service != "s3"
        || *terminal != "aws4_request"
    {
        return Err("unexpected credential scope".to_owned());
    }
    let amz_date = header_text(headers, "x-amz-date").ok_or("missing x-amz-date")?;
    let payload = header_text(headers, "x-amz-content-sha256").ok_or("missing payload hash")?;
    if !amz_date.starts_with(*date) {
        return Err("x-amz-date disagrees with the scope".to_owned());
    }
    match &state.credentials.session_token {
        Some(expected) => {
            if header_text(headers, "x-amz-security-token").as_deref() != Some(expected) {
                return Err("wrong session token".to_owned());
            }
            if !parsed
                .signed_headers
                .iter()
                .any(|name| name == "x-amz-security-token")
            {
                return Err("the session token is not signed".to_owned());
            }
        },
        None => {
            if headers.contains_key("x-amz-security-token") {
                return Err("unexpected session token".to_owned());
            }
        },
    }
    if !parsed.signed_headers.iter().any(|name| name == "host") {
        return Err("host is not signed".to_owned());
    }

    let mut canonical_headers = String::new();
    for name in &parsed.signed_headers {
        let mut values = Vec::new();
        for value in headers.get_all(name.as_str()) {
            values.push(normalize(value.to_str().map_err(|_| "non-text header")?));
        }
        if values.is_empty() {
            return Err(format!("signed header {name} is absent"));
        }
        let joined = values.join(",");
        let _ = writeln!(canonical_headers, "{name}:{joined}");
    }
    let signed_names = parsed.signed_headers.join(";");
    let canonical = format!(
        "{method}\n{}\n{}\n{canonical_headers}\n{signed_names}\n{payload}",
        uri.path(),
        canonical_query(uri.query().unwrap_or_default())
    );
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{date}/{region}/{service}/aws4_request\n{}",
        sha256_hex(canonical.as_bytes())
    );
    let secret = &state.credentials.secret_key;
    let mut key = hmac_sha256(format!("AWS4{secret}").as_bytes(), date.as_bytes());
    key = hmac_sha256(&key, region.as_bytes());
    key = hmac_sha256(&key, service.as_bytes());
    key = hmac_sha256(&key, b"aws4_request");
    let expected = hex::encode(hmac_sha256(&key, string_to_sign.as_bytes()));
    if expected != parsed.signature.to_lowercase() {
        return Err("signature mismatch".to_owned());
    }
    Ok(())
}

fn normalize(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn canonical_query(query: &str) -> String {
    let mut pairs: Vec<(String, String)> = query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (
                percent_encode(&percent_decode(key)),
                percent_encode(&percent_decode(value)),
            )
        })
        .collect();
    pairs.sort();
    pairs
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("&")
}

fn query_param(
    query: &str,
    name: &str,
) -> Option<String> {
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find_map(|(key, value)| (percent_decode(key) == name).then(|| percent_decode(value)))
}

fn header_text(
    headers: &HeaderMap,
    name: &str,
) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

fn hmac_sha256(
    key: &[u8],
    data: &[u8],
) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("hmac accepts any key");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
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

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                let high = char::from(bytes[index + 1]).to_digit(16);
                let low = char::from(bytes[index + 2]).to_digit(16);
                if let (Some(high), Some(low)) = (high, low) {
                    decoded.push(u8::try_from(high * 16 + low).expect("two hex digits fit"));
                    index += 3;
                } else {
                    decoded.push(b'%');
                    index += 1;
                }
            },
            b'+' => {
                decoded.push(b' ');
                index += 1;
            },
            byte => {
                decoded.push(byte);
                index += 1;
            },
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}
