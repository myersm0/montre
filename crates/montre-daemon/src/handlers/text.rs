use montre_index::{Corpus, ForwardIndex, SpanIndex};

use crate::dispatch::{parse_params, serialize_reply, RpcContext};
use crate::handlers::corpus::{document_component, document_sentence_count};
use crate::protocol::{
	AnnotationEntry, AnnotationRow, AnnotationValue, ProtocolError, SentenceEntry, Span,
	TextAnnotationsParams, TextAnnotationsRangeParams, TextAnnotationsRangeReply,
	TextAnnotationsReply, TextDocumentParams, TextDocumentReply, TextSentenceParams,
	TextSentenceReply, TextSentencesParams, TextSentencesReply, TextSurfaceParams,
	TextSurfaceReply,
};

fn fetch_annotation(corpus: &Corpus, position: u64, layer: &str) -> Option<AnnotationValue> {
	match corpus.layer_kind(layer)? {
		montre_index::LayerKind::String => corpus
			.forward()
			.get_str(position, layer)
			.map(|s| AnnotationValue::String(s.to_string())),
		montre_index::LayerKind::Int => corpus
			.forward()
			.get_int(position, layer)
			.map(AnnotationValue::Int),
		other => {
			tracing::error!(
				layer,
				?other,
				"unknown montre_index::LayerKind variant in fetch_annotation",
			);
			debug_assert!(
				false,
				"unknown montre_index::LayerKind variant {:?} for layer {}",
				other, layer,
			);
			None
		}
	}
}

fn sentence_span_in_doc(corpus: &Corpus, doc: u32, sent: u32) -> Option<(usize, Span)> {
	let doc_idx = doc as usize;
	let first = corpus.first_sentence_of_document(doc_idx)?;
	let last = corpus.last_sentence_of_document(doc_idx)?;
	let global_idx = first.checked_add(sent as usize)?;
	if global_idx > last {
		return None;
	}
	let sentence_spans = corpus.spans().spans("sentence")?;
	let span = sentence_spans.get(global_idx).cloned()?;
	Some((global_idx, span))
}

fn document_count(corpus: &Corpus) -> usize {
	corpus.document_names().len()
}

pub(crate) fn handle_text_surface(
	params: Option<serde_json::Value>,
	ctx: &RpcContext,
) -> Result<serde_json::Value, ProtocolError> {
	let parsed: TextSurfaceParams = parse_params("text.surface", params)?;

	if parsed.start > parsed.end {
		return Err(ProtocolError::new(-32602, "start must be <= end"));
	}
	let token_count = ctx.handle.corpus.token_count();
	let start = parsed.start.min(token_count);
	let end = parsed.end.min(token_count);

	serialize_reply(TextSurfaceReply {
		surface: ctx.handle.corpus.surface_text(start, end),
	})
}

pub(crate) fn handle_text_sentence(
	params: Option<serde_json::Value>,
	ctx: &RpcContext,
) -> Result<serde_json::Value, ProtocolError> {
	let parsed: TextSentenceParams = parse_params("text.sentence", params)?;

	let (global_idx, span) =
		sentence_span_in_doc(&ctx.handle.corpus, parsed.doc, parsed.sent).ok_or_else(|| {
			ProtocolError::new(
				-32602,
				format!(
					"sentence {} not found in document {}",
					parsed.sent, parsed.doc
				),
			)
		})?;

	serialize_reply(TextSentenceReply {
		surface: ctx.handle.corpus.surface_text(span.start, span.end),
		sentence_id: ctx.handle
			.corpus
			.sentence_id(global_idx)
			.map(String::from)
			.unwrap_or_default(),
		span,
	})
}

