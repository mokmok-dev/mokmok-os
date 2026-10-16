use mokmok_search::{Corpus, Error, Hit, Limits, Refresh, Response, render};
use proptest::collection::vec;
use proptest::prelude::*;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, UNIX_EPOCH};

// The `cfg(test)` module is what tells the `allow-unwrap-in-tests`, `allow-expect-in-tests`, and
// `allow-panic-in-tests` settings in `.clippy.toml` that this harness is test code.
#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch tree of this test's own, removed when the test ends.
    struct Scratch {
        home: PathBuf,
    }

    impl Scratch {
        fn new(name: &str) -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);

            let scratch = Self {
                home: std::env::temp_dir().join(format!(
                    "mokmok-search-{name}-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed),
                )),
            };
            std::fs::create_dir_all(scratch.root()).unwrap();

            scratch
        }

        fn root(&self) -> PathBuf {
            self.home.join("root")
        }

        fn store(&self) -> PathBuf {
            self.home.join("store")
        }

        fn corpus(
            &self,
            limits: Limits,
        ) -> Corpus {
            Corpus::open(&self.root(), &self.store(), limits).unwrap()
        }

        fn write(
            &self,
            name: &str,
            body: &str,
        ) -> PathBuf {
            self.write_bytes(name, body.as_bytes())
        }

        fn write_bytes(
            &self,
            name: &str,
            body: &[u8],
        ) -> PathBuf {
            let path = self.root().join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, body).unwrap();

            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.home);
        }
    }

    fn paths(response: &Response) -> Vec<PathBuf> {
        response.hits.iter().map(|hit| hit.path.clone()).collect()
    }

    /// The modification time a pass records for `path`, in milliseconds.
    fn stamp(path: &Path) -> u128 {
        std::fs::metadata(path)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis()
    }

    /// Rewrites `body` at `path`, waiting until the stamp a pass records has moved: a filesystem
    /// clock is coarser than the write, and a pass sees the stamp alone.
    fn rewrite_later(
        path: &Path,
        body: &str,
    ) {
        let before = stamp(path);

        for _ in 0..500 {
            std::fs::write(path, body).unwrap();
            if stamp(path) != before {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// The hits of a response, ordered by path, so that two answers that differ only in the order
    /// of tied scores compare equal: tantivy orders ties by document address, which a later pass
    /// may lay out differently.
    fn ranked(response: &Response) -> Vec<(PathBuf, f32)> {
        let mut hits = response
            .hits
            .iter()
            .map(|hit| (hit.path.clone(), hit.score))
            .collect::<Vec<_>>();
        hits.sort_by(|left, right| left.0.cmp(&right.0).then(left.1.total_cmp(&right.1)));

        hits
    }

    #[test]
    fn the_defaults_are_the_ones_the_crate_documents() {
        assert_eq!(Corpus::DEFAULT_LIMIT, 10);
        assert_eq!(
            Limits::default(),
            Limits {
                file_bytes: 1024 * 1024,
                body_bytes: 256 * 1024,
                files: 50_000,
            },
        );
    }

    #[test]
    fn a_pass_indexes_the_tree_and_a_second_pass_leaves_it_alone() {
        let scratch = Scratch::new("unchanged");
        scratch.write("a.txt", "alpha");
        scratch.write("b.txt", "beta");
        let corpus = scratch.corpus(Limits::default());

        assert_eq!(
            corpus.refresh().unwrap(),
            Refresh {
                indexed: 2,
                updated: 2,
                removed: 0,
                unchanged: 0,
                skipped: 0,
            },
        );
        assert_eq!(
            corpus.refresh().unwrap(),
            Refresh {
                indexed: 2,
                updated: 0,
                removed: 0,
                unchanged: 2,
                skipped: 0,
            },
        );
    }

    #[test]
    fn touching_one_file_updates_exactly_that_one() {
        let scratch = Scratch::new("touch");
        scratch.write("a.txt", "alpha");
        let touched = scratch.write("b.txt", "beta");
        let corpus = scratch.corpus(Limits::default());
        corpus.refresh().unwrap();

        std::fs::write(&touched, "beta gamma").unwrap();
        let refresh = corpus.refresh().unwrap();

        assert_eq!(
            refresh,
            Refresh {
                indexed: 2,
                updated: 1,
                removed: 0,
                unchanged: 1,
                skipped: 0,
            },
        );
        assert_eq!(
            corpus
                .query("gamma", Corpus::DEFAULT_LIMIT)
                .unwrap()
                .hits
                .len(),
            1
        );
        assert_eq!(
            corpus
                .query("alpha", Corpus::DEFAULT_LIMIT)
                .unwrap()
                .hits
                .len(),
            1
        );
    }

    #[test]
    fn deleting_a_file_removes_exactly_its_hit() {
        let scratch = Scratch::new("delete");
        let deleted = scratch.write("a.txt", "alpha aardvark");
        let kept = scratch.write("b.txt", "beta");
        let corpus = scratch.corpus(Limits::default());
        corpus.refresh().unwrap();

        std::fs::remove_file(&deleted).unwrap();
        let refresh = corpus.refresh().unwrap();

        assert_eq!(
            refresh,
            Refresh {
                indexed: 1,
                updated: 0,
                removed: 1,
                unchanged: 1,
                skipped: 0,
            },
        );
        assert_eq!(
            corpus
                .query("aardvark", Corpus::DEFAULT_LIMIT)
                .unwrap()
                .hits,
            Vec::new()
        );
        assert_eq!(
            paths(&corpus.query("beta", Corpus::DEFAULT_LIMIT).unwrap()),
            vec![kept],
        );
    }

    #[test]
    fn a_query_with_no_match_answers_no_hits() {
        let scratch = Scratch::new("no-match");
        scratch.write("a.txt", "alpha beta");
        let corpus = scratch.corpus(Limits::default());
        corpus.refresh().unwrap();

        let response = corpus.query("zebra", Corpus::DEFAULT_LIMIT).unwrap();

        assert_eq!(
            response,
            Response {
                indexed: 1,
                hits: Vec::new(),
            },
        );
        assert_eq!(render(&response), "indexed=1 hits=0\n");
    }

    #[test]
    fn an_empty_query_answers_no_hits_rather_than_every_document() {
        let scratch = Scratch::new("empty");
        scratch.write("a.txt", "alpha");
        scratch.write("b.txt", "beta");
        let corpus = scratch.corpus(Limits::default());
        corpus.refresh().unwrap();

        for query in ["", "   ", "---"] {
            let response = corpus.query(query, Corpus::DEFAULT_LIMIT).unwrap();

            assert_eq!(response.hits, Vec::new(), "{query:?}");
            assert_eq!(response.indexed, 2, "{query:?}");
        }
    }

    #[test]
    fn a_limit_bounds_the_hits_and_the_order_is_by_descending_score() {
        let scratch = Scratch::new("limit");
        scratch.write("a.txt", "alpha");
        scratch.write("b.txt", "alpha beta gamma delta");
        scratch.write("c.txt", "alpha beta");
        let corpus = scratch.corpus(Limits::default());
        corpus.refresh().unwrap();

        let all = corpus.query("alpha", Corpus::DEFAULT_LIMIT).unwrap();
        assert_eq!(all.hits.len(), 3);
        assert!(all.hits[0].score >= all.hits[1].score);
        assert!(all.hits[1].score >= all.hits[2].score);

        let bounded = corpus.query("alpha", 2).unwrap();

        assert_eq!(paths(&bounded), paths(&all)[..2].to_vec());
    }

    #[test]
    fn a_query_for_nothing_answers_nothing_even_at_no_limit() {
        let scratch = Scratch::new("zero-limit");
        scratch.write("a.txt", "alpha");
        let corpus = scratch.corpus(Limits::default());
        corpus.refresh().unwrap();

        assert_eq!(corpus.query("alpha", 0).unwrap().hits, Vec::new());
    }

    #[test]
    fn an_exact_word_outranks_its_one_edit_neighbour() {
        let scratch = Scratch::new("exact");
        let exact = scratch.write("verify.txt", "the verifier verifies a verify");
        let near = scratch.write("typo.txt", "the verity of it");
        let corpus = scratch.corpus(Limits::default());
        corpus.refresh().unwrap();

        let response = corpus.query("verify", Corpus::DEFAULT_LIMIT).unwrap();

        assert_eq!(paths(&response), vec![exact, near]);
        assert!(response.hits[0].score > response.hits[1].score);
    }

    #[test]
    fn a_one_edit_typo_still_finds_the_file() {
        let scratch = Scratch::new("typo");
        let file = scratch.write("note.txt", "a tantivy index over a tree");
        let corpus = scratch.corpus(Limits::default());
        corpus.refresh().unwrap();

        let response = corpus.query("tantviy", Corpus::DEFAULT_LIMIT).unwrap();

        assert_eq!(paths(&response), vec![file]);
    }

    #[test]
    fn a_name_match_outranks_a_body_only_match() {
        let scratch = Scratch::new("name-weight");
        let named = scratch.write("agentRuntime.rs", "the scheduler");
        let mentioned = scratch.write("notes.txt", "the runtime");
        let corpus = scratch.corpus(Limits::default());
        corpus.refresh().unwrap();

        let response = corpus.query("runtime", Corpus::DEFAULT_LIMIT).unwrap();

        assert_eq!(paths(&response), vec![named, mentioned]);
        assert!(response.hits[0].score > response.hits[1].score);
    }

    #[test]
    fn a_two_word_query_ranks_the_file_holding_both_first() {
        let scratch = Scratch::new("two-words");
        let both = scratch.write("both.txt", "alpha beta");
        let one = scratch.write("one.txt", "alpha gamma");
        let corpus = scratch.corpus(Limits::default());
        corpus.refresh().unwrap();

        let response = corpus.query("alpha beta", Corpus::DEFAULT_LIMIT).unwrap();

        assert_eq!(paths(&response), vec![both, one]);
    }

    #[test]
    fn the_three_spellings_of_one_identifier_find_the_same_two_files() {
        let scratch = Scratch::new("spellings");
        let camel = scratch.write("camel.txt", "let verifyToken = false;");
        let underscored = scratch.write("snake.txt", "let verify_token = false;");
        let corpus = scratch.corpus(Limits::default());
        corpus.refresh().unwrap();

        for query in ["verifyToken", "verify_token", "verify-token"] {
            let response = corpus.query(query, Corpus::DEFAULT_LIMIT).unwrap();
            let mut found = paths(&response);
            found.sort();

            assert_eq!(found, vec![camel.clone(), underscored.clone()], "{query}");
        }
    }

    #[test]
    fn a_file_past_the_size_cap_is_skipped_and_counted() {
        let scratch = Scratch::new("oversize");
        scratch.write("small.txt", "alpha");
        scratch.write("large.txt", "alpha beta gamma delta");
        let corpus = scratch.corpus(Limits {
            file_bytes: 8,
            body_bytes: 256,
            files: 50_000,
        });

        let refresh = corpus.refresh().unwrap();

        assert_eq!(
            refresh,
            Refresh {
                indexed: 1,
                updated: 1,
                removed: 0,
                unchanged: 0,
                skipped: 1,
            },
        );
        assert_eq!(
            corpus.query("beta", Corpus::DEFAULT_LIMIT).unwrap().hits,
            Vec::new()
        );
        assert_eq!(
            corpus
                .query("alpha", Corpus::DEFAULT_LIMIT)
                .unwrap()
                .hits
                .len(),
            1
        );
    }

    #[test]
    fn a_file_with_a_nul_byte_is_skipped() {
        let scratch = Scratch::new("nul");
        scratch.write("text.txt", "alpha");
        scratch.write_bytes("binary.dat", b"alpha\0beta");
        let corpus = scratch.corpus(Limits::default());

        let refresh = corpus.refresh().unwrap();

        assert_eq!(
            refresh,
            Refresh {
                indexed: 1,
                updated: 1,
                removed: 0,
                unchanged: 0,
                skipped: 1,
            },
        );
        assert_eq!(
            corpus.query("beta", Corpus::DEFAULT_LIMIT).unwrap().hits,
            Vec::new()
        );
    }

    #[test]
    fn a_dot_directory_and_build_output_are_not_entered() {
        let scratch = Scratch::new("pruned");
        let kept = scratch.write("kept.txt", "alpha");
        scratch.write(".hidden/buried.txt", "alpha");
        scratch.write("target/debug/artifact.txt", "alpha");
        scratch.write("node_modules/package.txt", "alpha");
        let corpus = scratch.corpus(Limits::default());

        let refresh = corpus.refresh().unwrap();

        assert_eq!(refresh.indexed, 1);
        assert_eq!(refresh.skipped, 0);
        assert_eq!(
            paths(&corpus.query("alpha", Corpus::DEFAULT_LIMIT).unwrap()),
            vec![kept]
        );
    }

    #[test]
    fn a_symlink_out_of_the_root_is_not_followed() {
        let outside = Scratch::new("symlink-target");
        let buried = outside.write("inside.txt", "alpha");
        let scratch = Scratch::new("symlink");
        scratch.write("kept.txt", "beta");
        std::os::unix::fs::symlink(outside.root(), scratch.root().join("link")).unwrap();
        let corpus = scratch.corpus(Limits::default());

        let refresh = corpus.refresh().unwrap();

        assert_eq!(refresh.indexed, 1);
        assert_eq!(refresh.skipped, 1);
        assert!(buried.exists());
        assert_eq!(
            corpus.query("alpha", Corpus::DEFAULT_LIMIT).unwrap().hits,
            Vec::new()
        );
    }

    #[test]
    fn a_store_inside_the_root_is_not_indexed() {
        let scratch = Scratch::new("store-inside");
        let kept = scratch.write("note.txt", "alpha");
        let store = scratch.root().join("index");
        let corpus = Corpus::open(&scratch.root(), &store, Limits::default()).unwrap();

        let refresh = corpus.refresh().unwrap();

        assert_eq!(refresh.indexed, 1);
        assert!(std::fs::read_dir(&store).unwrap().count() > 0);
        assert_eq!(
            corpus.query("meta", Corpus::DEFAULT_LIMIT).unwrap().hits,
            Vec::new()
        );
        assert_eq!(
            paths(&corpus.query("alpha", Corpus::DEFAULT_LIMIT).unwrap()),
            vec![kept]
        );
    }

    #[test]
    fn an_unreadable_root_is_refused() {
        let scratch = Scratch::new("root-unreadable");
        let root = scratch.root();
        scratch.write("note.txt", "alpha");
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o000)).unwrap();

        let opened = Corpus::open(&root, &scratch.store(), Limits::default());

        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(opened, Err(Error::Root { path }) if path == root));
    }

    #[test]
    fn an_unreadable_subdirectory_is_skipped_and_counted() {
        let scratch = Scratch::new("subdirectory-unreadable");
        let kept = scratch.write("kept.txt", "alpha");
        let buried = scratch.write("sub/buried.txt", "alpha");
        let directory = buried.parent().unwrap().to_path_buf();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o000)).unwrap();
        let corpus = scratch.corpus(Limits::default());

        let refresh = corpus.refresh().unwrap();

        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            refresh,
            Refresh {
                indexed: 1,
                updated: 1,
                removed: 0,
                unchanged: 0,
                skipped: 1,
            },
        );
        assert_eq!(
            paths(&corpus.query("alpha", Corpus::DEFAULT_LIMIT).unwrap()),
            vec![kept]
        );
    }

    #[test]
    fn an_unreadable_entry_is_skipped_and_counted() {
        let scratch = Scratch::new("entry-unreadable");
        let kept = scratch.write("kept.txt", "alpha");
        let buried = scratch.write("sub/buried.txt", "alpha");
        let directory = buried.parent().unwrap().to_path_buf();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o444)).unwrap();
        let corpus = scratch.corpus(Limits::default());

        let refresh = corpus.refresh().unwrap();

        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            refresh,
            Refresh {
                indexed: 1,
                updated: 1,
                removed: 0,
                unchanged: 0,
                skipped: 1,
            },
        );
        assert_eq!(
            paths(&corpus.query("alpha", Corpus::DEFAULT_LIMIT).unwrap()),
            vec![kept]
        );
    }

    #[test]
    fn a_socket_in_the_tree_is_skipped_and_counted() {
        let scratch = Scratch::new("socket");
        let kept = scratch.write("kept.txt", "alpha");
        let socket = scratch.root().join("agent.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let corpus = scratch.corpus(Limits::default());

        let refresh = corpus.refresh().unwrap();

        assert_eq!(
            refresh,
            Refresh {
                indexed: 1,
                updated: 1,
                removed: 0,
                unchanged: 0,
                skipped: 1,
            },
        );
        assert!(socket.exists());
        assert_eq!(
            paths(&corpus.query("alpha", Corpus::DEFAULT_LIMIT).unwrap()),
            vec![kept]
        );
        assert_eq!(
            corpus.query("agent", Corpus::DEFAULT_LIMIT).unwrap().hits,
            Vec::new()
        );
    }

    #[test]
    fn a_name_that_is_not_utf8_is_skipped_and_counted() {
        let scratch = Scratch::new("not-utf8");
        let kept = scratch.write("kept.txt", "alpha");
        let unnameable = scratch
            .root()
            .join(std::ffi::OsStr::from_bytes(b"bin\xffary.txt"));
        std::fs::write(&unnameable, "bravo").unwrap();
        let corpus = scratch.corpus(Limits::default());

        let refresh = corpus.refresh().unwrap();

        assert_eq!(
            refresh,
            Refresh {
                indexed: 1,
                updated: 1,
                removed: 0,
                unchanged: 0,
                skipped: 1,
            },
        );
        assert!(unnameable.exists());
        assert_eq!(
            paths(&corpus.query("alpha", Corpus::DEFAULT_LIMIT).unwrap()),
            vec![kept]
        );
        assert_eq!(
            corpus.query("bravo", Corpus::DEFAULT_LIMIT).unwrap().hits,
            Vec::new()
        );
    }

    #[test]
    fn a_file_exactly_at_the_size_cap_is_indexed() {
        let scratch = Scratch::new("size-cap-edge");
        let kept = scratch.write("kept.txt", "alpha");
        let corpus = scratch.corpus(Limits {
            file_bytes: 5,
            body_bytes: 256,
            files: 50_000,
        });

        let refresh = corpus.refresh().unwrap();

        assert_eq!(
            refresh,
            Refresh {
                indexed: 1,
                updated: 1,
                removed: 0,
                unchanged: 0,
                skipped: 0,
            },
        );
        assert_eq!(
            paths(&corpus.query("alpha", Corpus::DEFAULT_LIMIT).unwrap()),
            vec![kept]
        );
    }

    #[test]
    fn a_tree_past_the_file_cap_stops_the_walk_and_counts() {
        let scratch = Scratch::new("file-cap");
        scratch.write("a.txt", "alpha");
        scratch.write("b.txt", "alpha");
        scratch.write("c.txt", "alpha");
        let corpus = scratch.corpus(Limits {
            file_bytes: 1024 * 1024,
            body_bytes: 256,
            files: 2,
        });

        let refresh = corpus.refresh().unwrap();

        assert_eq!(refresh.indexed, 2);
        assert_eq!(refresh.updated, 2);
        assert_eq!(refresh.skipped, 1);
        assert_eq!(
            corpus
                .query("alpha", Corpus::DEFAULT_LIMIT)
                .unwrap()
                .hits
                .len(),
            2
        );
    }

    #[test]
    fn a_rewrite_with_a_later_stamp_is_an_update() {
        let scratch = Scratch::new("stamp");
        let file = scratch.write("a.txt", "alpha");
        let corpus = scratch.corpus(Limits::default());
        corpus.refresh().unwrap();

        rewrite_later(&file, "bravo");
        let refresh = corpus.refresh().unwrap();

        assert_eq!(
            refresh,
            Refresh {
                indexed: 1,
                updated: 1,
                removed: 0,
                unchanged: 0,
                skipped: 0,
            },
        );
        assert_eq!(
            paths(&corpus.query("bravo", Corpus::DEFAULT_LIMIT).unwrap()),
            vec![file]
        );
        assert_eq!(
            corpus.query("alpha", Corpus::DEFAULT_LIMIT).unwrap().hits,
            Vec::new()
        );
    }

    #[test]
    fn a_body_is_cut_on_a_character_boundary() {
        let scratch = Scratch::new("body-cut");
        let file = scratch.write("unicode.txt", &"é".repeat(300));
        let corpus = scratch.corpus(Limits {
            file_bytes: 1024 * 1024,
            body_bytes: 5,
            files: 50_000,
        });

        let refresh = corpus.refresh().unwrap();
        let response = corpus.query("é", Corpus::DEFAULT_LIMIT).unwrap();

        assert_eq!(
            refresh,
            Refresh {
                indexed: 1,
                updated: 1,
                removed: 0,
                unchanged: 0,
                skipped: 0,
            },
        );
        assert_eq!(paths(&response), vec![file]);
        assert_eq!(response.hits[0].snippet, "éé");
    }

    #[test]
    fn a_snippet_in_a_long_line_is_cut_on_a_character_boundary() {
        let scratch = Scratch::new("snippet-cut");
        let file = scratch.write("unicode.txt", &format!("needle \t\t{}", "é".repeat(100)));
        let corpus = scratch.corpus(Limits::default());

        corpus.refresh().unwrap();
        let response = corpus.query("needle", Corpus::DEFAULT_LIMIT).unwrap();

        assert_eq!(paths(&response), vec![file]);
        assert_eq!(
            response.hits[0].snippet,
            format!("needle \t\t{}", "é".repeat(75)),
            "the cut lands on the character before the cap rather than inside a two-byte one",
        );
    }

    #[test]
    fn a_nul_byte_inside_the_head_window_keeps_the_file_out_of_the_index() {
        let scratch = Scratch::new("nul-deep");
        let kept = scratch.write("kept.txt", "alpha");
        let mut body = vec![b'a'; 2_000];
        body.push(0);
        body.extend_from_slice(b"bravo");
        scratch.write_bytes("binary.dat", &body);
        let corpus = scratch.corpus(Limits::default());

        let refresh = corpus.refresh().unwrap();

        assert_eq!(
            refresh,
            Refresh {
                indexed: 1,
                updated: 1,
                removed: 0,
                unchanged: 0,
                skipped: 1,
            },
        );
        assert_eq!(
            paths(&corpus.query("alpha", Corpus::DEFAULT_LIMIT).unwrap()),
            vec![kept],
            "the text file is indexed",
        );
        assert!(
            paths(&corpus.query("bravo", Corpus::DEFAULT_LIMIT).unwrap()).is_empty(),
            "the one file carrying that word is the one the NUL byte kept out",
        );
    }

    #[test]
    fn a_hit_renders_its_score_its_path_and_its_snippet() {
        let response = Response {
            indexed: 831,
            hits: vec![
                Hit {
                    path: PathBuf::from("/srv/checkout/docs/spec/index.md"),
                    score: 23.822,
                    snippet: "# The specification".to_owned(),
                },
                Hit {
                    path: PathBuf::from("/srv/checkout/mokmok-search/src/lib.rs"),
                    score: 17.81,
                    snippet: "//! A BM25 index over a directory tree".to_owned(),
                },
            ],
        };

        assert_eq!(
            render(&response),
            "indexed=831 hits=2\n\
             23.822  /srv/checkout/docs/spec/index.md  # The specification\n\
             17.810  /srv/checkout/mokmok-search/src/lib.rs  //! A BM25 index over a directory tree\n",
        );
    }

    #[test]
    fn a_hitless_response_renders_its_header_and_nothing_else() {
        let response = Response {
            indexed: 831,
            hits: Vec::new(),
        };

        assert_eq!(render(&response), "indexed=831 hits=0\n");
    }

    #[test]
    fn a_hit_without_a_snippet_renders_its_score_and_its_path_alone() {
        let response = Response {
            indexed: 1,
            hits: vec![Hit {
                path: PathBuf::from("/srv/checkout/a/b.rs"),
                score: 17.81,
                snippet: String::new(),
            }],
        };

        assert_eq!(
            render(&response),
            "indexed=1 hits=1\n17.810  /srv/checkout/a/b.rs\n"
        );
    }

    fn file_name() -> impl Strategy<Value = String> {
        ("[a-z]{2,6}", 0_usize..4).prop_map(|(stem, index)| format!("{stem}-{index}.txt"))
    }

    fn file_body() -> impl Strategy<Value = String> {
        "\\PC{0,40}"
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(32))]

        #[test]
        fn a_second_pass_over_an_unchanged_tree_rewrites_nothing(
            files in vec((file_name(), file_body()), 1..6),
        ) {
            let scratch = Scratch::new("second-pass");
            for (name, body) in &files {
                scratch.write(name, body);
            }

            let corpus = scratch.corpus(Limits::default());
            let query = files[0].0.clone();
            let first = corpus.refresh().unwrap();
            let before = corpus.query(&query, Corpus::DEFAULT_LIMIT).unwrap();

            let second = corpus.refresh().unwrap();
            let after = corpus.query(&query, Corpus::DEFAULT_LIMIT).unwrap();

            prop_assert_eq!(second.updated, 0);
            prop_assert_eq!(second.removed, 0);
            prop_assert_eq!(second.unchanged, first.indexed);
            prop_assert_eq!(first.updated, first.indexed);
            prop_assert_eq!(before.indexed, after.indexed);
            prop_assert_eq!(ranked(&before), ranked(&after));
        }
    }
}
