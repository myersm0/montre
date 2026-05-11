use std::sync::mpsc::sync_channel;

use montre_query::QueryError;

use crate::dispatch::{RpcContext, STATE_DISPATCH_FAILURE, STATE_REPLY_FAILURE};
use crate::protocol::error_codes;
use crate::protocol::{
	Hit, ProtocolError, QueryExecuteCountReply, QueryExecuteParams, QueryExecuteReply,
	QueryHitsParams, QueryHitsReply, QueryMetadataParams,
};
use crate::state::Command;

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
	let raw = params
		.ok_or_else(|| ProtocolError::new(-32602, "query.execute requires params"))?;
	let parsed: QueryExecuteParams = serde_json::from_value(raw)
		.map_err(|e| ProtocolError::new(-32602, format!("invalid params: {}", e)))?;

	let ast = montre_query::parse(&parsed.cql).map_err(map_query_error)?;
	let plan = montre_query::planner::plan(&ast).map_err(map_query_error)?;
	let mut results =
		montre_query::executor::execute(&plan, &ctx.handle.corpus).map_err(map_query_error)?;
	results.populate_context(&ctx.handle.corpus);
	let hits = results.into_hits();
	let hit_count = hits.len() as u64;

	let (reply_tx, reply_rx) = sync_channel(1);
	ctx.state_tx
		.send(Command::InsertResult {
			cql: parsed.cql,
			hits,
			reply: reply_tx,
		})
		.map_err(|_| ProtocolError::new(-32603, STATE_DISPATCH_FAILURE))?;
	let handle = reply_rx
		.recv()
		.map_err(|_| ProtocolError::new(-32603, STATE_REPLY_FAILURE))?;

	let reply = QueryExecuteReply { handle, hit_count };
	serde_json::to_value(reply)
		.map_err(|e| ProtocolError::new(-32603, format!("response serialization failed: {}", e)))
}

pub(crate) fn handle_query_execute_count(
	params: Option<serde_json::Value>,
	ctx: &RpcContext,
) -> Result<serde_json::Value, ProtocolError> {
	let raw = params
		.ok_or_else(|| ProtocolError::new(-32602, "query.execute_count requires params"))?;
	let parsed: QueryExecuteParams = serde_json::from_value(raw)
		.map_err(|e| ProtocolError::new(-32602, format!("invalid params: {}", e)))?;

	let ast = montre_query::parse(&parsed.cql).map_err(map_query_error)?;
	let plan = montre_query::planner::plan(&ast).map_err(map_query_error)?;
	let count = montre_query::executor::execute_count(&plan, &ctx.handle.corpus)
		.map_err(map_query_error)?;

	let reply = QueryExecuteCountReply {
		count: count as u64,
	};
	serde_json::to_value(reply)
		.map_err(|e| ProtocolError::new(-32603, format!("response serialization failed: {}", e)))
}

pub(crate) fn handle_query_hits(
	params: Option<serde_json::Value>,
	ctx: &RpcContext,
) -> Result<serde_json::Value, ProtocolError> {
	let raw = params.ok_or_else(|| ProtocolError::new(-32602, "query.hits requires params"))?;
	let parsed: QueryHitsParams = serde_json::from_value(raw)
		.map_err(|e| ProtocolError::new(-32602, format!("invalid params: {}", e)))?;

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

	let reply = QueryHitsReply {
		hits,
		offset: parsed.offset,
		limit: parsed.limit,
		total_count,
	};
	serde_json::to_value(reply)
		.map_err(|e| ProtocolError::new(-32603, format!("response serialization failed: {}", e)))
}

pub(crate) fn handle_query_metadata(
	params: Option<serde_json::Value>,
	ctx: &RpcContext,
) -> Result<serde_json::Value, ProtocolError> {
	let raw = params
		.ok_or_else(|| ProtocolError::new(-32602, "query.metadata requires params"))?;
	let parsed: QueryMetadataParams = serde_json::from_value(raw)
		.map_err(|e| ProtocolError::new(-32602, format!("invalid params: {}", e)))?;

	let entry = {
		let table = ctx.handle.results.read().expect("results lock poisoned");
		table.get(&parsed.handle).cloned().ok_or_else(|| {
			ProtocolError::new(error_codes::RESULT_HANDLE_INVALID, "handle not found")
		})?
	};

	serde_json::to_value(entry.metadata.clone())
		.map_err(|e| ProtocolError::new(-32603, format!("response serialization failed: {}", e)))
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::dispatch::dispatch_request;
	use crate::dispatch::test_support::with_registered_context;
	use crate::protocol::ResultMetadata;

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
}
