use montre_core::LayerKind;
use montre_index::{Corpus, InvertedIndex, SpanIndex};

use crate::ast::GlobalConstraint;
use crate::planner::PlanNode;
use crate::{QueryError, Result};

/// Reject names the corpus does not have, so that a typo is an error and not
/// an empty result.
pub(crate) fn validate(node: &PlanNode, corpus: &Corpus) -> Result<()> {
	match node {
		PlanNode::ScanLiteral { layer, .. } | PlanNode::ScanRegex { layer, .. } => {
			token_layer(layer, corpus)
		}
		PlanNode::ScanAll => Ok(()),
		PlanNode::Intersect(nodes) | PlanNode::Union(nodes) => {
			nodes.iter().try_for_each(|node| validate(node, corpus))
		}
		PlanNode::Difference { base, subtract } => {
			validate(base, corpus)?;
			validate(subtract, corpus)
		}
		PlanNode::SequenceScan { steps } => steps
			.iter()
			.try_for_each(|step| validate(&step.node, corpus)),
		PlanNode::FilterBySpan { inner, span_layer } => {
			let known = corpus.spans().spans(span_layer).is_some()
				|| corpus.span_layers().contains(span_layer);
			if !known {
				return Err(QueryError::UnknownSpanLayer(span_layer.clone()));
			}
			validate(inner, corpus)
		}
		PlanNode::FilterByComponent { inner, components } => {
			if let Some(name) = components.iter().find(|name| corpus.component(name).is_none()) {
				return Err(QueryError::UnknownComponent(name.clone()));
			}
			validate(inner, corpus)
		}
		PlanNode::FilterByDocument { inner, documents } => {
			let unknown = documents
				.iter()
				.find(|name| corpus.document_index_by_name(name).is_none());
			if let Some(name) = unknown {
				return Err(QueryError::UnknownDocument(name.clone()));
			}
			validate(inner, corpus)
		}
		PlanNode::ProjectAlignment { inner, alignment } => {
			if corpus.alignment_meta(alignment).is_none() {
				return Err(QueryError::Execution(format!(
					"Alignment not found: {alignment}"
				)));
			}
			validate(inner, corpus)
		}
		PlanNode::GlobalConstraintFilter { inner, constraints } => {
			for constraint in constraints {
				if let GlobalConstraint::Eq { left, right } | GlobalConstraint::Ne { left, right } =
					constraint
				{
					compared_layer(&left.attr, corpus)?;
					compared_layer(&right.attr, corpus)?;
				}
			}
			validate(inner, corpus)
		}
	}
}

fn token_layer(layer: &str, corpus: &Corpus) -> Result<()> {
	if corpus.inverted().value_count(layer).is_some() {
		Ok(())
	} else if corpus.is_forward_only(layer) || corpus.layer_kind(layer) == Some(LayerKind::Int) {
		Err(QueryError::UnindexedLayer(layer.to_string()))
	} else if corpus.layers().iter().any(|known| known == layer) {
		Ok(())
	} else {
		Err(QueryError::UnknownLayer(layer.to_string()))
	}
}

fn compared_layer(layer: &str, corpus: &Corpus) -> Result<()> {
	match corpus.layer_kind(layer) {
		Some(LayerKind::String) => Ok(()),
		Some(_) => Err(QueryError::Execution(format!(
			"Layer '{layer}' does not hold strings and cannot be compared in a constraint"
		))),
		None if corpus.layers().iter().any(|known| known == layer) => Ok(()),
		None => Err(QueryError::UnknownLayer(layer.to_string())),
	}
}
