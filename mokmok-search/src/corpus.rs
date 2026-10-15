//! The corpus: one directory tree, the index derived from it, and the manifest between the two.
//!
//! A pass over the tree is incremental. The manifest records the length and the modification time
//! of every file the last pass indexed, so a file whose stamp has not moved is never read. Two
//! orders make a pass that is interrupted cheap rather than wrong:
//!
//! - The index is committed first and the manifest is written second, so a crash between them
//!   costs a re-index on the next pass, never a document the manifest claims and the index does
//!   not hold. A manifest that is absent, torn, or unreadable reads as empty and costs the same.
//! - A document is deleted under its path term before it is added, so a path re-added after a
//!   crash that lost the manifest lands once: the delete takes the lower opstamp and the add
//!   survives it.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tantivy::collector::TopDocs;
use tantivy::directory::MmapDirectory;
use tantivy::schema::{
    Field, IndexRecordOption, STORED, STRING, Schema, TEXT, TextFieldIndexing, Value,
};
use tantivy::{Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument, Term, doc};

use crate::error::Error;
use crate::query::{self, Hit, Response};
use crate::token::{self, CodeTokenizer};
use crate::walk::{self, Candidate};

/// The tokenizer name the schema's analyzed fields carry.
const TOKENIZER: &str = "code";
/// The manifest's file name inside the store.
const MANIFEST: &str = "manifest.json";
/// How many bytes of writer memory one pass may fill before it spills a segment.
const WRITER_BYTES: usize = 50_000_000;
/// How many bytes of a file the NUL check looks at.
const HEAD_BYTES: usize = 8 * 1024;
/// The default size cap: one mebibyte.
const FILE_BYTES: u64 = 1024 * 1024;
/// The default body cap: 256 KiB, the longest body one file contributes.
const BODY_BYTES: usize = 256 * 1024;
/// The default file cap: a walk past it stops and says so.
const FILES: usize = 50_000;

/// A directory tree and the BM25 index derived from it.
///
/// Both [`Corpus::refresh`] and [`Corpus::query`] take `&self`. The writer is created per pass and
/// dropped when the pass ends, so the index's exclusive lock is held for the length of one pass
/// and no longer, and a caller can hold a `Corpus` behind no mutex at all.
pub struct Corpus {
    root: PathBuf,
    store: PathBuf,
    resolved: PathBuf,
    limits: Limits,
    fields: Fields,
    index: Index,
}

impl Corpus {
    /// How many hits one query returns when the caller names no limit.
    pub const DEFAULT_LIMIT: usize = 10;

    /// Opens the index at `store`, creating it when absent, and binds it to `root`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Root`] when `root` cannot be listed, [`Error::Io`] when the store cannot be
    /// created or reached, and [`Error::Index`] when the index cannot be opened or the schema it
    /// holds is not the one this build writes.
    pub fn open(
        root: &Path,
        store: &Path,
        limits: Limits,
    ) -> Result<Self, Error> {
        std::fs::read_dir(root).map_err(|_| Error::Root {
            path: root.to_path_buf(),
        })?;
        std::fs::create_dir_all(store).map_err(Error::Io)?;

        let (schema, fields) = schema();
        let index =
            Index::open_or_create(MmapDirectory::open(store).map_err(index_failure)?, schema)
                .map_err(index_failure)?;
        index.tokenizers().register(TOKENIZER, CodeTokenizer);

        Ok(Self {
            resolved: std::fs::canonicalize(store).map_err(Error::Io)?,
            root: root.to_path_buf(),
            store: store.to_path_buf(),
            limits,
            fields,
            index,
        })
    }

    /// The directory tree this corpus is bound to.
    #[must_use]
    pub fn root(&self) -> &Path {
        self.root.as_path()
    }

