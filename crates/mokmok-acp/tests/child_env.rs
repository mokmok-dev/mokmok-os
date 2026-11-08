//! The environment a spawned agent sees.

use std::time::Duration;

use mokmok_acp::{StdioConfig, StdioTransport, Transport};

// This test binary holds one test, so no other thread reads the environment while the
// variable is set.
#[test]
#[allow(unsafe_code)]
fn the_control_token_is_not_inherited() -> Result<(), Box<dyn std::error::Error>> {
    // SAFETY: the test binary has a single test, so nothing reads the environment
    // concurrently.
    unsafe { std::env::set_var("MOKMOK_CONTROL_TOKEN", "s3cret") };

    let config = StdioConfig::new("sh").args(["-c", "echo ${MOKMOK_CONTROL_TOKEN-unset}"]);
    let mut transport = StdioTransport::spawn(&config)?;
    let line = transport
        .recv_timeout(Duration::from_secs(5))?
        .expect("the child prints one line");
    assert_eq!(line, "unset", "the child inherited the control token");
    transport.close()?;
    Ok(())
}
