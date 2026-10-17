//! What the operator asked a session to do, and the limits it runs under.

use mokmok_skill::SkillName;
use std::path::PathBuf;
use std::time::Duration;

/// The metadata of one discovered skill, frozen into the task the session records.
///
/// The body is deliberately absent: progressive disclosure reads it only when the model asks for it
/// by name, and the read is a separate port. The digest binds the record to the bytes activation
/// will hand the model, so a file that changed after discovery is refused rather than silently
/// substituted.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Advert {
    /// The validated name the model loads the skill by.
    pub name: SkillName,
    /// The one-line description the prompt advertises the name with.
    pub description: String,
    /// The `SKILL.md` activation re-reads. Never advertised; it is the read source, not metadata.
    pub source: PathBuf,
    /// The sha256 of that file at discovery.
    ///
    /// Activation refuses if the file no longer hashes to this, so the body the model receives is the
    /// body the session was told about.
    pub digest: String,
}

/// The goal, the world it runs in, and the budget it may spend.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Task {
    pub goal: String,
    pub setting: Setting,
    pub budget: Budget,
}

/// What the model is told it has, and nothing about how the operator enforces it.
///
/// The grant that confines a command is the operator's; nothing here is a policy or a capability,
/// so a `Policy`, `Rule`, or grant never reaches the runtime.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Setting {
    /// The scratch checkout the goal lives in, reached by absolute path inside a command.
    pub checkout: PathBuf,
    /// Directories the operator granted for reading and executing.
    pub readable: Vec<PathBuf>,
    /// Directories the operator granted for reading, writing, and executing.
    pub writable: Vec<PathBuf>,
    /// The one program a command runs as, named by the operator and never by the model.
    pub program: PathBuf,
    /// How long one command may run, as the operator set it when the task was recorded.
    pub deadline: Duration,
    /// Directories the operator granted for searching. Empty means no finder is offered.
    ///
    /// `default` keeps a task record written before this field readable: an older setting folds with
    /// no root rather than refusing to decode.
    #[serde(default)]
    pub search: Vec<PathBuf>,
    /// Skills the operator advertised to this session, frozen at discovery.
    ///
    /// `default` keeps a task record written before the field readable, the same rule as `search`.
    #[serde(default)]
    pub skills: Vec<Advert>,
}

/// The ceilings one session runs under.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Budget {
    /// How many turns the session may open.
    pub turns: u32,
    /// How many model attempts one turn may spend before it closes failed.
    pub attempts: u32,
    /// How many tokens the session may spend; zero means unbounded.
    pub tokens: u64,
    /// How many bytes of one observation the model is shown.
    pub observation_bytes: u64,
    /// How many settled turns the model is shown.
    pub context_turns: u32,
    /// How many bytes of context the model is shown in total.
    pub context_bytes: u64,
}

impl Budget {
    /// The attempt ceiling a turn gets when the caller names none.
    pub const DEFAULT_ATTEMPTS: u32 = 3;
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            turns: 0,
            attempts: Self::DEFAULT_ATTEMPTS,
            tokens: 0,
            observation_bytes: 0,
            context_turns: 0,
            context_bytes: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Setting;
    use std::time::Duration;

    #[test]
    fn a_setting_written_before_the_search_field_still_decodes() {
        let json = r#"{
            "checkout": "/srv/checkout",
            "readable": ["/nix/store"],
            "writable": ["/srv/checkout"],
            "program": "/bin/sh",
            "deadline": {"secs": 120, "nanos": 0}
        }"#;

        let setting: Setting = serde_json::from_str(json).unwrap();

        assert_eq!(
            setting,
            Setting {
                checkout: "/srv/checkout".into(),
                readable: vec!["/nix/store".into()],
                writable: vec!["/srv/checkout".into()],
                program: "/bin/sh".into(),
                deadline: Duration::from_secs(120),
                search: Vec::new(),
                skills: Vec::new(),
            },
        );
    }

    #[test]
    fn a_setting_written_before_the_skills_field_still_decodes() {
        let json = r#"{
            "checkout": "/srv/checkout",
            "readable": ["/nix/store"],
            "writable": ["/srv/checkout"],
            "program": "/bin/sh",
            "deadline": {"secs": 120, "nanos": 0},
            "search": ["/srv/checkout"]
        }"#;

        let setting: Setting = serde_json::from_str(json).unwrap();

        assert_eq!(
            setting.search,
            vec![std::path::PathBuf::from("/srv/checkout")]
        );
        assert_eq!(setting.skills, Vec::new());
    }
}
