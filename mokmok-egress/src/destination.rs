//! What a destination names, and which classes of address the rules let it reach.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// The transport a destination's port speaks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Scheme {
    /// Cleartext HTTP.
    Http,
    /// TLS, the transport a `CONNECT` tunnel carries.
    Https,
}

/// The host half of a destination: a name to look up, or an address to use as written.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Host {
    /// A DNS name.
    Name(String),
    /// A literal address.
    Ip(IpAddr),
}

/// A destination port.
///
/// Zero is not a port, so there is no value of this type for it.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Port(u16);

impl Port {
    /// The port `value` names, or `None` for zero.
    #[must_use]
    pub const fn new(value: u16) -> Option<Self> {
        if value == 0 { None } else { Some(Self(value)) }
    }

    /// The port number.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

/// Where a proxied connection is asked to go.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Destination {
    /// The transport the port speaks.
    pub scheme: Scheme,
    /// The name or literal address the caller wrote.
    pub host: Host,
    /// The port, never zero.
    pub port: Port,
}

/// The class an address falls in.
///
/// The classes partition the address space, so a rule that forbids one cannot be read as a
/// statement about another.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum AddressClass {
    /// A globally routed address.
    Public,
    /// The loopback interface.
    Loopback,
    /// A private network: `10/8`, `172.16/12`, or `192.168/16`.
    Private,
    /// A link-local network: `169.254/16` or `fe80::/10`.
    LinkLocal,
    /// A unique-local IPv6 network: `fc00::/7`.
    UniqueLocal,
    /// A multicast group.
    Multicast,
    /// The unspecified address.
    Unspecified,
    /// A documentation network: `192.0.2/24`, `198.51.100/24`, `203.0.113/24`, or `2001:db8::/32`.
    Documentation,
    /// The limited broadcast address.
    Broadcast,
}

/// The class `address` falls in.
///
/// An IPv4-mapped IPv6 address is classified as the IPv4 address it carries: otherwise the IPv6
/// wrapper would hide the class of what is really a loopback or private destination.
#[must_use]
pub fn classify(address: IpAddr) -> AddressClass {
    match address {
        IpAddr::V4(address) => classify_v4(address),
        IpAddr::V6(address) => address
            .to_ipv4_mapped()
            .map_or_else(|| classify_v6(address), classify_v4),
    }
}

const fn classify_v4(address: Ipv4Addr) -> AddressClass {
    if address.is_unspecified() {
        AddressClass::Unspecified
    } else if address.is_loopback() {
        AddressClass::Loopback
    } else if address.is_private() {
        AddressClass::Private
    } else if address.is_link_local() {
        AddressClass::LinkLocal
    } else if address.is_broadcast() {
        AddressClass::Broadcast
    } else if address.is_multicast() {
        AddressClass::Multicast
    } else if address.is_documentation() {
        AddressClass::Documentation
    } else {
        AddressClass::Public
    }
}

const fn classify_v6(address: Ipv6Addr) -> AddressClass {
    if address.is_unspecified() {
        AddressClass::Unspecified
    } else if address.is_loopback() {
        AddressClass::Loopback
    } else if address.is_unique_local() {
        AddressClass::UniqueLocal
    } else if address.is_unicast_link_local() {
        AddressClass::LinkLocal
    } else if address.is_multicast() {
        AddressClass::Multicast
    } else if is_documentation_v6(address) {
        AddressClass::Documentation
    } else {
        AddressClass::Public
    }
}

// `Ipv6Addr::is_documentation` is not stable, so `2001:db8::/32` is matched on its own.
const fn is_documentation_v6(address: Ipv6Addr) -> bool {
    let [first, second, ..] = address.segments();

    first == 0x2001 && second == 0x0db8
}

/// The classes a destination may and may not reach.
///
/// The two lists are kept apart rather than merged: a permit widens the default and a forbid
/// closes a class a permit opened, so the narrower rule decides. An address is admitted only when
/// its class is permitted and not forbidden.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Rules {
    permitted: u16,
    forbidden: u16,
}

const fn bit(class: AddressClass) -> u16 {
    1u16 << (class as u8)
}

impl Rules {
    /// Rules that admit only [`AddressClass::Public`].
    #[must_use]
    pub const fn public_only() -> Self {
        Self {
            permitted: bit(AddressClass::Public),
            forbidden: 0,
        }
    }

    /// The same rules, also admitting `class`.
    #[must_use]
    pub const fn permit(
        self,
        class: AddressClass,
    ) -> Self {
        Self {
            permitted: self.permitted | bit(class),
            ..self
        }
    }

    /// The same rules, refusing `class` even where it is permitted.
    #[must_use]
    pub const fn forbid(
        self,
        class: AddressClass,
    ) -> Self {
        Self {
            forbidden: self.forbidden | bit(class),
            ..self
        }
    }

    /// Whether `class` may be reached.
    #[must_use]
    pub const fn allows(
        self,
        class: AddressClass,
    ) -> bool {
        self.permitted & bit(class) != 0 && self.forbidden & bit(class) == 0
    }
}
