//! A grant names an object. The gate and the ruleset have to mean the same object by that name.

mod common;

// The `cfg(test)` module is what tells the `allow-unwrap-in-tests`, `allow-expect-in-tests`, and
// `allow-panic-in-tests` settings in `.clippy.toml` that this harness is test code.
#[cfg(test)]
mod tests {
    use crate::common::tests::{Scratch, config, issued, shell, within};
    use mokmok_sandbox::{Access, Capability, Confined, Exposure, Outcome, Refusal, Sandbox};
    use std::path::PathBuf;

    /// A granted directory holding a symlink that leads outside the grant.
    fn escape(scratch: &Scratch) -> (PathBuf, PathBuf, PathBuf) {
        let secret = scratch.write("private/secret.txt", "top-secret\n");
        let granted = scratch.path().join("granted");
        std::fs::create_dir_all(&granted).unwrap();
        std::os::unix::fs::symlink(secret.parent().unwrap(), granted.join("link")).unwrap();

        (granted, secret, scratch.path().join("private"))
    }

    #[tokio::test]
    async fn an_exposure_that_resolves_out_of_the_grant_is_refused() {
        let scratch = Scratch::new("escape-gate");
        let (granted, secret, _private) = escape(&scratch);
        let requested = granted.join("link/secret.txt");
        let mut request = shell(&format!("cat {}", requested.display()));
        request.exposures = vec![
            Exposure::read_execute("/nix/store"),
            Exposure::read(&requested),
        ];
        let mut capabilities = issued();
        capabilities.push(Capability::issued(&granted, Access::READ));

        let sandbox = Confined::new(config(&scratch)).unwrap();
        let refused = match sandbox.spawn(&request, &capabilities) {
            Err(refused) => refused,
            Ok(_run) => panic!(
                "the gate approved a path that resolves to {}",
                secret.display()
            ),
        };

        assert!(
            matches!(
                &refused,
                Refusal::Uncovered(exposure, resolved)
                    if exposure.as_path() == requested.as_path() && resolved == secret.as_path()
            ),
            "got {refused:?}",
        );
    }

    #[tokio::test]
    async fn an_exposure_of_a_symlinked_directory_does_not_widen_the_grant() {
        let scratch = Scratch::new("escape-directory");
        let (granted, secret, _private) = escape(&scratch);
        let mut request = shell(&format!("cat {}", secret.display()));
        request.exposures = vec![
            Exposure::read_execute("/nix/store"),
            Exposure::read(granted.join("link")),
        ];
        let mut capabilities = issued();
        capabilities.push(Capability::issued(&granted, Access::READ));

        let sandbox = Confined::new(config(&scratch)).unwrap();
        let refused = match sandbox.spawn(&request, &capabilities) {
            Err(refused) => refused,
            Ok(_run) => panic!(
                "the gate approved a directory that resolves to {}",
                secret.parent().unwrap().display(),
            ),
        };

        assert!(
            matches!(
                &refused,
                Refusal::Uncovered(exposure, resolved)
                    if exposure.as_path() == granted.join("link").as_path()
                        && resolved == secret.parent().unwrap()
            ),
            "got {refused:?}",
        );
    }

    #[tokio::test]
    async fn a_symlink_that_stays_inside_the_grant_still_works() {
        let scratch = Scratch::new("escape-inside");
        let real = scratch.write("real/file.txt", "inside\n");
        let directory = scratch.path().join("area");
        std::fs::create_dir_all(&directory).unwrap();
        std::os::unix::fs::symlink(scratch.path().join("real"), directory.join("link")).unwrap();

        let requested = directory.join("link/file.txt");
        let mut request = shell(&format!("cat {}", requested.display()));
        request.exposures = vec![
            Exposure::read_execute("/nix/store"),
            Exposure::read(scratch.path().join("real")),
        ];
        let mut capabilities = issued();
        capabilities.push(Capability::issued(
            scratch.path().join("real"),
            Access::READ,
        ));

        let sandbox = Confined::new(config(&scratch)).unwrap();
        let run = sandbox.spawn(&request, &capabilities).unwrap();
        let completed = within("wait", run.wait()).await.unwrap();

        assert_eq!(completed.outcome(), Outcome::Exited(0));
        assert_eq!(completed.stdout().bytes(), b"inside\n");
        assert!(real.is_file());
        completed.cleanup().unwrap();
    }
}
