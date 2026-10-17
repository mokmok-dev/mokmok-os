//! Discovery and activation of the operator's skills: the `SKILL.md` files under the roots.
//!
//! Discovery is the kernel's, because it opens files; the runtime only freezes what it is handed. A
//! root's immediate subdirectories are candidates. A candidate without `SKILL.md` is not a skill; a
//! candidate whose file is invalid or too large is skipped and reported, never fatal, because a root
//! is a directory the operator shares with other tools and skipping withholds instructions rather
//! than refusing the whole run. One name held by two roots is the exception: two files claiming one
//! name would make the model's choice ambiguous, so the run is refused naming both.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use mokmok_agent_runtime::{Advert, Error as AgentError, Read, Skills};
use mokmok_artifact::ContentHash;
use mokmok_skill::SkillName;
use mokmok_skill::{MAX_FRONTMATTER_BYTES, MAX_INSTRUCTIONS_BYTES, parse};

use crate::config::ConfigError;

const SKILL_FILE: &str = "SKILL.md";

/// Reads every valid skill under `roots` into one advert, in `roots` order and by directory name.
///
/// # Errors
///
/// Returns [`ConfigError::DuplicateSkill`] when two roots hold a directory of the same name, naming
/// both files. A candidate whose `SKILL.md` cannot be read, is too large for the parser, or the
/// parser refuses is skipped with a warning rather than aborting the run.
pub fn discover(roots: &[PathBuf]) -> Result<Vec<Advert>, ConfigError> {
    let mut found: Vec<Advert> = Vec::new();
    let mut held: HashMap<SkillName, PathBuf> = HashMap::new();

    for root in roots {
        for directory in candidates(root) {
            let file = directory.join(SKILL_FILE);
            if !file.is_file() {
                continue;
            }
            let Some(text) = read_candidate(&file) else {
                continue;
            };
            let Some(name) = directory_name(&directory) else {
                tracing::warn!(path = %file.display(), "skipping a skill whose directory name is not text");
                continue;
            };
            match parse(name, &text) {
                Ok(skill) => {
                    if let Some(first) = held.get(&skill.summary.name) {
                        return Err(ConfigError::DuplicateSkill {
                            name: skill.summary.name.to_string(),
                            first: first.clone(),
                            second: file,
                        });
                    }
                    held.insert(skill.summary.name.clone(), file.clone());
                    found.push(Advert {
                        name: skill.summary.name,
                        description: skill.summary.description,
                        source: file,
                        digest: ContentHash::of(text.as_bytes()).to_string(),
                    });
                },
                Err(refusal) => {
                    tracing::warn!(path = %file.display(), reason = %refusal, "skipping a skill");
                },
            }
        }
    }

    Ok(found)
}

/// The immediate subdirectories of one root, sorted by path.
///
/// `read_dir` yields entries in an unspecified order, so the sort is what keeps a re-run's advert
/// list — and the record it freezes — identical from one invocation to the next.
fn candidates(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        tracing::warn!(root = %root.display(), "skipping a skill root that cannot be listed");

        return Vec::new();
    };
    let mut candidates: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .collect();
    candidates.sort();

    candidates
}

/// One candidate's text, or `None` after a warning when it is too large or cannot be read.
///
/// The size is checked before the read, so a runaway file never has its bytes brought into memory.
fn read_candidate(file: &Path) -> Option<String> {
    let limit = u64::try_from(MAX_FRONTMATTER_BYTES + MAX_INSTRUCTIONS_BYTES).unwrap_or(u64::MAX);
    match std::fs::metadata(file) {
        Ok(metadata) if metadata.len() > limit => {
            tracing::warn!(
                path = %file.display(),
                bytes = metadata.len(),
                "skipping a skill larger than the parser reads",
            );

            return None;
        },
        Ok(_) => {},
        Err(error) => {
            tracing::warn!(path = %file.display(), reason = %error, "skipping a skill that cannot be read");

            return None;
        },
    }

    match std::fs::read_to_string(file) {
        Ok(text) => Some(text),
        Err(error) => {
            tracing::warn!(path = %file.display(), reason = %error, "skipping a skill that cannot be read");

            None
        },
    }
}

