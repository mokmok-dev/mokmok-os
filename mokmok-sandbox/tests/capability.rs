mod common;

// The `cfg(test)` module is what tells the `allow-unwrap-in-tests`, `allow-expect-in-tests`, and
// `allow-panic-in-tests` settings in `.clippy.toml` that this harness is test code.
#[cfg(test)]
mod tests {
    use crate::common::tests::Scratch;
    use mokmok_sandbox::{Access, Capability, Exposure, uncovered};
    use proptest::prelude::*;
    use std::path::PathBuf;

    /// The rights a grant or a request can name, as indices a proptest can generate.
    const ACCESS: [Access; 6] = [
        Access::READ,
        Access::WRITE,
        Access::EXECUTE,
        Access::READ_WRITE,
        Access::READ_EXECUTE,
        Access::READ_WRITE_EXECUTE,
    ];

    #[test]
    fn an_equal_path_is_covered() {
        let scratch = Scratch::new("gate-equal");
        let file = scratch.write("file.txt", "x");
        let exposures = [Exposure::read(&file)];
        let issued = [Capability::issued(&file, Access::READ)];

        assert_eq!(uncovered(&exposures, &issued), None);
    }

    #[test]
    fn an_ancestor_directory_covers_what_is_beneath_it() {
        let scratch = Scratch::new("gate-ancestor");
        let file = scratch.write("tree/leaves/file.txt", "x");
        let tree = scratch.path().join("tree");
        let exposures = [Exposure::read(&file)];
        let issued = [Capability::issued(&tree, Access::READ)];

        assert_eq!(uncovered(&exposures, &issued), None);
    }

    #[test]
    fn a_sibling_that_shares_a_text_prefix_is_not_covered() {
        let scratch = Scratch::new("gate-sibling");
        std::fs::create_dir_all(scratch.path().join("a/b")).unwrap();
        let bc = scratch.path().join("a/bc");
        let exposures = [Exposure::read(&bc)];
        let issued = [Capability::issued(scratch.path().join("a/b"), Access::READ)];

        let declined = uncovered(&exposures, &issued).expect("a sibling is not covered");

        assert_eq!(declined.as_path(), bc.as_path());
    }

    #[test]
    fn a_file_capability_covers_its_own_name_only() {
        let scratch = Scratch::new("gate-file");
        let file = scratch.write("file.txt", "x");
        let beneath = file.join("child");
        let exposures = [Exposure::read(&file), Exposure::read(&beneath)];
        let issued = [Capability::issued(&file, Access::READ)];

        let declined = uncovered(&exposures, &issued).expect("a file covers no path beneath it");

        assert_eq!(declined.as_path(), beneath.as_path());
    }

    #[test]
    fn a_read_capability_does_not_cover_a_write() {
        let scratch = Scratch::new("gate-access");
        let file = scratch.write("file.txt", "x");
        let exposures = [Exposure::read_write(&file)];
        let issued = [Capability::issued(&file, Access::READ)];

        assert_eq!(uncovered(&exposures, &issued), Some(&exposures[0]));
    }

    #[test]
    fn the_first_uncovered_exposure_is_the_one_named() {
        let scratch = Scratch::new("gate-first");
        let covered = scratch.write("covered.txt", "x");
        let open = scratch.write("uncovered.txt", "x");
        let exposures = [Exposure::read(&covered), Exposure::read(&open)];
        let issued = [Capability::issued(&covered, Access::READ)];

        assert_eq!(uncovered(&exposures, &issued), Some(&exposures[1]));
    }

    proptest! {
        #[test]
        fn a_subset_of_the_issued_grants_is_always_covered(
            grant in 0_usize..6,
            needed in 0_usize..6,
            keep in prop::collection::vec(any::<bool>(), 1..8),
        ) {
            prop_assume!(ACCESS[grant].covers(ACCESS[needed]));

            let mut exposures = Vec::new();
            let mut issued = Vec::new();

            for (index, kept) in keep.iter().enumerate() {
                let path = PathBuf::from(format!("/grant-{index}"));
                issued.push(Capability::issued(&path, ACCESS[grant]));

                if *kept {
                    exposures.push(Exposure::path(&path, ACCESS[needed]));
                }
            }

            prop_assert_eq!(uncovered(&exposures, &issued), None);
        }

        #[test]
        fn one_extra_exposure_is_declined_and_named(
            grant in 0_usize..6,
            needed in 0_usize..6,
            beyond in 0_usize..4,
            kept in 1_usize..6,
        ) {
            prop_assume!(ACCESS[grant].covers(ACCESS[needed]));

            let mut exposures: Vec<Exposure> = (0..kept)
                .map(|index| Exposure::path(format!("/grant-{index}"), ACCESS[needed]))
                .collect();
            let issued: Vec<Capability> = (0..kept)
                .map(|index| Capability::issued(format!("/grant-{index}"), ACCESS[grant]))
                .collect();
            let extra = Exposure::path(format!("/elsewhere/outside-{beyond}"), ACCESS[needed]);
            exposures.push(extra.clone());

            prop_assert_eq!(uncovered(&exposures, &issued), Some(&extra));
        }
    }
}
