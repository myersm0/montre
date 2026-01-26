use crate::ast::Query;
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
	Intersect(Vec<PlanNode>),
	Union(Vec<PlanNode>),
	PositionShift {
		inner: Box<PlanNode>,
		offset: i64,
	},
	FilterBySpan {
		inner: Box<PlanNode>,
		span_layer: String,
	},
	SequenceScan {
		steps: Vec<PlanNode>,
	},
}

#[derive(Debug)]
pub struct QueryPlan {
	pub root: PlanNode,
}

pub fn plan(query: &Query) -> Result<QueryPlan> {
	let root = plan_node(query)?;
	Ok(QueryPlan { root })
}

fn plan_node(query: &Query) -> Result<PlanNode> {
	match query {
		Query::Token(pattern) => {
			if pattern.constraints.is_empty() {
				todo!("Any token scan")
			}

			let mut nodes: Vec<PlanNode> = Vec::new();
			for constraint in &pattern.constraints {
				let node = match &constraint.value {
					crate::ast::ConstraintValue::Literal(v) => PlanNode::ScanLiteral {
						layer: constraint.layer.clone(),
						value: v.clone(),
					},
					crate::ast::ConstraintValue::Regex(r) => PlanNode::ScanRegex {
						layer: constraint.layer.clone(),
						pattern: r.clone(),
					},
				};
				nodes.push(node);
			}

			if nodes.len() == 1 {
				Ok(nodes.remove(0))
			} else {
				Ok(PlanNode::Intersect(nodes))
			}
		}

		Query::Sequence(parts) => {
			let steps: Result<Vec<PlanNode>> = parts.iter().map(plan_node).collect();
			Ok(PlanNode::SequenceScan { steps: steps? })
		}

		Query::Within { inner, span_layer } => {
			let inner_plan = plan_node(inner)?;
			Ok(PlanNode::FilterBySpan {
				inner: Box::new(inner_plan),
				span_layer: span_layer.clone(),
			})
		}

		_ => todo!("Planner not implemented for this query type"),
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
}
