mod common;

// The `cfg(test)` module is what tells the `allow-unwrap-in-tests`, `allow-expect-in-tests`, and
// `allow-panic-in-tests` settings in `.clippy.toml` that this harness is test code.
#[cfg(test)]
mod tests {
    use crate::common::tests::{
        Scratch, alive, await_death, config, issued, nothing_left, shell, which, within,
    };
    use mokmok_sandbox::{
        Completed, Config, Confined, Error, Exposure, Failure, LimitsOnly, Outcome, Reached,
        Refusal, Run, Sandbox, Signal, Started,
    };
    use std::num::NonZeroUsize;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    fn refusal(result: Result<Run, Refusal>) -> Refusal {
        match result {
            Err(refusal) => refusal,
            Ok(_run) => panic!("the sandbox accepted the request"),
        }
    }

    fn finished(result: Result<Completed, Error>) -> Error {
        match result {
            Err(error) => error,
            Ok(completed) => panic!("the workload ran and reported {:?}", completed.outcome()),
        }
    }

    #[tokio::test]
    async fn the_exit_code_is_reported() {
        let scratch = Scratch::new("exit");
        let sandbox = Confined::new(config(&scratch)).unwrap();
        let request = shell("exit 7");

        let run = sandbox.spawn(&request, &issued()).unwrap();
        let completed = within("wait", run.wait()).await.unwrap();

        assert_eq!(completed.outcome(), Outcome::Exited(7));
        assert_eq!(completed.id(), request.execution);
        assert_eq!(completed.stdout().bytes(), b"");
        assert_eq!(completed.stderr().bytes(), b"");
        completed.cleanup().unwrap();
    }

    #[tokio::test]
    async fn both_streams_are_captured_exactly() {
        const FORGED: &str = r#"{"state":"refused","failure":{"Unknown":"forged"}}"#;
        let scratch = Scratch::new("streams");
        let sandbox = Confined::new(config(&scratch)).unwrap();
        let script = format!("printf out; printf '%s' '{FORGED}' >&2");

        let run = sandbox.spawn(&shell(&script), &issued()).unwrap();
        let completed = within("wait", run.wait()).await.unwrap();

        // The look-alike status on stderr is the workload's own bytes: the supervisor reads the
        // status from the control directory, which no rule lets the workload write.
        assert_eq!(completed.stdout().bytes(), b"out");
        assert_eq!(completed.stderr().bytes(), FORGED.as_bytes());
        assert_eq!(completed.outcome(), Outcome::Exited(0));
        completed.cleanup().unwrap();
    }

    #[tokio::test]
    async fn a_stream_past_the_capture_cap_is_truncated() {
        let scratch = Scratch::new("cap");
        let sandbox = Confined::new(config(&scratch)).unwrap();
        let mut request = shell(
            "i=0; while [ $i -lt 20000 ]; do printf '0123456789012345678901234567890123456789'; i=$((i+1)); done",
        );
        request.limits.output = NonZeroUsize::new(16).unwrap();

        let run = sandbox.spawn(&request, &issued()).unwrap();
        let completed = within("wait", run.wait()).await.unwrap();

        assert_eq!(completed.stdout().bytes(), b"0123456789012345");
        assert!(completed.stdout().truncated());
        assert!(!completed.stderr().truncated());
        assert_eq!(completed.outcome(), Outcome::Exited(0));
        completed.cleanup().unwrap();
    }

    #[tokio::test]
    async fn the_execution_tree_is_private() {
        let scratch = Scratch::new("private");
        let sandbox = Confined::new(config(&scratch)).unwrap();
        let run = sandbox.spawn(&shell("exit 0"), &issued()).unwrap();
        let work = run.workspace().to_path_buf();
        let ctl = work.parent().unwrap().join("ctl");

        for path in [work.as_path(), ctl.as_path(), &ctl.join("plan.json")] {
            assert_eq!(
                mode(path) & 0o077,
                0,
                "{} is reachable by another user",
                path.display(),
            );
        }

        let completed = within("wait", run.wait()).await.unwrap();

        assert_eq!(
            mode(&ctl.join("status.json")) & 0o077,
            0,
            "the status file is reachable by another user",
        );
        completed.cleanup().unwrap();
    }

