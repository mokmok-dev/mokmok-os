// The `cfg(test)` module is what tells the `allow-unwrap-in-tests`, `allow-expect-in-tests`, and
// `allow-panic-in-tests` settings in `.clippy.toml` that this harness is test code.
#[cfg(test)]
mod tests {
    use mokmok_capability::{
        Action, ActionSet, Authority, Context, Decision, Name, Permission, Policy, Principal,
        PrincipalKind, Request, Resource, Rule,
    };
    use mokmok_protocol::{Moment, PrincipalID};
    use proptest::prelude::*;
    use uuid::Uuid;

    const NOW: Moment = Moment::from_millis(1_000);
    const ACTIONS: [Action; 5] = [
        Action::Read,
        Action::Write,
        Action::Execute,
        Action::Connect,
        Action::Invoke,
    ];

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

    fn tool(seed: u128) -> Resource {
        Resource::Tool(Name::new(format!("tool-{seed}")).unwrap())
    }

    /// Whether a child expiry would outlive its parent's, derived here rather than read out of the
    /// code the property checks.
    fn outlives(
        child: Option<Moment>,
        parent: Option<Moment>,
    ) -> bool {
        parent.is_some_and(|bound| child.is_none_or(|limit| limit > bound))
    }

    proptest! {
        #[test]
        fn an_allow_names_a_grant_attenuated_from_a_root(
            ops in prop::collection::vec(0u8..6, 0..48),
            picks in prop::collection::vec(0u8..8, 0..48),
        ) {
            // A policy that names every operation, so what a decision allows is decided by the
            // grants alone and the generated sequence exercises issue, revoke, and decide.
            let mut authority = Authority::new(Policy::new().with(Rule::allow()), NOW);
            let mut minted: Vec<mokmok_capability::Grant> = Vec::new();
            let mut allowed = 0;

            for seed in 0..3u128 {
                minted.push(authority.root(
                    agent(seed),
                    Permission::new(tool(seed), ActionSet::ALL),
                    NOW,
                ));
            }

            for (index, op) in ops.iter().enumerate() {
                let pick = usize::from(picks.get(index).copied().unwrap_or(0)) % minted.len();

                match op % 3 {
                    0 => {
                        // Half the requests are for everything, which a grant issued from an
                        // already narrowed grant cannot cover, so some issues are refused.
                        let requested = if op % 6 < 3 {
                            ActionSet::ALL
                        } else {
                            ActionSet::of(ACTIONS[pick % ACTIONS.len()])
                        };
                        let permission = Permission::new(
                            minted[pick].permission().resource().clone(),
                            requested,
                        );

                        if let Ok(child) = authority.issue(&minted[pick], principal(3), permission, None, NOW) {
                            minted.push(child);
                        }
                    },
                    1 => {
                        authority.revoke(minted[pick].id(), NOW);
                    },
                    _ => {
                        let request = Request {
                            principal: principal(u128::from(op % 4)),
                            action: ACTIONS[pick % ACTIONS.len()],
                            resource: tool(u128::from(op % 4)),
                            context: Context::none(),
                        };

                        if let Decision::Allow(grant) = authority.decide(&request, NOW) {
                            allowed += 1;

                            prop_assert_eq!(grant.principal(), request.principal);
                            prop_assert!(minted.iter().any(|held| held.id() == grant.id()));
                            prop_assert!(grant.permission().covers(&Permission::new(
                                request.resource.clone(),
                                ActionSet::of(request.action),
                            )));

                            let mut current = &grant;
                            let mut links = 0;

                            while let Some(parent) = current.parent() {
                                let ancestor = minted.iter().find(|held| held.id() == parent);
                                prop_assert!(ancestor.is_some());
                                let ancestor = ancestor.unwrap();

                                prop_assert!(ancestor.permission().covers(current.permission()));
                                prop_assert!(!outlives(current.not_after(), ancestor.not_after()));

                                current = ancestor;
                                links += 1;
                                prop_assert!(links <= minted.len());
                            }
                        }
                    },
                }
            }

            // A sequence that allowed nothing would satisfy the property without checking
            // anything, and a decision that stopped allowing would leave it with nothing to state.
            prop_assume!(allowed > 0);
        }
    }
}
