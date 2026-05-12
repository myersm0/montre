use std::sync::Arc;

use montre_query::QueryError;

use crate::dispatch::{
	parse_params, serialize_reply, state_roundtrip, RpcContext,
};
use crate::protocol::error_codes;
use crate::protocol::{
	Hit, NamedResultEntry, OkReply, ProtocolError, QueryDeleteNamedParams, QueryDiscardParams,
	QueryExecuteCountReply, QueryExecuteParams, QueryExecuteReply, QueryHitsParams,
	QueryHitsReply, QueryListNamedReply, QueryLoadParams, QueryLoadReply,
	QueryMaterializeParams, QueryMaterializeReply, QueryMetadataParams, QuerySaveParams,
	QuerySaveReply, ResultForm, ResultMetadata,
};
use crate::state::{Command, LoadOutcome, ResultEntry};

const MAX_HITS_PER_PAGE: u64 = 1000;

fn map_query_error(err: QueryError) -> ProtocolError {
	match err {
		QueryError::Parse { position, message } => {
			ProtocolError::new(error_codes::CQL_PARSE_ERROR, message)
				.with_data(serde_json::json!({ "position": position }))
		}
		QueryError::Regex(e) => ProtocolError::new(
			error_codes::CQL_PARSE_ERROR,
			format!("regex error: {}", e),
		),
		QueryError::UnknownLayer(name) => ProtocolError::new(
			error_codes::PLAN_ERROR,
			format!("unknown layer: {}", name),
		),
		QueryError::UnknownLabel(name) => ProtocolError::new(
			error_codes::PLAN_ERROR,
			format!("unknown label: {}", name),
		),
		QueryError::Execution(msg) => ProtocolError::new(error_codes::EXECUTION_ERROR, msg),
	}
}

pub(crate) fn handle_query_execute(
	params: Option<serde_json::Value>,
	ctx: &RpcContext,
) -> Result<serde_json::Value, ProtocolError> {
	let parsed: QueryExecuteParams = parse_params("query.execute", params)?;

	let ast = montre_query::parse(&parsed.cql).map_err(map_query_error)?;
	let plan = montre_query::planner::plan(&ast).map_err(map_query_error)?;
	let mut results =
		montre_query::executor::execute(&plan, &ctx.handle.corpus).map_err(map_query_error)?;
	results.populate_context(&ctx.handle.corpus);
	let hits = results.into_hits();
	let hit_count = hits.len() as u64;

	let handle = state_roundtrip(ctx, |reply| Command::InsertResult {
		cql: parsed.cql,
		hits,
		reply,
	})?;

	serialize_reply(QueryExecuteReply { handle, hit_count })
}

pub(crate) fn handle_query_execute_count(
	params: Option<serde_json::Value>,
	ctx: &RpcContext,
) -> Result<serde_json::Value, ProtocolError> {
	let parsed: QueryExecuteParams = parse_params("query.execute_count", params)?;

	let ast = montre_query::parse(&parsed.cql).map_err(map_query_error)?;
	let plan = montre_query::planner::plan(&ast).map_err(map_query_error)?;
	let count = montre_query::executor::execute_count(&plan, &ctx.handle.corpus)
		.map_err(map_query_error)?;

	serialize_reply(QueryExecuteCountReply {
		count: count as u64,
	})
}

pub(crate) fn handle_query_hits(
	params: Option<serde_json::Value>,
	ctx: &RpcContext,
) -> Result<serde_json::Value, ProtocolError> {
	let parsed: QueryHitsParams = parse_params("query.hits", params)?;

	if parsed.limit > MAX_HITS_PER_PAGE {
		return Err(ProtocolError::new(
			error_codes::PAGE_LIMIT_EXCEEDED,
			format!("limit {} exceeds maximum {}", parsed.limit, MAX_HITS_PER_PAGE),
		));
	}

	let entry = {
		let table = ctx.handle.results.read().expect("results lock poisoned");
		table.get(&parsed.handle).cloned().ok_or_else(|| {
			ProtocolError::new(error_codes::RESULT_HANDLE_INVALID, "handle not found")
		})?
	};

	let total_count = entry.hits.len() as u64;
	let offset = parsed.offset as usize;
	let limit = parsed.limit as usize;
	let start = offset.min(entry.hits.len());
	let end = start.saturating_add(limit).min(entry.hits.len());

	let hits: Vec<Hit> = entry.hits[start..end]
		.iter()
		.cloned()
		.map(Hit::from)
		.collect();

	serialize_reply(QueryHitsReply {
		hits,
		offset: parsed.offset,
		limit: parsed.limit,
		total_count,
	})
}