pub(crate) fn handle_text_sentences(
	params: Option<serde_json::Value>,
	ctx: &RpcContext,
) -> Result<serde_json::Value, ProtocolError> {
	let parsed: TextSentencesParams = parse_params("text.sentences", params)?;

	if parsed.sent_start > parsed.sent_end {
		return Err(ProtocolError::new(-32602, "sent_start must be <= sent_end"));
	}

	let doc_idx = parsed.doc as usize;
	if doc_idx >= document_count(&ctx.handle.corpus) {
		return Err(ProtocolError::new(
			-32602,
			format!("document index {} out of range", parsed.doc),
		));
	}

	let first = ctx.handle
		.corpus
		.first_sentence_of_document(doc_idx)
		.ok_or_else(|| ProtocolError::new(-32602, "document has no sentences"))?;
	let last = ctx.handle
		.corpus
		.last_sentence_of_document(doc_idx)
		.ok_or_else(|| ProtocolError::new(-32602, "document has no sentences"))?;
	let doc_sentence_count = (last - first + 1) as u32;
	let sent_start = parsed.sent_start.min(doc_sentence_count);
	let sent_end = parsed.sent_end.min(doc_sentence_count);

	let sentence_spans = ctx.handle
		.corpus
		.spans()
		.spans("sentence")
		.ok_or_else(|| ProtocolError::new(-32603, "sentence span layer missing"))?;

	let mut sentences = Vec::with_capacity((sent_end - sent_start) as usize);
	for sent in sent_start..sent_end {
		let global_idx = first + sent as usize;
		let span = sentence_spans
			.get(global_idx)
			.cloned()
			.ok_or_else(|| ProtocolError::new(-32603, "sentence span lookup failed"))?;
		sentences.push(SentenceEntry {
			sent,
			surface: ctx.handle.corpus.surface_text(span.start, span.end),
			sentence_id: ctx.handle
				.corpus
				.sentence_id(global_idx)
				.map(String::from)
				.unwrap_or_default(),
			span,
		});
	}

	serialize_reply(TextSentencesReply { sentences })
}

pub(crate) fn handle_text_document(
	params: Option<serde_json::Value>,
	ctx: &RpcContext,
) -> Result<serde_json::Value, ProtocolError> {
	let parsed: TextDocumentParams = parse_params("text.document", params)?;

	let doc_idx = parsed.doc as usize;
	let names = ctx.handle.corpus.document_names();
	let name = names.get(doc_idx).cloned().ok_or_else(|| {
		ProtocolError::new(-32602, format!("document index {} out of range", parsed.doc))
	})?;

	let document_spans = ctx.handle
		.corpus
		.spans()
		.spans("document")
		.ok_or_else(|| ProtocolError::new(-32603, "document span layer missing"))?;
	let span = document_spans
		.get(doc_idx)
		.cloned()
		.ok_or_else(|| ProtocolError::new(-32603, "document span lookup failed"))?;

	serialize_reply(TextDocumentReply {
		index: parsed.doc,
		name,
		component: document_component(ctx, doc_idx),
		span,
		sentence_count: document_sentence_count(ctx, doc_idx),
	})
}

pub(crate) fn handle_text_annotations(
	params: Option<serde_json::Value>,
	ctx: &RpcContext,
) -> Result<serde_json::Value, ProtocolError> {
	let parsed: TextAnnotationsParams = parse_params("text.annotations", params)?;

	let mut values = Vec::new();
	for &position in &parsed.positions {
		for layer in &parsed.layers {
			if let Some(value) = fetch_annotation(&ctx.handle.corpus, position, layer) {
				values.push(AnnotationEntry {
					position,
					layer: layer.clone(),
					value,
				});
			}
		}
	}

	serialize_reply(TextAnnotationsReply { values })
}

pub(crate) fn handle_text_annotations_range(
	params: Option<serde_json::Value>,
	ctx: &RpcContext,
) -> Result<serde_json::Value, ProtocolError> {
	let parsed: TextAnnotationsRangeParams = parse_params("text.annotations_range", params)?;

	if parsed.start > parsed.end {
		return Err(ProtocolError::new(-32602, "start must be <= end"));
	}
	let token_count = ctx.handle.corpus.token_count();
	let start = parsed.start.min(token_count);
	let end = parsed.end.min(token_count);

	let layers: Vec<String> = match parsed.layers {
		Some(ls) => ls,
		None => ctx.handle.corpus.layers().to_vec(),
	};

	let mut rows = Vec::with_capacity((end - start) as usize);
	for position in start..end {
		let mut values = std::collections::HashMap::new();
		for layer in &layers {
			if let Some(value) = fetch_annotation(&ctx.handle.corpus, position, layer) {
				values.insert(layer.clone(), value);
			}
		}
		rows.push(AnnotationRow { position, values });
	}

	serialize_reply(TextAnnotationsRangeReply { rows })
}


