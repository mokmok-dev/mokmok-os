//! Opens one ACP connection and prints the handshake and a new session.
//!
//! This proves the wire without running a model turn.
//!
//! ```sh
//! cargo run -p mokmok-acp --example handshake -- stdio
//! cargo run -p mokmok-acp --example handshake -- ws ws://127.0.0.1:2419/ws s3cret
//! ```

use std::error::Error;

use mokmok_acp::{AcpClient, ConnectionConfig, StdioConfig, WebSocketConfig};

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let config = match args.next().as_deref() {
        Some("stdio") => {
            let program = args.next().unwrap_or_else(|| "grok".to_owned());
            let mut config = StdioConfig::new(&program);
            if program == "grok" {
                config = config.args(["agent", "stdio"]);
            }
            ConnectionConfig::Stdio(config)
        },
        Some("ws") => {
            let url = args.next().ok_or("ws needs a URL")?;
            let mut config = WebSocketConfig::new(url);
            if let Some(secret) = args.next() {
                config = config.server_key(secret);
            }
            ConnectionConfig::WebSocket(config)
        },
        _ => return Err("usage: handshake stdio [program] | ws <url> [secret]".into()),
    };
    let mut client = AcpClient::open(config)?;
    let session = client.new_session(std::env::current_dir()?)?;
    let agent = client.agent();
    println!(
        "acp={} agent={} version={} session={}",
        agent.protocol_version,
        agent
            .agent_info
            .as_ref()
            .map_or("unknown", |info| info.name.as_str()),
        agent
            .agent_info
            .as_ref()
            .map_or("unknown", |info| info.version.as_str()),
        session.id()
    );
    Ok(())
}