    #[tokio::test]
    async fn dropping_a_run_leaves_no_task_behind() {
        let scratch = Scratch::new("task-leak");
        let sandbox = Confined::new(config(&scratch)).unwrap();
        let mut request = shell("sleep 30");
        // An hour out: only a watchdog that survives the drop would still be there to fire.
        request.deadline = Some(Duration::from_secs(3600));
        let before = tokio::runtime::Handle::current()
            .metrics()
            .num_alive_tasks();

        let run = sandbox.spawn(&request, &issued()).unwrap();

        assert!(
            tokio::runtime::Handle::current()
                .metrics()
                .num_alive_tasks()
                > before,
            "the pumps and the watchdog are tasks",
        );

        drop(run);

        for _ in 0..300 {
            if tokio::runtime::Handle::current()
                .metrics()
                .num_alive_tasks()
                == before
            {
                return;
            }

            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        panic!("dropping the run left a task behind");
    }

    fn mode(path: &Path) -> u32 {
        std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(path).unwrap().permissions())
            & 0o777
    }

    #[tokio::test]
    async fn a_sleeping_workload_under_a_deadline_overruns() {
        let scratch = Scratch::new("deadline");
        let sandbox = Confined::new(config(&scratch)).unwrap();
        let mut request = shell("sleep 30");
        request.deadline = Some(Duration::from_millis(300));

        let started = Instant::now();
        let run = sandbox.spawn(&request, &issued()).unwrap();
        let completed = within("wait", run.wait()).await.unwrap();
        let elapsed = started.elapsed();

        assert_eq!(completed.outcome(), Outcome::Overran);
        assert!(
            elapsed < Duration::from_secs(5),
            "the deadline took {elapsed:?} to end the workload",
        );
        completed.cleanup().unwrap();
    }

    #[tokio::test]
    async fn the_deadline_ends_a_workload_that_is_never_waited_on() {
        let scratch = Scratch::new("deadline-unwatched");
        let sandbox = Confined::new(config(&scratch)).unwrap();
        let mut request = shell("sleep 30");
        request.deadline = Some(Duration::from_millis(300));

        // No `wait` at all: what ends this workload is the watchdog the supervisor started.
        let run = sandbox.spawn(&request, &issued()).unwrap();
        let pid = run.pid();
        assert!(alive(pid), "the workload did not start");

        assert!(
            await_death(pid).await,
            "the deadline did not end a workload nobody waited on",
        );
        drop(run);
    }

