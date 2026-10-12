// The `cfg(test)` module is what tells the `allow-unwrap-in-tests`, `allow-expect-in-tests`, and
// `allow-panic-in-tests` settings in `.clippy.toml` that this harness is test code.
#[cfg(test)]
mod tests {
    use mokmok_capability::{
        Action, ApprovalError, Attenuation, Authority, Context, Decision, Denial, DenialReason,
        InvalidName, Name, Policy, Request, Requirement, Resource, Rule,
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

    fn invoke() -> Request {
        Request {
            principal: principal(2),
            action: Action::Invoke,
            resource: deploy(),
            context: Context {
                session: Some(session(10)),
                execution: None,
                turn: None,
            },
        }
    }

    fn policy(bound: Option<Moment>) -> Policy {
        let rule = Rule::requires_approval()
            .for_action(Action::Invoke)
            .for_resource(deploy());

        match bound {
            Some(at) => Policy::new().with(rule.not_after(at)),
            None => Policy::new().with(rule),
        }
    }

    fn requirement(bound: Option<Moment>) -> Requirement {
        let authority = Authority::new(policy(bound), NOW);

        match authority.decide(&invoke(), NOW) {
            Decision::Approve(requirement) => requirement,
            other => panic!("the approval rule should have asked for one, got {other:?}"),
        }
    }

    #[test]
    fn a_resource_renders_its_kind_and_name() {
        assert_eq!(
            Resource::Path(String::from("/data")).to_string(),
            "path /data"
        );
        assert_eq!(deploy().to_string(), "tool deploy");
        assert_eq!(
            Resource::Model(Name::new("gpt").unwrap()).to_string(),
            "model gpt",
        );
        assert_eq!(
            Resource::Host {
                host: String::from("example.com"),
                port: 443,
            }
            .to_string(),
            "host example.com:443",
        );
    }

    #[test]
    fn an_action_renders_its_lowercase_name() {
        assert_eq!(Action::Read.to_string(), "read");
        assert_eq!(Action::Write.to_string(), "write");
        assert_eq!(Action::Execute.to_string(), "execute");
        assert_eq!(Action::Connect.to_string(), "connect");
        assert_eq!(Action::Invoke.to_string(), "invoke");
    }

    #[test]
    fn a_name_renders_the_token_it_holds() {
        assert_eq!(Name::new("analyst").unwrap().to_string(), "analyst");
    }

    #[test]
    fn a_refused_name_renders_the_rule_it_broke() {
        assert_eq!(InvalidName::Empty.to_string(), "a name cannot be empty");
        assert_eq!(
            InvalidName::Whitespace.to_string(),
            "a name cannot hold whitespace",
        );
        assert_eq!(
            InvalidName::TooLong.to_string(),
            "a name cannot be longer than 253 bytes",
        );
    }

    #[test]
    fn a_refused_attenuation_renders_why() {
        assert_eq!(
            Attenuation::NotASubset.to_string(),
            "the requested permission is not a subset of the parent grant's",
        );
        assert_eq!(
            Attenuation::OutlivesParent.to_string(),
            "the requested expiry outlives the parent grant's",
        );
        assert_eq!(
            Attenuation::AlreadyExpired.to_string(),
            "the requested expiry is not in the future",
        );
        assert_eq!(
            Attenuation::UnknownParent.to_string(),
            "the parent grant was not issued by this authority",
        );
    }

    #[test]
    fn a_refused_approval_renders_why() {
        assert_eq!(
            ApprovalError::SelfApproved.to_string(),
            "a principal cannot approve the authority it requests",
        );
        assert_eq!(
            ApprovalError::UnknownApprover.to_string(),
            "the approver is not a principal this authority holds",
        );
        assert_eq!(
            ApprovalError::RequirementExpired.to_string(),
            "the approval window has already closed",
        );
    }

    #[test]
    fn a_denial_renders_the_reason_it_rests_on() {
        assert_eq!(
            Denial::new(DenialReason::NoMatchingRule).to_string(),
            "no policy rule matches the request",
        );
        assert_eq!(
            Denial::new(DenialReason::DeniedByRule { rule: 3 }).to_string(),
            "policy rule 3 denies the request",
        );
        assert_eq!(
            Denial::new(DenialReason::NotGranted).to_string(),
            "no live grant from this authority covers the request",
        );
        assert_eq!(
            Denial::new(DenialReason::Expired {
                at: Moment::from_millis(1_500),
            })
            .to_string(),
            "the grant expired at 1500 ms",
        );
        assert_eq!(
            Denial::new(DenialReason::Revoked {
                at: Moment::from_millis(2_000),
            })
            .to_string(),
            "the grant was revoked at 2000 ms",
        );
    }

    #[test]
    fn a_requirement_renders_the_operation_it_names() {
        assert_eq!(
            requirement(None).to_string(),
            "00000000-0000-0000-0000-000000000002 may invoke tool deploy",
        );
    }

    #[test]
    fn a_bounded_requirement_renders_its_window() {
        assert_eq!(
            requirement(Some(Moment::from_millis(1_500))).to_string(),
            "00000000-0000-0000-0000-000000000002 may invoke tool deploy until 1500 ms",
        );
    }
}
