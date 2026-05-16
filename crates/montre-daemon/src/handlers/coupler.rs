use crate::dispatch::{
	parse_params, parse_params_or_default, serialize_reply, state_roundtrip, RpcContext,
};
use crate::protocol::{
	CouplerCreateParams, CouplerCreateReply, CouplerListParams, CouplerListReply,
	CouplerRemoveParams, OkReply, ProtocolError,
};
use crate::state::Command;

pub(crate) fn handle_coupler_create(
	params: Option<serde_json::Value>,
	ctx: &RpcContext,
) -> Result<serde_json::Value, ProtocolError> {
	let parsed: CouplerCreateParams = parse_params("coupler.create", params)?;

	let coupler_id = state_roundtrip(ctx, |reply| Command::CouplerCreate {
		master: parsed.master_id,
		follower: parsed.follower_id,
		kind: parsed.kind,
		reply,
	})??;

	serialize_reply(CouplerCreateReply { coupler_id })
}

pub(crate) fn handle_coupler_remove(
	params: Option<serde_json::Value>,
	ctx: &RpcContext,
) -> Result<serde_json::Value, ProtocolError> {
	let parsed: CouplerRemoveParams = parse_params("coupler.remove", params)?;

	state_roundtrip(ctx, |reply| Command::CouplerRemove {
		coupler_id: parsed.coupler_id,
		reply,
	})??;

	serialize_reply(OkReply { ok: true })
}

pub(crate) fn handle_coupler_list(
	params: Option<serde_json::Value>,
	ctx: &RpcContext,
) -> Result<serde_json::Value, ProtocolError> {
	let parsed: CouplerListParams = parse_params_or_default(params)?;

	let couplers = state_roundtrip(ctx, |reply| Command::CouplerList {
		process_id: parsed.process_id,
		reply,
	})?;

	serialize_reply(CouplerListReply { couplers })
}

#[cfg(test)]
mod tests {
	use crate::dispatch::dispatch_request;
	use crate::dispatch::test_support::{register_context, with_state_thread};
	use crate::protocol::error_codes;
	use crate::protocol::{
		CouplerCreateReply, CouplerListReply, OkReply, ProcessKind,
	};
	use std::sync::Arc;

	#[test]
	fn coupler_create_succeeds_between_compatible_processes() {
		with_state_thread(|state_tx, handle| {
			let (mut ctx_a, _rx_a) = register_context(
				state_tx.clone(),
				Arc::clone(&handle),
				ProcessKind::External,
				&["position", "span", "sentence"],
				&["sentence"],
			);
			let (ctx_b, _rx_b) = register_context(
				state_tx,
				handle,
				ProcessKind::External,
				&[],
				&["sentence"],
			);

			let params = serde_json::json!({
				"master_id": ctx_a.process_id.unwrap(),
				"follower_id": ctx_b.process_id.unwrap(),
				"kind": { "type": "sentence_mirror" },
			});
			let result =
				dispatch_request("coupler.create", Some(params), &mut ctx_a).unwrap();
			let reply: CouplerCreateReply = serde_json::from_value(result).unwrap();
			assert!(reply.coupler_id > 0);
		});
	}

	#[test]
	fn coupler_create_incompatible_returns_1400() {
		with_state_thread(|state_tx, handle| {
			let (mut ctx_a, _rx_a) = register_context(
				state_tx.clone(),
				Arc::clone(&handle),
				ProcessKind::External,
				&["hit"],
				&[],
			);
			let (ctx_b, _rx_b) = register_context(
				state_tx,
				handle,
				ProcessKind::External,
				&[],
				&["sentence"],
			);

			let params = serde_json::json!({
				"master_id": ctx_a.process_id.unwrap(),
				"follower_id": ctx_b.process_id.unwrap(),
				"kind": { "type": "sentence_mirror" },
			});
			let err = dispatch_request("coupler.create", Some(params), &mut ctx_a).unwrap_err();
			assert_eq!(err.code, error_codes::COUPLER_INCOMPATIBLE);
			assert!(err.data.is_some());
		});
	}

	#[test]
	fn coupler_create_unknown_master_returns_1500() {
		with_state_thread(|state_tx, handle| {
			let (mut ctx_a, _rx_a) = register_context(
				state_tx,
				handle,
				ProcessKind::External,
				&["sentence"],
				&["sentence"],
			);
			let params = serde_json::json!({
				"master_id": 9999,
				"follower_id": ctx_a.process_id.unwrap(),
				"kind": { "type": "sentence_mirror" },
			});
			let err = dispatch_request("coupler.create", Some(params), &mut ctx_a).unwrap_err();
			assert_eq!(err.code, error_codes::PROCESS_NOT_FOUND);
		});
	}

