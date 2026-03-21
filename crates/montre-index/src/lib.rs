#![allow(non_upper_case_globals)]

pub mod inverted;
pub mod forward;
pub mod forward_flat;
pub mod spans;
pub mod spans_flat;
pub mod lexicon;
pub mod corpus;

use std::path::Path;
use thiserror::Error;

pub use corpus::{Corpus, CorpusMeta, ComponentMeta, AlignmentMeta, AlignmentIndex};
pub use inverted::{InvertedIndex, InMemoryInverted};
pub use forward::{ForwardIndex, InMemoryForward};
pub use spans::{SpanIndex, InMemorySpans};
pub use spans_flat::{MappedSpans, SpanStore, write_flat_spans};
pub use forward_flat::write_flat_forward;
pub use lexicon::{Lexicon, InMemoryLexicon};

#[derive(Error, Debug)]
pub enum IndexError {
	#[error("IO error: {0}")]
	Io(#[from] std::io::Error),

	#[error("Index format error: {0}")]
	Format(String),

	#[error("Corpus not found: {0}")]
	NotFound(String),

	#[error("Layer not found: {0}")]
	LayerNotFound(String),

	#[error("Index version mismatch: expected {expected}, found {found}")]
	VersionMismatch { expected: u32, found: u32 },
}

pub type Result<T> = std::result::Result<T, IndexError>;

pub const index_version: u32 = 3;

pub fn open(path: impl AsRef<Path>) -> Result<Corpus> {
	Corpus::open(path)
}
