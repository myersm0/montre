#[derive(Debug, Clone, PartialEq)]
pub enum Query {
	Token(TokenPattern),
	Sequence(Vec<Query>),
	Repetition {
		inner: Box<Query>,
		min: u32,
		max: Option<u32>,
	},
	Or(Vec<Query>),
	Within {
		inner: Box<Query>,
		span_layer: String,
	},
	Containing {
		inner: Box<Query>,
		span_layer: String,
	},
	Capture {
		name: String,
		inner: Box<Query>,
	},
	WithinComponent {
		inner: Box<Query>,
		components: Vec<String>,
	},
	WithinDocument {
		inner: Box<Query>,
		documents: Vec<String>,
	},
	Project {
		inner: Box<Query>,
		alignment: String,
	},
	Constrained {
		inner: Box<Query>,
		constraints: Vec<GlobalConstraint>,
	},
}

#[derive(Debug, Clone, PartialEq)]
pub struct TokenPattern {
	pub constraints: Vec<Constraint>,
}

impl TokenPattern {
	pub fn new() -> Self {
		Self {
			constraints: Vec::new(),
		}
	}

	pub fn with_constraint(mut self, constraint: Constraint) -> Self {
		self.constraints.push(constraint);
		self
	}

	pub fn any() -> Self {
		Self::new()
	}
}

impl Default for TokenPattern {
	fn default() -> Self {
		Self::new()
	}
}

#[derive(Debug, Clone, PartialEq)]
pub struct Constraint {
	pub layer: String,
	pub op: ConstraintOp,
	pub value: ConstraintValue,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConstraintOp {
	Eq,
	Ne,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConstraintValue {
	Literal(String),
	Regex(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum GlobalConstraint {
	Eq { left: LabelAttr, right: LabelAttr },
	Ne { left: LabelAttr, right: LabelAttr },
	Distance {
		left: String,
		right: String,
		op: CmpOp,
		value: u32,
	},
}

#[derive(Debug, Clone, PartialEq)]
pub struct LabelAttr {
	pub label: String,
	pub attr: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CmpOp {
	Ge,
	Gt,
	Le,
	Lt,
	Eq,
	Ne,
}

impl Query {
	pub fn token(pattern: TokenPattern) -> Self {
		Query::Token(pattern)
	}

	pub fn sequence(queries: Vec<Query>) -> Self {
		Query::Sequence(queries)
	}

	pub fn repetition(inner: Query, min: u32, max: Option<u32>) -> Self {
		Query::Repetition {
			inner: Box::new(inner),
			min,
			max,
		}
	}

	pub fn within(inner: Query, span_layer: impl Into<String>) -> Self {
		Query::Within {
			inner: Box::new(inner),
			span_layer: span_layer.into(),
		}
	}

	pub fn capture(name: impl Into<String>, inner: Query) -> Self {
		Query::Capture {
			name: name.into(),
			inner: Box::new(inner),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn build_simple_query() {
		let query = Query::token(
			TokenPattern::new().with_constraint(Constraint {
				layer: "word".into(),
				op: ConstraintOp::Eq,
				value: ConstraintValue::Literal("house".into()),
			}),
		);

		match query {
			Query::Token(pattern) => {
				assert_eq!(pattern.constraints.len(), 1);
				assert_eq!(pattern.constraints[0].layer, "word");
			}
			_ => panic!("Expected Token"),
		}
	}

	#[test]
	fn build_sequence_query() {
		let det = Query::token(
			TokenPattern::new().with_constraint(Constraint {
				layer: "pos".into(),
				op: ConstraintOp::Eq,
				value: ConstraintValue::Literal("DET".into()),
			}),
		);
		let noun = Query::token(
			TokenPattern::new().with_constraint(Constraint {
				layer: "pos".into(),
				op: ConstraintOp::Eq,
				value: ConstraintValue::Literal("NOUN".into()),
			}),
		);

		let query = Query::sequence(vec![det, noun]);

		match query {
			Query::Sequence(parts) => assert_eq!(parts.len(), 2),
			_ => panic!("Expected Sequence"),
		}
	}
}
