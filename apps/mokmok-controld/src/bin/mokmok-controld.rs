//! The control-plane daemon.

use std::env::VarError;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::{ArgGroup, Parser, ValueEnum};
use mokmok_control::{
    AcpRunner, AgentRunner, AgentTable, ControlEvent, EnumerableStore, PluginHost, PluginRunner,
    Runner, ScriptedRunner, TaskId,
};
use mokmok_controld::{ControlConfig, allowed_hosts, build_router, build_state};
use mokmok_jobs_jsonl::JsonlEventStore;
use mokmok_jobs_s3::{S3Config, S3Credentials, S3EventStore};
use mokmok_journal::{
    DEFAULT_KEEP_SEGMENTS, DEFAULT_MAX_AGE, DEFAULT_MAX_BYTES, FileJournal, Journal, JournalConfig,
    JournalEvent,
};
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

const AGENT_TIMEOUT: Duration = Duration::from_secs(600);
const SCRIPTED_SUMMARY: &str = "the scripted run completed";

// A connection that outlives the shutdown watch must not hold the process open, but the
// bound starts with the signal: a daemon that never receives one runs until it does.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum StoreKind {
    File,
    S3,
}

#[derive(Parser)]
#[command(name = "mokmok-controld", about = "Serve the mokmok control plane")]
#[command(group(ArgGroup::new("source").required(true).args(["agents", "scripted"])))]
struct Args {
    /// Root of the local data; the journal lives in `<data-dir>/journal`, and with
    /// `--store file` the task streams live in `<data-dir>/tasks`.
    #[arg(long, value_name = "PATH")]
    data_dir: PathBuf,

    /// The backend that holds each task's domain stream.
    #[arg(long, value_enum, default_value_t = StoreKind::File)]
    store: StoreKind,

    /// Base URL of the S3-compatible endpoint, such as `http://127.0.0.1:9000`;
    /// required with `--store s3`.
    #[arg(long, value_name = "URL")]
    s3_endpoint: Option<String>,

    /// Bucket that holds the task streams; required with `--store s3`.
    #[arg(long, value_name = "NAME")]
    s3_bucket: Option<String>,

    /// Path inside the bucket under which the stream objects live.
    #[arg(long, value_name = "PATH", default_value = "tasks")]
    s3_prefix: String,

    /// Region every request is signed for.
    #[arg(long, value_name = "REGION", default_value = "us-east-1")]
    s3_region: String,

    /// Whole-request deadline for one S3 call. It must be at least 1.
    #[arg(long, value_name = "SECS", default_value_t = 30)]
    s3_timeout_secs: u64,

    /// Address the HTTP server binds.
    #[arg(long, value_name = "ADDR", default_value = "127.0.0.1:8787")]
    bind: SocketAddr,

    /// JSON agent table the ACP runner reads.
    #[arg(long, value_name = "FILE")]
    agents: Option<PathBuf>,

    /// Serve a scripted agent instead of an ACP agent table.
    #[arg(long)]
    scripted: bool,

    /// Worker threads that run submitted tasks.
    #[arg(long, value_name = "N", default_value_t = 2)]
    workers: usize,

    /// Size in bytes at which a task's active journal segment rotates.
    #[arg(long, value_name = "BYTES", default_value_t = DEFAULT_MAX_BYTES)]
    journal_max_bytes: u64,

    /// Newest journal segments a task keeps, including the active one.
    #[arg(long, value_name = "N", default_value_t = DEFAULT_KEEP_SEGMENTS)]
    journal_segments: usize,

    /// Age in seconds after which a rotated journal segment is dropped; 0 keeps them.
    #[arg(long, value_name = "SECS", default_value_t = DEFAULT_MAX_AGE.as_secs())]
    journal_max_age_secs: u64,

    /// Root every ACP task's workspace must live under.
    #[arg(long, value_name = "PATH")]
    workspace_root: Option<PathBuf>,

    /// Node executable that runs TypeScript plugins; defaults to `node` on PATH.
    #[arg(long, value_name = "PATH", default_value = "node")]
    node: PathBuf,

