pub mod conllu;

use crate::Result;
use montre_core::Span;

pub struct ParsedSentence {
	pub tokens: Vec<ParsedToken>,
	pub span: Span,
}

pub struct ParsedToken {
	pub word: String,
	pub lemma: Option<String>,
	pub pos: Option<String>,
	pub xpos: Option<String>,
	pub feats: Option<String>,
	pub head: Option<i64>,
	pub deprel: Option<String>,
}

pub trait CorpusReader {
	fn read_sentences(&mut self) -> Result<Vec<ParsedSentence>>;
}
