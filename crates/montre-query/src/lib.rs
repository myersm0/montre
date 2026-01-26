pub mod ast;
pub mod parser;
pub mod planner;
pub mod executor;

use thiserror::Error;

pub use ast::Query;
pub use executor::Results;

#[derive(Error, Debug)]
pub enum QueryError {
	#[error("Parse error at position {position}: {message}")]
	Parse { position: usize, message: String },

	#[error("Invalid regex: {0}")]
	Regex(#[from] regex::Error),

	#[error("Unknown layer: {0}")]
	UnknownLayer(String),

	#[error("Execution error: {0}")]
	Execution(String),
}

pub type Result<T> = std::result::Result<T, QueryError>;

pub fn parse(input: &str) -> Result<Query> {
	parser::parse(input)
}
