pub mod conllu;

use crate::Result;
use montre_core::Span;

pub use conllu::ParseStats;

pub struct ParsedSentence {
	pub tokens: Vec<ParsedToken>,
	pub span: Span,
	pub sent_id: Option<String>,
	pub mwts: Vec<ParsedMWT>,
	pub empty_nodes: Vec<ParsedEmptyNode>,
}

pub struct ParsedToken {
	pub word: String,
	pub lemma: Option<String>,
	pub upos: Option<String>,
	pub xpos: Option<String>,
	pub feats: Option<String>,
	pub head: Option<i64>,
	pub deprel: Option<String>,
	pub deps: Option<String>,
	pub space_after_no: bool,
}

pub struct ParsedMWT {
	pub first: usize,
	pub last: usize,
	pub form: String,
	pub no_space_after: bool,
}

pub struct ParsedEmptyNode {
	pub major: u16,
	pub minor: u16,
	pub form: String,
	pub lemma: Option<String>,
	pub upos: Option<String>,
	pub xpos: Option<String>,
	pub feats: Option<String>,
	pub deps: Option<String>,
	pub misc: Option<String>,
}

pub trait CorpusReader {
	fn read_sentences(&mut self) -> Result<Vec<ParsedSentence>>;
}
