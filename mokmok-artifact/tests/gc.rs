mod common;

#[cfg(test)]
mod tests {
    use crate::common::tests::{MemoryReferences, Scratch, after, artifact, now};
    use mokmok_artifact::{
        BlobStore, Collector, ContentHash, Error, Reference, ReferenceStore, Report, Retention,
    };
    use mokmok_protocol::Moment;
    use std::cell::Cell;
    use std::time::{Duration, UNIX_EPOCH};

    const MIN_AGE: Duration = Duration::from_secs(1);

    fn retention() -> Retention {
        Retention {
            min_age: MIN_AGE,
            max_bytes: u64::MAX,
        }
    }

    #[test]
    fn an_unreferenced_blob_past_min_age_is_collected() {
        let scratch = Scratch::new("collected");
        let store = BlobStore::open(scratch.path()).unwrap();
        let hash = store.put(b"garbage").unwrap();
        let references = MemoryReferences::holding(Vec::new());

        let report = Collector::new(&store, &references, retention())
            .sweep(after(2))
            .unwrap();

        assert_eq!(report.collected, vec![hash]);
        assert!(matches!(store.read(hash), Err(Error::NotFound { .. })));
    }

    #[test]
    fn a_blob_inside_min_age_is_kept() {
        let scratch = Scratch::new("grace");
        let store = BlobStore::open(scratch.path()).unwrap();
        let hash = store.put(b"fresh").unwrap();
        let references = MemoryReferences::holding(Vec::new());
        let retention = Retention {
            min_age: Duration::from_secs(3600),
            max_bytes: u64::MAX,
        };

        let report = Collector::new(&store, &references, retention)
            .sweep(now())
            .unwrap();

        assert_eq!(report.collected, Vec::<ContentHash>::new());
        assert_eq!(store.read(hash).unwrap(), b"fresh");
    }

    #[test]
    fn a_reference_appearing_between_the_mark_and_the_sweep_keeps_its_blob() {
        let scratch = Scratch::new("race");
        let store = BlobStore::open(scratch.path()).unwrap();
        let hash = store.put(b"just referenced").unwrap();

        // The mark asks first and is told no; the sweep asks again and is told yes, which is the
        // reference a publisher wrote in between. A sweep that trusted the mark would unlink it.
        let flipping = Flipping::new();
        let report = Collector::new(&store, &flipping, retention())
            .sweep(after(2))
            .unwrap();

        assert_eq!(report.collected, Vec::<ContentHash>::new());
        assert_eq!(store.read(hash).unwrap(), b"just referenced");
        assert_eq!(flipping.asked(), 2);
    }

    #[test]
    fn max_bytes_evicts_the_oldest_unreferenced_blobs_until_the_store_fits() {
        let scratch = Scratch::new("evict");
        let store = BlobStore::open(scratch.path()).unwrap();
        let references = MemoryReferences::holding(Vec::new());

        let oldest = store.put(&vec![b'a'; 1000]).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        let middle = store.put(&vec![b'b'; 1000]).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        let newest = store.put(&vec![b'c'; 1000]).unwrap();

        let retention = Retention {
            min_age: MIN_AGE,
            max_bytes: 1000,
        };
        let report = Collector::new(&store, &references, retention)
            .sweep(after(2))
            .unwrap();

        assert_eq!(report.collected, vec![oldest, middle]);
        assert_eq!(report.bytes, 1000);
        assert!(!report.over_budget);
        assert!(matches!(store.read(oldest), Err(Error::NotFound { .. })));
        assert!(matches!(store.read(middle), Err(Error::NotFound { .. })));
        assert_eq!(store.read(newest).unwrap(), vec![b'c'; 1000]);
    }