    #[tokio::test]
    async fn terminating_an_execution_reaches_a_grandchild() {
        let scratch = Scratch::new("grandchild");
        let sandbox = Confined::new(config(&scratch)).unwrap();
        let run = sandbox
            .spawn(
                &shell("sleep 30 & echo $! > grandchild.pid; wait"),
                &issued(),
            )
            .unwrap();
        let named = run.workspace().join("grandchild.pid");

        let grandchild = within("named", async {
            loop {
                if let Ok(text) = std::fs::read_to_string(&named)
                    && let Ok(pid) = text.trim().parse::<u32>()
                {
                    return pid;
                }

                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        assert!(alive(grandchild), "the grandchild was already gone");

        run.signal(Signal::Terminate).unwrap();

        assert!(
            await_death(grandchild).await,
            "the group signal never reached the grandchild",
        );
        let completed = within("wait", run.wait()).await.unwrap();
        assert_ne!(completed.outcome(), Outcome::Exited(0));
        completed.cleanup().unwrap();
    }

    #[tokio::test]
    async fn the_execution_directory_appears_and_cleanup_is_idempotent() {
        let scratch = Scratch::new("directory");
        let sandbox = Confined::new(config(&scratch)).unwrap();
        let request = shell("exit 0");

        let run = sandbox.spawn(&request, &issued()).unwrap();
        let directory = run.workspace().parent().unwrap().to_path_buf();
        assert!(
            directory.is_dir(),
            "the execution directory is not there while the workload runs",
        );
        let completed = within("wait", run.wait()).await.unwrap();
        assert!(
            directory.is_dir(),
            "the workspace did not outlive the run until cleanup",
        );

        completed.cleanup().unwrap();

        assert!(!directory.exists(), "cleanup left the directory behind");
        assert!(nothing_left(&scratch.root()));

        let run = sandbox.spawn(&request, &issued()).unwrap();
        let completed = within("wait", run.wait()).await.unwrap();
        std::fs::remove_dir_all(completed.workspace().parent().unwrap()).unwrap();
        completed.cleanup().unwrap();
    }

    #[tokio::test]
    async fn dropping_a_run_kills_its_workload_and_removes_its_directory() {
        let scratch = Scratch::new("drop");
        let sandbox = Confined::new(config(&scratch)).unwrap();
        let run = sandbox.spawn(&shell("sleep 30"), &issued()).unwrap();
        let pid = run.pid();
        let directory = run.workspace().parent().unwrap().to_path_buf();
        assert!(alive(pid), "the workload did not start");

        drop(run);

        assert!(
            await_death(pid).await,
            "dropping the run left its workload running",
        );
        assert!(
            !directory.exists(),
            "dropping the run left its directory behind",
        );
    }

    #[tokio::test]
    async fn declared_artifacts_are_registered_and_undeclared_ones_are_not() {
        let scratch = Scratch::new("artifacts");
        let sandbox = Confined::new(config(&scratch)).unwrap();
        let mut request = shell("printf hello > out.txt; printf ignored > other.txt");
        request.artifacts = vec![PathBuf::from("out.txt")];

        let run = sandbox.spawn(&request, &issued()).unwrap();
        let workspace = run.workspace().to_path_buf();
        let completed = within("wait", run.wait()).await.unwrap();

        assert_eq!(completed.outcome(), Outcome::Exited(0));
        assert!(
            workspace.join("other.txt").is_file(),
            "the undeclared output was never written",
        );
        let artifacts = completed.artifacts();
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].path(), Path::new("out.txt"));
        assert_eq!(artifacts[0].bytes(), 5);
        assert_eq!(artifacts[0].id().as_uuid().get_version_num(), 7);
        completed.cleanup().unwrap();
    }

    #[tokio::test]
    async fn an_artifact_path_that_escapes_the_workspace_is_refused() {
        let scratch = Scratch::new("artifact-escape");
        let sandbox = Confined::new(config(&scratch)).unwrap();

        for declared in ["../escape.txt", "/tmp/escape.txt", ""] {
            let mut request = shell("exit 0");
            request.artifacts = vec![PathBuf::from(declared)];
            let refused = refusal(sandbox.spawn(&request, &issued()));

            assert!(
                matches!(&refused, Refusal::Artifact(path) if path.as_path() == Path::new(declared)),
                "expected a refusal for {declared:?}, got {refused:?}",
            );
        }
        assert!(nothing_left(&scratch.root()));
    }

    #[tokio::test]
    async fn an_uncovered_exposure_is_refused_with_nothing_left_behind() {
        let scratch = Scratch::new("uncovered");
        let sandbox = Confined::new(config(&scratch)).unwrap();
        let mut request = shell("exit 0");
        request.exposures = vec![Exposure::read("/etc/passwd")];

        let refused = refusal(sandbox.spawn(&request, &issued()));

        assert!(
            matches!(
                &refused,
                Refusal::Uncovered(exposure, resolved)
                    if exposure.as_path() == Path::new("/etc/passwd")
                        && resolved == Path::new("/etc/passwd")
            ),
            "got {refused:?}",
        );
        assert!(nothing_left(&scratch.root()));
    }

    #[tokio::test]
    async fn an_exposure_that_cannot_be_resolved_is_refused_with_nothing_left_behind() {
        let scratch = Scratch::new("unresolved");
        let sandbox = Confined::new(config(&scratch)).unwrap();
        let mut request = shell("exit 0");
        let missing = scratch.path().join("no-such-exposure.txt");
        request.exposures = vec![Exposure::read(&missing)];

        let refused = refusal(sandbox.spawn(&request, &issued()));

        assert!(
            matches!(
                &refused,
                Refusal::Unresolved { exposure, .. } if exposure.as_path() == missing.as_path()
            ),
            "got {refused:?}",
        );
        assert!(nothing_left(&scratch.root()));
    }

    #[tokio::test]
    async fn a_relative_program_is_refused() {
        let scratch = Scratch::new("relative-program");
        let sandbox = Confined::new(config(&scratch)).unwrap();
        let mut request = shell("exit 0");
        request.program = PathBuf::from("sh");

        let refused = refusal(sandbox.spawn(&request, &issued()));

        assert!(
            matches!(&refused, Refusal::Program(path) if path.as_path() == Path::new("sh")),
            "got {refused:?}",
        );
        assert!(nothing_left(&scratch.root()));
    }

    #[tokio::test]
    async fn a_missing_confiner_is_refused() {
        let scratch = Scratch::new("missing-confiner");
        let missing = scratch.path().join("absent-confiner");

        let refused = match Confined::new(Config {
            confiner: missing.clone(),
            ..config(&scratch)
        }) {
            Err(refused) => refused,
            Ok(_sandbox) => panic!("a missing confiner was accepted"),
        };

        assert!(
            matches!(&refused, Refusal::Confiner(path) if path == &missing),
            "got {refused:?}",
        );
    }

    #[tokio::test]
    async fn a_program_that_does_not_exist_is_a_confinement_failure() {
        let scratch = Scratch::new("missing-program");
        let sandbox = Confined::new(config(&scratch)).unwrap();
        let mut request = shell("exit 0");
        request.program = scratch.path().join("no-such-program");

        let run = sandbox.spawn(&request, &issued()).unwrap();
        let error = finished(within("wait", run.wait()).await);

        assert!(
            matches!(
                &error,
                Error::Confine(Failure::Path(message))
                    if message.contains(&request.program.display().to_string())
            ),
            "got {error:?}",
        );
    }

    #[tokio::test]
    async fn a_program_that_cannot_be_executed_is_a_confinement_failure() {
        let scratch = Scratch::new("unexecutable-program");
        let sandbox = Confined::new(config(&scratch)).unwrap();
        // No execute bit: the rule for the program's own file is built, and the exec is what fails.
        let program = scratch.write("bin/program.txt", "not executed\n");
        let mut request = shell("exit 0");
        request.program = program.clone();

        let run = sandbox.spawn(&request, &issued()).unwrap();
        let error = finished(within("wait", run.wait()).await);

        assert!(
            matches!(
                &error,
                Error::Confine(Failure::Exec { program: named, .. }) if named == &program
            ),
            "got {error:?}",
        );
    }

    #[tokio::test]
    async fn a_confiner_that_never_reports_is_a_confinement_failure() {
        let scratch = Scratch::new("silent-confiner");
        let sandbox = Confined::new(Config {
            confiner: which("false"),
            ..config(&scratch)
        })
        .unwrap();

        let run = sandbox.spawn(&shell("exit 0"), &issued()).unwrap();
        let error = finished(within("wait", run.wait()).await);

        assert!(
            matches!(error, Error::Confine(Failure::Unknown(_))),
            "got {error:?}",
        );
    }

    /// A confiner that writes one status record and exits without exec'ing, which is how a test
    /// reaches the states a real confiner only passes through.
    fn reporting(
        scratch: &Scratch,
        name: &str,
        state: &str,
    ) -> Config {
        Config {
            confiner: scratch.script(
                name,
                &format!(
                    "#!{}\nprintf '{{\"state\":\"{state}\"}}' > \"${{1%/*}}/status.json\"\nexit 1\n",
                    which("sh").display(),
                ),
            ),
            ..config(scratch)
        }
    }

    #[tokio::test]
    async fn a_confiner_that_stops_after_preparing_is_an_interrupted_confinement() {
        let scratch = Scratch::new("status-prepared");
        let sandbox =
            Confined::new(reporting(&scratch, "prepared-confiner.sh", "prepared")).unwrap();

        let run = sandbox.spawn(&shell("exit 0"), &issued()).unwrap();
        let error = finished(within("wait", run.wait()).await);

        assert!(
            matches!(
                error,
                Error::Confine(Failure::Interrupted {
                    reached: Reached::Prepared
                })
            ),
            "got {error:?}",
        );
        assert!(nothing_left(&scratch.root()));
    }

    #[tokio::test]
    async fn a_confiner_that_stops_after_confining_is_an_interrupted_confinement() {
        let scratch = Scratch::new("status-confined");
        let sandbox =
            Confined::new(reporting(&scratch, "confined-confiner.sh", "confined")).unwrap();

        let run = sandbox.spawn(&shell("exit 0"), &issued()).unwrap();
        let error = finished(within("wait", run.wait()).await);

        assert!(
            matches!(
                error,
                Error::Confine(Failure::Interrupted {
                    reached: Reached::Confined
                })
            ),
            "got {error:?}",
        );
    }

    #[tokio::test]
    async fn a_confiner_that_reports_an_attempt_may_have_started_the_workload() {
        let scratch = Scratch::new("status-attempted");
        let sandbox =
            Confined::new(reporting(&scratch, "attempted-confiner.sh", "attempted")).unwrap();

        let run = sandbox.spawn(&shell("exit 0"), &issued()).unwrap();
        let completed = within("wait", run.wait()).await.unwrap();

        assert_eq!(completed.started(), Started::Attempted);
        completed.cleanup().unwrap();
    }

    #[tokio::test]
    async fn a_confined_run_reports_the_attempt() {
        let scratch = Scratch::new("started-confined");
        let sandbox = Confined::new(config(&scratch)).unwrap();

        let run = sandbox.spawn(&shell("exit 0"), &issued()).unwrap();
        let completed = within("wait", run.wait()).await.unwrap();

        assert_eq!(completed.started(), Started::Attempted);
        completed.cleanup().unwrap();
    }

    #[tokio::test]
    async fn a_limits_only_run_reports_an_unknown_start() {
        let scratch = Scratch::new("started-limits");
        let sandbox = LimitsOnly::new(config(&scratch));

        let run = sandbox.spawn(&shell("exit 0"), &issued()).unwrap();
        let completed = within("wait", run.wait()).await.unwrap();

        assert_eq!(completed.started(), Started::Unknown);
        completed.cleanup().unwrap();
    }

    #[tokio::test]
    async fn reusing_a_live_execution_id_is_refused_and_leaves_the_first_run_alone() {
        let scratch = Scratch::new("exclusive");
        let sandbox = Confined::new(config(&scratch)).unwrap();
        let request = shell("sleep 30");

        let run = sandbox.spawn(&request, &issued()).unwrap();
        let survivor = run.workspace().join("survivor.txt");
        std::fs::write(&survivor, b"first\n").unwrap();

        let refused = refusal(sandbox.spawn(&request, &issued()));

        assert!(
            matches!(&refused, Refusal::Exists(id) if id == &request.execution),
            "got {refused:?}",
        );
        assert!(
            survivor.is_file(),
            "the refused attempt touched the first run's workspace",
        );

        drop(run);
    }

    #[tokio::test]
    async fn an_execution_id_is_usable_again_after_cleanup() {
        let scratch = Scratch::new("reusable");
        let sandbox = Confined::new(config(&scratch)).unwrap();
        let request = shell("exit 0");

        let run = sandbox.spawn(&request, &issued()).unwrap();
        within("wait", run.wait()).await.unwrap().cleanup().unwrap();

        let run = sandbox.spawn(&request, &issued()).unwrap();
        let completed = within("wait", run.wait()).await.unwrap();

        assert_eq!(completed.outcome(), Outcome::Exited(0));
        completed.cleanup().unwrap();
    }

    #[tokio::test]
    async fn a_workload_that_exits_125_is_reported_as_its_own_exit() {
        let scratch = Scratch::new("exit-125");
        let sandbox = Confined::new(config(&scratch)).unwrap();

        let run = sandbox.spawn(&shell("exit 125"), &issued()).unwrap();
        let completed = within("wait", run.wait()).await.unwrap();

        assert_eq!(completed.outcome(), Outcome::Exited(125));
        completed.cleanup().unwrap();
    }

    #[tokio::test]
    async fn a_finished_run_does_not_stand_on_its_deadline() {
        let scratch = Scratch::new("early-deadline");
        let sandbox = Confined::new(config(&scratch)).unwrap();
        let mut request = shell("exit 0");
        request.deadline = Some(Duration::from_secs(3600));

        let started = Instant::now();
        let run = sandbox.spawn(&request, &issued()).unwrap();
        let completed = within("wait", run.wait()).await.unwrap();

        assert_eq!(completed.outcome(), Outcome::Exited(0));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "wait stood on a deadline an hour away: {:?}",
            started.elapsed(),
        );
        completed.cleanup().unwrap();
    }