pub(crate) fn handle_query_metadata(
	params: Option<serde_json::Value>,
	ctx: &RpcContext,
) -> Result<serde_json::Value, ProtocolError> {
	let parsed: QueryMetadataParams = parse_params("query.metadata", params)?;

	let entry = {
		let table = ctx.handle.results.read().expect("results lock poisoned");
		table.get(&parsed.handle).cloned().ok_or_else(|| {
			ProtocolError::new(error_codes::RESULT_HANDLE_INVALID, "handle not found")
		})?
	};

	serialize_reply(entry.metadata.clone())
}

pub(crate) fn handle_query_save(
	params: Option<serde_json::Value>,
	ctx: &RpcContext,
) -> Result<serde_json::Value, ProtocolError> {
	let parsed: QuerySaveParams = parse_params("query.save", params)?;

	state_roundtrip(ctx, |reply| Command::SaveResult {
		handle: parsed.handle,
		name: parsed.name,
		reply,
	})??;

	serialize_reply(QuerySaveReply {
		ok: true,
		form: ResultForm::QueryBacked,
	})
}

pub(crate) fn handle_query_materialize(
	params: Option<serde_json::Value>,
	ctx: &RpcContext,
) -> Result<serde_json::Value, ProtocolError> {
	let parsed: QueryMaterializeParams = parse_params("query.materialize", params)?;

	let metadata = state_roundtrip(ctx, |reply| Command::MaterializeResult {
		name: parsed.name,
		reply,
	})??;

	let materialized_at = metadata
		.materialized_at
		.expect("state guarantees materialized_at after MaterializeResult");
	serialize_reply(QueryMaterializeReply {
		ok: true,
		hit_count: metadata.hit_count,
		materialized_at,
	})
}

pub(crate) fn handle_query_load(
	params: Option<serde_json::Value>,
	ctx: &RpcContext,
) -> Result<serde_json::Value, ProtocolError> {
	let parsed: QueryLoadParams = parse_params("query.load", params)?;

	let outcome = state_roundtrip(ctx, |reply| Command::LoadNamed {
		name: parsed.name.clone(),
		reply,
	})??;

	match outcome {
		LoadOutcome::Realized(metadata) => serialize_reply(QueryLoadReply {
			handle: metadata.handle,
			hit_count: metadata.hit_count,
			form: metadata.form,
		}),
		LoadOutcome::Pending { handle, cql, created_at } => {
			let ast = montre_query::parse(&cql).map_err(stored_query_invalid)?;
			let plan = montre_query::planner::plan(&ast).map_err(stored_query_invalid)?;
			let mut results = montre_query::executor::execute(&plan, &ctx.handle.corpus)
				.map_err(stored_query_invalid)?;
			results.populate_context(&ctx.handle.corpus);
			let hits = results.into_hits();
			let hit_count = hits.len() as u64;

			let metadata = ResultMetadata {
				handle: handle.clone(),
				query: cql.clone(),
				created_at,
				materialized_at: None,
				hit_count,
				corpus_id: ctx.handle.corpus_id.clone(),
				name: Some(parsed.name),
				form: ResultForm::QueryBacked,
			};
			let entry = Arc::new(ResultEntry {
				cql,
				hits: Arc::new(hits),
				metadata,
			});

			state_roundtrip(ctx, |reply| Command::InstallReplayedResult {
				entry,
				reply,
			})?;

			serialize_reply(QueryLoadReply {
				handle,
				hit_count,
				form: ResultForm::QueryBacked,
			})
		}
	}
}

