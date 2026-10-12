mod common;

#[cfg(test)]
mod tests {
    use crate::common::tests::{MemoryReferences, Scratch, after};
    use mokmok_artifact::{BlobStore, Collector, ContentHash, Error, HexError, Retention};
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    #[test]
    fn the_hash_of_empty_input_is_the_known_vector() {
        assert_eq!(
            ContentHash::of(b"").to_string(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        );
    }

    #[test]
    fn the_hash_of_a_one_byte_input_is_the_known_vector() {
        assert_eq!(
            ContentHash::of(b"a").to_string(),
            "ca978112ca1bbdcafac231b39a23dc4da786eff8147c4e72b9807785afee48bb",
        );
    }

    #[test]
    fn a_hash_displays_and_parses_back_to_itself() {
        let hash = ContentHash::of(b"round trip");

        assert_eq!(hash.to_string().parse::<ContentHash>(), Ok(hash));
    }

    #[test]
    fn a_hash_rejects_a_wrong_length() {
        assert_eq!(
            "00".parse::<ContentHash>(),
            Err(HexError::Length { chars: 2 }),
        );
    }

    #[test]
    fn a_hash_rejects_a_non_hex_character() {
        assert_eq!(
            "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz"
                .parse::<ContentHash>(),
            Err(HexError::Digit { found: 'z' }),
        );
    }

    #[test]
    fn put_then_read_round_trips_the_bytes() {
        let scratch = Scratch::new("round-trip");
        let store = BlobStore::open(scratch.path()).unwrap();

        let hash = store.put(b"the whole blob").unwrap();

        assert_eq!(
            hash.to_string(),
            ContentHash::of(b"the whole blob").to_string()
        );
        assert_eq!(store.len(hash).unwrap(), 14);
        assert_eq!(store.read(hash).unwrap(), b"the whole blob");
    }

    #[test]
    fn an_interrupted_write_is_invisible_and_its_staging_file_is_collected() {
        let scratch = Scratch::new("interrupted");
        let store = BlobStore::open(scratch.path()).unwrap();
        let partial = b"half a blob";
        let hash = ContentHash::of(partial);

        // What an interrupted `put` leaves: bytes under no hash, in the directory the finished
        // blob would have gone to.
        let blob = store.path(hash);
        let directory = blob.parent().unwrap();
        std::fs::create_dir_all(directory).unwrap();
        let staging = directory.join(".tmp-interrupted");
        std::fs::write(&staging, partial).unwrap();

        assert!(matches!(store.read(hash), Err(Error::NotFound { .. })));

        let references = MemoryReferences::holding(Vec::new());
        let retention = Retention {
            min_age: Duration::from_secs(1),
            max_bytes: u64::MAX,
        };
        let report = Collector::new(&store, &references, retention)
            .sweep(after(2))
            .unwrap();

        assert_eq!(report.temporaries, 1);
        assert_eq!(report.collected, Vec::<ContentHash>::new());
        assert_eq!(report.bytes, 0);
        assert!(!staging.exists());
    }

    #[test]
    fn remove_reports_whether_the_blob_was_there() {
        let scratch = Scratch::new("remove");
        let store = BlobStore::open(scratch.path()).unwrap();
        let hash = store.put(b"removable").unwrap();

        assert!(store.remove(hash).unwrap());
        assert!(!store.remove(hash).unwrap());
    }

    #[test]
    fn remove_reports_an_io_error_when_the_path_is_not_a_file() {
        let scratch = Scratch::new("remove-directory");
        let store = BlobStore::open(scratch.path()).unwrap();
        let hash = ContentHash::of(b"a directory stands here");
        std::fs::create_dir_all(store.path(hash)).unwrap();

        assert!(matches!(store.remove(hash), Err(Error::Io(_))));
    }

    #[test]
    fn a_write_fails_when_the_blob_directory_cannot_be_synced() {
        let scratch = Scratch::new("unsynced");
        let store = BlobStore::open(scratch.path()).unwrap();
        let bytes = b"cannot sync this directory";
        let hash = ContentHash::of(bytes);
        let directory = store.path(hash).parent().unwrap().to_path_buf();
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o333)).unwrap();

        let result = store.put(bytes);

        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(result, Err(Error::Io(_))));
    }
}
