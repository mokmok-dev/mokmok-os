//! The `CONNECT` request line, and the authority it carries.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use crate::destination::{Destination, Host, Port, Scheme};

/// Why a request line or an authority is not one this proxy will act on.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ParseError {
    /// The first field is not `CONNECT`.
    #[error("the line is not a CONNECT request")]
    NotConnect,
    /// The line is not `CONNECT <authority> HTTP/1.1`.
    #[error("the line is not `CONNECT <authority> HTTP/1.1`")]
    MalformedRequest,
    /// The line carries more fields than a `CONNECT` line does.
    #[error("the request line carries more fields than a CONNECT line")]
    TrailingField,
    /// A field is empty, or carries a tab, a carriage return, or a line feed.
    #[error("the request line carries whitespace where a single space belongs")]
    Whitespace,
    /// An absolute URI stands where an authority belongs.
    #[error("the request carries an absolute URI where an authority belongs")]
    AbsoluteUri,
    /// The authority names no host.
    #[error("the authority names no host")]
    EmptyHost,
    /// The authority names no port.
    #[error("the authority names no port")]
    MissingPort,
    /// The port is not a sequence of ASCII digits.
    #[error("the port is not a sequence of digits")]
    MalformedPort,
    /// The port is zero.
    #[error("the port is zero")]
    PortZero,
    /// The port does not fit in `u16`.
    #[error("the port is above 65535")]
    PortOutOfRange,
    /// An IPv6 literal is not bracketed, or its port does not follow the closing bracket.
    #[error("an IPv6 literal must be bracketed, with its port after the closing bracket")]
    MalformedIpv6,
}

/// Parses the `CONNECT host:port HTTP/1.1` request line a client sends to a proxy.
///
/// # Errors
///
/// Returns [`ParseError`] for a request line that is not exactly `CONNECT`, one authority, and
/// `HTTP/1.1`, and for an authority that does not name a host and a nonzero port.
pub fn parse_connect(line: &str) -> Result<Destination, ParseError> {
    // The line arrives from an untrusted client, and an empty field or a tab, CR, or LF is a
    // smuggling vector: nothing here is split on runs of whitespace or trimmed first.
    if line
        .split(' ')
        .any(|field| field.is_empty() || field.contains(char::is_whitespace))
    {
        return Err(ParseError::Whitespace);
    }

    let mut fields = line.split(' ');
    match (fields.next(), fields.next(), fields.next(), fields.next()) {
        (Some("CONNECT"), Some(authority), Some("HTTP/1.1"), None) => parse_authority(authority),
        (Some("CONNECT"), Some(_), Some(_), Some(_)) => Err(ParseError::TrailingField),
        (Some("CONNECT"), Some(_), _, None) | (Some("CONNECT"), None, _, _) => {
            Err(ParseError::MalformedRequest)
        },
        _ => Err(ParseError::NotConnect),
    }
}

/// Parses the authority of a proxied destination: `host:port`, or `[literal]:port`.
///
/// # Errors
///
/// Returns [`ParseError`] for an authority that carries an absolute URI, whitespace, no port, a
/// port that is zero or above 65535, or an IPv6 literal that is not bracketed.
pub fn parse_authority(authority: &str) -> Result<Destination, ParseError> {
    if authority.contains('/') || authority.contains('?') || authority.contains('#') {
        return Err(ParseError::AbsoluteUri);
    }
    if authority.contains(char::is_whitespace) {
        return Err(ParseError::Whitespace);
    }

    // The colon before the port is indistinguishable from the colons inside an IPv6 literal, so
    // the literal has to be bracketed for the split below to mean anything.
    let (host, port) = if let Some(bracketed) = authority.strip_prefix('[') {
        let (literal, remainder) = bracketed.split_once(']').ok_or(ParseError::MalformedIpv6)?;
        let address = literal
            .parse::<Ipv6Addr>()
            .map_err(|_| ParseError::MalformedIpv6)?;
        let port = match remainder.strip_prefix(':') {
            Some(port) => port,
            None if remainder.is_empty() => return Err(ParseError::MissingPort),
            None => return Err(ParseError::MalformedIpv6),
        };
        (Host::Ip(IpAddr::V6(address)), port)
    } else {
        let (host, port) = authority.rsplit_once(':').ok_or(ParseError::MissingPort)?;
        if host.is_empty() {
            return Err(ParseError::EmptyHost);
        }
        if host.contains(':') {
            return Err(ParseError::MalformedIpv6);
        }
        let host = host.parse::<Ipv4Addr>().map_or_else(
            |_| Host::Name(host.to_owned()),
            |address| Host::Ip(IpAddr::V4(address)),
        );
        (host, port)
    };

    let port = parse_port(port)?;
    // The wire form names no scheme, so the registered TLS port is the only evidence of one.
    let scheme = if port.get() == 443 {
        Scheme::Https
    } else {
        Scheme::Http
    };

    Ok(Destination { scheme, host, port })
}

fn parse_port(text: &str) -> Result<Port, ParseError> {
    if text.is_empty() {
        return Err(ParseError::MissingPort);
    }
    if !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(ParseError::MalformedPort);
    }

    let value = text
        .parse::<u16>()
        .map_err(|_| ParseError::PortOutOfRange)?;

    Port::new(value).ok_or(ParseError::PortZero)
}
