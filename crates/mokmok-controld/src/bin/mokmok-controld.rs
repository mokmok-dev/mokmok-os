//! The control-plane daemon.

use std::env::VarError;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::{ArgGroup, Parser};
use mokmok_control::{
    AcpRunner, AgentTable, AgentUpdate, ControlEvent, EnumerableStore, Runner, ScriptedRunner,
    TaskId,
};
use mokmok_controld::{ControlConfig, allowed_hosts, build_router, build_state};
use mokmok_jobs_jsonl::JsonlEventStore;
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

const AGENT_TIMEOUT: Duration = Duration::from_secs(600);
const SCRIPTED_SUMMARY: &str = "the scripted run completed";

// A connection that outlives the shutdown watch must not hold the process open, but the
// bound starts with the signal: a daemon that never receives one runs until it does.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Parser)]
#[command(name = "mokmok-controld", about = "Serve the mokmok control plane")]
#[command(group(ArgGroup::new("source").required(true).args(["agents", "scripted"])))]
struct Args {
    /// Root of the JSONL store; the task streams live in `<data-dir>/tasks`.
    #[arg(long, value_name = "PATH")]
    data_dir: PathBuf,

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

    /// Root every ACP task's workspace must live under.
    #[arg(long, value_name = "PATH")]
    workspace_root: Option<PathBuf>,

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
    let (runner, agents, default_agent, transport_hosts) = pick_runner(&args)?;
    let store = JsonlEventStore::<TaskId, ControlEvent>::open(args.data_dir.join("tasks"))?;
    let store: Arc<dyn EnumerableStore<TaskId, ControlEvent>> = Arc::new(store);
    let listener = TcpListener::bind(args.bind).await?;
    let address = listener.local_addr()?;
    let allowed_hosts = allowed_hosts(address, &args.allowed_host);
    let state = build_state(
        store,
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
        let updates = vec![
            AgentUpdate::Message {
                text: "the scripted agent received the task".to_owned(),
            },
            AgentUpdate::ToolCall {
                title: "inspect the workspace".to_owned(),
                status: "completed".to_owned(),
            },
        ];
        let runner: Arc<dyn Runner> = Arc::new(ScriptedRunner::new(updates, SCRIPTED_SUMMARY));
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
    let mut runner = AcpRunner::new(table, AGENT_TIMEOUT);
    if let Some(root) = &args.workspace_root {
        runner = runner.with_workspace_root(root);
    }
    let runner: Arc<dyn Runner> = Arc::new(runner);
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