/// The name of a candidate's directory, which the parser requires its `name` to equal.
fn directory_name(directory: &Path) -> Option<&str> {
    directory.file_name().and_then(std::ffi::OsStr::to_str)
}

/// The skills one session's recorded task advertised, as the runtime's [`Skills`] port.
///
/// Built from the recorded [`Task`](mokmok_agent_runtime::Task), so a resumed session offers exactly
/// the skills its record advertises rather than whatever the roots hold now.
pub struct Library {
    adverts: Vec<Advert>,
}

impl Library {
    #[must_use]
    pub fn open(adverts: &[Advert]) -> Self {
        Self {
            adverts: adverts.to_vec(),
        }
    }
}

impl Skills for Library {
    /// Re-reads the advert's own file and answers its body only if it still matches.
    ///
    /// # Errors
    ///
    /// Never fails for a skill that is gone, changed, or no longer valid: each of those is a
    /// [`Read::Refused`] the model reads. The port's own failure is reserved for a real adapter
    /// fault, which none of these reads produce.
    fn read(
        &self,
        advert: &Advert,
    ) -> Result<Read, AgentError> {
        let Some(held) = self.adverts.iter().find(|held| held.name == advert.name) else {
            return Ok(Read::Refused(format!(
                "no skill named {} was advertised",
                advert.name,
            )));
        };

        Ok(activation(held))
    }
}