    #[test]
    fn a_store_exactly_at_max_bytes_evicts_nothing() {
        let scratch = Scratch::new("at-budget");
        let store = BlobStore::open(scratch.path()).unwrap();
        let references = MemoryReferences::holding(Vec::new());

        let oldest = store.put(&vec![b'a'; 1000]).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        let newest = store.put(&vec![b'b'; 1000]).unwrap();

        let retention = Retention {
            min_age: MIN_AGE,
            max_bytes: 2000,
        };
        let report = Collector::new(&store, &references, retention)
            .sweep(after(2))
            .unwrap();

        assert_eq!(
            report,
            Report {
                collected: vec![oldest, newest],
                temporaries: 0,
                bytes: 0,
                over_budget: false,
            }
        );
    }

    #[test]
    fn a_sweep_reports_the_bytes_of_the_referenced_blob_it_kept() {
        let scratch = Scratch::new("report");
        let store = BlobStore::open(scratch.path()).unwrap();
        let kept = store.put(&vec![b'k'; 2000]).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        let garbage = store.put(&vec![b'g'; 500]).unwrap();
        let reference = Reference::new(&store, artifact(), kept, Moment::EPOCH).unwrap();
        let references = MemoryReferences::holding(vec![reference]);

        let retention = Retention {
            min_age: MIN_AGE,
            max_bytes: 1000,
        };
        let report = Collector::new(&store, &references, retention)
            .sweep(after(2))
            .unwrap();

        assert_eq!(
            report,
            Report {
                collected: vec![garbage],
                temporaries: 0,
                bytes: 2000,
                over_budget: true,
            }
        );
        assert!(matches!(store.read(garbage), Err(Error::NotFound { .. })));
        assert_eq!(store.read(kept).unwrap(), vec![b'k'; 2000]);
    }

    #[test]
    fn a_blob_exactly_at_min_age_is_a_candidate() {
        let scratch = Scratch::new("boundary");
        let store = BlobStore::open(scratch.path()).unwrap();
        let hash = store.put(b"on the boundary").unwrap();
        let published = std::fs::metadata(store.path(hash))
            .unwrap()
            .modified()
            .unwrap();
        let since = published.duration_since(UNIX_EPOCH).unwrap();
        let at =
            Moment::from_millis(u64::try_from(since.as_millis()).unwrap()).saturating_add(MIN_AGE);
        let references = MemoryReferences::holding(Vec::new());

        let report = Collector::new(&store, &references, retention())
            .sweep(at)
            .unwrap();

        assert_eq!(report.collected, vec![hash]);
    }

    #[test]
    fn a_store_whose_root_vanished_reports_an_empty_sweep() {
        let scratch = Scratch::new("vanished");
        let store = BlobStore::open(scratch.path()).unwrap();
        std::fs::remove_dir_all(scratch.path()).unwrap();
        let references = MemoryReferences::holding(Vec::new());

        let report = Collector::new(&store, &references, retention())
            .sweep(after(2))
            .unwrap();

        assert_eq!(report, Report::default());
    }

    #[test]
    fn a_root_that_is_not_a_directory_is_an_io_error() {
        let scratch = Scratch::new("not-a-directory");
        let store = BlobStore::open(scratch.path()).unwrap();
        std::fs::remove_dir_all(scratch.path()).unwrap();
        std::fs::write(scratch.path(), b"in the way").unwrap();
        let references = MemoryReferences::holding(Vec::new());

        let result = Collector::new(&store, &references, retention()).sweep(after(2));

        std::fs::remove_file(scratch.path()).unwrap();
        assert!(matches!(result, Err(Error::Io(_))));
    }

    /// Answers "no" the first time it is asked and "yes" afterwards: a reference that appears
    /// between the mark and the sweep.
    struct Flipping {
        calls: Cell<usize>,
    }

    impl Flipping {
        fn new() -> Self {
            Self {
                calls: Cell::new(0),
            }
        }

        fn asked(&self) -> usize {
            self.calls.get()
        }
    }

    impl ReferenceStore for Flipping {
        fn referenced(
            &self,
            _hash: ContentHash,
        ) -> Result<bool, Error> {
            let calls = self.calls.get();
            self.calls.set(calls + 1);

            Ok(calls > 0)
        }

        fn list(&self) -> Result<Vec<Reference>, Error> {
            Ok(Vec::new())
        }
    }
}
