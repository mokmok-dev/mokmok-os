// The `cfg(test)` module is what tells the `allow-unwrap-in-tests`, `allow-expect-in-tests`, and
// `allow-panic-in-tests` settings in `.clippy.toml` that this harness is test code.
#[cfg(test)]
mod tests {
    use mokmok_capability::{
        ActionSet, Attenuation, Authority, Name, Permission, Policy, Principal, PrincipalKind,
        Resource, Rule,
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

    fn data() -> Resource {
        Resource::Path(String::from("/data"))
    }

    fn read() -> Permission {
        Permission::new(data(), ActionSet::READ)
    }

    /// A policy that names the operation, leaving the grants to say who may perform it.
    fn authority() -> Authority {
        Authority::new(Policy::new().with(Rule::allow()), NOW)
    }

    #[test]
    fn a_wider_permission_is_refused() {
        let mut authority = authority();
        let root = authority.root(agent(1), read(), NOW);

        let wider = authority.issue(
            &root,
            principal(2),
            Permission::new(data(), ActionSet::ALL),
            None,
            NOW,
        );
        let elsewhere = authority.issue(
            &root,
            principal(2),
            Permission::new(Resource::Path(String::from("/other")), ActionSet::READ),
            None,
            NOW,
        );

        assert_eq!(wider, Err(Attenuation::NotASubset));
        assert_eq!(elsewhere, Err(Attenuation::NotASubset));
    }

    #[test]
    fn a_child_that_outlives_its_parent_is_refused() {
        let bound = Moment::from_millis(2_000);
        let mut authority = authority();
        let root = authority.root(agent(1), read(), NOW);
        let parent = authority
            .issue(&root, principal(1), read(), Some(bound), NOW)
            .unwrap();

        let later = authority.issue(
            &parent,
            principal(2),
            read(),
            Some(Moment::from_millis(2_001)),
            NOW,
        );
        let endless = authority.issue(&parent, principal(2), read(), None, NOW);

        assert_eq!(later, Err(Attenuation::OutlivesParent));
        assert_eq!(endless, Err(Attenuation::OutlivesParent));
    }

    #[test]
    fn a_child_may_expire_exactly_with_its_parent() {
        let bound = Moment::from_millis(2_000);
        let mut authority = authority();
        let root = authority.root(agent(1), read(), NOW);
        let parent = authority
            .issue(&root, principal(1), read(), Some(bound), NOW)
            .unwrap();

        let child = authority
            .issue(&parent, principal(2), read(), Some(bound), NOW)
            .unwrap();

        assert_eq!(child.not_after(), Some(bound));
    }

    #[test]
    fn a_parent_this_authority_never_issued_is_refused() {
        let mut issuer = authority();
        let mut stranger = authority();
        let root = issuer.root(agent(1), read(), NOW);

        assert_eq!(
            stranger.issue(&root, principal(2), read(), None, NOW),
            Err(Attenuation::UnknownParent),
        );
    }

    #[test]
    fn an_expiry_already_past_is_refused() {
        let mut authority = authority();
        let root = authority.root(agent(1), read(), NOW);

        assert_eq!(
            authority.issue(
                &root,
                principal(2),
                read(),
                Some(Moment::from_millis(999)),
                NOW
            ),
            Err(Attenuation::AlreadyExpired),
        );
    }
}