fn stored_query_invalid(error: QueryError) -> ProtocolError {
	let mapped = map_query_error(error);
	ProtocolError::new(
		error_codes::STORED_QUERY_INVALID,
		format!("stored query no longer valid: {}", mapped.message),
	)
}

pub(crate) fn handle_query_list_named(
	ctx: &RpcContext,
) -> Result<serde_json::Value, ProtocolError> {
	let entries = state_roundtrip(ctx, |reply| Command::ListNamed { reply })?;

	let names: Vec<NamedResultEntry> = entries
		.into_iter()
		.filter_map(|m| {
			m.name.map(|name| NamedResultEntry {
				name,
				hit_count: m.hit_count,
				created_at: m.created_at,
			})
		})
		.collect();

	serialize_reply(QueryListNamedReply { names })
}

pub(crate) fn handle_query_delete_named(
	params: Option<serde_json::Value>,
	ctx: &RpcContext,
) -> Result<serde_json::Value, ProtocolError> {
	let parsed: QueryDeleteNamedParams = parse_params("query.delete_named", params)?;

	state_roundtrip(ctx, |reply| Command::DeleteNamed {
		name: parsed.name,
		reply,
	})??;

	serialize_reply(OkReply { ok: true })
}

pub(crate) fn handle_query_discard(
	params: Option<serde_json::Value>,
	ctx: &RpcContext,
) -> Result<serde_json::Value, ProtocolError> {
	let parsed: QueryDiscardParams = parse_params("query.discard", params)?;

	state_roundtrip(ctx, |reply| Command::DiscardHandle {
		handle: parsed.handle,
		reply,
	})?;

	serialize_reply(OkReply { ok: true })
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::dispatch::dispatch_request;
	use crate::dispatch::test_support::with_registered_context;

	fn execute_and_get_handle(ctx: &mut crate::dispatch::RpcContext) -> String {
		let params = serde_json::json!({ "cql": "[pos=\"NOUN\"]" });
		let result = dispatch_request("query.execute", Some(params), ctx).unwrap();
		let reply: QueryExecuteReply = serde_json::from_value(result).unwrap();
		reply.handle
	}

	#[test]
	fn query_execute_count_returns_count() {
		with_registered_context(|ctx| {
			let params = serde_json::json!({ "cql": "[pos=\"NOUN\"]" });
			let result = dispatch_request("query.execute_count", Some(params), ctx).unwrap();
			let reply: QueryExecuteCountReply = serde_json::from_value(result).unwrap();
			assert!(reply.count > 0);
		});
	}

	#[test]
	fn query_execute_count_parse_error() {
		with_registered_context(|ctx| {
			let params = serde_json::json!({ "cql": "[pos=]" });
			let err = dispatch_request("query.execute_count", Some(params), ctx).unwrap_err();
			assert_eq!(err.code, error_codes::CQL_PARSE_ERROR);
		});
	}

	#[test]
	fn query_execute_returns_handle_and_count() {
		with_registered_context(|ctx| {
			let params = serde_json::json!({ "cql": "[pos=\"NOUN\"]" });
			let result = dispatch_request("query.execute", Some(params), ctx).unwrap();
			let reply: QueryExecuteReply = serde_json::from_value(result).unwrap();
			assert!(reply.handle.starts_with("r-"));
			assert!(reply.hit_count > 0);
		});
	}

	#[test]
	fn query_execute_parse_error() {
		with_registered_context(|ctx| {
			let params = serde_json::json!({ "cql": "[pos=]" });
			let err = dispatch_request("query.execute", Some(params), ctx).unwrap_err();
			assert_eq!(err.code, error_codes::CQL_PARSE_ERROR);
		});
	}

	#[test]
	fn query_hits_returns_all_hits_under_limit() {
		with_registered_context(|ctx| {
			let execute_params = serde_json::json!({ "cql": "[pos=\"NOUN\"]" });
			let execute_result =
				dispatch_request("query.execute", Some(execute_params), ctx).unwrap();
			let execute_reply: QueryExecuteReply =
				serde_json::from_value(execute_result).unwrap();

			let hits_params = serde_json::json!({
				"handle": execute_reply.handle,
				"offset": 0,
				"limit": 100,
			});
			let result = dispatch_request("query.hits", Some(hits_params), ctx).unwrap();
			let reply: QueryHitsReply = serde_json::from_value(result).unwrap();

			assert_eq!(reply.offset, 0);
			assert_eq!(reply.limit, 100);
			assert_eq!(reply.total_count, execute_reply.hit_count);
			assert_eq!(reply.hits.len() as u64, execute_reply.hit_count);

			for hit in &reply.hits {
				assert_ne!(hit.document_index, u32::MAX, "doc index unpopulated");
				assert_ne!(hit.sentence_index, u32::MAX, "sent index unpopulated");
			}
		});
	}

	#[test]
	fn query_hits_pagination_slices_correctly() {
		with_registered_context(|ctx| {
			let execute_params = serde_json::json!({ "cql": "[pos=\"NOUN\"]" });
			let execute_result =
				dispatch_request("query.execute", Some(execute_params), ctx).unwrap();
			let execute_reply: QueryExecuteReply =
				serde_json::from_value(execute_result).unwrap();
			assert!(execute_reply.hit_count >= 2);

			let hits_params = serde_json::json!({
				"handle": execute_reply.handle,
				"offset": 1,
				"limit": 1,
			});
			let result = dispatch_request("query.hits", Some(hits_params), ctx).unwrap();
			let reply: QueryHitsReply = serde_json::from_value(result).unwrap();

			assert_eq!(reply.hits.len(), 1);
			assert_eq!(reply.offset, 1);
			assert_eq!(reply.total_count, execute_reply.hit_count);
		});
	}

	#[test]
	fn query_hits_invalid_handle_rejected() {
		with_registered_context(|ctx| {
			let params = serde_json::json!({
				"handle": "r-nonexistent",
				"offset": 0,
				"limit": 100,
			});
			let err = dispatch_request("query.hits", Some(params), ctx).unwrap_err();
			assert_eq!(err.code, error_codes::RESULT_HANDLE_INVALID);
		});
	}

	#[test]
	fn query_hits_limit_exceeded_rejected() {
		with_registered_context(|ctx| {
			let params = serde_json::json!({
				"handle": "r-nonexistent",
				"offset": 0,
				"limit": 5000,
			});
			let err = dispatch_request("query.hits", Some(params), ctx).unwrap_err();
			assert_eq!(err.code, error_codes::PAGE_LIMIT_EXCEEDED);
		});
	}

	#[test]
	fn query_metadata_returns_metadata() {
		use crate::protocol::{ResultForm, ResultMetadata};

		with_registered_context(|ctx| {
			let execute_params = serde_json::json!({ "cql": "[pos=\"NOUN\"]" });
			let execute_result =
				dispatch_request("query.execute", Some(execute_params), ctx).unwrap();
			let execute_reply: QueryExecuteReply =
				serde_json::from_value(execute_result).unwrap();

			let params = serde_json::json!({ "handle": execute_reply.handle });
			let result = dispatch_request("query.metadata", Some(params), ctx).unwrap();
			let metadata: ResultMetadata = serde_json::from_value(result).unwrap();

			assert_eq!(metadata.handle, execute_reply.handle);
			assert_eq!(metadata.query, "[pos=\"NOUN\"]");
			assert_eq!(metadata.hit_count, execute_reply.hit_count);
			assert!(metadata.name.is_none());
			assert!(matches!(metadata.form, ResultForm::Session));
		});
	}

	#[test]
	fn query_metadata_invalid_handle_rejected() {
		with_registered_context(|ctx| {
			let params = serde_json::json!({ "handle": "r-nonexistent" });
			let err = dispatch_request("query.metadata", Some(params), ctx).unwrap_err();
			assert_eq!(err.code, error_codes::RESULT_HANDLE_INVALID);
		});
	}

	#[test]
	fn query_save_succeeds_and_returns_query_backed_form() {
		with_registered_context(|ctx| {
			let handle = execute_and_get_handle(ctx);
			let params = serde_json::json!({ "handle": handle, "name": "saved-nouns" });
			let result = dispatch_request("query.save", Some(params), ctx).unwrap();
			let reply: QuerySaveReply = serde_json::from_value(result).unwrap();
			assert!(reply.ok);
			assert!(matches!(reply.form, ResultForm::QueryBacked));
		});
	}

	#[test]
	fn query_save_duplicate_name_returns_1201() {
		with_registered_context(|ctx| {
			let h1 = execute_and_get_handle(ctx);
			let h2 = execute_and_get_handle(ctx);
			let params = serde_json::json!({ "handle": h1, "name": "dup" });
			dispatch_request("query.save", Some(params), ctx).unwrap();
			let params2 = serde_json::json!({ "handle": h2, "name": "dup" });
			let err = dispatch_request("query.save", Some(params2), ctx).unwrap_err();
			assert_eq!(err.code, error_codes::NAMED_RESULT_ALREADY_EXISTS);
		});
	}

	#[test]
	fn query_save_invalid_handle_returns_1200() {
		with_registered_context(|ctx| {
			let params =
				serde_json::json!({ "handle": "r-nonexistent", "name": "whatever" });
			let err = dispatch_request("query.save", Some(params), ctx).unwrap_err();
			assert_eq!(err.code, error_codes::RESULT_HANDLE_INVALID);
		});
	}

	#[test]
	fn query_materialize_succeeds_and_populates_materialized_at() {
		with_registered_context(|ctx| {
			let handle = execute_and_get_handle(ctx);
			let save_params = serde_json::json!({ "handle": handle, "name": "to-mat" });
			dispatch_request("query.save", Some(save_params), ctx).unwrap();

			let params = serde_json::json!({ "name": "to-mat" });
			let result = dispatch_request("query.materialize", Some(params), ctx).unwrap();
			let reply: QueryMaterializeReply = serde_json::from_value(result).unwrap();
			assert!(reply.ok);
			assert!(!reply.materialized_at.is_empty());
		});
	}

	#[test]
	fn query_materialize_unknown_name_returns_1202() {
		with_registered_context(|ctx| {
			let params = serde_json::json!({ "name": "never-saved" });
			let err = dispatch_request("query.materialize", Some(params), ctx).unwrap_err();
			assert_eq!(err.code, error_codes::NAMED_RESULT_NOT_FOUND);
		});
	}

	#[test]
	fn query_materialize_already_materialized_returns_1205() {
		with_registered_context(|ctx| {
			let handle = execute_and_get_handle(ctx);
			let save_params = serde_json::json!({ "handle": handle, "name": "twice" });
			dispatch_request("query.save", Some(save_params), ctx).unwrap();
			let mat_params = serde_json::json!({ "name": "twice" });
			dispatch_request("query.materialize", Some(mat_params.clone()), ctx).unwrap();
			let err = dispatch_request("query.materialize", Some(mat_params), ctx).unwrap_err();
			assert_eq!(err.code, error_codes::RESULT_ALREADY_MATERIALIZED);
		});
	}

	#[test]
	fn query_load_returns_handle_and_form() {
		with_registered_context(|ctx| {
			let handle = execute_and_get_handle(ctx);
			let save_params = serde_json::json!({ "handle": handle.clone(), "name": "loadable" });
			dispatch_request("query.save", Some(save_params), ctx).unwrap();

			let params = serde_json::json!({ "name": "loadable" });
			let result = dispatch_request("query.load", Some(params), ctx).unwrap();
			let reply: QueryLoadReply = serde_json::from_value(result).unwrap();
			assert_eq!(reply.handle, handle);
			assert!(matches!(reply.form, ResultForm::QueryBacked));
		});
	}

	#[test]
	fn query_load_unknown_name_returns_1202() {
		with_registered_context(|ctx| {
			let params = serde_json::json!({ "name": "nope" });
			let err = dispatch_request("query.load", Some(params), ctx).unwrap_err();
			assert_eq!(err.code, error_codes::NAMED_RESULT_NOT_FOUND);
		});
	}

	#[test]
	fn query_list_named_empty() {
		with_registered_context(|ctx| {
			let result = dispatch_request("query.list_named", None, ctx).unwrap();
			let reply: QueryListNamedReply = serde_json::from_value(result).unwrap();
			assert!(reply.names.is_empty());
		});
	}

	#[test]
	fn query_list_named_includes_saved() {
		with_registered_context(|ctx| {
			let handle = execute_and_get_handle(ctx);
			let save_params = serde_json::json!({ "handle": handle, "name": "listed" });
			dispatch_request("query.save", Some(save_params), ctx).unwrap();

			let result = dispatch_request("query.list_named", None, ctx).unwrap();
			let reply: QueryListNamedReply = serde_json::from_value(result).unwrap();
			assert_eq!(reply.names.len(), 1);
			assert_eq!(reply.names[0].name, "listed");
			assert!(reply.names[0].hit_count > 0);
		});
	}

	#[test]
	fn query_delete_named_succeeds() {
		with_registered_context(|ctx| {
			let handle = execute_and_get_handle(ctx);
			let save_params = serde_json::json!({ "handle": handle, "name": "doomed" });
			dispatch_request("query.save", Some(save_params), ctx).unwrap();

			let params = serde_json::json!({ "name": "doomed" });
			let result = dispatch_request("query.delete_named", Some(params), ctx).unwrap();
			let reply: OkReply = serde_json::from_value(result).unwrap();
			assert!(reply.ok);

			let load_params = serde_json::json!({ "name": "doomed" });
			let err = dispatch_request("query.load", Some(load_params), ctx).unwrap_err();
			assert_eq!(err.code, error_codes::NAMED_RESULT_NOT_FOUND);
		});
	}

	#[test]
	fn query_delete_named_unknown_returns_1202() {
		with_registered_context(|ctx| {
			let params = serde_json::json!({ "name": "ghost" });
			let err = dispatch_request("query.delete_named", Some(params), ctx).unwrap_err();
			assert_eq!(err.code, error_codes::NAMED_RESULT_NOT_FOUND);
		});
	}

	#[test]
	fn query_discard_session_handle_removes_entry() {
		with_registered_context(|ctx| {
			let handle = execute_and_get_handle(ctx);
			let params = serde_json::json!({ "handle": handle.clone() });
			let result = dispatch_request("query.discard", Some(params), ctx).unwrap();
			let reply: OkReply = serde_json::from_value(result).unwrap();
			assert!(reply.ok);

			let meta_params = serde_json::json!({ "handle": handle });
			let err = dispatch_request("query.metadata", Some(meta_params), ctx).unwrap_err();
			assert_eq!(err.code, error_codes::RESULT_HANDLE_INVALID);
		});
	}

	#[test]
	fn query_discard_named_handle_is_no_op() {
		with_registered_context(|ctx| {
			let handle = execute_and_get_handle(ctx);
			let save_params =
				serde_json::json!({ "handle": handle.clone(), "name": "persistent" });
			dispatch_request("query.save", Some(save_params), ctx).unwrap();

			let params = serde_json::json!({ "handle": handle.clone() });
			let result = dispatch_request("query.discard", Some(params), ctx).unwrap();
			let reply: OkReply = serde_json::from_value(result).unwrap();
			assert!(reply.ok);

			let meta_params = serde_json::json!({ "handle": handle });
			dispatch_request("query.metadata", Some(meta_params), ctx).unwrap();
		});
	}
}