    /// Another `Host` header value the server answers; repeatable.
    #[arg(long, value_name = "NAME")]
    allowed_host: Vec<String>,
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    match run(Args::parse()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("mokmok-controld: {error}");
            ExitCode::from(2)
        },
    }
}

async fn run(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    let token = control_token(std::env::var("MOKMOK_CONTROL_TOKEN"), args.bind)
        .map_err(|reason| -> Box<dyn std::error::Error> { reason.into() })?;
    let journal_config =
        journal_config(&args).map_err(|reason| -> Box<dyn std::error::Error> { reason.into() })?;
    let (runner, agents, default_agent, transport_hosts) = pick_runner(&args)?;
    let store = pick_store(&args)?;
    let journal = FileJournal::open(args.data_dir.join("journal"), journal_config)?;
    let journal: Arc<dyn Journal> = Arc::new(journal);
    let listener = TcpListener::bind(args.bind).await?;
    let address = listener.local_addr()?;
    let allowed_hosts = allowed_hosts(address, &args.allowed_host);
    let state = build_state(
        store,
        journal,
        runner,
        ControlConfig {
            agents,
            default_agent,
            transport_hosts,
            allowed_hosts,
            workers: args.workers,
            token,
        },
    );
    state.supervisor.start();
    tracing::info!(%address, data_dir = %args.data_dir.display(), "control plane listening");
    println!("mokmok-controld listening on http://{address}");
    let router = build_router(state.clone());
    let (signal, signalled) = tokio::sync::oneshot::channel::<()>();
    let shutdown_state = state.clone();
    let server = axum::serve(listener, router).with_graceful_shutdown(async move {
        shutdown_signal().await;
        tracing::info!("shutdown signal received");
        let _ = shutdown_state.shutdown.send(true);
        let _ = signal.send(());
        // Tokio drops the signal future that already fired, so a second interrupt is only
        // seen by a fresh watcher; a stuck connection must not make the process killable
        // only by SIGKILL.
        tokio::spawn(async {
            if tokio::signal::ctrl_c().await.is_ok() {
                eprintln!("mokmok-controld: second interrupt, exiting now");
                std::process::exit(130);
            }
        });
    });
    serve_until_signalled(server, signalled, SHUTDOWN_TIMEOUT).await?;
    state.supervisor.shutdown();
    Ok(())
}

// The grace has to start with the signal. Wrapping the whole server future instead would
// end a daemon that never received one, which is how a five-second timeout around every
// `serve` turned into a daemon that stopped itself.
async fn serve_until_signalled<S>(
    server: S,
    signalled: tokio::sync::oneshot::Receiver<()>,
    grace: Duration,
) -> std::io::Result<()>
where
    S: std::future::IntoFuture<Output = std::io::Result<()>>,
{
    let server = server.into_future();
    tokio::pin!(server);
    tokio::select! {
        // Polled first, so a server that has already returned is never awaited again.
        biased;
        result = &mut server => result,
        signalled = signalled => {
            // `Err` means the server ended and took the sender with it.
            if signalled.is_ok()
                && tokio::time::timeout(grace, &mut server).await.is_err()
            {
                tracing::warn!("graceful shutdown timed out; dropping connections");
            }
            Ok(())
        },
    }
}

// The token is the only thing separating the API from any local process, so an empty
// value is refused rather than treated as "auth off", and a remote bind requires one.
fn control_token(
    value: Result<String, VarError>,
    bind: SocketAddr,
) -> Result<Option<String>, String> {
    match value {
        Ok(token) if token.is_empty() => {
            Err("MOKMOK_CONTROL_TOKEN is set but empty; unset it or give it a value".to_owned())
        },
        Ok(token) => Ok(Some(token)),
        Err(VarError::NotUnicode(_)) => Err("MOKMOK_CONTROL_TOKEN is not valid UTF-8".to_owned()),
        Err(VarError::NotPresent) if bind.ip().is_loopback() => Ok(None),
        Err(VarError::NotPresent) => {
            Err("MOKMOK_CONTROL_TOKEN must be set when --bind is not a loopback address".to_owned())
        },
    }
}

