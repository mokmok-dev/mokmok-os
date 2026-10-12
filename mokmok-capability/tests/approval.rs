// The `cfg(test)` module is what tells the `allow-unwrap-in-tests`, `allow-expect-in-tests`, and
// `allow-panic-in-tests` settings in `.clippy.toml` that this harness is test code.
#[cfg(test)]
mod tests {
    use mokmok_capability::{
        Action, ApprovalError, Authority, Context, Decision, Denial, DenialReason, Name, Policy,
        Request, Requirement, Resource, Rule,
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

    fn deploy() -> Resource {
        Resource::Tool(Name::new("deploy").unwrap())
    }

    fn context(seed: u128) -> Context {
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

    /// A rule that allows an invocation of the deployment tool, under one that asks for an approval
    /// inside a single session. The approval rule constrains more axes, so it speaks for requests
    /// in that session and the allow speaks for the rest.
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

    fn requirement(
        authority: &Authority,
        request: &Request,
    ) -> Requirement {
        match authority.decide(request, NOW) {
            Decision::Approve(requirement) => requirement,
            other => panic!("the approval rule should have asked for one, got {other:?}"),
        }
    }

    #[test]
    fn an_approval_authorizes_only_the_request_it_was_earned_for() {
        let approved = invoke(context(10));
        let mut authority = Authority::new(policy(context(10)), NOW);
        let requirement = requirement(&authority, &approved);

        assert_eq!(
            authority.decide(&approved, NOW),
            Decision::Approve(requirement.clone()),
        );

        let grant = authority.approve(&requirement, principal(1), NOW).unwrap();

        assert_eq!(authority.decide(&approved, NOW), Decision::Allow(grant));

        let elsewhere = Request {
            context: context(11),
            ..approved
        };

        assert_eq!(
            authority.decide(&elsewhere, NOW),
            Decision::Deny(Denial::new(DenialReason::NotGranted)),
        );
    }

    #[test]
    fn a_principal_cannot_approve_its_own_request() {
        let requested = invoke(context(10));
        let mut authority = Authority::new(policy(context(10)), NOW);
        let requirement = requirement(&authority, &requested);

        assert_eq!(
            authority.approve(&requirement, principal(2), NOW),
            Err(ApprovalError::SelfApproved),
        );
    }

    #[test]
    fn an_approval_whose_window_closed_is_refused() {
        let closed = Moment::from_millis(1_500);
        let mut authority = Authority::new(
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
                        .in_context(context(10))
                        .not_after(closed),
                ),
            NOW,
        );
        let requirement = requirement(&authority, &invoke(context(10)));

        assert_eq!(requirement.not_after(), Some(closed));

        let grant = authority
            .approve(&requirement, principal(1), Moment::from_millis(1_499))
            .unwrap();

        assert_eq!(grant.not_after(), Some(closed));
        assert_eq!(
            authority.approve(&requirement, principal(1), closed),
            Err(ApprovalError::RequirementExpired),
        );
    }
}
