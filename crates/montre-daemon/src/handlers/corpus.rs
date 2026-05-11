use montre_index::{Corpus, InvertedIndex};

use crate::dispatch::{parse_params, parse_params_or_default, serialize_reply, RpcContext};
use crate::protocol::{
	CorpusDocumentsParams, CorpusDocumentsReply, CorpusInfo, CorpusLayerInfoParams,
	DocumentEntry, LayerInfo, LayerKind, ProtocolError,
};

pub(crate) fn handle_corpus_info(ctx: &RpcContext) -> Result<serde_json::Value, ProtocolError> {
	let components = ctx.handle
		.corpus
		.components()
		.iter()
		.map(|c| c.name.clone())
		.collect();
	let alignments = ctx.handle
		.corpus
		.alignments()
		.iter()
		.map(|a| a.name.clone())
		.collect();
	let info = CorpusInfo {
		name: ctx.handle.corpus.name().to_string(),
		canonical_path: ctx.handle.canonical_path.display().to_string(),
		stable_key: ctx.handle.corpus_id.clone(),
		components,
		layers: ctx.handle.corpus.layers().to_vec(),
		alignments,
	};
	serialize_reply(info)
}

pub(crate) fn handle_corpus_documents(
	params: Option<serde_json::Value>,
	ctx: &RpcContext,
) -> Result<serde_json::Value, ProtocolError> {
	let parsed: CorpusDocumentsParams = parse_params_or_default(params)?;

	let allowed_range = match parsed.component.as_deref() {
		None => None,
		Some(name) => {
			let component = ctx.handle.corpus.component(name).ok_or_else(|| {
				ProtocolError::new(-32602, format!("unknown component '{}'", name))
			})?;
			Some(component.document_range)
		}
	};

	let document_names = ctx.handle.corpus.document_names();
	let mut documents = Vec::with_capacity(document_names.len());
	for (idx, name) in document_names.iter().enumerate() {
		if let Some((start, end)) = allowed_range {
			if idx < start || idx >= end {
				continue;
			}
		}
		documents.push(DocumentEntry {
			index: idx as u32,
			name: name.clone(),
			component: document_component(ctx, idx),
			sentence_count: document_sentence_count(ctx, idx),
		});
	}

	serialize_reply(CorpusDocumentsReply { documents })
}

pub(crate) fn document_component(ctx: &RpcContext, document_index: usize) -> String {
	ctx.handle.corpus
		.component_for_document(document_index)
		.map(|c| c.name.clone())
		.unwrap_or_else(|| ctx.handle.corpus.name().to_string())
}

pub(crate) fn document_sentence_count(ctx: &RpcContext, document_index: usize) -> u32 {
	match (
		ctx.handle.corpus.first_sentence_of_document(document_index),
		ctx.handle.corpus.last_sentence_of_document(document_index),
	) {
		(Some(first), Some(last)) if last >= first => (last - first + 1) as u32,
		_ => 0,
	}
}

pub(crate) fn handle_corpus_layer_info(
	params: Option<serde_json::Value>,
	ctx: &RpcContext,
) -> Result<serde_json::Value, ProtocolError> {
	let parsed: CorpusLayerInfoParams = parse_params("corpus.layer_info", params)?;

	let (kind, value_count) = classify_layer(&ctx.handle.corpus, &parsed.layer).ok_or_else(|| {
		ProtocolError::new(-32602, format!("unknown layer '{}'", parsed.layer))
	})?;

	serialize_reply(LayerInfo {
		name: parsed.layer,
		kind,
		value_count,
	})
}