	#[test]
	fn coupler_create_missing_params_rejected() {
		with_state_thread(|state_tx, handle| {
			let (mut ctx, _rx) = register_context(
				state_tx,
				handle,
				ProcessKind::External,
				&[],
				&[],
			);
			let err = dispatch_request("coupler.create", None, &mut ctx).unwrap_err();
			assert_eq!(err.code, -32602);
		});
	}

	#[test]
	fn coupler_remove_succeeds() {
		with_state_thread(|state_tx, handle| {
			let (mut ctx_a, _rx_a) = register_context(
				state_tx.clone(),
				Arc::clone(&handle),
				ProcessKind::External,
				&["position", "span", "sentence"],
				&["sentence"],
			);
			let (ctx_b, _rx_b) = register_context(
				state_tx,
				handle,
				ProcessKind::External,
				&[],
				&["sentence"],
			);

			let create_params = serde_json::json!({
				"master_id": ctx_a.process_id.unwrap(),
				"follower_id": ctx_b.process_id.unwrap(),
				"kind": { "type": "sentence_mirror" },
			});
			let create_result =
				dispatch_request("coupler.create", Some(create_params), &mut ctx_a).unwrap();
			let create_reply: CouplerCreateReply =
				serde_json::from_value(create_result).unwrap();

			let remove_params = serde_json::json!({ "coupler_id": create_reply.coupler_id });
			let result =
				dispatch_request("coupler.remove", Some(remove_params), &mut ctx_a).unwrap();
			let reply: OkReply = serde_json::from_value(result).unwrap();
			assert!(reply.ok);
		});
	}

	#[test]
	fn coupler_remove_unknown_returns_1401() {
		with_state_thread(|state_tx, handle| {
			let (mut ctx, _rx) = register_context(
				state_tx,
				handle,
				ProcessKind::External,
				&[],
				&[],
			);
			let params = serde_json::json!({ "coupler_id": 9999 });
			let err = dispatch_request("coupler.remove", Some(params), &mut ctx).unwrap_err();
			assert_eq!(err.code, error_codes::COUPLER_NOT_FOUND);
		});
	}

	#[test]
	fn coupler_list_empty_when_no_couplers() {
		with_state_thread(|state_tx, handle| {
			let (mut ctx, _rx) = register_context(
				state_tx,
				handle,
				ProcessKind::External,
				&[],
				&[],
			);
			let result = dispatch_request("coupler.list", None, &mut ctx).unwrap();
			let reply: CouplerListReply = serde_json::from_value(result).unwrap();
			assert!(reply.couplers.is_empty());
		});
	}

	#[test]
	fn coupler_list_includes_created_coupler() {
		with_state_thread(|state_tx, handle| {
			let (mut ctx_a, _rx_a) = register_context(
				state_tx.clone(),
				Arc::clone(&handle),
				ProcessKind::External,
				&["position", "span", "sentence"],
				&["sentence"],
			);
			let (ctx_b, _rx_b) = register_context(
				state_tx,
				handle,
				ProcessKind::External,
				&[],
				&["sentence"],
			);

			let create_params = serde_json::json!({
				"master_id": ctx_a.process_id.unwrap(),
				"follower_id": ctx_b.process_id.unwrap(),
				"kind": { "type": "sentence_mirror" },
			});
			dispatch_request("coupler.create", Some(create_params), &mut ctx_a).unwrap();

			let result = dispatch_request("coupler.list", None, &mut ctx_a).unwrap();
			let reply: CouplerListReply = serde_json::from_value(result).unwrap();
			assert_eq!(reply.couplers.len(), 1);
		});
	}

	#[test]
	fn coupler_list_filter_by_process_id() {
		with_state_thread(|state_tx, handle| {
			let (mut ctx_a, _rx_a) = register_context(
				state_tx.clone(),
				Arc::clone(&handle),
				ProcessKind::External,
				&["position", "span", "sentence"],
				&["sentence"],
			);
			let (ctx_b, _rx_b) = register_context(
				state_tx.clone(),
				Arc::clone(&handle),
				ProcessKind::External,
				&[],
				&["sentence"],
			);
			let (ctx_c, _rx_c) = register_context(
				state_tx,
				handle,
				ProcessKind::External,
				&[],
				&[],
			);

			let create_params = serde_json::json!({
				"master_id": ctx_a.process_id.unwrap(),
				"follower_id": ctx_b.process_id.unwrap(),
				"kind": { "type": "sentence_mirror" },
			});
			dispatch_request("coupler.create", Some(create_params), &mut ctx_a).unwrap();

			let params = serde_json::json!({ "process_id": ctx_c.process_id.unwrap() });
			let result = dispatch_request("coupler.list", Some(params), &mut ctx_a).unwrap();
			let reply: CouplerListReply = serde_json::from_value(result).unwrap();
			assert!(reply.couplers.is_empty());
		});
	}
}
