pub mod format;
pub mod builder;

use std::path::Path;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum BuildError {
	#[error("IO error: {0}")]
	Io(#[from] std::io::Error),

	#[error("Parse error at line {line}: {message}")]
	Parse { line: usize, message: String },

	#[error("Unsupported format: {0}")]
	UnsupportedFormat(String),

	#[error("JSON error: {0}")]
	Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, BuildError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputFormat {
	ConllU,
	StanzaJson,
	SpacyJson,
	Vrt,
	Tsv,
}

impl InputFormat {
	pub fn from_path(path: &Path) -> Option<Self> {
		let ext = path.extension()?.to_str()?;
		match ext {
			"conllu" | "conll" => Some(InputFormat::ConllU),
			"json" => Some(InputFormat::StanzaJson), // default JSON to Stanza
			"vrt" => Some(InputFormat::Vrt),
			"tsv" => Some(InputFormat::Tsv),
			_ => None,
		}
	}
}