fn journal_config(args: &Args) -> Result<JournalConfig, String> {
    if args.journal_max_bytes == 0 {
        return Err("--journal-max-bytes must be at least 1".to_owned());
    }
    if args.journal_segments == 0 {
        return Err("--journal-segments must be at least 1".to_owned());
    }
    Ok(JournalConfig {
        max_bytes: args.journal_max_bytes,
        keep_segments: args.journal_segments,
        max_age: Duration::from_secs(args.journal_max_age_secs),
    })
}

/// The endpoint and object layout the `--s3-*` flags select.
#[derive(Debug, PartialEq, Eq)]
struct S3Settings {
    endpoint: String,
    bucket: String,
    region: String,
    prefix: String,
    timeout: Duration,
}

fn store_settings(args: &Args) -> Result<Option<S3Settings>, String> {
    if args.store == StoreKind::File {
        // An alias that carries the three defaulted `--s3-*` flags must still run against
        // a local store, so only the two flags that have no default are refused here.
        if args.s3_endpoint.is_some() {
            return Err("--s3-endpoint is only valid with --store s3".to_owned());
        }
        if args.s3_bucket.is_some() {
            return Err("--s3-bucket is only valid with --store s3".to_owned());
        }
        return Ok(None);
    }
    let endpoint = args
        .s3_endpoint
        .as_ref()
        .ok_or("--s3-endpoint is required with --store s3")?;
    let bucket = args
        .s3_bucket
        .as_ref()
        .ok_or("--s3-bucket is required with --store s3")?;
    if args.s3_timeout_secs == 0 {
        return Err("--s3-timeout-secs must be at least 1".to_owned());
    }
    Ok(Some(S3Settings {
        endpoint: endpoint.clone(),
        bucket: bucket.clone(),
        region: args.s3_region.clone(),
        prefix: args.s3_prefix.clone(),
        timeout: Duration::from_secs(args.s3_timeout_secs),
    }))
}

fn pick_store(
    args: &Args
) -> Result<Arc<dyn EnumerableStore<TaskId, ControlEvent>>, Box<dyn std::error::Error>> {
    let settings =
        store_settings(args).map_err(|reason| -> Box<dyn std::error::Error> { reason.into() })?;
    let Some(settings) = settings else {
        let store = JsonlEventStore::<TaskId, ControlEvent>::open(args.data_dir.join("tasks"))?;
        return Ok(Arc::new(store));
    };
    let store = S3EventStore::<TaskId, ControlEvent>::open(S3Config {
        endpoint: settings.endpoint,
        bucket: settings.bucket,
        region: settings.region,
        prefix: settings.prefix,
        credentials: S3Credentials::from_env()?,
        timeout: settings.timeout,
    })?;
    Ok(Arc::new(store))
}

// A failed watcher must not stop the server at once: the process still ends on SIGTERM,
// so waiting forever is the honest behaviour.
async fn shutdown_signal() {
    #[cfg(unix)]
    match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
        Ok(mut terminate) => {
            tokio::select! {
                result = tokio::signal::ctrl_c() => {
                    if let Err(error) = result {
                        tracing::warn!(%error, "cannot watch for ctrl-c");
                        std::future::pending::<()>().await;
                    }
                },
                _ = terminate.recv() => {},
            }
            return;
        },
        Err(error) => tracing::warn!(%error, "cannot watch for SIGTERM"),
    }
    match tokio::signal::ctrl_c().await {
        Ok(()) => {},
        Err(error) => {
            tracing::warn!(%error, "cannot watch for ctrl-c");
            std::future::pending::<()>().await;
        },
    }
}

type Selected = (Arc<dyn Runner>, Vec<String>, String, Vec<String>);

