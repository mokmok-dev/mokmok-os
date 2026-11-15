//! The rendered screens and the request that assembles them.

pub mod index;
pub mod page;

use std::sync::Arc;
use std::time::Duration;

use topcoat::context::{Cx, try_context};

use crate::port::{ControlPlane, PortError};

/// How often a live transcript asks the control plane for new entries.
pub const LIVE_POLL: Duration = Duration::from_millis(1000);

/// The control-plane port the router registered.
///
/// # Errors
///
/// Returns [`PortError::Unavailable`] when the request context carries no port, which
/// only happens if the pages were mounted on a router built by hand.
pub fn plane_arc(cx: &Cx) -> Result<Arc<dyn ControlPlane>, PortError> {
    try_context::<Arc<dyn ControlPlane>>(cx)
        .map(Arc::clone)
        .ok_or(PortError::Unavailable)
}

/// Percent-encodes a query value, leaving the path separator readable.
#[must_use]
pub fn encode(value: &str) -> String {
    use std::fmt::Write;

    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(char::from(byte));
            },
            _ => {
                let _ = write!(out, "%{byte:02X}");
            },
        }
    }
    out
}

/// The stylesheet, served beside the pages.
pub const STYLESHEET: &str = include_str!("app.css");

/// The favicon the document links to.
pub const FAVICON: &str = include_str!("favicon.svg");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_query_value_is_percent_encoded() {
        assert_eq!(encode("/srv/mokmok"), "/srv/mokmok");
        assert_eq!(encode("a b&c"), "a%20b%26c");
        assert_eq!(encode("é"), "%C3%A9");
    }

    #[test]
    fn a_request_without_the_port_has_no_plane() {
        let cx = Cx::new();
        assert_eq!(plane_arc(&cx).err(), Some(PortError::Unavailable));
    }

    #[test]
    fn the_stylesheet_declares_the_two_themes() {
        assert!(STYLESHEET.contains("html[data-mode='light']"));
        assert!(STYLESHEET.contains("--brand"));
    }
}
