// The `cfg(test)` module is what tells the `allow-unwrap-in-tests`, `allow-expect-in-tests`, and
// `allow-panic-in-tests` settings in `.clippy.toml` that this harness is test code.
#[cfg(test)]
mod tests {
    use mokmok_capability::{
        Action, ActionSet, Authority, Context, Decision, Denial, DenialReason, GrantId, Name,
        Permission, Policy, Principal, PrincipalKind, Request, Resource, Rule,
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

    fn request(seed: u128) -> Request {
        Request {
            principal: principal(seed),
            action: Action::Read,
            resource: data(),
            context: Context::none(),
        }
    }

    /// A policy that names the operation, leaving the grants to say who may perform it.
    fn authority(at: Moment) -> Authority {
        Authority::new(Policy::new().with(Rule::allow()), at)
    }

    #[test]
    fn a_grant_authorizes_before_its_boundary_and_denies_at_it() {
        let bound = Moment::from_millis(2_000);
        let mut authority = authority(NOW);
        let root = authority.root(agent(1), read(), NOW);
        let child = authority
            .issue(&root, principal(2), read(), Some(bound), NOW)
            .unwrap();
        let asked = request(2);

        assert_ne!(root.id(), child.id());
        assert_eq!(
            authority.decide(&asked, Moment::from_millis(1_999)),
            Decision::Allow(child),
        );
        assert_eq!(
            authority.decide(&asked, bound),
            Decision::Deny(Denial::new(DenialReason::Expired { at: bound })),
        );
        assert_eq!(
            authority.decide(&asked, Moment::from_millis(2_001)),
            Decision::Deny(Denial::new(DenialReason::Expired { at: bound })),
        );
    }

    #[test]
    fn revoking_a_grant_denies_it_and_everything_issued_from_it() {
        let revoked_at = Moment::from_millis(3_000);
        let mut authority = authority(NOW);
        let root = authority.root(agent(1), read(), NOW);
        let child = authority
            .issue(&root, principal(2), read(), None, NOW)
            .unwrap();
        let grandchild = authority
            .issue(&child, principal(3), read(), None, NOW)
            .unwrap();

        assert_ne!(root.id(), child.id());
        assert_ne!(child.id(), grandchild.id());
        assert_eq!(
            authority.decide(&request(3), NOW),
            Decision::Allow(grandchild)
        );

        assert!(authority.revoke(child.id(), revoked_at));
        assert!(!authority.revoke(child.id(), revoked_at));

        assert_eq!(
            authority.decide(&request(3), revoked_at),
            Decision::Deny(Denial::new(DenialReason::Revoked { at: revoked_at })),
        );
        assert_eq!(
            authority.decide(&request(2), revoked_at),
            Decision::Deny(Denial::new(DenialReason::Revoked { at: revoked_at })),
        );
        assert_eq!(
            authority.decide(&request(1), revoked_at),
            Decision::Allow(root)
        );
    }

    #[test]
    fn revoking_a_principal_denies_everything_it_held() {
        let revoked_at = Moment::from_millis(3_000);
        let mut authority = authority(NOW);
        let root = authority.root(agent(1), read(), NOW);
        let child = authority
            .issue(&root, principal(2), read(), None, NOW)
            .unwrap();

        assert_ne!(root.id(), child.id());
        assert_eq!(authority.decide(&request(2), NOW), Decision::Allow(child));

        authority.revoke_principal(principal(2), revoked_at);

        assert_eq!(
            authority.decide(&request(2), revoked_at),
            Decision::Deny(Denial::new(DenialReason::Revoked { at: revoked_at })),
        );
        assert_eq!(
            authority.decide(&request(1), revoked_at),
            Decision::Allow(root)
        );
    }

    #[test]
    fn an_authority_stamps_each_change_with_the_callers_instant() {
        let opened = Moment::from_millis(500);
        let issued = Moment::from_millis(1_000);
        let mut authority = authority(opened);
        let root = authority.root(agent(1), read(), issued);

        assert_eq!(authority.since(), opened);
        assert_eq!(authority.issued_at(root.id()), Some(issued));
        assert_eq!(authority.issued_at(GrantId::from_value(u64::MAX)), None);
        assert_eq!(authority.principal(principal(1)), Some(&agent(1)));
    }

    #[test]
    fn revoking_one_grant_spares_the_grant_minted_after_it() {
        let revoked_at = Moment::from_millis(3_000);
        let mut authority = authority(NOW);
        let first = authority.root(agent(1), read(), NOW);
        let second = authority.root(agent(2), read(), NOW);

        assert_ne!(first.id().value(), second.id().value());
        assert!(authority.revoke(first.id(), revoked_at));

        assert_eq!(
            authority.decide(&request(1), revoked_at),
            Decision::Deny(Denial::new(DenialReason::Revoked { at: revoked_at })),
        );
        assert_eq!(
            authority.decide(&request(2), revoked_at),
            Decision::Allow(second),
        );
    }
}
