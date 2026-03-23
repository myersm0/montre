use crate::ast::{ConstraintOp, ConstraintValue, Query, TokenPattern};
use crate::Result;

#[derive(Debug, Clone)]
pub enum PlanNode {
	ScanLiteral {
		layer: String,
		value: String,
	},
	ScanRegex {
		layer: String,
		pattern: String,
	},
	ScanAll,
	Intersect(Vec<PlanNode>),
	Union(Vec<PlanNode>),
	Difference {
		base: Box<PlanNode>,
		subtract: Box<PlanNode>,
	},
	FilterBySpan {
		inner: Box<PlanNode>,
		span_layer: String,
	},
	FilterByComponent {
		inner: Box<PlanNode>,
		components: Vec<String>,
	},
	FilterByDocument {
		inner: Box<PlanNode>,
		documents: Vec<String>,
	},
	ProjectAlignment {
		inner: Box<PlanNode>,
		alignment: String,
	},
	SequenceScan {
		steps: Vec<SequenceStep>,
	},
}

#[derive(Debug, Clone)]
pub struct SequenceStep {
	pub node: PlanNode,
	pub min: u32,
	pub max: Option<u32>,
	pub label: Option<String>,
}

impl SequenceStep {
	pub fn once(node: PlanNode) -> Self {
		Self {
			node,
			min: 1,
			max: Some(1),
			label: None,
		}
	}

	pub fn repeated(node: PlanNode, min: u32, max: Option<u32>) -> Self {
		Self { node, min, max, label: None }
	}
}

#[derive(Debug)]
pub struct QueryPlan {
	pub root: PlanNode,
}

pub fn plan(query: &Query) -> Result<QueryPlan> {
	let root = plan_node(query)?;
	Ok(QueryPlan { root })
}

fn extract_label(query: &Query) -> (Option<String>, &Query) {
	match query {
		Query::Capture { name, inner } => (Some(name.clone()), inner),
		_ => (None, query),
	}
}

fn plan_node(query: &Query) -> Result<PlanNode> {
	match query {
		Query::Token(pattern) => plan_token(pattern),

		Query::Sequence(parts) => {
			let mut steps = Vec::new();
			for q in parts {
				match q {
					Query::Repetition { inner, min, max } => {
						let (label, actual_inner) = extract_label(inner);
						let inner_plan = plan_node(actual_inner)?;
						steps.push(SequenceStep {
							node: inner_plan,
							min: *min,
							max: *max,
							label,
						});
					}
					_ => {
						let (label, actual) = extract_label(q);
						let node = plan_node(actual)?;
						steps.push(SequenceStep {
							node,
							min: 1,
							max: Some(1),
							label,
						});
					}
				}
			}
			Ok(PlanNode::SequenceScan { steps })
		}

		Query::Repetition { inner, min, max } => {
			let (label, actual_inner) = extract_label(inner);
			let inner_plan = plan_node(actual_inner)?;
			Ok(PlanNode::SequenceScan {
				steps: vec![SequenceStep {
					node: inner_plan,
					min: *min,
					max: *max,
					label,
				}],
			})
		}

		Query::Or(alternatives) => {
			let nodes: Result<Vec<PlanNode>> = alternatives.iter().map(plan_node).collect();
			Ok(PlanNode::Union(nodes?))
		}

		Query::Within { inner, span_layer } => {
			let inner_plan = plan_node(inner)?;
			Ok(PlanNode::FilterBySpan {
				inner: Box::new(inner_plan),
				span_layer: span_layer.clone(),
			})
		}

		Query::Containing { inner, span_layer } => {
			let inner_plan = plan_node(inner)?;
			Ok(PlanNode::FilterBySpan {
				inner: Box::new(inner_plan),
				span_layer: span_layer.clone(),
			})
		}

		Query::Capture { name, inner } => {
			let inner_plan = plan_node(inner)?;
			Ok(PlanNode::SequenceScan {
				steps: vec![SequenceStep {
					node: inner_plan,
					min: 1,
					max: Some(1),
					label: Some(name.clone()),
				}],
			})
		}

		Query::WithinComponent { inner, components } => {
			let inner_plan = plan_node(inner)?;
			Ok(PlanNode::FilterByComponent {
				inner: Box::new(inner_plan),
				components: components.clone(),
			})
		}

		Query::WithinDocument { inner, documents } => {
			let inner_plan = plan_node(inner)?;
			Ok(PlanNode::FilterByDocument {
				inner: Box::new(inner_plan),
				documents: documents.clone(),
			})
		}

		Query::Project { inner, alignment } => {
			let inner_plan = plan_node(inner)?;
			Ok(PlanNode::ProjectAlignment {
				inner: Box::new(inner_plan),
				alignment: alignment.clone(),
			})
		}
	}
}