/// What re-reading one advertised file answers.
///
/// The parse takes the advert's name as the directory name, because discovery only advertised a
/// file whose directory and name already agreed, and a change that breaks that agreement is exactly
/// what the re-parse must refuse.
fn activation(advert: &Advert) -> Read {
    let text = match std::fs::read_to_string(&advert.source) {
        Ok(text) => text,
        Err(error) => {
            return Read::Refused(format!(
                "the skill {} could not be read: {error}",
                advert.name,
            ));
        },
    };

    if ContentHash::of(text.as_bytes()).to_string() != advert.digest {
        return Read::Refused(format!(
            "the skill {} changed after it was discovered",
            advert.name,
        ));
    }

    match parse(advert.name.as_str(), &text) {
        Ok(skill) => Read::Body(skill.instructions),
        Err(refusal) => Read::Refused(format!(
            "the skill {} is no longer valid: {refusal}",
            advert.name,
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use mokmok_agent_runtime::{Read, Skills};
    use mokmok_skill::{MAX_FRONTMATTER_BYTES, MAX_INSTRUCTIONS_BYTES};

    use super::{Library, discover};

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("mokmok-kernel-skill-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();

            Self(path)
        }

        fn root(
            &self,
            name: &str,
        ) -> PathBuf {
            let root = self.0.join(name);
            std::fs::create_dir_all(&root).unwrap();

            root
        }
    }

    /// A skill root holding `directory/SKILL.md` with `text`.
    fn skill(
        root: &Path,
        directory: &str,
        text: &str,
    ) -> PathBuf {
        let directory = root.join(directory);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("SKILL.md"), text).unwrap();

        directory
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn valid(name: &str) -> String {
        format!("---\nname: {name}\ndescription: a {name} skill\n---\nthe {name} body\n")
    }

    use std::sync::{Mutex, Once};

    /// Every logged byte, gathered so a test can read the reason discovery gave.
    static LOGS: Mutex<Vec<u8>> = Mutex::new(Vec::new());

    /// A [`MakeWriter`](tracing_subscriber::fmt::MakeWriter) over [`LOGS`].
    struct GlobalShim;

    struct GlobalWriter;

    impl std::io::Write for GlobalWriter {
        fn write(
            &mut self,
            bytes: &[u8],
        ) -> std::io::Result<usize> {
            LOGS.lock().unwrap().extend_from_slice(bytes);

            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for GlobalShim {
        type Writer = GlobalWriter;

        fn make_writer(&'a self) -> Self::Writer {
            GlobalWriter
        }
    }

    /// Installs [`LOGS`] as the process's subscriber, once, so a warning has somewhere to go.
    fn capture_logs() {
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            let subscriber = tracing_subscriber::fmt()
                .with_writer(GlobalShim)
                .with_ansi(false)
                .with_max_level(tracing::Level::TRACE)
                .finish();
            let _ = tracing::subscriber::set_global_default(subscriber);
        });
    }

    fn logged() -> String {
        String::from_utf8(LOGS.lock().unwrap().clone()).unwrap_or_default()
    }

    #[test]
    fn an_invalid_skill_is_skipped_and_reported_naming_the_path_and_the_reason() {
        let scratch = Scratch::new("reported");
        let root = scratch.root("skills");
        let directory = skill(
            &root,
            "bad",
            "---\nname: Bad Name\ndescription: d\n---\nbody\n",
        );
        capture_logs();

        let adverts = discover(std::slice::from_ref(&root)).unwrap();

        assert_eq!(adverts.len(), 0, "an invalid skill is not advertised");
        let logged = logged();
        assert!(
            logged.contains(&directory.join("SKILL.md").display().to_string()),
            "the warning names the path: {logged}",
        );
        assert!(
            logged.contains("Bad Name"),
            "the warning carries the parser's reason: {logged}",
        );
    }

    #[test]
    fn a_valid_skill_is_discovered_with_its_digest_and_source() {
        let scratch = Scratch::new("valid");
        let root = scratch.root("skills");
        let directory = skill(&root, "pdf-processing", &valid("pdf-processing"));

        let adverts = discover(std::slice::from_ref(&root)).unwrap();

        assert_eq!(adverts.len(), 1);
        assert_eq!(adverts[0].name.as_str(), "pdf-processing");
        assert_eq!(adverts[0].description, "a pdf-processing skill");
        assert_eq!(adverts[0].source, directory.join("SKILL.md"));
        assert_ne!(adverts[0].digest, "");
    }

    #[test]
    fn a_directory_without_a_skill_file_is_not_a_skill() {
        let scratch = Scratch::new("no-file");
        let root = scratch.root("skills");
        std::fs::create_dir_all(root.join("just-a-directory")).unwrap();

        assert_eq!(discover(std::slice::from_ref(&root)).unwrap().len(), 0);
    }

    #[test]
    fn an_invalid_skill_is_skipped_and_a_valid_one_beside_it_is_kept() {
        let scratch = Scratch::new("skip");
        let root = scratch.root("skills");
        skill(&root, "good", &valid("good"));
        skill(
            &root,
            "bad",
            "---\nname: Bad Name\ndescription: d\n---\nbody\n",
        );

        let adverts = discover(std::slice::from_ref(&root)).unwrap();

        assert_eq!(adverts.len(), 1);
        assert_eq!(adverts[0].name.as_str(), "good");
    }

    #[test]
    fn an_oversized_skill_file_is_skipped_by_the_size_warning() {
        let scratch = Scratch::new("oversize");
        let root = scratch.root("skills");
        let body = "x".repeat(64 * 1024);
        let directory = skill(
            &root,
            "huge",
            &format!("---\nname: huge\ndescription: d\n---\n{body}"),
        );
        capture_logs();

        assert_eq!(discover(std::slice::from_ref(&root)).unwrap().len(), 0);
        let logged = logged();
        assert!(
            logged.contains("larger than the parser reads"),
            "the size gate is what skips it, not the parser's later refusal: {logged}",
        );
        assert!(
            logged.contains(&directory.join("SKILL.md").display().to_string()),
            "the size warning names the path: {logged}",
        );
    }

    #[test]
    fn a_file_exactly_at_the_size_limit_is_advertised() {
        let scratch = Scratch::new("at-limit");
        let root = scratch.root("skills");
        let name = "exact";
        let fence = "---\n";
        let prefix = format!("{fence}name: {name}\ndescription: d\n");
        let vendor = "vendor: \n";
        let padding = MAX_FRONTMATTER_BYTES - prefix.len() - fence.len() - vendor.len();
        let body = "x".repeat(MAX_INSTRUCTIONS_BYTES);
        let text = format!("{prefix}vendor: {}\n{fence}{body}", "v".repeat(padding));
        assert_eq!(text.len(), MAX_FRONTMATTER_BYTES + MAX_INSTRUCTIONS_BYTES);
        skill(&root, name, &text);

        let adverts = discover(std::slice::from_ref(&root)).unwrap();

        assert_eq!(
            adverts.len(),
            1,
            "a file exactly at the gate is read, and both parser caps are inclusive",
        );
        assert_eq!(adverts[0].name.as_str(), name);
    }

    #[test]
    fn entries_are_sorted_by_directory_name() {
        let scratch = Scratch::new("order");
        let root = scratch.root("skills");
        // Created out of order, so only the sort makes the advert order deterministic.
        skill(&root, "zebra", &valid("zebra"));
        skill(&root, "alpha", &valid("alpha"));
        skill(&root, "mango", &valid("mango"));

        let names: Vec<String> = discover(std::slice::from_ref(&root))
            .unwrap()
            .into_iter()
            .map(|advert| advert.name.to_string())
            .collect();

        assert_eq!(names, vec!["alpha", "mango", "zebra"]);
    }

    #[test]
    fn one_name_held_by_two_roots_is_refused_naming_both_paths() {
        let scratch = Scratch::new("duplicate");
        let first = scratch.root("one");
        let second = scratch.root("two");
        skill(&first, "pdf-processing", &valid("pdf-processing"));
        skill(&second, "pdf-processing", &valid("pdf-processing"));

        let refused = discover(&[first, second]).unwrap_err();

        assert!(
            matches!(refused, crate::config::ConfigError::DuplicateSkill { .. }),
            "{refused}",
        );
        assert!(refused.to_string().contains("pdf-processing"), "{refused}");
    }

    #[test]
    fn the_library_answers_the_body_when_the_file_still_matches() {
        let scratch = Scratch::new("activation");
        let root = scratch.root("skills");
        skill(&root, "pdf-processing", &valid("pdf-processing"));
        let adverts = discover(std::slice::from_ref(&root)).unwrap();
        let library = Library::open(&adverts);

        assert_eq!(
            library.read(&adverts[0]).unwrap(),
            Read::Body("the pdf-processing body\n".to_owned()),
        );
    }

    #[test]
    fn the_library_refuses_a_file_that_changed_after_discovery() {
        let scratch = Scratch::new("changed");
        let root = scratch.root("skills");
        let directory = skill(&root, "pdf-processing", &valid("pdf-processing"));
        let adverts = discover(std::slice::from_ref(&root)).unwrap();
        let library = Library::open(&adverts);
        std::fs::write(
            directory.join("SKILL.md"),
            valid("pdf-processing") + "changed\n",
        )
        .unwrap();

        let read = library.read(&adverts[0]).unwrap();

        assert!(
            matches!(read, Read::Refused(ref reason) if reason.contains("changed")),
            "{read:?}",
        );
    }

    #[test]
    fn the_library_refuses_a_file_that_vanished() {
        let scratch = Scratch::new("vanished");
        let root = scratch.root("skills");
        let directory = skill(&root, "pdf-processing", &valid("pdf-processing"));
        let adverts = discover(std::slice::from_ref(&root)).unwrap();
        let library = Library::open(&adverts);
        std::fs::remove_file(directory.join("SKILL.md")).unwrap();

        assert!(matches!(
            library.read(&adverts[0]).unwrap(),
            Read::Refused(_),
        ));
    }

    #[test]
    fn the_library_refuses_a_name_it_does_not_hold() {
        let scratch = Scratch::new("unheld");
        let root = scratch.root("skills");
        skill(&root, "pdf-processing", &valid("pdf-processing"));
        let adverts = discover(std::slice::from_ref(&root)).unwrap();
        let library = Library::open(&adverts);
        let mut other = adverts[0].clone();
        other.name = mokmok_skill::SkillName::new("quokka").unwrap();

        assert!(matches!(library.read(&other).unwrap(), Read::Refused(_)));
    }
}
