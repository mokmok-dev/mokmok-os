mod common;

#[cfg(test)]
mod tests {
    use crate::common::tests::{Allow, Deny, MemoryReferences, Scratch, artifact, principal};
    use mokmok_artifact::{BlobStore, Catalog, ContentHash, Error, Reference};
    use mokmok_protocol::Moment;

    #[test]
    fn a_read_without_authority_is_refused() {
        let scratch = Scratch::new("denied");
        let store = BlobStore::open(scratch.path()).unwrap();
        let hash = store.put(b"classified").unwrap();
        let named = artifact();
        let reference = Reference::new(&store, named, hash, Moment::EPOCH).unwrap();
        let catalog = Catalog::new(store, MemoryReferences::holding(vec![reference]));

        assert!(matches!(
            catalog.read(&Deny, principal(), named),
            Err(Error::Denied { .. }),
        ));
    }

    #[test]
    fn an_authorized_read_returns_the_blob() {
        let scratch = Scratch::new("allowed");
        let store = BlobStore::open(scratch.path()).unwrap();
        let hash = store.put(b"shared").unwrap();
        let named = artifact();
        let reference = Reference::new(&store, named, hash, Moment::EPOCH).unwrap();
        let catalog = Catalog::new(store, MemoryReferences::holding(vec![reference]));

        assert_eq!(catalog.read(&Allow, principal(), named).unwrap(), b"shared",);
    }

    #[test]
    fn a_reference_to_a_blob_the_store_never_finished_is_refused() {
        let scratch = Scratch::new("unfinished");
        let store = BlobStore::open(scratch.path()).unwrap();
        let missing = ContentHash::of(b"never stored");

        assert!(matches!(
            Reference::new(&store, artifact(), missing, Moment::EPOCH),
            Err(Error::NotFound { .. }),
        ));
    }
}
