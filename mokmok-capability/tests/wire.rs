// The `cfg(test)` module is what tells the `allow-unwrap-in-tests`, `allow-expect-in-tests`, and
// `allow-panic-in-tests` settings in `.clippy.toml` that this harness is test code.
#[cfg(test)]
mod tests {
    use mokmok_capability::{Action, ActionSet, Context, Name, Request, Resource};
    use mokmok_protocol::{PrincipalID, SessionID};
    use uuid::Uuid;

    const fn principal(seed: u128) -> PrincipalID {
        PrincipalID::from_uuid(Uuid::from_u128(seed))
    }

    const fn session(seed: u128) -> SessionID {
        SessionID::from_uuid(Uuid::from_u128(seed))
    }

    #[test]
    fn an_action_set_arrives_as_a_nonempty_subset_of_the_five() {
        assert_eq!(serde_json::to_string(&ActionSet::READ).unwrap(), "1");
        assert_eq!(serde_json::to_string(&ActionSet::ALL).unwrap(), "31");
        assert_eq!(
            serde_json::from_str::<ActionSet>("1").unwrap(),
            ActionSet::READ,
        );
        assert!(serde_json::from_str::<ActionSet>("0").is_err());
        assert!(serde_json::from_str::<ActionSet>("32").is_err());
    }

    #[test]
    fn a_name_arrives_only_as_valid_as_construction_makes_it() {
        assert_eq!(
            serde_json::from_str::<Name>("\"deploy\"").unwrap().as_str(),
            "deploy",
        );
        assert!(serde_json::from_str::<Name>("\"write file\"").is_err());
        assert!(serde_json::from_str::<Name>("\"\"").is_err());
    }

    #[test]
    fn a_request_carries_its_context_in_full() {
        let request = Request {
            principal: principal(1),
            action: Action::Read,
            resource: Resource::Tool(Name::new("deploy").unwrap()),
            context: Context {
                session: Some(session(7)),
                execution: None,
                turn: None,
            },
        };

        assert_eq!(
            serde_json::to_string(&request).unwrap(),
            r#"{"principal":"00000000-0000-0000-0000-000000000001","action":"read","resource":{"tool":"deploy"},"context":{"session":"00000000-0000-0000-0000-000000000007","execution":null,"turn":null}}"#,
        );
    }
}