#[cfg(test)]
mod tests {
	use super::*;
	use crate::dispatch::dispatch_request;
	use crate::dispatch::test_support::{find_doc_index, with_registered_context};

	#[test]
	fn text_surface_returns_text_for_range() {
		with_registered_context(|ctx| {
			let params = serde_json::json!({ "start": 0, "end": 4 });
			let result = dispatch_request("text.surface", Some(params), ctx).unwrap();
			let reply: TextSurfaceReply = serde_json::from_value(result).unwrap();
			assert!(!reply.surface.is_empty());
		});
	}

	#[test]
	fn text_surface_rejects_inverted_range() {
		with_registered_context(|ctx| {
			let params = serde_json::json!({ "start": 10, "end": 5 });
			let err = dispatch_request("text.surface", Some(params), ctx).unwrap_err();
			assert_eq!(err.code, -32602);
		});
	}

	#[test]
	fn text_surface_clamps_end_past_token_count() {
		with_registered_context(|ctx| {
			let token_count = ctx.handle.corpus.token_count();
			let params = serde_json::json!({ "start": 0, "end": token_count + 100 });
			let result = dispatch_request("text.surface", Some(params), ctx).unwrap();
			let reply: TextSurfaceReply = serde_json::from_value(result).unwrap();
			assert!(!reply.surface.is_empty());

			let in_range_params = serde_json::json!({ "start": 0, "end": token_count });
			let in_range = dispatch_request("text.surface", Some(in_range_params), ctx).unwrap();
			let in_range_reply: TextSurfaceReply = serde_json::from_value(in_range).unwrap();
			assert_eq!(reply.surface, in_range_reply.surface);
		});
	}

	#[test]
	fn text_surface_empty_range_returns_empty_surface() {
		with_registered_context(|ctx| {
			let params = serde_json::json!({ "start": 3, "end": 3 });
			let result = dispatch_request("text.surface", Some(params), ctx).unwrap();
			let reply: TextSurfaceReply = serde_json::from_value(result).unwrap();
			assert!(reply.surface.is_empty());
		});
	}

	#[test]
	fn text_surface_start_past_token_count_clamps_to_empty() {
		with_registered_context(|ctx| {
			let token_count = ctx.handle.corpus.token_count();
			let params = serde_json::json!({ "start": token_count + 50, "end": token_count + 100 });
			let result = dispatch_request("text.surface", Some(params), ctx).unwrap();
			let reply: TextSurfaceReply = serde_json::from_value(result).unwrap();
			assert!(reply.surface.is_empty());
		});
	}

	#[test]
	fn text_sentence_returns_sentence_data() {
		with_registered_context(|ctx| {
			let doc = find_doc_index(&ctx.handle.corpus, "la_maison");
			let params = serde_json::json!({ "doc": doc, "sent": 0 });
			let result = dispatch_request("text.sentence", Some(params), ctx).unwrap();
			let reply: TextSentenceReply = serde_json::from_value(result).unwrap();
			assert_eq!(reply.sentence_id, "1");
			assert!(reply.span.end > reply.span.start);
			assert!(reply.surface.contains("La"));
		});
	}

	#[test]
	fn text_sentence_out_of_range_rejected() {
		with_registered_context(|ctx| {
			let params = serde_json::json!({ "doc": 999, "sent": 0 });
			let err = dispatch_request("text.sentence", Some(params), ctx).unwrap_err();
			assert_eq!(err.code, -32602);
		});
	}

	#[test]
	fn text_sentences_returns_range() {
		with_registered_context(|ctx| {
			let params = serde_json::json!({ "doc": 0, "sent_start": 0, "sent_end": 2 });
			let result = dispatch_request("text.sentences", Some(params), ctx).unwrap();
			let reply: TextSentencesReply = serde_json::from_value(result).unwrap();
			assert_eq!(reply.sentences.len(), 2);
			assert_eq!(reply.sentences[0].sent, 0);
			assert_eq!(reply.sentences[1].sent, 1);
			assert!(reply.sentences[0].span.end <= reply.sentences[1].span.start);
		});
	}

