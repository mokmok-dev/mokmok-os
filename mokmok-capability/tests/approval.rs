// The `cfg(test)` module is what tells the `allow-unwrap-in-tests`, `allow-expect-in-tests`, and
// `allow-panic-in-tests` settings in `.clippy.toml` that this harness is test code.
#[cfg(test)]
mod tests {
    use mokmok_capability::{
        Action, ActionSet, ApprovalError, Authority, Context, Decision, Denial, DenialReason, Name,
        Permission, Policy, Principal, PrincipalKind, Request, Requirement, Resource, Rule,
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

    /// Admits `id` as an approver. A held root grant is the only way an authority comes to hold a
    /// principal, and the permission it carries is on an unrelated resource so the grant cannot
    /// speak for the requests under test.
    fn admit(
        authority: &mut Authority,
        id: PrincipalID,
    ) {
        let _ = authority.root(
            Principal::new(id, PrincipalKind::Agent, Name::new("approver").unwrap()),
            Permission::new(
                Resource::Tool(Name::new("unrelated").unwrap()),
                ActionSet::INVOKE,
            ),
            NOW,
        );
    }

    #[test]
    fn an_approval_authorizes_only_the_request_it_was_earned_for() {
        let approved = invoke(context(10));
        let mut authority = Authority::new(policy(context(10)), NOW);
        let requirement = requirement(&authority, &approved);
        admit(&mut authority, principal(1));

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
    fn an_unadmitted_principal_cannot_approve() {
        let requested = invoke(context(10));
        let mut authority = Authority::new(policy(context(10)), NOW);
        let requirement = requirement(&authority, &requested);

        assert_eq!(
            authority.approve(&requirement, principal(1), NOW),
            Err(ApprovalError::UnknownApprover),
        );
    }

    #[test]
    fn an_admitted_principal_can_approve() {
        let approved = invoke(context(10));
        let mut authority = Authority::new(policy(context(10)), NOW);
        let requirement = requirement(&authority, &approved);
        admit(&mut authority, principal(1));

        let grant = authority.approve(&requirement, principal(1), NOW).unwrap();

        assert_eq!(authority.decide(&approved, NOW), Decision::Allow(grant));
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
        admit(&mut authority, principal(1));

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