fn pick_runner(args: &Args) -> Result<Selected, Box<dyn std::error::Error>> {
    if args.scripted {
        let events = vec![
            JournalEvent::Message {
                text: "the scripted agent received the task".to_owned(),
            },
            JournalEvent::ToolCall {
                title: "inspect the workspace".to_owned(),
                status: "completed".to_owned(),
            },
        ];
        let runner: Arc<dyn Runner> = Arc::new(ScriptedRunner::new(events, SCRIPTED_SUMMARY));
        return Ok((
            runner,
            vec!["scripted".to_owned()],
            "scripted".to_owned(),
            Vec::new(),
        ));
    }
    let path = args
        .agents
        .as_ref()
        .ok_or("--agents is required without --scripted")?;
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let table = AgentTable::from_json(&text)?;
    let agents = table.names().into_iter().map(str::to_owned).collect();
    let default_agent = table.default_name().map(str::to_owned).unwrap_or_default();
    let transport_hosts = table.hosts.keys().cloned().collect();
    let mut acp = AcpRunner::new(table.clone(), AGENT_TIMEOUT);
    let mut plugins = PluginRunner::new(table.clone(), PluginHost::new(&args.node));
    if let Some(root) = &args.workspace_root {
        acp = acp.with_workspace_root(root);
        plugins = plugins.with_workspace_root(root);
    }
    let runner: Arc<dyn Runner> = Arc::new(AgentRunner::new(table, acp, plugins));
    Ok((runner, agents, default_agent, transport_hosts))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loopback() -> SocketAddr {
        "127.0.0.1:8787".parse().expect("a loopback address")
    }

    fn public() -> SocketAddr {
        "192.0.2.10:8787".parse().expect("a non-loopback address")
    }

    fn args() -> Args {
        Args::parse_from(["mokmok-controld", "--scripted", "--data-dir", "/tmp/mokmok"])
    }

    #[test]
    fn a_zero_journal_budget_is_refused() {
        let mut args = args();
        args.journal_max_bytes = 0;
        assert_eq!(
            journal_config(&args),
            Err("--journal-max-bytes must be at least 1".to_owned())
        );
    }

    #[test]
    fn zero_journal_segments_are_refused() {
        let mut args = args();
        args.journal_segments = 0;
        assert_eq!(
            journal_config(&args),
            Err("--journal-segments must be at least 1".to_owned())
        );
    }

    #[test]
    fn a_zero_journal_age_disables_pruning() {
        let mut args = args();
        args.journal_max_age_secs = 0;
        assert_eq!(
            journal_config(&args)
                .expect("zero seconds is valid")
                .max_age,
            Duration::ZERO
        );
    }

    fn store_args(extra: &[&str]) -> Args {
        let mut argv = vec!["mokmok-controld", "--scripted", "--data-dir", "/tmp/mokmok"];
        argv.extend_from_slice(extra);
        Args::try_parse_from(argv).expect("the flags parse")
    }

    fn s3_args(extra: &[&str]) -> Args {
        let mut argv = vec![
            "--store",
            "s3",
            "--s3-endpoint",
            "http://127.0.0.1:9000",
            "--s3-bucket",
            "mokmok",
        ];
        argv.extend_from_slice(extra);
        store_args(&argv)
    }

    #[test]
    fn a_local_store_selects_no_s3_settings() {
        assert_eq!(
            store_settings(&store_args(&[])),
            Ok(None),
            "the default store is the filesystem"
        );
    }

    #[test]
    fn s3_defaults_fill_the_prefix_the_region_and_the_timeout() {
        let settings = store_settings(&s3_args(&[]))
            .expect("an endpoint and a bucket are enough")
            .expect("the s3 store is selected");
        assert_eq!(settings.prefix, "tasks");
        assert_eq!(settings.region, "us-east-1");
        assert_eq!(settings.timeout, Duration::from_secs(30));
        assert_eq!(settings.endpoint, "http://127.0.0.1:9000");
        assert_eq!(settings.bucket, "mokmok");
    }

    #[test]
    fn s3_overrides_replace_the_defaults() {
        let settings = store_settings(&s3_args(&[
            "--s3-prefix",
            "control",
            "--s3-region",
            "eu-central-1",
            "--s3-timeout-secs",
            "5",
        ]))
        .expect("the overrides are valid")
        .expect("the s3 store is selected");
        assert_eq!(settings.prefix, "control");
        assert_eq!(settings.region, "eu-central-1");
        assert_eq!(settings.timeout, Duration::from_secs(5));
    }

    #[test]
    fn s3_without_an_endpoint_names_the_flag() {
        let error = store_settings(&store_args(&["--store", "s3", "--s3-bucket", "mokmok"]))
            .expect_err("the endpoint is missing");
        assert!(error.contains("--s3-endpoint"), "{error}");
    }

    #[test]
    fn s3_without_a_bucket_names_the_flag() {
        let error = store_settings(&store_args(&[
            "--store",
            "s3",
            "--s3-endpoint",
            "http://127.0.0.1:9000",
        ]))
        .expect_err("the bucket is missing");
        assert!(error.contains("--s3-bucket"), "{error}");
    }

    #[test]
    fn a_zero_s3_timeout_names_the_flag() {
        let error = store_settings(&s3_args(&["--s3-timeout-secs", "0"]))
            .expect_err("a zero deadline is refused");
        assert!(error.contains("--s3-timeout-secs"), "{error}");
    }

    #[test]
    fn an_s3_flag_with_the_file_store_names_the_s3_store() {
        for flag in [
            &["--s3-endpoint", "http://127.0.0.1:9000"][..],
            &["--s3-bucket", "mokmok"][..],
        ] {
            let error = store_settings(&store_args(flag)).expect_err("the flag needs the s3 store");
            assert!(error.contains("--store s3"), "{error}");
        }
    }

    #[test]
    fn a_token_on_loopback_is_used_or_absent() {
        assert_eq!(
            control_token(Ok("secret".to_owned()), loopback()),
            Ok(Some("secret".to_owned()))
        );
        assert_eq!(
            control_token(Err(VarError::NotPresent), loopback()),
            Ok(None)
        );
    }

    #[test]
    fn an_empty_token_is_refused() {
        assert!(control_token(Ok(String::new()), loopback()).is_err());
    }

    #[test]
    fn a_public_bind_requires_a_token() {
        assert!(control_token(Err(VarError::NotPresent), public()).is_err());
        assert_eq!(
            control_token(Ok("secret".to_owned()), public()),
            Ok(Some("secret".to_owned()))
        );
    }

    #[tokio::test]
    async fn the_grace_starts_with_the_signal_not_the_server() {
        // The sender is held, so no signal arrives while the server runs.
        let (_signal, signalled) = tokio::sync::oneshot::channel::<()>();
        let server = async {
            tokio::time::sleep(Duration::from_millis(200)).await;
            Ok(())
        };

        let started = tokio::time::Instant::now();
        serve_until_signalled(server, signalled, Duration::from_millis(50))
            .await
            .expect("the finished server is not an error");
        assert!(
            started.elapsed() >= Duration::from_millis(190),
            "the daemon gave up before its server finished"
        );
    }

    #[tokio::test]
    async fn a_signalled_drain_is_bounded_by_the_grace() {
        let (signal, signalled) = tokio::sync::oneshot::channel::<()>();
        let server = std::future::pending::<std::io::Result<()>>();
        signal.send(()).expect("the receiver is held");

        let started = tokio::time::Instant::now();
        serve_until_signalled(server, signalled, Duration::from_millis(50))
            .await
            .expect("a drain that times out is not an error");
        assert!(
            started.elapsed() >= Duration::from_millis(40),
            "the grace was not applied to the drain"
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "the drain outlived its grace"
        );
    }

    #[tokio::test]
    async fn a_vanished_signal_sender_waits_for_no_drain() {
        let (signal, signalled) = tokio::sync::oneshot::channel::<()>();
        drop(signal);
        let server = std::future::pending::<std::io::Result<()>>();

        let started = tokio::time::Instant::now();
        serve_until_signalled(server, signalled, Duration::from_secs(2))
            .await
            .expect("nothing left to drain is not an error");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "a drain ran for a signal that was never sent"
        );
    }
}
