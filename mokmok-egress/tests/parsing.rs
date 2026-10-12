//! The request line the proxy reads, and the classes an address can fall in.

use std::net::{IpAddr, Ipv6Addr};

use mokmok_egress::{
    AddressClass, Host, ParseError, Port, Scheme, classify, parse_authority, parse_connect,
};

#[test]
fn connect_parses_the_method_authority_and_version() {
    let destination = parse_connect("CONNECT example.com:443 HTTP/1.1").unwrap();

    assert_eq!(destination.scheme, Scheme::Https);
    assert_eq!(destination.host, Host::Name("example.com".to_owned()));
    assert_eq!(destination.port, Port::new(443).unwrap());
}

#[test]
fn authority_parses_a_bracketed_ipv6_literal() {
    let destination = parse_authority("[2001:db8::1]:8443").unwrap();

    assert_eq!(destination.scheme, Scheme::Http);
    assert_eq!(
        destination.host,
        Host::Ip(IpAddr::V6("2001:db8::1".parse().unwrap())),
    );
    assert_eq!(destination.port, Port::new(8443).unwrap());
}

#[test]
fn authority_parses_an_ipv4_literal_as_an_address() {
    let destination = parse_authority("127.0.0.1:8080").unwrap();

    assert_eq!(destination.host, Host::Ip("127.0.0.1".parse().unwrap()),);
    assert_eq!(destination.port, Port::new(8080).unwrap());
}

#[test]
fn connect_refuses_a_missing_port() {
    assert_eq!(
        parse_connect("CONNECT example.com HTTP/1.1"),
        Err(ParseError::MissingPort),
    );
}

#[test]
fn connect_refuses_a_non_numeric_port() {
    assert_eq!(
        parse_connect("CONNECT example.com:https HTTP/1.1"),
        Err(ParseError::MalformedPort),
    );
}

#[test]
fn connect_refuses_a_port_of_zero() {
    assert_eq!(
        parse_connect("CONNECT example.com:0 HTTP/1.1"),
        Err(ParseError::PortZero),
    );
}

#[test]
fn connect_refuses_an_out_of_range_port() {
    assert_eq!(
        parse_connect("CONNECT example.com:65536 HTTP/1.1"),
        Err(ParseError::PortOutOfRange),
    );
}

#[test]
fn connect_refuses_an_absolute_uri() {
    assert_eq!(
        parse_connect("CONNECT https://example.com:443 HTTP/1.1"),
        Err(ParseError::AbsoluteUri),
    );
}

#[test]
fn connect_refuses_embedded_whitespace() {
    assert_eq!(
        parse_connect("CONNECT  example.com:443 HTTP/1.1"),
        Err(ParseError::Whitespace),
    );
    assert_eq!(
        parse_connect("CONNECT exa\tmple.com:443 HTTP/1.1"),
        Err(ParseError::Whitespace),
    );
}

#[test]
fn connect_refuses_an_empty_host() {
    assert_eq!(
        parse_connect("CONNECT :443 HTTP/1.1"),
        Err(ParseError::EmptyHost),
    );
}

#[test]
fn connect_refuses_a_trailing_field() {
    assert_eq!(
        parse_connect("CONNECT example.com:443 HTTP/1.1 extra"),
        Err(ParseError::TrailingField),
    );
}

#[test]
fn connect_refuses_a_malformed_ipv6_bracket_form() {
    assert_eq!(
        parse_connect("CONNECT [::1:443 HTTP/1.1"),
        Err(ParseError::MalformedIpv6),
    );
    assert_eq!(
        parse_connect("CONNECT [::1]443 HTTP/1.1"),
        Err(ParseError::MalformedIpv6),
    );
    assert_eq!(
        parse_connect("CONNECT ::1:443 HTTP/1.1"),
        Err(ParseError::MalformedIpv6),
    );
}

#[test]
fn classify_names_every_address_class() {
    assert_eq!(
        classify(IpAddr::from([93, 184, 216, 34])),
        AddressClass::Public
    );
    assert_eq!(
        classify(IpAddr::from([127, 0, 0, 1])),
        AddressClass::Loopback
    );
    assert_eq!(classify(IpAddr::from([10, 0, 0, 1])), AddressClass::Private);
    assert_eq!(
        classify(IpAddr::from([169, 254, 1, 1])),
        AddressClass::LinkLocal
    );
    assert_eq!(
        classify(IpAddr::from([224, 0, 0, 1])),
        AddressClass::Multicast
    );
    assert_eq!(
        classify(IpAddr::from([0, 0, 0, 0])),
        AddressClass::Unspecified
    );
    assert_eq!(
        classify(IpAddr::from([192, 0, 2, 1])),
        AddressClass::Documentation
    );
    assert_eq!(
        classify(IpAddr::from([255, 255, 255, 255])),
        AddressClass::Broadcast,
    );

    assert_eq!(
        classify(IpAddr::V6(Ipv6Addr::LOCALHOST)),
        AddressClass::Loopback,
    );
    assert_eq!(
        classify(IpAddr::V6(Ipv6Addr::UNSPECIFIED)),
        AddressClass::Unspecified,
    );
    assert_eq!(
        classify("fd00::1".parse().unwrap()),
        AddressClass::UniqueLocal
    );
    assert_eq!(
        classify("fe80::1".parse().unwrap()),
        AddressClass::LinkLocal
    );
    assert_eq!(
        classify("ff02::1".parse().unwrap()),
        AddressClass::Multicast
    );
    assert_eq!(
        classify("2001:db8::1".parse().unwrap()),
        AddressClass::Documentation,
    );
    assert_eq!(
        classify("2606:4700::1111".parse().unwrap()),
        AddressClass::Public,
    );
}

#[test]
fn classify_reads_an_ipv4_mapped_ipv6_address_as_its_ipv4_address() {
    assert_eq!(
        classify("::ffff:127.0.0.1".parse().unwrap()),
        AddressClass::Loopback,
    );
    assert_eq!(
        classify("::ffff:10.0.0.1".parse().unwrap()),
        AddressClass::Private,
    );
    assert_eq!(
        classify("::ffff:8.8.8.8".parse().unwrap()),
        AddressClass::Public,
    );
}
