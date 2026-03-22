pub mod error;
pub mod strings;
pub mod corpus;
pub mod tokens;
pub mod query;
pub mod spans;
pub mod alignment;
pub mod build;

use montre_query::executor::Hit;

pub struct HitList {
	pub(crate) hits: Vec<Hit>,
}