    /// The confiner calls `setsid` after it starts, so for its first moments the pid is the only
    /// handle. This confiner never gets that far, which is the state a signal cannot name by group.
    fn sessionless(scratch: &Scratch) -> Config {
        Config {
            confiner: scratch.script(
                "sleepy-confiner.sh",
                &format!("#!{}\nexec sleep 30\n", which("sh").display()),
            ),
            ..config(scratch)
        }
    }

    #[tokio::test]
    async fn dropping_a_run_without_a_session_kills_its_child() {
        let scratch = Scratch::new("no-session-drop");
        let sandbox = Confined::new(sessionless(&scratch)).unwrap();
        let run = sandbox.spawn(&shell("exit 0"), &issued()).unwrap();
        let pid = run.pid();
        assert!(alive(pid), "the confiner did not start");

        drop(run);

        assert!(
            await_death(pid).await,
            "dropping the run left a child that never led a group",
        );
    }

    #[tokio::test]
    async fn stopping_a_run_without_a_session_stops_its_child() {
        let scratch = Scratch::new("no-session-stop");
        let sandbox = Confined::new(Config {
            grace: Duration::from_millis(50),
            ..sessionless(&scratch)
        })
        .unwrap();
        let mut run = sandbox.spawn(&shell("exit 0"), &issued()).unwrap();
        let pid = run.pid();
        assert!(alive(pid), "the confiner did not start");

        within("stop", run.stop()).await.unwrap();

        assert!(
            await_death(pid).await,
            "stop left a child that never led a group running",
        );
        drop(run);
    }
}