	#[test]
	fn text_sentences_clamps_sent_end_to_doc_count() {
		with_registered_context(|ctx| {
			let doc = find_doc_index(&ctx.handle.corpus, "la_maison");
			let count_params = serde_json::json!({ "doc": doc });
			let count_result = dispatch_request("text.document", Some(count_params), ctx).unwrap();
			let document_reply: TextDocumentReply = serde_json::from_value(count_result).unwrap();
			let count = document_reply.sentence_count;

			let params = serde_json::json!({ "doc": doc, "sent_start": 0, "sent_end": 999 });
			let result = dispatch_request("text.sentences", Some(params), ctx).unwrap();
			let reply: TextSentencesReply = serde_json::from_value(result).unwrap();
			assert_eq!(reply.sentences.len() as u32, count);
		});
	}

	#[test]
	fn text_sentences_empty_range_after_clamp_returns_empty() {
		with_registered_context(|ctx| {
			let doc = find_doc_index(&ctx.handle.corpus, "la_maison");
			let params = serde_json::json!({ "doc": doc, "sent_start": 999, "sent_end": 999 });
			let result = dispatch_request("text.sentences", Some(params), ctx).unwrap();
			let reply: TextSentencesReply = serde_json::from_value(result).unwrap();
			assert!(reply.sentences.is_empty());
		});
	}

	#[test]
	fn text_sentences_nonexistent_doc_rejected() {
		with_registered_context(|ctx| {
			let params = serde_json::json!({ "doc": 999, "sent_start": 0, "sent_end": 1 });
			let err = dispatch_request("text.sentences", Some(params), ctx).unwrap_err();
			assert_eq!(err.code, -32602);
		});
	}

	#[test]
	fn text_sentences_inverted_range_rejected() {
		with_registered_context(|ctx| {
			let params = serde_json::json!({ "doc": 0, "sent_start": 5, "sent_end": 1 });
			let err = dispatch_request("text.sentences", Some(params), ctx).unwrap_err();
			assert_eq!(err.code, -32602);
		});
	}

	#[test]
	fn text_document_returns_metadata() {
		with_registered_context(|ctx| {
			let doc = find_doc_index(&ctx.handle.corpus, "la_maison");
			let params = serde_json::json!({ "doc": doc });
			let result = dispatch_request("text.document", Some(params), ctx).unwrap();
			let reply: TextDocumentReply = serde_json::from_value(result).unwrap();
			assert_eq!(reply.index, doc);
			assert!(reply.name.contains("la_maison"));
			assert_eq!(reply.component, "fr");
			assert_eq!(reply.sentence_count, 2);
			assert!(reply.span.end > reply.span.start);
		});
	}

	#[test]
	fn text_document_out_of_range_rejected() {
		with_registered_context(|ctx| {
			let params = serde_json::json!({ "doc": 999 });
			let err = dispatch_request("text.document", Some(params), ctx).unwrap_err();
			assert_eq!(err.code, -32602);
		});
	}

	#[test]
	fn text_annotations_returns_values_at_positions() {
		with_registered_context(|ctx| {
			let doc = find_doc_index(&ctx.handle.corpus, "la_maison");
			let sent_params = serde_json::json!({ "doc": doc, "sent": 0 });
			let sent_result = dispatch_request("text.sentence", Some(sent_params), ctx).unwrap();
			let sentence: TextSentenceReply = serde_json::from_value(sent_result).unwrap();
			let position = sentence.span.start;

			let params = serde_json::json!({
				"positions": [position],
				"layers": ["upos", "lemma"],
			});
			let result = dispatch_request("text.annotations", Some(params), ctx).unwrap();
			let reply: TextAnnotationsReply = serde_json::from_value(result).unwrap();
			assert_eq!(reply.values.len(), 2);

			let upos = reply
				.values
				.iter()
				.find(|e| e.layer == "upos")
				.expect("upos entry");
			assert_eq!(upos.value, AnnotationValue::String("DET".to_string()));

			let lemma = reply
				.values
				.iter()
				.find(|e| e.layer == "lemma")
				.expect("lemma entry");
			assert_eq!(lemma.value, AnnotationValue::String("le".to_string()));
		});
	}

	#[test]
	fn text_annotations_skips_missing_values() {
		with_registered_context(|ctx| {
			let params = serde_json::json!({
				"positions": [0],
				"layers": ["totally-not-a-layer"],
			});
			let result = dispatch_request("text.annotations", Some(params), ctx).unwrap();
			let reply: TextAnnotationsReply = serde_json::from_value(result).unwrap();
			assert_eq!(reply.values.len(), 0);
		});
	}

