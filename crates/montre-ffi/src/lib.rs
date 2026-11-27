pub mod alignment;
pub mod build;
pub mod corpus;
pub mod error;
pub mod query;
pub mod spans;
pub mod strings;
pub mod tokens;

use montre_query::executor::Hit;

pub struct HitList {
	pub(crate) hits: Vec<Hit>,
}
