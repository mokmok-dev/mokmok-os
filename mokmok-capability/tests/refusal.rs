// The `cfg(test)` module is what tells the `allow-unwrap-in-tests`, `allow-expect-in-tests`, and
// `allow-panic-in-tests` settings in `.clippy.toml` that this harness is test code.
#[cfg(test)]
mod tests {
    use mokmok_capability::{
        Action, ActionSet, Authority, Context, Decision, Denial, DenialReason, Name, Permission,
        Policy, Principal, PrincipalKind, Request, Resource, Rule,
    };
    use mokmok_protocol::{Moment, PrincipalID, SessionID};
    use uuid::Uuid;

    const NOW: Moment = Moment::from_millis(1_000);

    const fn principal(seed: u128) -> PrincipalID {
        PrincipalID::from_uuid(Uuid::from_u128(seed))
    }

    const fn session(seed: u128) -> SessionID {
        SessionID::from_uuid(Uuid::from_u128(seed))
    }

    fn agent(seed: u128) -> Principal {
        Principal::new(
            principal(seed),
            PrincipalKind::Agent,
            Name::new(format!("agent-{seed}")).unwrap(),
        )
    }

    fn data() -> Resource {
        Resource::Path(String::from("/data"))
    }

    fn read() -> Permission {
        Permission::new(data(), ActionSet::READ)
    }

    fn request(seed: u128) -> Request {
        Request {
            principal: principal(seed),
            action: Action::Read,
            resource: data(),
            context: Context::none(),
        }
    }

    fn deploy() -> Resource {
        Resource::Tool(Name::new("deploy").unwrap())
    }

    fn in_session(seed: u128) -> Context {
        Context {
            session: Some(session(seed)),
            execution: None,
            turn: None,
        }
    }

    fn invoke(requested_in: Context) -> Request {
        Request {
            principal: principal(2),
            action: Action::Invoke,
            resource: deploy(),
            context: requested_in,
        }
    }

    /// Allows the invocation everywhere, and asks for an approval inside one session. The approval
    /// rule names one axis more, so it speaks for requests in that session and the allow for the
    /// rest.
    fn policy(approved_in: Context) -> Policy {
        Policy::new()
            .with(
                Rule::allow()
                    .for_action(Action::Invoke)
                    .for_resource(deploy()),
            )
            .with(
                Rule::requires_approval()
                    .for_action(Action::Invoke)
                    .for_resource(deploy())
                    .in_context(approved_in),
            )
    }

    #[test]
    fn a_revocation_outranks_a_grant_that_cannot_bind_the_request() {
        let revoked_at = Moment::from_millis(3_000);
        let approved_in = in_session(10);
        let mut authority = Authority::new(policy(approved_in.clone()), NOW);

        let Decision::Approve(requirement) = authority.decide(&invoke(approved_in), NOW) else {
            panic!("the approval rule should have asked for one");
        };
        let _ = authority.root(agent(1), read(), NOW);
        let _bound = authority.approve(&requirement, principal(1), NOW).unwrap();
        let root = authority.root(
            agent(2),
            Permission::new(deploy(), ActionSet::of(Action::Invoke)),
            NOW,
        );
        assert!(authority.revoke(root.id(), revoked_at));

        assert_eq!(
            authority.decide(&invoke(in_session(11)), revoked_at),
            Decision::Deny(Denial::new(DenialReason::Revoked { at: revoked_at })),
        );
    }

    #[test]
    fn the_earliest_of_two_equally_strong_refusals_is_the_one_reported() {
        let first_at = Moment::from_millis(2_000);
        let later_at = Moment::from_millis(3_000);
        let mut authority = Authority::new(Policy::new().with(Rule::allow()), NOW);

        let first = authority.root(agent(2), read(), NOW);
        let second = authority.root(agent(2), read(), NOW);
        assert!(authority.revoke(first.id(), first_at));
        assert!(authority.revoke(second.id(), later_at));

        assert_eq!(
            authority.decide(&request(2), later_at),
            Decision::Deny(Denial::new(DenialReason::Revoked { at: first_at })),
        );
    }
}
