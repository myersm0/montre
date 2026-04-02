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

pub use corpus::{Corpus, CorpusMeta, ComponentMeta, AlignmentMeta, AlignmentIndex};
pub use inverted::{InvertedIndex, InMemoryInverted};
pub use forward::{ForwardIndex, InMemoryForward};
pub use spans::{SpanIndex, InMemorySpans};
pub use spans_flat::{MappedSpans, SpanStore, write_flat_spans};
pub use forward_flat::{MappedForward, ForwardStore, write_flat_forward, write_mfwd, LayerBuild, build_dict_encoded_layer, build_dense_numeric_layer, is_numeric_layer};
pub use sentence_ids::{MappedSentenceIds, write_sentence_ids};
pub use mwt::{MWTEntry, MappedMWTs, write_mwts};
pub use spacing::{MappedSpacing, write_spacing};
pub use empty_nodes::{EmptyNode, EmptyNodeStore, write_empty_nodes};
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

	#[error("Index version mismatch: expected {expected}, found {found}. Rebuild with `montre build`.")]
	VersionMismatch { expected: u32, found: u32 },
}

pub type Result<T> = std::result::Result<T, IndexError>;

pub const index_version: u32 = 5;

pub fn open(path: impl AsRef<Path>) -> Result<Corpus> {
	Corpus::open(path)
}
