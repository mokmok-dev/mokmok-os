//! A BM25 index over a directory tree, refreshed incrementally and queried fuzzily.
//!
//! [`Corpus`] binds one directory tree to one on-disk tantivy index and answers a model's finds
//! from it. The index holds names and bodies, because locating a file by what is in it is the
//! commonest need and bodies are affordable at the scale of a checkout.
//!
//! The walk holds to the boundary the operator named:
//!
//! - A symlink is never followed: the root is a boundary the operator named, and a link is a name
//!   that reaches out of it.
//! - A directory whose name begins with a dot is not entered, and neither is `target` or
//!   `node_modules`: build output is not source, and it dwarfs the tree it sits in.
//! - The store, when it lies inside the root, is skipped by resolved path, so that the index never
//!   indexes itself.
//! - A file containing a NUL byte in its first 8 KiB is not text, and is skipped.
//! - A file past [`Limits::file_bytes`], or a tree past [`Limits::files`], is skipped and counted,
//!   never fatal, and so is a file or a directory that cannot be read. An unreadable *root* is an
//!   error: an index that quietly covers less than the operator named is worse than no index.
//!
//! A pass reads only what changed. The manifest records the length and the modification time of
//! every file the last pass indexed, and the index is committed before the manifest is written, so
//! an interrupted pass costs a re-index and never a document the manifest claims and the index does
//! not hold. [`Corpus::refresh`] takes `&self` and holds the writer for the length of the pass
//! alone, so a caller needs no lock around a corpus.
//!
//! Every word of a query contributes its exact body and name terms, its one-edit transpose-aware
//! terms on both fields, and the last word a name prefix. [`render`] is the text a model reads.
//!
//! # Examples
//!
//! ```
//! use mokmok_search::{Corpus, Limits, render};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let root = std::env::temp_dir().join(format!("mokmok-search-doc-{}", std::process::id()));
//! std::fs::create_dir_all(&root)?;
//! let file = root.join("agentRuntime.rs");
//! std::fs::write(&file, "//! The agent runtime.\n")?;
//!
//! let corpus = Corpus::open(&root, &root.join("index"), Limits::default())?;
//! let refresh = corpus.refresh()?;
//! assert_eq!(refresh.indexed, 1);
//!
//! let response = corpus.query("agent runtime", Corpus::DEFAULT_LIMIT)?;
//! assert_eq!(response.hits[0].path, file);
//! assert_eq!(response.hits[0].snippet, "//! The agent runtime.");
//! assert_eq!(render(&response).lines().next(), Some("indexed=1 hits=1"));
//!
//! std::fs::remove_dir_all(&root)?;
//! # Ok(())
//! # }
//! ```
//!
//! A query for a name the tree spells differently still lands, and one for a typo finds its file:
//!
//! ```
//! use std::path::Path;
//!
//! use mokmok_search::{Corpus, Limits};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! # let root = std::env::temp_dir().join(format!("mokmok-search-doc-fuzz-{}", std::process::id()));
//! # std::fs::create_dir_all(&root)?;
//! # std::fs::write(root.join("note.txt"), "the tantivy index\n")?;
//! let corpus = Corpus::open(&root, &root.join("index"), Limits::default())?;
//! corpus.refresh()?;
//!
//! let typo = corpus.query("tantviy", Corpus::DEFAULT_LIMIT)?;
//! assert_eq!(typo.hits[0].path, Path::new(&root).join("note.txt"));
//! # std::fs::remove_dir_all(&root)?;
//! # Ok(())
//! # }
//! ```

mod corpus;
mod error;
mod query;
mod render;
mod token;
mod walk;

pub use corpus::{Corpus, Limits, Refresh};
pub use error::{Error, Source};
pub use query::{Hit, Response};
pub use render::render;
