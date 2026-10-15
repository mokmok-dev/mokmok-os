//! The finder: one BM25 corpus per granted root, read in the daemon's own process.
//!
//! A find is answered here rather than in the confined workload, so a search adds nothing to what a
//! workload may reach and no index can be poisoned by the thing it describes. Each root's corpus
//! keeps its index in a directory of its own under the configured store, named by a hash of the
//! root's path, so a session resumed with the same root reuses the index an earlier run built
//! instead of indexing the tree again.
//!
//! One find refreshes every corpus before it queries it: a pass reads only the files whose stamp
//! moved, and a command may have changed the checkout since the last one.

use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};

use mokmok_agent_runtime::{Error as AgentError, FindReport, Finder, NO_SEARCH_ROOT};
use mokmok_artifact::ContentHash;
use mokmok_search::{Corpus, Error as SearchError, Hit, Limits, Response, render};

pub struct Index {
    corpora: Vec<Corpus>,
}

impl Index {
    /// Opens one corpus per granted root, each under `store/<key>`, `key` naming the root's path.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::Finder`] when a root cannot be listed or its index cannot be opened or
    /// created. An index that quietly covers fewer roots than the operator named would answer
    /// every later find from a tree the operator did not choose, so the caller refuses the run
    /// instead.
    pub fn open(
        roots: &[PathBuf],
        store: &Path,
    ) -> Result<Self, AgentError> {
        let mut corpora = Vec::with_capacity(roots.len());

        for root in roots {
            let corpus =
                Corpus::open(root, &store.join(key(root)), Limits::default()).map_err(finder)?;
            corpora.push(corpus);
        }

        Ok(Self { corpora })
    }

    /// The finder a session that was granted no root runs.
    ///
    /// It opens no index and creates no directory, and every answer it gives is [`NO_SEARCH_ROOT`].
    #[must_use]
    pub const fn none() -> Self {
        Self {
            corpora: Vec::new(),
        }
    }
}

impl Finder for Index {
    fn find(
        &self,
        query: &str,
    ) -> Result<FindReport, AgentError> {
        if self.corpora.is_empty() {
            return Ok(FindReport {
                observation: NO_SEARCH_ROOT.to_owned(),
            });
        }

        let mut indexed = 0_u64;
        let mut hits: Vec<Hit> = Vec::new();
        for corpus in &self.corpora {
            let refresh = corpus.refresh().map_err(finder)?;
            indexed += refresh.indexed;
            hits.extend(
                corpus
                    .query(query, Corpus::DEFAULT_LIMIT)
                    .map_err(finder)?
                    .hits,
            );
        }

        // Every root's hits compete on their own score, so a root is never privileged for coming
        // first in the grant list, and a score tie is settled by path. The header's `indexed` is the
        // sum across the roots, which is the whole tree the operator granted rather than any one
        // root's part of it.
        hits.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.path.cmp(&right.path))
        });
        hits.truncate(Corpus::DEFAULT_LIMIT);

        Ok(FindReport {
            observation: render(&Response { indexed, hits }),
        })
    }
}

/// The directory one root's index lives in, named by the hash of the root's path.
///
/// The path is hashed rather than carried through, because a grant is an absolute path that would
/// not fit a directory name and a root renamed on disk must not silently inherit another tree's
/// index.
fn key(root: &Path) -> String {
    ContentHash::of(root.as_os_str().as_bytes()).to_string()
}

