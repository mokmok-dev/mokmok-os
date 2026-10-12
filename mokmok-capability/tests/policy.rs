// The `cfg(test)` module is what tells the `allow-unwrap-in-tests`, `allow-expect-in-tests`, and
// `allow-panic-in-tests` settings in `.clippy.toml` that this harness is test code.
#[cfg(test)]
mod tests {
    use mokmok_capability::{
        Action, ActionSet, Authority, Context, Decision, Denial, DenialReason, Name, Permission,
        Policy, Principal, PrincipalKind, Request, Resource, Rule,
    };
    use mokmok_protocol::{Moment, PrincipalID};
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
}