	#[test]
	fn text_annotations_range_with_explicit_layers() {
		with_registered_context(|ctx| {
			let params = serde_json::json!({
				"start": 0,
				"end": 5,
				"layers": ["upos"],
			});
			let result = dispatch_request("text.annotations_range", Some(params), ctx).unwrap();
			let reply: TextAnnotationsRangeReply = serde_json::from_value(result).unwrap();
			assert_eq!(reply.rows.len(), 5);
			assert_eq!(reply.rows[0].position, 0);
			assert_eq!(
				reply.rows[0].values.get("upos"),
				Some(&AnnotationValue::String("DET".to_string()))
			);
		});
	}

	#[test]
	fn text_annotations_range_with_default_layers() {
		with_registered_context(|ctx| {
			let params = serde_json::json!({ "start": 0, "end": 2 });
			let result = dispatch_request("text.annotations_range", Some(params), ctx).unwrap();
			let reply: TextAnnotationsRangeReply = serde_json::from_value(result).unwrap();
			assert_eq!(reply.rows.len(), 2);
			assert!(reply.rows[0].values.contains_key("upos"));
			assert!(reply.rows[0].values.contains_key("lemma"));
		});
	}

	#[test]
	fn text_annotations_range_serializes_head_as_int() {
		with_registered_context(|ctx| {
			let params = serde_json::json!({
				"start": 0,
				"end": 1,
				"layers": ["head"],
			});
			let result = dispatch_request("text.annotations_range", Some(params), ctx).unwrap();
			let reply: TextAnnotationsRangeReply = serde_json::from_value(result).unwrap();
			assert_eq!(reply.rows.len(), 1);
			match reply.rows[0].values.get("head") {
				Some(AnnotationValue::Int(_)) => {}
				other => panic!("expected Int variant, got {:?}", other),
			}
		});
	}

	#[test]
	fn text_annotations_range_rejects_inverted_range() {
		with_registered_context(|ctx| {
			let params = serde_json::json!({ "start": 10, "end": 5 });
			let err = dispatch_request("text.annotations_range", Some(params), ctx).unwrap_err();
			assert_eq!(err.code, -32602);
		});
	}

	#[test]
	fn text_annotations_range_clamps_end_past_token_count() {
		with_registered_context(|ctx| {
			let token_count = ctx.handle.corpus.token_count();
			let params = serde_json::json!({
				"start": 0,
				"end": token_count + 100,
				"layers": ["upos"],
			});
			let result = dispatch_request("text.annotations_range", Some(params), ctx).unwrap();
			let reply: TextAnnotationsRangeReply = serde_json::from_value(result).unwrap();
			assert_eq!(reply.rows.len() as u64, token_count);
		});
	}

	#[test]
	fn text_annotations_range_empty_after_clamp_returns_no_rows() {
		with_registered_context(|ctx| {
			let token_count = ctx.handle.corpus.token_count();
			let params = serde_json::json!({
				"start": token_count + 10,
				"end": token_count + 20,
				"layers": ["upos"],
			});
			let result = dispatch_request("text.annotations_range", Some(params), ctx).unwrap();
			let reply: TextAnnotationsRangeReply = serde_json::from_value(result).unwrap();
			assert!(reply.rows.is_empty());
		});
	}

	#[test]
	fn text_annotations_empty_positions_returns_empty_values() {
		with_registered_context(|ctx| {
			let params = serde_json::json!({
				"positions": [],
				"layers": ["upos"],
			});
			let result = dispatch_request("text.annotations", Some(params), ctx).unwrap();
			let reply: TextAnnotationsReply = serde_json::from_value(result).unwrap();
			assert!(reply.values.is_empty());
		});
	}

	#[test]
	fn text_annotations_out_of_range_position_omitted() {
		with_registered_context(|ctx| {
			let token_count = ctx.handle.corpus.token_count();
			let params = serde_json::json!({
				"positions": [0, token_count + 5],
				"layers": ["upos"],
			});
			let result = dispatch_request("text.annotations", Some(params), ctx).unwrap();
			let reply: TextAnnotationsReply = serde_json::from_value(result).unwrap();
			assert!(reply.values.iter().all(|e| e.position == 0));
		});
	}
}