/// Erases the search crate's failure behind the runtime's finder port.
///
/// A failed search stays a finder failure rather than becoming a tool failure: no command ran, so
/// nothing about a find may read as one that did.
fn finder(error: SearchError) -> AgentError {
    AgentError::Finder(Box::new(error))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use mokmok_agent_runtime::{Error as AgentError, Finder, NO_SEARCH_ROOT};

    use super::Index;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("mokmok-kernel-index-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();

            Self(path)
        }

        fn root(&self) -> PathBuf {
            let root = self.0.join("root");
            std::fs::create_dir_all(&root).unwrap();

            root
        }

        fn store(&self) -> PathBuf {
            self.0.join("search")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write(
        root: &Path,
        name: &str,
        body: &str,
    ) -> PathBuf {
        let path = root.join(name);
        std::fs::write(&path, body).unwrap();

        path
    }

    fn observation(
        index: &Index,
        query: &str,
    ) -> String {
        index.find(query).unwrap().observation
    }

    #[test]
    fn a_hit_names_the_file_the_query_landed_in() {
        let scratch = Scratch::new("hit");
        let root = scratch.root();
        let file = write(&root, "notes.txt", "the quokka walks at dawn\n");
        write(&root, "other.txt", "nothing to see\n");
        let index = Index::open(&[root], &scratch.store()).unwrap();

        let found = observation(&index, "quokka");

        assert_eq!(
            found.lines().next(),
            Some("indexed=2 hits=1"),
            "the header counts the files the index holds and the one hit: {found}",
        );
        assert!(
            found.contains(&file.display().to_string()),
            "the observation names the hit's path: {found}",
        );
    }

    #[test]
    fn a_query_that_matches_nothing_answers_a_header_and_no_hit() {
        let scratch = Scratch::new("hitless");
        let root = scratch.root();
        write(&root, "notes.txt", "the quokka walks at dawn\n");
        let index = Index::open(&[root], &scratch.store()).unwrap();

        assert_eq!(observation(&index, "petrichor"), "indexed=1 hits=0\n");
    }

    #[test]
    fn a_find_over_two_roots_merges_both_into_one_ranking() {
        let scratch = Scratch::new("merge");
        let first = scratch.root();
        let second = scratch.0.join("second");
        std::fs::create_dir_all(&second).unwrap();
        let here = write(&first, "here.txt", "the quokka walks at dawn\n");
        let there = write(&second, "there.txt", "a quokka is here too\n");
        let index = Index::open(&[first, second], &scratch.store()).unwrap();

        let found = observation(&index, "quokka");

        assert_eq!(found.lines().next(), Some("indexed=2 hits=2"), "{found}");
        assert!(found.contains(&here.display().to_string()), "{found}");
        assert!(found.contains(&there.display().to_string()), "{found}");
        assert_eq!(
            std::fs::read_dir(scratch.store()).unwrap().count(),
            2,
            "two roots name two index directories, so one tree never answers for the other",
        );
    }

    #[test]
    fn one_find_carries_no_more_than_the_default_limit() {
        let scratch = Scratch::new("limit");
        let root = scratch.root();
        for number in 0..12 {
            write(
                &root,
                &format!("notes-{number}.txt"),
                "the quokka walks at dawn\n",
            );
        }
        let index = Index::open(&[root], &scratch.store()).unwrap();

        let found = observation(&index, "quokka");

        assert_eq!(
            found.lines().next(),
            Some("indexed=12 hits=10"),
            "the model reads the best few of what the index holds, not all of it: {found}",
        );
    }

    #[test]
    fn a_session_with_no_root_answers_that_it_was_granted_none() {
        assert_eq!(observation(&Index::none(), "quokka"), NO_SEARCH_ROOT);
    }

    #[test]
    fn a_root_that_cannot_be_listed_is_a_finder_failure() {
        let scratch = Scratch::new("missing-root");
        let absent = scratch.0.join("no-such-root");

        let Err(refused) = Index::open(&[absent], &scratch.store()) else {
            panic!("a root that cannot be listed is refused rather than searched empty");
        };

        assert!(
            matches!(refused, AgentError::Finder(_)),
            "a search failure must not read as a tool failure: {refused:?}",
        );
        assert!(
            !scratch.store().exists(),
            "a refused root creates no index directory",
        );
    }

    #[test]
    fn reopening_the_same_root_reuses_the_one_index_it_built() {
        let scratch = Scratch::new("stable-key");
        let root = scratch.root();
        let file = write(&root, "notes.txt", "the quokka walks at dawn\n");

        let first = Index::open(std::slice::from_ref(&root), &scratch.store()).unwrap();
        assert!(observation(&first, "quokka").contains(&file.display().to_string()));
        drop(first);

        let indexed: Vec<PathBuf> = std::fs::read_dir(scratch.store())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(indexed.len(), 1, "one root owns one index directory");

        let second = Index::open(&[root], &scratch.store()).unwrap();
        assert!(observation(&second, "quokka").contains(&file.display().to_string()));
        assert_eq!(
            std::fs::read_dir(scratch.store()).unwrap().count(),
            1,
            "a resumed session reuses the index the earlier one built",
        );
    }

    #[test]
    fn a_root_the_workload_wrote_to_is_searched_as_it_stands_now() {
        let scratch = Scratch::new("refresh");
        let root = scratch.root();
        write(&root, "notes.txt", "nothing to see\n");
        let index = Index::open(std::slice::from_ref(&root), &scratch.store()).unwrap();

        assert_eq!(observation(&index, "quokka"), "indexed=1 hits=0\n");

        let file = write(&root, "later.txt", "the quokka walks at dawn\n");

        let found = observation(&index, "quokka");

        assert_eq!(found.lines().next(), Some("indexed=2 hits=1"), "{found}");
        assert!(found.contains(&file.display().to_string()), "{found}");
    }
}
