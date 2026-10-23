//! Bounded model checks for the egress destination classifier.
//!
//! The property is stated in `SKILL.md` beside this crate: an address the classifier calls
//! [`AddressClass::Public`] must be globally routable. Kani explores every `u32` and every `u128`
//! address, so the address space is covered exhaustively rather than sampled.
//!
//! The specification of "globally routable" is [`std::net::Ipv4Addr::is_global`] and
//! [`std::net::Ipv6Addr::is_global`]. Both are unstable, which is why this crate is not a
//! workspace member and runs on the nightly toolchain Kani pins.

#![feature(ip)]

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use mokmok_egress::{AddressClass, Rules, classify};

/// Whether the operating system considers `address` reachable from the public internet.
///
/// An IPv4-mapped IPv6 address is judged by the IPv4 address it carries, the same way
/// [`classify`] judges it.
fn globally_routable(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(v4) => v4.is_global(),
        IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map_or_else(|| v6.is_global(), |v4| v4.is_global()),
    }
}

fn any_class() -> AddressClass {
    let raw: u8 = kani::any();
    kani::assume(raw < 9);
    match raw {
        0 => AddressClass::Public,
        1 => AddressClass::Loopback,
        2 => AddressClass::Private,
        3 => AddressClass::LinkLocal,
        4 => AddressClass::UniqueLocal,
        5 => AddressClass::Multicast,
        6 => AddressClass::Unspecified,
        7 => AddressClass::Documentation,
        _ => AddressClass::Broadcast,
    }
}

/// Every address the classifier calls public is globally routable.
///
/// This is the property that guards `Rules::public_only()`: a sandbox handed a validated
/// destination must not be able to reach a shared, reserved, benchmark, or otherwise non-global
/// network. It fails on this tree; see the finding in `SKILL.md`.
#[kani::proof]
fn a_public_ipv4_address_is_globally_routable() {
    let raw: u32 = kani::any();
    let address = Ipv4Addr::from(raw);
    if classify(IpAddr::V4(address)) == AddressClass::Public {
        assert!(globally_routable(IpAddr::V4(address)));
    }
}

/// The same property over every IPv6 address.
#[kani::proof]
fn a_public_ipv6_address_is_globally_routable() {
    let raw: u128 = kani::any();
    let address = IpAddr::V6(Ipv6Addr::from(raw));
    if classify(address) == AddressClass::Public {
        assert!(globally_routable(address));
    }
}

/// The IPv4-mapped rule the classifier documents holds for every carried address.
#[kani::proof]
fn an_ipv4_mapped_address_is_classified_as_its_ipv4_address() {
    let raw: u32 = kani::any();
    let v4 = Ipv4Addr::from(raw);
    let mapped = IpAddr::V6(v4.to_ipv6_mapped());
    assert_eq!(classify(mapped), classify(IpAddr::V4(v4)));
}

/// The named classes are reached, not just the public fallback.
#[kani::proof]
fn a_loopback_address_is_loopback() {
    let raw: u32 = kani::any();
    let address = Ipv4Addr::from(raw);
    kani::assume(address.is_loopback());
    assert_eq!(classify(IpAddr::V4(address)), AddressClass::Loopback);
}

#[kani::proof]
fn a_private_address_is_private() {
    let raw: u32 = kani::any();
    let address = Ipv4Addr::from(raw);
    kani::assume(address.is_private());
    assert_eq!(classify(IpAddr::V4(address)), AddressClass::Private);
}

#[kani::proof]
fn a_link_local_address_is_link_local() {
    let raw: u32 = kani::any();
    let address = Ipv4Addr::from(raw);
    kani::assume(address.is_link_local());
    assert_eq!(classify(IpAddr::V4(address)), AddressClass::LinkLocal);
}

#[kani::proof]
fn a_forbidden_class_is_never_allowed() {
    let class = any_class();
    let rules = Rules::public_only().permit(class).forbid(class);
    assert!(!rules.allows(class));
}

#[kani::proof]
fn an_unpermitted_class_is_never_allowed() {
    let class = any_class();
    kani::assume(class != AddressClass::Public);
    assert!(!Rules::public_only().allows(class));
}

#[kani::proof]
fn a_permitted_and_unforbidden_class_is_allowed() {
    let class = any_class();
    let rules = Rules::public_only().permit(class);
    assert!(rules.allows(class));
}
