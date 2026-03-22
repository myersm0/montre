mod error;
mod strings;
mod corpus;
mod tokens;
mod query;
mod spans;
mod alignment;
mod build;

use montre_query::executor::Hit;

pub struct HitList {
	pub(crate) hits: Vec<Hit>,
}
