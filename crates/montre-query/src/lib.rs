pub mod ast;
pub mod parser;
pub mod planner;
mod budget;
pub mod executor;
mod sequence;
mod validate;

use thiserror::Error;

pub use ast::Query;
pub use budget::Limits;
pub use executor::Results;

#[derive(Error, Debug)]
pub enum QueryError {
	#[error("Parse error at position {position}: {message}")]
	Parse { position: usize, message: String },

	#[error("Invalid regex: {0}")]
	Regex(#[from] regex::Error),

	#[error("Unknown layer: {0}")]
	UnknownLayer(String),

	#[error("Layer '{0}' is stored but not indexed for token queries")]
	UnindexedLayer(String),

	#[error("Unknown span layer: {0}")]
	UnknownSpanLayer(String),

	#[error("Unknown component: {0}")]
	UnknownComponent(String),

	#[error("Unknown document: {0}")]
	UnknownDocument(String),

	#[error("Unknown label: {0}")]
	UnknownLabel(String),

	#[error("Quantifier bound {bound} exceeds the supported maximum of {limit}")]
	QuantifierTooLarge { bound: u32, limit: u32 },

	#[error("Query needs more than the limit of {limit} {resource}; narrow the pattern or raise the limit")]
	LimitExceeded { resource: &'static str, limit: u64 },

	#[error("Execution error: {0}")]
	Execution(String),
}

pub type Result<T> = std::result::Result<T, QueryError>;

pub fn parse(input: &str) -> Result<Query> {
	parser::parse(input)
}
