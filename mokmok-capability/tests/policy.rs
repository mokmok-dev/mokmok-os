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

    fn agent(seed: u128) -> Principal {
        Principal::new(
            principal(seed),
            PrincipalKind::Agent,
            Name::new(format!("agent-{seed}")).unwrap(),
        )
    }

    fn read(resource: Resource) -> Permission {
        Permission::new(resource, ActionSet::READ)
    }

    fn request(
        seed: u128,
        resource: Resource,
    ) -> Request {
        Request {
            principal: principal(seed),
            action: Action::Read,
            resource,
            context: Context::none(),
        }
    }

    #[test]
    fn nothing_is_allowed_by_default() {
        let data = Resource::Path(String::from("/data"));
        let mut authority = Authority::new(Policy::new(), NOW);
        let _root = authority.root(agent(1), read(data.clone()), NOW);

        assert_eq!(
            authority.decide(&request(1, data), NOW),
            Decision::Deny(Denial::new(DenialReason::NoMatchingRule)),
        );
    }

    #[test]
    fn a_denied_action_leaves_another_resource_allowed() {
        let secret = Resource::Path(String::from("/secret"));
        let public = Resource::Path(String::from("/public"));
        let mut authority = Authority::new(
            Policy::new()
                .with(Rule::deny().for_resource(secret.clone()))
                .with(Rule::allow().for_action(Action::Read)),
            NOW,
        );
        let _secret_grant = authority.root(agent(1), read(secret.clone()), NOW);
        let public_grant = authority.root(agent(1), read(public.clone()), NOW);

        assert_eq!(
            authority.decide(&request(1, secret), NOW),
            Decision::Deny(Denial::new(DenialReason::DeniedByRule { rule: 0 })),
        );
        assert_eq!(
            authority.decide(&request(1, public), NOW),
            Decision::Allow(public_grant),
        );
    }

    #[test]
    fn a_more_specific_rule_beats_a_broader_one() {
        let public = Resource::Path(String::from("/public"));
        let mut authority = Authority::new(
            Policy::new()
                .with(Rule::deny())
                .with(Rule::allow().for_resource(public.clone())),
            NOW,
        );
        let grant = authority.root(agent(1), read(public.clone()), NOW);

        assert_eq!(
            authority.decide(&request(1, public), NOW),
            Decision::Allow(grant),
        );
        assert_eq!(
            authority.decide(&request(1, Resource::Path(String::from("/secret"))), NOW),
            Decision::Deny(Denial::new(DenialReason::DeniedByRule { rule: 0 })),
        );
    }

    fn in_session(seed: u128) -> Context {
        Context {
            session: Some(SessionID::from_uuid(Uuid::from_u128(seed))),
            execution: None,
            turn: None,
        }
    }

    const fn ask(
        principal: PrincipalID,
        action: Action,
        resource: Resource,
        context: Context,
    ) -> Request {
        Request {
            principal,
            action,
            resource,
            context,
        }
    }

    #[test]
    fn a_rule_that_names_every_axis_beats_one_that_names_three() {
        let data = Resource::Path(String::from("/data"));
        let context = in_session(7);
        let mut authority = Authority::new(
            Policy::new()
                .with(
                    Rule::allow()
                        .for_principal(principal(1))
                        .for_action(Action::Read)
                        .for_resource(data.clone())
                        .in_context(context.clone()),
                )
                .with(
                    Rule::deny()
                        .for_action(Action::Read)
                        .for_resource(data.clone())
                        .in_context(context.clone()),
                ),
            NOW,
        );
        let grant = authority.root(agent(1), read(data.clone()), NOW);

        assert_eq!(
            authority.decide(&ask(principal(1), Action::Read, data, context), NOW),
            Decision::Allow(grant),
        );
    }

    #[test]
    fn naming_the_principal_raises_a_rule_above_one_that_names_only_the_action() {
        let data = Resource::Path(String::from("/data"));
        let mut authority = Authority::new(
            Policy::new()
                .with(
                    Rule::allow()
                        .for_principal(principal(1))
                        .for_action(Action::Read),
                )
                .with(Rule::deny().for_action(Action::Read)),
            NOW,
        );
        let grant = authority.root(agent(1), read(data.clone()), NOW);

        assert_eq!(
            authority.decide(&request(1, data), NOW),
            Decision::Allow(grant),
        );
    }

    #[test]
    fn naming_the_action_raises_a_rule_above_one_that_names_only_the_resource() {
        let data = Resource::Path(String::from("/data"));
        let mut authority = Authority::new(
            Policy::new()
                .with(
                    Rule::allow()
                        .for_action(Action::Read)
                        .for_resource(data.clone()),
                )
                .with(Rule::deny().for_resource(data.clone())),
            NOW,
        );
        let grant = authority.root(agent(1), read(data.clone()), NOW);

        assert_eq!(
            authority.decide(&request(1, data), NOW),
            Decision::Allow(grant),
        );
    }

    #[test]
    fn naming_the_context_raises_a_rule_above_one_that_names_only_the_action() {
        let data = Resource::Path(String::from("/data"));
        let context = in_session(7);
        let mut authority = Authority::new(
            Policy::new()
                .with(
                    Rule::allow()
                        .for_action(Action::Read)
                        .in_context(context.clone()),
                )
                .with(Rule::deny().for_action(Action::Read)),
            NOW,
        );
        let grant = authority.root(agent(1), read(data.clone()), NOW);

        assert_eq!(
            authority.decide(&ask(principal(1), Action::Read, data, context), NOW),
            Decision::Allow(grant),
        );
    }

    #[test]
    fn one_named_axis_weighs_the_same_whichever_axis_it_is() {
        let data = Resource::Path(String::from("/data"));
        let expected = Decision::Deny(Denial::new(DenialReason::DeniedByRule { rule: 1 }));

        let mut principal_first = Authority::new(
            Policy::new()
                .with(Rule::allow().for_principal(principal(1)))
                .with(Rule::deny().for_resource(data.clone())),
            NOW,
        );
        let _grant = principal_first.root(agent(1), read(data.clone()), NOW);

        let mut resource_first = Authority::new(
            Policy::new()
                .with(Rule::allow().for_resource(data.clone()))
                .with(Rule::deny().for_principal(principal(1))),
            NOW,
        );
        let _grant = resource_first.root(agent(1), read(data.clone()), NOW);

        assert_eq!(
            principal_first.decide(&request(1, data.clone()), NOW),
            expected
        );
        assert_eq!(resource_first.decide(&request(1, data), NOW), expected);
    }

    #[test]
    fn a_deny_wins_a_tie_in_specificity() {
        let data = Resource::Path(String::from("/data"));
        let mut authority =
            Authority::new(Policy::new().with(Rule::allow()).with(Rule::deny()), NOW);
        let _grant = authority.root(agent(1), read(data.clone()), NOW);

        assert_eq!(
            authority.decide(&request(1, data), NOW),
            Decision::Deny(Denial::new(DenialReason::DeniedByRule { rule: 1 })),
        );
    }

    #[test]
    fn an_approval_outranks_an_allow_at_equal_specificity() {
        let data = Resource::Path(String::from("/data"));
        let mut authority = Authority::new(
            Policy::new()
                .with(Rule::allow())
                .with(Rule::requires_approval()),
            NOW,
        );

        let decision = authority.decide(&request(1, data.clone()), NOW);
        let Decision::Approve(requirement) = decision else {
            panic!("the approval rule should have spoken, got {decision:?}");
        };
        let _ = authority.root(agent(2), read(data.clone()), NOW);
        let grant = authority.approve(&requirement, principal(2), NOW).unwrap();

        assert_eq!(
            authority.decide(&request(1, data), NOW),
            Decision::Allow(grant),
        );
    }

    #[test]
    fn a_deny_outranks_an_approval_at_equal_specificity() {
        let data = Resource::Path(String::from("/data"));
        let authority = Authority::new(
            Policy::new()
                .with(Rule::requires_approval())
                .with(Rule::deny()),
            NOW,
        );

        assert_eq!(
            authority.decide(&request(1, data), NOW),
            Decision::Deny(Denial::new(DenialReason::DeniedByRule { rule: 1 })),
        );
    }

    #[test]
    fn a_rule_that_names_a_principal_speaks_only_for_that_principal() {
        let data = Resource::Path(String::from("/data"));
        let mut authority = Authority::new(
            Policy::new()
                .with(Rule::allow().for_principal(principal(1)))
                .with(Rule::deny()),
            NOW,
        );
        let grant = authority.root(agent(1), read(data.clone()), NOW);
        let _other = authority.root(agent(2), read(data.clone()), NOW);

        assert_eq!(
            authority.decide(&request(1, data.clone()), NOW),
            Decision::Allow(grant),
        );
        assert_eq!(
            authority.decide(&request(2, data), NOW),
            Decision::Deny(Denial::new(DenialReason::DeniedByRule { rule: 1 })),
        );
    }

    #[test]
    fn two_rules_that_tie_completely_let_the_earlier_one_speak() {
        let data = Resource::Path(String::from("/data"));
        let authority = Authority::new(
            Policy::new()
                .with(Rule::deny().for_action(Action::Read))
                .with(Rule::deny().for_resource(data.clone())),
            NOW,
        );

        assert_eq!(
            authority.decide(&request(1, data), NOW),
            Decision::Deny(Denial::new(DenialReason::DeniedByRule { rule: 0 })),
        );
    }
}
