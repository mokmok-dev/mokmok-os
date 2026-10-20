mod common;

// The `cfg(test)` module is what tells the `allow-unwrap-in-tests`, `allow-expect-in-tests`, and
// `allow-panic-in-tests` settings in `.clippy.toml` that this harness is test code.
#[cfg(test)]
mod tests {
    use crate::common::tests::{Scratch, config, nothing_left, shell, within};
    use mokmok_sandbox::{
        Access, Backing, Capability, Confined, OpenSpec, Outcome, Refusal, Retired, Sandbox, Scope,
        ScopeError,
    };
    use std::path::{Path, PathBuf};

    fn git(
        checkout: &Path,
        arguments: &[&str],
    ) {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(checkout)
            .args([
                "-c",
                "user.name=mokmok",
                "-c",
                "user.email=mokmok@example.invalid",
            ])
            .args(arguments)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {arguments:?} failed: {}",
            String::from_utf8_lossy(&output.stderr),
        );
    }

    fn repository(
        checkout: &Path,
        name: &str,
        body: &str,
    ) {
        std::fs::create_dir_all(checkout).unwrap();
        std::fs::write(checkout.join(name), body).unwrap();
        git(checkout, &["init"]);
        git(checkout, &["add", "."]);
        git(checkout, &["commit", "-m", "checked in"]);
    }

    fn checkout_of(scratch: &Scratch) -> PathBuf {
        scratch.path().join("checkout")
    }

    fn spec(
        scratch: &Scratch,
        name: &str,
        checkout: &Path,
        backing: Backing,
    ) -> OpenSpec {
        OpenSpec {
            root: scratch.path().join("views"),
            name: name.to_owned(),
            checkout: checkout.to_path_buf(),
            backing,
        }
    }

    /// Every workload needs its interpreter reachable, so the two fixture grants the other
    /// sandbox helpers assume are recorded here before anything else.
    fn opened_scope(
        scratch: &Scratch,
        name: &str,
        checkout: &Path,
        backing: Backing,
    ) -> Scope {
        let mut scope = Scope::open(&spec(scratch, name, checkout, backing)).unwrap();
        scope
            .record(&Capability::issued("/nix/store", Access::READ_EXECUTE))
            .unwrap();
        scope
            .record(&Capability::issued("/dev/null", Access::READ_WRITE))
            .unwrap();

        scope
    }

    fn view_of(
        scratch: &Scratch,
        name: &str,
    ) -> PathBuf {
        std::fs::canonicalize(scratch.path().join("views").join(name)).unwrap()
    }

    #[tokio::test]
    async fn a_worktree_view_is_the_workloads_world() {
        let scratch = Scratch::new("scope-worktree");
        let checkout = checkout_of(&scratch);
        repository(&checkout, "tracked.txt", "tracked\n");
        let sandbox = Confined::new(config(&scratch)).unwrap();
        let mut scope = opened_scope(&scratch, "wt", &checkout, Backing::Worktree);
        scope
            .record(&Capability::issued(&checkout, Access::READ_WRITE_EXECUTE))
            .unwrap();

        let view = view_of(&scratch, "wt");
        let mut request = shell("cat tracked.txt; printf made > made.txt; pwd");
        request.artifacts = vec![PathBuf::from("made.txt")];
        let run = scope.spawn(&sandbox, request).unwrap();
        assert_eq!(run.workspace(), view.as_path(), "the cwd is not the view");
        let completed = within("wait", run.wait()).await.unwrap();

        assert_eq!(completed.workspace(), view.as_path());
        assert_eq!(completed.outcome(), Outcome::Exited(0));
        assert_eq!(
            completed.stdout().text(),
            format!("tracked\n{}\n", view.display()),
        );
        assert!(
            view.join("made.txt").exists(),
            "made.txt is inside the view"
        );
        assert!(
            !checkout.join("made.txt").exists(),
            "the source was written to"
        );
        let made = completed.artifacts();
        assert_eq!(made.len(), 1, "the declared artifact was not registered");
        assert_eq!(made[0].path(), Path::new("made.txt"));

        scope.close().unwrap();
        assert!(!view.exists(), "the worktree view survived close");
        assert!(
            checkout.join("tracked.txt").exists(),
            "the source lost its file"
        );
        let listed = std::process::Command::new("git")
            .arg("-C")
            .arg(&checkout)
            .args(["worktree", "list", "--porcelain"])
            .output()
            .unwrap();
        assert!(
            !String::from_utf8_lossy(&listed.stdout).contains(view.to_str().unwrap()),
            "git still names the view: {}",
            String::from_utf8_lossy(&listed.stdout),
        );
    }

    #[tokio::test]
    async fn a_copy_view_carries_uncommitted_state_and_leaves_the_source_alone() {
        let scratch = Scratch::new("scope-copy");
        let checkout = checkout_of(&scratch);
        std::fs::create_dir_all(&checkout).unwrap();
        std::fs::write(checkout.join("dirty.txt"), "dirty\n").unwrap();
        let sandbox = Confined::new(config(&scratch)).unwrap();
        let mut scope = opened_scope(&scratch, "cp", &checkout, Backing::for_checkout(&checkout));
        scope
            .record(&Capability::issued(&checkout, Access::READ_WRITE_EXECUTE))
            .unwrap();

        let view = view_of(&scratch, "cp");
        let request = shell("cat dirty.txt; printf fresh > fresh.txt");
        let run = scope.spawn(&sandbox, request).unwrap();
        let completed = within("wait", run.wait()).await.unwrap();

        assert_eq!(completed.outcome(), Outcome::Exited(0));
        assert_eq!(completed.stdout().text(), "dirty\n");
        assert!(
            view.join("fresh.txt").exists(),
            "the view lost the written file"
        );
        assert!(
            !checkout.join("fresh.txt").exists(),
            "the source took the written file",
        );

        scope.close().unwrap();
        assert!(!view.exists(), "close left the copy behind");
    }

    #[tokio::test]
    async fn two_grants_at_one_path_union_and_revoking_one_leaves_the_other() {
        let scratch = Scratch::new("scope-union");
        let checkout = checkout_of(&scratch);
        std::fs::create_dir_all(&checkout).unwrap();
        let sandbox = Confined::new(config(&scratch)).unwrap();
        let mut scope = opened_scope(&scratch, "union", &checkout, Backing::Copy);
        let read = scope
            .record(&Capability::issued(&checkout, Access::READ))
            .unwrap();
        let write = scope
            .record(&Capability::issued(&checkout, Access::READ_WRITE))
            .unwrap();
        let view = view_of(&scratch, "union");

        assert_eq!(
            scope.effective(&checkout),
            Some(Access::READ.union(Access::READ_WRITE)),
        );
        assert_eq!(scope.revoke(write), Some(Retired::Gone));
        assert_eq!(scope.effective(&checkout), Some(Access::READ));
        let snapshot = scope.snapshot();
        assert!(
            snapshot
                .rows()
                .iter()
                .any(|row| row.id() == read && row.access() == Access::READ),
            "the read grant did not survive the revocation",
        );
        let view_grant = snapshot
            .issued()
            .into_iter()
            .find(|capability| capability.as_path() == view);
        assert_eq!(
            view_grant.map(|capability| capability.access()),
            Some(Access::READ),
            "the view root still carries write: {:?}",
            snapshot.issued(),
        );

        // The view root is read-only now: a write there must not land.
        let request = shell("printf blocked > blocked.txt");
        let run = scope.spawn(&sandbox, request).unwrap();
        let completed = within("wait", run.wait()).await.unwrap();

        assert_ne!(completed.outcome(), Outcome::Exited(0));
        assert!(!view.join("blocked.txt").exists());
        completed.cleanup().unwrap();
    }

    #[tokio::test]
    async fn revoking_a_child_still_covered_by_an_ancestor_reports_it() {
        let scratch = Scratch::new("scope-covered");
        let checkout = checkout_of(&scratch);
        std::fs::create_dir_all(checkout.join("sub")).unwrap();
        std::fs::write(checkout.join("sub").join("nested.txt"), "nested\n").unwrap();
        let resolved = std::fs::canonicalize(&checkout).unwrap();
        let mut scope = opened_scope(&scratch, "covered", &checkout, Backing::Copy);
        scope
            .record(&Capability::issued(&checkout, Access::READ_WRITE_EXECUTE))
            .unwrap();
        let child = scope
            .record(&Capability::issued(checkout.join("sub"), Access::READ))
            .unwrap();

        let retired = scope.revoke(child);

        assert_eq!(
            retired,
            Some(Retired::Covered {
                by: resolved.clone(),
                access: Access::READ_WRITE_EXECUTE,
            }),
        );
        let snapshot = scope.snapshot();
        assert!(
            snapshot
                .rows()
                .iter()
                .any(|row| row.host() == resolved && row.access() == Access::READ_WRITE_EXECUTE),
            "the checkout grant did not survive the revocation: {:?}",
            snapshot.rows(),
        );
        scope.close().unwrap();
    }

    #[tokio::test]
    async fn a_grant_that_cannot_be_presented_is_refused() {
        let scratch = Scratch::new("scope-present");
        let checkout = checkout_of(&scratch);
        repository(&checkout, "tracked.txt", "tracked\n");
        std::fs::write(checkout.join("uncommitted.txt"), "now\n").unwrap();
        let mut scope = opened_scope(&scratch, "present", &checkout, Backing::Worktree);

        let hidden = scope
            .record(&Capability::issued(scratch.path(), Access::READ))
            .unwrap_err();
        assert!(
            matches!(hidden, ScopeError::Hidden { .. }),
            "got {hidden:?}"
        );

        // The host has the file, but a worktree view holds only the committed tree.
        let absent = scope
            .record(&Capability::issued(
                checkout.join("uncommitted.txt"),
                Access::READ,
            ))
            .unwrap_err();
        assert!(
            matches!(absent, ScopeError::Absent { .. }),
            "got {absent:?}"
        );

        // The two fixture grants the helper recorded, and nothing else.
        assert_eq!(scope.snapshot().rows().len(), 2);
        scope.close().unwrap();
    }

    #[tokio::test]
    async fn a_workdir_outside_the_grants_is_refused_and_creates_nothing() {
        let scratch = Scratch::new("scope-workdir");
        let checkout = checkout_of(&scratch);
        std::fs::create_dir_all(&checkout).unwrap();
        let sandbox = Confined::new(config(&scratch)).unwrap();
        let mut scope = opened_scope(&scratch, "wd", &checkout, Backing::Copy);
        scope
            .record(&Capability::issued(&checkout, Access::READ_WRITE_EXECUTE))
            .unwrap();

        let outside = scratch.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        // Scope::spawn pins the workdir to the view root, so the request goes to the sandbox
        // with the scope's own snapshot to reach the workdir gate at all.
        let mut request = shell("pwd");
        request.workdir = Some(outside);
        let snapshot = scope.snapshot();
        request.exposures = snapshot.exposures();
        let started = sandbox.spawn(&request, &snapshot.issued());

        assert!(
            matches!(started, Err(Refusal::Workdir { .. })),
            "a workdir outside every exposure was accepted",
        );
        assert!(
            nothing_left(&scratch.root()),
            "the sandbox root was touched by a refusal",
        );
        scope.close().unwrap();
    }

    #[tokio::test]
    async fn an_existing_view_is_neither_created_nor_removed() {
        let scratch = Scratch::new("scope-existing");
        let view = scratch.path().join("views").join("keep");
        std::fs::create_dir_all(&view).unwrap();
        std::fs::write(view.join("sentinel.txt"), "kept\n").unwrap();
        let mut scope = opened_scope(&scratch, "keep", &view, Backing::Existing);
        scope
            .record(&Capability::issued(&view, Access::READ_WRITE_EXECUTE))
            .unwrap();

        scope.close().unwrap();

        assert_eq!(std::fs::read(view.join("sentinel.txt")).unwrap(), b"kept\n");
    }

    #[tokio::test]
    async fn an_unregistered_worktree_directory_is_removed_and_remade() {
        let scratch = Scratch::new("scope-remade");
        let checkout = checkout_of(&scratch);
        repository(&checkout, "tracked.txt", "tracked\n");
        let stray = scratch.path().join("views").join("remade");
        std::fs::create_dir_all(&stray).unwrap();
        std::fs::write(stray.join("junk.txt"), "junk\n").unwrap();
        let mut scope = opened_scope(&scratch, "remade", &checkout, Backing::Worktree);
        scope
            .record(&Capability::issued(&checkout, Access::READ_WRITE_EXECUTE))
            .unwrap();

        let view = view_of(&scratch, "remade");
        assert_eq!(
            std::fs::read(view.join("tracked.txt")).unwrap(),
            b"tracked\n",
            "the remake does not hold HEAD's tree",
        );
        assert!(
            std::fs::symlink_metadata(view.join("junk.txt")).is_err(),
            "the stray file survived the remake",
        );
        scope.close().unwrap();
        assert!(!view.exists(), "the refilled worktree survived close");
    }

    #[tokio::test]
    async fn a_view_does_not_expose_the_checkout_by_its_host_path() {
        let scratch = Scratch::new("scope-host-path");
        let checkout = checkout_of(&scratch);
        repository(&checkout, "tracked.txt", "tracked\n");
        let sandbox = Confined::new(config(&scratch)).unwrap();
        let mut scope = opened_scope(&scratch, "host", &checkout, Backing::Worktree);
        scope
            .record(&Capability::issued(&checkout, Access::READ_WRITE_EXECUTE))
            .unwrap();

        let host_file = checkout.join("tracked.txt");
        let leak = checkout.join("leak.txt");
        let request = shell(&format!(
            "cat tracked.txt; cat {}; printf leak > {}",
            host_file.display(),
            leak.display(),
        ));
        let completed = within("wait", scope.spawn(&sandbox, request).unwrap().wait())
            .await
            .unwrap();

        assert_eq!(
            completed.stdout().text(),
            "tracked\n",
            "the view's own file did not read",
        );
        assert!(
            !leak.exists(),
            "the workload wrote the host checkout through its host path",
        );
        assert_ne!(
            completed.outcome(),
            Outcome::Exited(0),
            "the host path was reachable from the view",
        );
        scope.close().unwrap();
    }

    #[tokio::test]
    async fn a_registered_worktree_whose_directory_vanished_is_remade() {
        let scratch = Scratch::new("scope-revive");
        let checkout = checkout_of(&scratch);
        repository(&checkout, "tracked.txt", "tracked\n");
        let scope = opened_scope(&scratch, "revive", &checkout, Backing::Worktree);
        let view = view_of(&scratch, "revive");
        std::fs::remove_dir_all(&view).unwrap();
        // Dropped without `close`, leaving the registration behind: the state a crash leaves.
        drop(scope);

        let scope = Scope::open(&spec(&scratch, "revive", &checkout, Backing::Worktree)).unwrap();

        assert_eq!(
            std::fs::read(view.join("tracked.txt")).unwrap(),
            b"tracked\n",
            "the stale registration was not pruned and remade",
        );
        scope.close().unwrap();
    }

    #[tokio::test]
    async fn a_read_only_source_directory_copies_and_closes() {
        use std::os::unix::fs::PermissionsExt as _;

        let scratch = Scratch::new("scope-read-only");
        let checkout = checkout_of(&scratch);
        let read_only = checkout.join("ro");
        std::fs::create_dir_all(&read_only).unwrap();
        std::fs::write(read_only.join("kept.txt"), "kept\n").unwrap();
        std::fs::set_permissions(&read_only, std::fs::Permissions::from_mode(0o500)).unwrap();

        let scope = opened_scope(&scratch, "ro", &checkout, Backing::Copy);
        let view = view_of(&scratch, "ro");

        assert_eq!(
            std::fs::read(view.join("ro/kept.txt")).unwrap(),
            b"kept\n",
            "the read-only directory was not copied",
        );
        scope.close().unwrap();
        assert!(!view.exists(), "the read-only view survived close");
        std::fs::set_permissions(&read_only, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[tokio::test]
    async fn closing_succeeds_after_the_checkout_vanished() {
        let scratch = Scratch::new("scope-checkout-gone");
        let checkout = checkout_of(&scratch);
        repository(&checkout, "tracked.txt", "tracked\n");
        let scope = opened_scope(&scratch, "gone", &checkout, Backing::Worktree);
        let view = view_of(&scratch, "gone");
        std::fs::remove_dir_all(&checkout).unwrap();

        scope.close().unwrap();

        assert!(!view.exists(), "the view survived a close with no checkout");
    }

    #[test]
    fn generations_advance_on_record_and_revoke_and_a_snapshot_holds_one() {
        let scratch = Scratch::new("scope-generation");
        let checkout = checkout_of(&scratch);
        std::fs::create_dir_all(&checkout).unwrap();
        let mut scope = Scope::open(&spec(&scratch, "gen", &checkout, Backing::Copy)).unwrap();

        assert_eq!(scope.generation(), 0);
        assert_eq!(scope.snapshot().generation(), 0);

        let first = scope
            .record(&Capability::issued("/nix/store", Access::READ))
            .unwrap();
        assert_eq!(scope.generation(), 1);
        assert_eq!(scope.snapshot().generation(), 1);

        let second = scope
            .record(&Capability::issued("/dev/null", Access::READ))
            .unwrap();
        assert_eq!(scope.generation(), 2);

        assert!(scope.revoke(second).is_some());
        assert_eq!(scope.generation(), 3);
        assert_eq!(scope.snapshot().generation(), 3);
        assert!(
            scope.snapshot().rows().iter().any(|row| row.id() == first),
            "the first grant did not survive the second's revocation",
        );

        scope.close().unwrap();
    }

    #[test]
    fn a_name_that_is_not_one_component_is_refused() {
        for name in ["", "a/b", ".", ".."] {
            let scratch = Scratch::new("scope-name");
            let checkout = checkout_of(&scratch);
            std::fs::create_dir_all(&checkout).unwrap();
            let Err(error) = Scope::open(&spec(&scratch, name, &checkout, Backing::Copy)) else {
                panic!("the name {name:?} was accepted");
            };

            match error {
                ScopeError::InvalidName { name: refused } => {
                    assert_eq!(refused.as_str(), name);
                },
                other => panic!("the name {name:?} was refused as another kind: {other:?}"),
            }
        }
    }

    #[test]
    fn an_existing_view_inside_the_checkout_but_not_equal_is_refused() {
        let scratch = Scratch::new("scope-alias");
        let checkout = checkout_of(&scratch);
        let views = checkout.join("views");
        std::fs::create_dir_all(views.join("nested")).unwrap();
        let spec = OpenSpec {
            root: views,
            name: String::from("nested"),
            checkout,
            backing: Backing::Existing,
        };
        let Err(error) = Scope::open(&spec) else {
            panic!("a view inside its own source was accepted");
        };

        assert!(
            matches!(error, ScopeError::Setup { .. }),
            "the aliasing view was refused as another kind: {error:?}",
        );
    }

    #[test]
    fn a_file_at_the_view_path_is_refused() {
        let scratch = Scratch::new("scope-file-path");
        let checkout = checkout_of(&scratch);
        std::fs::create_dir_all(&checkout).unwrap();
        let views = scratch.path().join("views");
        std::fs::create_dir_all(&views).unwrap();
        std::fs::write(views.join("taken"), "not a directory\n").unwrap();
        let Err(error) = Scope::open(&spec(&scratch, "taken", &checkout, Backing::Copy)) else {
            panic!("a file at the view path was accepted");
        };

        match error {
            ScopeError::Materialize { source, .. } => assert!(
                source.to_string().contains("is not a directory"),
                "the refusal does not name the file: {source}",
            ),
            other => panic!("the file was refused as another kind: {other:?}"),
        }
    }

    #[test]
    fn a_nonempty_copy_view_is_reused() {
        let scratch = Scratch::new("scope-reuse");
        let checkout = checkout_of(&scratch);
        std::fs::create_dir_all(&checkout).unwrap();
        std::fs::write(checkout.join("tracked.txt"), "tracked\n").unwrap();
        let view = scratch.path().join("views").join("reuse");
        std::fs::create_dir_all(&view).unwrap();
        std::fs::write(view.join("sentinel.txt"), "kept\n").unwrap();

        let scope = Scope::open(&spec(&scratch, "reuse", &checkout, Backing::Copy)).unwrap();

        assert!(
            view.join("sentinel.txt").exists(),
            "the open recopied over the view's own state",
        );
        assert!(
            !view.join("tracked.txt").exists(),
            "the open copied into a view it should have reused",
        );
        scope.close().unwrap();
    }

    #[test]
    fn an_empty_copy_view_is_filled() {
        let scratch = Scratch::new("scope-empty");
        let checkout = checkout_of(&scratch);
        std::fs::create_dir_all(&checkout).unwrap();
        std::fs::write(checkout.join("tracked.txt"), "tracked\n").unwrap();
        let view = scratch.path().join("views").join("empty");
        std::fs::create_dir_all(&view).unwrap();

        let scope = Scope::open(&spec(&scratch, "empty", &checkout, Backing::Copy)).unwrap();

        assert_eq!(
            std::fs::read(view.join("tracked.txt")).unwrap(),
            b"tracked\n",
            "an empty view was reused instead of filled",
        );
        scope.close().unwrap();
    }

    #[test]
    fn opening_a_worktree_on_a_non_repository_reports_gits_message() {
        let scratch = Scratch::new("scope-not-a-repo");
        let checkout = checkout_of(&scratch);
        std::fs::create_dir_all(&checkout).unwrap();
        let Err(error) = Scope::open(&spec(&scratch, "git", &checkout, Backing::Worktree)) else {
            panic!("a worktree was made from a non-repository");
        };

        match error {
            ScopeError::Materialize { source, .. } => assert!(
                source.to_string().contains("not a git repository"),
                "the refusal does not carry git's message: {source}",
            ),
            other => panic!("the non-repository was refused as another kind: {other:?}"),
        }
    }

    #[test]
    fn opening_a_worktree_on_a_repository_without_commits_reports_gits_message() {
        let scratch = Scratch::new("scope-unborn");
        let checkout = checkout_of(&scratch);
        std::fs::create_dir_all(&checkout).unwrap();
        git(&checkout, &["init"]);

        let Err(error) = Scope::open(&spec(&scratch, "unborn", &checkout, Backing::Worktree))
        else {
            panic!("a worktree was made from an unborn HEAD");
        };

        match error {
            ScopeError::Materialize { source, .. } => assert!(
                source.to_string().contains("invalid reference"),
                "the refusal does not carry git's message: {source}",
            ),
            other => panic!("the unborn repository was refused as another kind: {other:?}"),
        }
    }
}
