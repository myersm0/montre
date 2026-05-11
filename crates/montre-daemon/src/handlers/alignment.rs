use montre_index::SpanIndex;

use crate::dispatch::RpcContext;
use crate::protocol::error_codes;
use crate::protocol::{
	AlignmentInfo, AlignmentListReply, AlignmentProjectParams, AlignmentProjectReply,
	AlignmentTarget, ProtocolError,
};

pub(crate) fn handle_alignment_list(ctx: &RpcContext) -> Result<serde_json::Value, ProtocolError> {
	let alignments: Vec<AlignmentInfo> = ctx.handle
		.corpus
		.alignments()
		.iter()
		.map(|a| AlignmentInfo {
			name: a.name.clone(),
			source_component: a.source_component.clone(),
			target_component: a.target_component.clone(),
			source_layer: a.source_layer.clone(),
			target_layer: a.target_layer.clone(),
			edge_count: a.edge_count as u32,
		})
		.collect();
	let reply = AlignmentListReply { alignments };
	serde_json::to_value(reply)
		.map_err(|e| ProtocolError::new(-32603, format!("response serialization failed: {}", e)))
}

pub(crate) fn handle_alignment_project(
	params: Option<serde_json::Value>,
	ctx: &RpcContext,
) -> Result<serde_json::Value, ProtocolError> {
	let raw = params
		.ok_or_else(|| ProtocolError::new(-32602, "alignment.project requires params"))?;
	let parsed: AlignmentProjectParams = serde_json::from_value(raw)
		.map_err(|e| ProtocolError::new(-32602, format!("invalid params: {}", e)))?;

	if parsed.source.start > parsed.source.end {
		return Err(ProtocolError::new(-32602, "source.start must be <= source.end"));
	}

	let alignment = ctx.handle.corpus.alignment_meta(&parsed.alignment_name).ok_or_else(|| {
		ProtocolError::new(
			error_codes::ALIGNMENT_NOT_FOUND,
			format!("alignment '{}' not found", parsed.alignment_name),
		)
	})?;

	let source_comp = ctx.handle
		.corpus
		.component(&alignment.source_component)
		.ok_or_else(|| {
			ProtocolError::new(
				-32603,
				format!("source component '{}' not found", alignment.source_component),
			)
		})?;
	let target_comp = ctx.handle
		.corpus
		.component(&alignment.target_component)
		.ok_or_else(|| {
			ProtocolError::new(
				-32603,
				format!("target component '{}' not found", alignment.target_component),
			)
		})?;

	let doc = parsed.source.doc as usize;
	let (src_first_doc, src_last_doc) = source_comp.document_range;
	if doc < src_first_doc || doc >= src_last_doc {
		return Err(ProtocolError::new(
			error_codes::SPAN_OUTSIDE_ALIGNMENT,
			format!(
				"document {} not in alignment source component '{}'",
				parsed.source.doc, alignment.source_component
			),
		));
	}

	let first_sent = ctx.handle
		.corpus
		.first_sentence_of_document(doc)
		.ok_or_else(|| ProtocolError::new(-32603, "source document has no sentences"))?;
	let last_sent = ctx.handle
		.corpus
		.last_sentence_of_document(doc)
		.ok_or_else(|| ProtocolError::new(-32603, "source document has no sentences"))?;
	let sentence_spans = ctx.handle
		.corpus
		.spans()
		.spans("sentence")
		.ok_or_else(|| ProtocolError::new(-32603, "sentence span layer missing"))?;

	let mut overlapping_sents: Vec<u32> = Vec::new();
	for global_idx in first_sent..=last_sent {
		let sent_span = sentence_spans
			.get(global_idx)
			.ok_or_else(|| ProtocolError::new(-32603, "sentence span lookup failed"))?;
		if sent_span.end > parsed.source.start && sent_span.start < parsed.source.end {
			overlapping_sents.push((global_idx - first_sent) as u32);
		}
	}

	let edges = ctx.handle
		.corpus
		.alignment_edges(&parsed.alignment_name)
		.unwrap_or(&[]);
	let doc_within_source = (doc - src_first_doc) as u32;

	let mut targets: Vec<AlignmentTarget> = Vec::new();
	for sent_within_doc in overlapping_sents {
		let source_unit = (doc_within_source, sent_within_doc);
		for edge in edges {
			let (src, tgt) = *edge;
			if src != source_unit {
				continue;
			}
			let target_global_doc =
				(target_comp.document_range.0 as u32).saturating_add(tgt.0);
			let target_first_sent = ctx.handle
				.corpus
				.first_sentence_of_document(target_global_doc as usize)
				.ok_or_else(|| ProtocolError::new(-32603, "target document has no sentences"))?;
			let target_global_sent_idx = target_first_sent + tgt.1 as usize;
			let target_span = sentence_spans
				.get(target_global_sent_idx)
				.cloned()
				.ok_or_else(|| ProtocolError::new(-32603, "target sentence span lookup failed"))?;
			let candidate = AlignmentTarget {
				doc: target_global_doc,
				start: target_span.start,
				end: target_span.end,
			};
			if !targets.contains(&candidate) {
				targets.push(candidate);
			}
		}
	}

	let reply = AlignmentProjectReply { targets };
	serde_json::to_value(reply)
		.map_err(|e| ProtocolError::new(-32603, format!("response serialization failed: {}", e)))
}


