use std::collections::HashSet;

use crate::ast::{ConstraintOp, ConstraintValue, GlobalConstraint, Query, TokenPattern};
use crate::sequence::MAX_QUANTIFIER;
use crate::{QueryError, Result};

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
	GlobalConstraintFilter {
		inner: Box<PlanNode>,
		constraints: Vec<GlobalConstraint>,
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

	// an unlabeled, unquantified parenthesized sequence, equivalent to its own steps
	fn is_plain_group(&self) -> bool {
		self.label.is_none()
			&& self.min == 1
			&& self.max == Some(1)
			&& matches!(self.node, PlanNode::SequenceScan { .. })
	}
}

impl PlanNode {
	/// True when every match of this node is exactly one token.
	pub(crate) fn is_token_level(&self) -> bool {
		match self {
			PlanNode::ScanLiteral { .. } | PlanNode::ScanRegex { .. } | PlanNode::ScanAll => true,
			PlanNode::Intersect(nodes) | PlanNode::Union(nodes) => {
				nodes.iter().all(PlanNode::is_token_level)
			}
			PlanNode::Difference { base, subtract } => {
				base.is_token_level() && subtract.is_token_level()
			}
			_ => false,
		}
	}

	/// True when this node can match the empty span.
	pub(crate) fn is_nullable(&self) -> bool {
		match self {
			PlanNode::SequenceScan { steps } => steps
				.iter()
				.all(|step| step.min == 0 || step.node.is_nullable()),
			PlanNode::Union(nodes) => nodes.iter().any(PlanNode::is_nullable),
			PlanNode::FilterBySpan { inner, .. }
			| PlanNode::FilterByComponent { inner, .. }
			| PlanNode::FilterByDocument { inner, .. } => inner.is_nullable(),
			_ => false,
		}
	}

	/// Labels declared anywhere beneath this node, in query order.
	pub(crate) fn collect_labels(&self, out: &mut Vec<String>) {
		match self {
			PlanNode::SequenceScan { steps } => {
				for step in steps {
					if let Some(label) = &step.label {
						if !out.contains(label) {
							out.push(label.clone());
						}
					}
					step.node.collect_labels(out);
				}
			}
			PlanNode::Intersect(nodes) | PlanNode::Union(nodes) => {
				nodes.iter().for_each(|node| node.collect_labels(out));
			}
			PlanNode::Difference { base, subtract } => {
				base.collect_labels(out);
				subtract.collect_labels(out);
			}
			PlanNode::FilterBySpan { inner, .. }
			| PlanNode::FilterByComponent { inner, .. }
			| PlanNode::FilterByDocument { inner, .. }
			| PlanNode::ProjectAlignment { inner, .. }
			| PlanNode::GlobalConstraintFilter { inner, .. } => inner.collect_labels(out),
			PlanNode::ScanLiteral { .. } | PlanNode::ScanRegex { .. } | PlanNode::ScanAll => {}
		}
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

fn plan_step(query: &Query) -> Result<SequenceStep> {
	let (quantified, min, max) = match query {
		Query::Repetition { inner, min, max } => (inner.as_ref(), *min, *max),
		_ => (query, 1, Some(1)),
	};
	if let Some(bound) = [Some(min), max]
		.into_iter()
		.flatten()
		.find(|&bound| bound > MAX_QUANTIFIER)
	{
		return Err(QueryError::QuantifierTooLarge {
			bound,
			limit: MAX_QUANTIFIER,
		});
	}
	let (label, inner) = extract_label(quantified);
	Ok(SequenceStep {
		node: plan_node(inner)?,
		min,
		max,
		label,
	})
}

fn plan_node(query: &Query) -> Result<PlanNode> {
	match query {
		Query::Token(pattern) => plan_token(pattern),

		Query::Sequence(parts) => {
			let mut steps = Vec::new();
			for part in parts {
				let step = plan_step(part)?;
				if step.is_plain_group() {
					if let PlanNode::SequenceScan { steps: inner } = step.node {
						steps.extend(inner);
					}
				} else {
					steps.push(step);
				}
			}
			Ok(PlanNode::SequenceScan { steps })
		}

		Query::Repetition { .. } => {
			let step = plan_step(query)?;
			if step.is_plain_group() {
				return Ok(step.node);
			}
			Ok(PlanNode::SequenceScan { steps: vec![step] })
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

		Query::Constrained { inner, constraints } => {
			validate_constraint_labels(inner, constraints)?;
			let inner_plan = plan_node(inner)?;
			Ok(PlanNode::GlobalConstraintFilter {
				inner: Box::new(inner_plan),
				constraints: constraints.clone(),
			})
		}
	}
}

fn collect_declared_labels(query: &Query, out: &mut HashSet<String>) {
	match query {
		Query::Capture { name, inner } => {
			out.insert(name.clone());
			collect_declared_labels(inner, out);
		}
		Query::Sequence(parts) => {
			for part in parts {
				collect_declared_labels(part, out);
			}
		}
		Query::Repetition { inner, .. } => collect_declared_labels(inner, out),
		Query::Or(alts) => {
			for alt in alts {
				collect_declared_labels(alt, out);
			}
		}
		Query::Within { inner, .. }
		| Query::WithinComponent { inner, .. }
		| Query::WithinDocument { inner, .. }
		| Query::Project { inner, .. }
		| Query::Constrained { inner, .. } => collect_declared_labels(inner, out),
		Query::Token(_) => {}
	}
}

fn referenced_labels(constraints: &[GlobalConstraint]) -> HashSet<String> {
	let mut out = HashSet::new();
	for c in constraints {
		match c {
			GlobalConstraint::Eq { left, right } | GlobalConstraint::Ne { left, right } => {
				out.insert(left.label.clone());
				out.insert(right.label.clone());
			}
			GlobalConstraint::Distance { left, right, .. } => {
				out.insert(left.clone());
				out.insert(right.clone());
			}
		}
	}
	out
}

fn validate_constraint_labels(inner: &Query, constraints: &[GlobalConstraint]) -> Result<()> {
	let mut declared = HashSet::new();
	collect_declared_labels(inner, &mut declared);
	for label in referenced_labels(constraints) {
		if !declared.contains(&label) {
			return Err(QueryError::UnknownLabel(label));
		}
	}
	Ok(())
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