fn classify_layer(corpus: &Corpus, name: &str) -> Option<(LayerKind, u32)> {
	let kind = corpus.layer_kind(name)?;
	let value_count = corpus
		.inverted()
		.values(name)
		.map(|v| v.len() as u32)
		.unwrap_or(0);
	Some((kind.into(), value_count))
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::dispatch::dispatch_request;
	use crate::dispatch::test_support::with_registered_context;
	use crate::protocol::CorpusDocumentsReply;

	#[test]
	fn corpus_info_returns_expected_shape() {
		with_registered_context(|ctx| {
			let result = dispatch_request("corpus.info", None, ctx).unwrap();
			let info: CorpusInfo = serde_json::from_value(result).unwrap();

			assert_eq!(info.name, "test-parallel");
			assert_eq!(info.stable_key, ctx.handle.corpus_id);
			assert_eq!(info.canonical_path, ctx.handle.canonical_path.display().to_string());

			let mut components = info.components.clone();
			components.sort();
			assert_eq!(components, vec!["en".to_string(), "fr".to_string()]);

			assert_eq!(info.alignments, vec!["sentence".to_string()]);
			assert!(info.layers.iter().any(|l| l == "upos"));
			assert!(info.layers.iter().any(|l| l == "lemma"));
		});
	}

	#[test]
	fn corpus_documents_returns_all_without_filter() {
		with_registered_context(|ctx| {
			let result = dispatch_request("corpus.documents", None, ctx).unwrap();
			let reply: CorpusDocumentsReply = serde_json::from_value(result).unwrap();

			assert_eq!(reply.documents.len(), 4);
			let names: Vec<&str> = reply.documents.iter().map(|d| d.name.as_str()).collect();
			assert!(names.iter().any(|n| n.contains("la_maison")));
			assert!(names.iter().any(|n| n.contains("le_chat")));
			assert!(names.iter().any(|n| n.contains("the_house")));
			assert!(names.iter().any(|n| n.contains("the_cat")));

			for doc in &reply.documents {
				assert!(doc.sentence_count >= 1);
				assert!(doc.component == "fr" || doc.component == "en");
			}
		});
	}

	#[test]
	fn corpus_documents_filters_by_component() {
		with_registered_context(|ctx| {
			let params = serde_json::json!({ "component": "fr" });
			let result = dispatch_request("corpus.documents", Some(params), ctx).unwrap();
			let reply: CorpusDocumentsReply = serde_json::from_value(result).unwrap();

			assert_eq!(reply.documents.len(), 2);
			for doc in &reply.documents {
				assert_eq!(doc.component, "fr");
			}
		});
	}

	#[test]
	fn corpus_documents_unknown_component_rejected() {
		with_registered_context(|ctx| {
			let params = serde_json::json!({ "component": "nonexistent" });
			let err = dispatch_request("corpus.documents", Some(params), ctx).unwrap_err();
			assert_eq!(err.code, -32602);
		});
	}

	#[test]
	fn corpus_layer_info_indexed_string_layer() {
		with_registered_context(|ctx| {
			let params = serde_json::json!({ "layer": "upos" });
			let result = dispatch_request("corpus.layer_info", Some(params), ctx).unwrap();
			let info: LayerInfo = serde_json::from_value(result).unwrap();

			assert_eq!(info.name, "upos");
			assert!(matches!(info.kind, LayerKind::String));
			assert!(info.value_count > 0);
		});
	}

	#[test]
	fn corpus_layer_info_head_classified_as_int() {
		with_registered_context(|ctx| {
			let params = serde_json::json!({ "layer": "head" });
			let result = dispatch_request("corpus.layer_info", Some(params), ctx).unwrap();
			let info: LayerInfo = serde_json::from_value(result).unwrap();

			assert_eq!(info.name, "head");
			assert!(matches!(info.kind, LayerKind::Int));
		});
	}

	#[test]
	fn corpus_layer_info_unknown_layer_rejected() {
		with_registered_context(|ctx| {
			let params = serde_json::json!({ "layer": "totally-not-a-layer" });
			let err = dispatch_request("corpus.layer_info", Some(params), ctx).unwrap_err();
			assert_eq!(err.code, -32602);
		});
	}
}