#[cfg(test)]
mod tests {
	use super::*;
	use crate::dispatch::dispatch_request;
	use crate::dispatch::test_support::{find_doc_index, with_registered_context};
	use crate::protocol::TextSentenceReply;

	#[test]
	fn alignment_list_returns_known_alignments() {
		with_registered_context(|ctx| {
			let result = dispatch_request("alignment.list", None, ctx).unwrap();
			let reply: AlignmentListReply = serde_json::from_value(result).unwrap();

			assert_eq!(reply.alignments.len(), 1);
			let alignment = &reply.alignments[0];
			assert_eq!(alignment.name, "sentence");
			assert_eq!(alignment.source_component, "fr");
			assert_eq!(alignment.target_component, "en");
			assert_eq!(alignment.source_layer, "sentence");
			assert_eq!(alignment.target_layer, "sentence");
			assert_eq!(alignment.edge_count, 4);
		});
	}

	#[test]
	fn alignment_project_returns_target_for_known_pair() {
		with_registered_context(|ctx| {
			let source_doc = find_doc_index(&ctx.handle.corpus, "la_maison");
			let sent_params = serde_json::json!({ "doc": source_doc, "sent": 0 });
			let sent_result = dispatch_request("text.sentence", Some(sent_params), ctx).unwrap();
			let source_sent: TextSentenceReply = serde_json::from_value(sent_result).unwrap();

			let params = serde_json::json!({
				"source": {
					"doc": source_doc,
					"start": source_sent.span.start,
					"end": source_sent.span.end,
				},
				"alignment_name": "sentence",
			});
			let result = dispatch_request("alignment.project", Some(params), ctx).unwrap();
			let reply: AlignmentProjectReply = serde_json::from_value(result).unwrap();

			assert_eq!(reply.targets.len(), 1);
			let expected_target_doc = find_doc_index(&ctx.handle.corpus, "the_house");
			assert_eq!(reply.targets[0].doc, expected_target_doc);
			assert!(reply.targets[0].end > reply.targets[0].start);
		});
	}

	#[test]
	fn alignment_project_unknown_alignment_rejected() {
		with_registered_context(|ctx| {
			let source_doc = find_doc_index(&ctx.handle.corpus, "la_maison");
			let params = serde_json::json!({
				"source": { "doc": source_doc, "start": 0, "end": 5 },
				"alignment_name": "totally-not-an-alignment",
			});
			let err = dispatch_request("alignment.project", Some(params), ctx).unwrap_err();
			assert_eq!(err.code, error_codes::ALIGNMENT_NOT_FOUND);
		});
	}

	#[test]
	fn alignment_project_source_outside_alignment_rejected() {
		with_registered_context(|ctx| {
			let target_side_doc = find_doc_index(&ctx.handle.corpus, "the_house");
			let params = serde_json::json!({
				"source": { "doc": target_side_doc, "start": 0, "end": 5 },
				"alignment_name": "sentence",
			});
			let err = dispatch_request("alignment.project", Some(params), ctx).unwrap_err();
			assert_eq!(err.code, error_codes::SPAN_OUTSIDE_ALIGNMENT);
		});
	}
}