    /// Reconciles the index with the tree, reading only what changed.
    ///
    /// A file whose stamp matches the manifest is left alone. Every other file is read, and the
    /// document under its path is deleted and written again, so that the index holds exactly the
    /// files the pass could index. Every manifest entry the walk did not offer is deleted.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Root`] when the root can no longer be listed, [`Error::Index`] when the
    /// index refuses the pass, and [`Error::Io`] when the manifest cannot be written. A file or a
    /// directory below the root that cannot be read is skipped and counted instead.
    pub fn refresh(&self) -> Result<Refresh, Error> {
        let found = walk::walk(&self.root, &self.resolved, &self.limits)?;
        let manifest = self.manifest();
        let mut writer: IndexWriter = self.index.writer(WRITER_BYTES).map_err(index_failure)?;
        let mut next = Manifest::new();
        let mut updated = 0_u64;
        let mut unchanged = 0_u64;
        let mut skipped = found.skipped;

        for candidate in found.files {
            let stamp = Stamp {
                modified: candidate.modified,
                size: candidate.size,
            };
            if manifest.get(&candidate.key) == Some(&stamp) {
                next.insert(candidate.key, stamp);
                unchanged += 1;
                continue;
            }

            let Some(body) = self.body(&candidate) else {
                skipped += 1;
                continue;
            };

            writer.delete_term(Term::from_field_text(self.fields.path, &candidate.key));
            writer
                .add_document(doc!(
                    self.fields.path => candidate.key.as_str(),
                    self.fields.name => candidate.name.as_str(),
                    self.fields.body => body.as_str(),
                ))
                .map_err(index_failure)?;
            next.insert(candidate.key, stamp);
            updated += 1;
        }

        let mut removed = 0_u64;
        for path in manifest.keys() {
            if !next.contains_key(path) {
                writer.delete_term(Term::from_field_text(self.fields.path, path));
                removed += 1;
            }
        }

        writer.commit().map_err(index_failure)?;
        drop(writer);
        self.write_manifest(&next)?;

        Ok(Refresh {
            indexed: updated + unchanged,
            updated,
            removed,
            unchanged,
            skipped,
        })
    }

    /// The `limit` highest-scoring documents for `text`, by BM25.
    ///
    /// Every word of `text` contributes an exact term on the body at weight 1 and on the name at
    /// weight 3, a one-edit transpose-aware term on both at half weight, and the last word a name
    /// prefix. An exact match therefore outranks a near one, and a name the model asked for
    /// outranks a body that mentions it. Empty text is no query at all: it answers no hits rather
    /// than every document.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Index`] when the index cannot be read or the query cannot be run.
    pub fn query(
        &self,
        text: &str,
        limit: usize,
    ) -> Result<Response, Error> {
        let reader: IndexReader = self
            .index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .map_err(index_failure)?;
        reader.reload().map_err(index_failure)?;

        let searcher = reader.searcher();
        let indexed = searcher.num_docs();
        let words = token::tokens(text);
        if words.is_empty() || limit == 0 {
            return Ok(Response {
                indexed,
                hits: Vec::new(),
            });
        }

        let query = query::boolean(&self.fields, &words);
        let ranked = searcher
            .search(&query, &TopDocs::with_limit(limit).order_by_score())
            .map_err(index_failure)?;
        let mut hits = Vec::with_capacity(ranked.len());

        for (score, address) in ranked {
            let document: TantivyDocument = searcher.doc(address).map_err(index_failure)?;
            let path = stored(&document, self.fields.path);
            let body = stored(&document, self.fields.body);
            hits.push(Hit {
                path: PathBuf::from(path),
                score,
                snippet: query::snippet(body, &words),
            });
        }

        // Tantivy breaks a score tie by document address, which a later commit can move, so the
        // order is pinned here: two identical queries over one index answer with the same list, and
        // a caller can hold the text to what it recorded.
        hits.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.path.cmp(&right.path))
        });

        Ok(Response { indexed, hits })
    }

    /// The manifest the last committed pass left, or nothing when there is none to read.
    fn manifest(&self) -> Manifest {
        std::fs::read(self.store.join(MANIFEST))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Writes the manifest that describes what the committed index now holds.
    fn write_manifest(
        &self,
        manifest: &Manifest,
    ) -> Result<(), Error> {
        let bytes = serde_json::to_vec(manifest)
            .map_err(|error| Error::Io(std::io::Error::other(error)))?;

        write_atomic(&self.store.join(MANIFEST), &bytes).map_err(Error::Io)
    }

    /// The text to index for `candidate`, or nothing when the file is not text.
    fn body(
        &self,
        candidate: &Candidate,
    ) -> Option<String> {
        let bytes = read_capped(&candidate.path, self.limits.file_bytes).ok()?;
        if bytes[..bytes.len().min(HEAD_BYTES)].contains(&0) {
            return None;
        }

        Some(capped(
            &String::from_utf8_lossy(&bytes),
            self.limits.body_bytes,
        ))
    }
}

