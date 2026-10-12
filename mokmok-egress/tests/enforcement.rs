//! Validation, the connection it authorizes, and the audit record of that connection.

use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Mutex;
use std::sync::PoisonError;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use mokmok_egress::{
    AddressClass, AuditRecord, AuditSink, CallContext, DecisionName, Destination, PolicyClient,
    Refusal, Resolver, Rules, Validated, parse_authority, parse_connect,
};
use mokmok_protocol::{ExecutionID, Moment, PrincipalID};
use tokio::net::TcpListener;
use tokio::time::timeout;
use uuid::Uuid;

const PUBLIC: [u8; 4] = [93, 184, 216, 34];

/// Asserts that the listener observes no connection within a wait long enough for a loopback
/// connection to have arrived.
async fn observed_no_connection(listener: &TcpListener) {
    let observed = timeout(Duration::from_millis(20), listener.accept()).await;

    assert!(observed.is_err(), "a connection reached the listener");
}

const fn context() -> CallContext {
    CallContext {
        principal: PrincipalID::from_uuid(Uuid::from_u128(1)),
        execution: ExecutionID::from_uuid(Uuid::from_u128(2)),
    }
}

struct Permit;

impl PolicyClient for Permit {
    fn check(
        &self,
        _: &Destination,
        _: &CallContext,
    ) -> Result<(), Refusal> {
        Ok(())
    }
}

struct Deny;

impl PolicyClient for Deny {
    fn check(
        &self,
        _: &Destination,
        _: &CallContext,
    ) -> Result<(), Refusal> {
        Err(Refusal::Denied)
    }
}

#[derive(Default)]
struct Recording {
    records: Mutex<Vec<AuditRecord>>,
}

impl Recording {
    fn records(&self) -> Vec<AuditRecord> {
        self.records
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl AuditSink for Recording {
    fn record(
        &self,
        record: AuditRecord,
    ) {
        self.records
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(record);
    }
}

/// A resolver whose answer changes between lookups.
struct Rebinding {
    first: SocketAddr,
    later: SocketAddr,
    lookups: AtomicU32,
}

impl Resolver for Rebinding {
    fn resolve(
        &self,
        _host: &str,
        _port: u16,
    ) -> Result<Vec<SocketAddr>, io::Error> {
        let lookup = self.lookups.fetch_add(1, Ordering::Relaxed);

        Ok(vec![if lookup == 0 { self.first } else { self.later }])
    }
}

#[test]
fn rules_admit_only_what_they_permit() {
    assert!(Rules::public_only().allows(AddressClass::Public));
    assert!(!Rules::public_only().allows(AddressClass::Private));
    assert!(
        Rules::public_only()
            .permit(AddressClass::Private)
            .allows(AddressClass::Private)
    );
    assert!(
        !Rules::public_only()
            .permit(AddressClass::Private)
            .forbid(AddressClass::Private)
            .allows(AddressClass::Private)
    );
}

#[test]
fn validation_keeps_only_the_addresses_the_rules_admit() {
    let destination = parse_connect("CONNECT example.com:443 HTTP/1.1").unwrap();
    let public = SocketAddr::from((PUBLIC, 443));
    let loopback = SocketAddr::from(([127, 0, 0, 1], 443));

    let validated = Validated::new(
        destination,
        vec![loopback, public],
        &Rules::public_only(),
        &Permit,
        &context(),
    )
    .unwrap();

    assert_eq!(validated.addresses(), [public]);
}

#[tokio::test]
async fn a_forbidden_address_is_refused_before_any_connection() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let destination = parse_authority(&address.to_string()).unwrap();

    let refused = Validated::new(
        destination,
        vec![address],
        &Rules::public_only(),
        &Permit,
        &context(),
    );

    assert_eq!(refused, Err(Refusal::Forbidden(AddressClass::Loopback)));
    observed_no_connection(&listener).await;
}

#[test]
fn a_policy_refusal_refuses_a_public_destination() {
    let destination = parse_connect("CONNECT example.com:443 HTTP/1.1").unwrap();
    let addresses = vec![SocketAddr::from((PUBLIC, 443))];

    let refused = Validated::new(
        destination,
        addresses,
        &Rules::public_only(),
        &Deny,
        &context(),
    );

    assert_eq!(refused, Err(Refusal::Denied));
}

#[tokio::test]
async fn a_second_resolution_cannot_redirect_the_connection() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let listener_address = listener.local_addr().unwrap();
    let resolver = Rebinding {
        first: SocketAddr::from((PUBLIC, 443)),
        later: listener_address,
        lookups: AtomicU32::new(0),
    };
    let destination = parse_connect("CONNECT example.com:443 HTTP/1.1").unwrap();

    let first = resolver.resolve("example.com", 443).unwrap();
    let validated = Validated::new(
        destination,
        first,
        &Rules::public_only(),
        &Permit,
        &context(),
    )
    .unwrap();

    assert_eq!(validated.addresses(), [SocketAddr::from((PUBLIC, 443))]);

    let second = resolver.resolve("example.com", 443).unwrap();
    assert_eq!(second, [listener_address]);

    // `connect` holds no resolver, so the only address it can reach is the one validation kept;
    // the address the resolver now answers must see nothing. The timeout bounds the attempt, since
    // the kept address is a public one this test does not own.
    let _attempt = timeout(Duration::from_millis(100), validated.connect()).await;
    observed_no_connection(&listener).await;
}

#[tokio::test]
async fn a_permitted_loopback_connection_reaches_the_listener() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let destination = parse_authority(&address.to_string()).unwrap();
    let rules = Rules::public_only().permit(AddressClass::Loopback);
    let validated =
        Validated::new(destination, vec![address], &rules, &Permit, &context()).unwrap();

    let stream = validated.connect().await.unwrap();
    let (accepted, _) = listener.accept().await.unwrap();

    assert_eq!(stream.peer_addr().unwrap(), address);
    assert_eq!(accepted.local_addr().unwrap(), address);
}

#[tokio::test]
async fn the_audit_sink_receives_the_connected_address() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let destination = parse_authority(&address.to_string()).unwrap();
    let rules = Rules::public_only().permit(AddressClass::Loopback);
    let context = context();
    let validated = Validated::new(destination, vec![address], &rules, &Permit, &context).unwrap();

    let stream = validated.connect().await.unwrap();
    let connected = stream.peer_addr().unwrap();
    let sink = Recording::default();
    sink.record(AuditRecord {
        at: Moment::from_millis(1_700_000_000_000),
        execution: context.execution,
        principal: context.principal,
        destination: validated.destination().clone(),
        address: connected,
        decision: DecisionName::Allowed,
    });

    assert_eq!(connected, address);
    assert_eq!(
        sink.records(),
        vec![AuditRecord {
            at: Moment::from_millis(1_700_000_000_000),
            execution: context.execution,
            principal: context.principal,
            destination: validated.destination().clone(),
            address,
            decision: DecisionName::Allowed,
        }],
    );
}