fn plan_token(pattern: &TokenPattern) -> Result<PlanNode> {
	if pattern.constraints.is_empty() {
		return Ok(PlanNode::ScanAll);
	}

	let mut positive_nodes = Vec::new();
	let mut negative_nodes = Vec::new();

	for constraint in &pattern.constraints {
		let scan_node = match &constraint.value {
			ConstraintValue::Literal(v) => PlanNode::ScanLiteral {
				layer: constraint.layer.clone(),
				value: v.clone(),
			},
			ConstraintValue::Regex(r) => PlanNode::ScanRegex {
				layer: constraint.layer.clone(),
				pattern: r.clone(),
			},
		};

		match constraint.op {
			ConstraintOp::Eq => positive_nodes.push(scan_node),
			ConstraintOp::Ne => negative_nodes.push(scan_node),
		}
	}

	let positive = if positive_nodes.is_empty() {
		PlanNode::ScanAll
	} else if positive_nodes.len() == 1 {
		positive_nodes.remove(0)
	} else {
		PlanNode::Intersect(positive_nodes)
	};

	if negative_nodes.is_empty() {
		Ok(positive)
	} else {
		let negative = if negative_nodes.len() == 1 {
			negative_nodes.remove(0)
		} else {
			PlanNode::Union(negative_nodes)
		};

		Ok(PlanNode::Difference {
			base: Box::new(positive),
			subtract: Box::new(negative),
		})
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::ast::{Constraint, ConstraintOp, ConstraintValue, TokenPattern};

	#[test]
	fn plan_simple_token() {
		let query = Query::Token(TokenPattern::new().with_constraint(Constraint {
			layer: "word".into(),
			op: ConstraintOp::Eq,
			value: ConstraintValue::Literal("test".into()),
		}));

		let plan = plan(&query).unwrap();
		match plan.root {
			PlanNode::ScanLiteral { layer, value } => {
				assert_eq!(layer, "word");
				assert_eq!(value, "test");
			}
			_ => panic!("Expected ScanLiteral"),
		}
	}

	#[test]
	fn plan_matchall() {
		let query = Query::Token(TokenPattern::any());
		let plan = plan(&query).unwrap();
		assert!(matches!(plan.root, PlanNode::ScanAll));
	}

	#[test]
	fn plan_negation() {
		let query = Query::Token(TokenPattern::new().with_constraint(Constraint {
			layer: "pos".into(),
			op: ConstraintOp::Ne,
			value: ConstraintValue::Literal("PUNCT".into()),
		}));

		let plan = plan(&query).unwrap();
		match plan.root {
			PlanNode::Difference { base, subtract } => {
				assert!(matches!(*base, PlanNode::ScanAll));
				assert!(matches!(*subtract, PlanNode::ScanLiteral { .. }));
			}
			_ => panic!("Expected Difference"),
		}
	}

	#[test]
	fn plan_conjunction_with_negation() {
		let query = Query::Token(
			TokenPattern::new()
				.with_constraint(Constraint {
					layer: "pos".into(),
					op: ConstraintOp::Eq,
					value: ConstraintValue::Literal("NOUN".into()),
				})
				.with_constraint(Constraint {
					layer: "word".into(),
					op: ConstraintOp::Ne,
					value: ConstraintValue::Literal("house".into()),
				}),
		);

		let plan = plan(&query).unwrap();
		match plan.root {
			PlanNode::Difference { base, subtract } => {
				assert!(matches!(*base, PlanNode::ScanLiteral { .. }));
				assert!(matches!(*subtract, PlanNode::ScanLiteral { .. }));
			}
			_ => panic!("Expected Difference"),
		}
	}

	#[test]
	fn plan_alternation() {
		let query = Query::Or(vec![
			Query::Token(TokenPattern::new().with_constraint(Constraint {
				layer: "pos".into(),
				op: ConstraintOp::Eq,
				value: ConstraintValue::Literal("NOUN".into()),
			})),
			Query::Token(TokenPattern::new().with_constraint(Constraint {
				layer: "pos".into(),
				op: ConstraintOp::Eq,
				value: ConstraintValue::Literal("VERB".into()),
			})),
		]);

		let plan = plan(&query).unwrap();
		match plan.root {
			PlanNode::Union(nodes) => {
				assert_eq!(nodes.len(), 2);
			}
			_ => panic!("Expected Union"),
		}
	}

	#[test]
	fn plan_repetition() {
		let query = Query::Repetition {
			inner: Box::new(Query::Token(TokenPattern::new().with_constraint(Constraint {
				layer: "pos".into(),
				op: ConstraintOp::Eq,
				value: ConstraintValue::Literal("ADJ".into()),
			}))),
			min: 1,
			max: None,
		};

		let plan = plan(&query).unwrap();
		match plan.root {
			PlanNode::SequenceScan { steps } => {
				assert_eq!(steps.len(), 1);
				assert_eq!(steps[0].min, 1);
				assert_eq!(steps[0].max, None);
			}
			_ => panic!("Expected SequenceScan"),
		}
	}

	#[test]
	fn plan_within() {
		let query = Query::Within {
			inner: Box::new(Query::Token(TokenPattern::new().with_constraint(Constraint {
				layer: "pos".into(),
				op: ConstraintOp::Eq,
				value: ConstraintValue::Literal("NOUN".into()),
			}))),
			span_layer: "s".into(),
		};

		let plan = plan(&query).unwrap();
		match plan.root {
			PlanNode::FilterBySpan { span_layer, .. } => {
				assert_eq!(span_layer, "s");
			}
			_ => panic!("Expected FilterBySpan"),
		}
	}
}