/// The caps one corpus holds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    /// The largest file the walk indexes, in bytes. A file past it is skipped and counted.
    pub file_bytes: u64,
    /// The longest body kept from one file, in bytes.
    pub body_bytes: usize,
    /// The most files the walk offers. Past it the walk stops and says so.
    pub files: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            file_bytes: FILE_BYTES,
            body_bytes: BODY_BYTES,
            files: FILES,
        }
    }
}

/// What one pass over the tree did.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Refresh {
    /// The files the index holds after the pass: the updated ones and the unchanged ones.
    pub indexed: u64,
    /// The documents the pass wrote, because the file was new or its stamp had moved.
    pub updated: u64,
    /// The documents the pass deleted, because the tree no longer holds the file, or holds one the
    /// limits no longer let through.
    pub removed: u64,
    /// The files whose stamp matched the manifest, which the pass did not read.
    pub unchanged: u64,
    /// The entries the pass refused: past a limit, unreadable, or not text.
    pub skipped: u64,
}

/// The three fields every document carries.
#[derive(Clone, Copy, Debug)]
pub struct Fields {
    /// The path, untokenized: the term a document is deleted under and the value the model reads.
    pub path: Field,
    /// The base name, analyzed so that `agentRuntime.rs` and `agent_runtime.rs` reach each other.
    pub name: Field,
    /// The capped bytes of the file, analyzed the same way.
    pub body: Field,
}

/// One file as the pass that indexed it saw it.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct Stamp {
    /// The file's modification time in milliseconds since the epoch, when the platform has one.
    modified: Option<u64>,
    /// The file's length, which catches a change a coarse clock would miss.
    size: u64,
}

/// Every path the index holds, with the stamp it was indexed under.
type Manifest = BTreeMap<String, Stamp>;

/// The one document shape, and the fields it is written through.
fn schema() -> (Schema, Fields) {
    let analyzed = TEXT.set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer(TOKENIZER)
            .set_index_option(IndexRecordOption::WithFreqsAndPositions),
    );
    let mut builder = Schema::builder();
    let path = builder.add_text_field("path", STRING | STORED);
    let name = builder.add_text_field("name", analyzed.clone() | STORED);
    let body = builder.add_text_field("body", analyzed | STORED);

    (builder.build(), Fields { path, name, body })
}

/// The text one stored field holds in `document`, or the empty string when it holds none.
fn stored(
    document: &TantivyDocument,
    field: Field,
) -> &str {
    document
        .get_first(field)
        .and_then(|value| value.as_str())
        .unwrap_or_default()
}

/// At most `cap` bytes of the file at `path`.
fn read_capped(
    path: &Path,
    cap: u64,
) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?.take(cap).read_to_end(&mut bytes)?;

    Ok(bytes)
}

/// `text` cut to `limit` bytes on a character boundary.
fn capped(
    text: &str,
    limit: usize,
) -> String {
    let end = text.len().min(limit);

    text[..text.floor_char_boundary(end)].to_owned()
}

/// Writes `bytes` at `path` so that a reader sees either the file that was there or the whole new
/// one, never half of either.
fn write_atomic(
    path: &Path,
    bytes: &[u8],
) -> std::io::Result<()> {
    let staging = path.with_extension("new");
    std::fs::write(&staging, bytes)?;

    std::fs::rename(staging, path)
}

/// The index's own failure, erased so that no tantivy type crosses this crate's boundary.
fn index_failure(error: impl std::error::Error + Send + Sync + 'static) -> Error {
    Error::Index(Box::from(error))
}
