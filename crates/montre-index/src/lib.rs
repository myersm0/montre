#![allow(non_upper_case_globals)]

pub mod inverted;
pub mod forward;
pub mod forward_flat;
pub mod spans;
pub mod spans_flat;
pub mod sentence_ids;
pub mod mwt;
pub mod spacing;
pub mod empty_nodes;
pub mod lexicon;
pub mod corpus;

use std::path::Path;
use thiserror::Error;

// Re-exports from montre-core (foundational types used throughout the API surface)
pub use montre_core::{Position, Span};

// Reading API
pub use corpus::{Corpus, CorpusMeta, ComponentMeta, AlignmentMeta, AlignmentIndex};
pub use inverted::{InvertedIndex, InMemoryInverted};
pub use forward::{ForwardIndex, InMemoryForward};
pub use forward_flat::{MappedForward, ForwardStore};
pub use spans::{SpanIndex, InMemorySpans};
pub use spans_flat::{MappedSpans, SpanStore};
pub use sentence_ids::MappedSentenceIds;
pub use mwt::{MWTEntry, MappedMWTs};
pub use spacing::SpacingIndex;
pub use empty_nodes::{EmptyNode, EmptyNodeStore};
pub use lexicon::{Lexicon, InMemoryLexicon};

/// Common traits re-exported for ergonomic bulk import.
pub mod prelude {
	pub use crate::{ForwardIndex, InvertedIndex, SpanIndex};
}

// Build helpers (consumed by montre-build)
pub use forward_flat::{write_flat_forward, write_mfwd, LayerBuild, build_dict_encoded_layer, build_dense_numeric_layer, is_numeric_layer};
pub use spans_flat::write_flat_spans;
pub use sentence_ids::write_sentence_ids;
pub use mwt::write_mwts;
pub use spacing::write_spacing;
pub use empty_nodes::write_empty_nodes;

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

	#[error("Index version mismatch: expected {expected}, found {found}. Rebuild with `montre build`.")]
	VersionMismatch { expected: u32, found: u32 },
}

pub type Result<T> = std::result::Result<T, IndexError>;

pub const index_version: u32 = 5;

pub fn open(path: impl AsRef<Path>) -> Result<Corpus> {
	Corpus::open(path)
}

pub(crate) fn align_to_8(n: usize) -> usize {
	(n + 7) & !7
}
