use crate::dispatch::{
	parse_params, parse_params_or_default, serialize_reply, state_roundtrip, RpcContext,
};
use crate::protocol::{
	OkReply, ProtocolError, SessionRosterParams, SessionRosterReply,
	SessionUpdateLabelParams,
};
use crate::state::Command;

pub(crate) fn handle_session_unregister(
	ctx: &mut RpcContext,
) -> Result<serde_json::Value, ProtocolError> {
	let process_id = ctx.process_id.ok_or_else(|| {
		ProtocolError::new(-32603, "internal: unregister without process_id")
	})?;

	state_roundtrip(ctx, |reply| Command::Unregister {
		process_id,
		reply,
	})?;

	ctx.process_id = None;

	serialize_reply(OkReply { ok: true })
}

pub(crate) fn handle_session_update_label(
	params: Option<serde_json::Value>,
	ctx: &RpcContext,
) -> Result<serde_json::Value, ProtocolError> {
	let parsed: SessionUpdateLabelParams = parse_params("session.update_label", params)?;

	let process_id = ctx.process_id.ok_or_else(|| {
		ProtocolError::new(-32603, "internal: update_label without process_id")
	})?;

	state_roundtrip(ctx, |reply| Command::UpdateLabel {
		process_id,
		label: parsed.label,
		reply,
	})??;

	serialize_reply(OkReply { ok: true })
}

pub(crate) fn handle_session_roster(
	params: Option<serde_json::Value>,
	ctx: &RpcContext,
) -> Result<serde_json::Value, ProtocolError> {
	let parsed: SessionRosterParams = parse_params_or_default(params)?;

	let processes = state_roundtrip(ctx, |reply| Command::Roster {
		filter: parsed.filter,
		reply,
	})?;

	serialize_reply(SessionRosterReply { processes })
}

#[cfg(test)]
mod tests {
	use crate::dispatch::dispatch_request;
	use crate::dispatch::test_support::{register_context, with_registered_context, with_state_thread};
	use crate::protocol::error_codes;
	use crate::protocol::{OkReply, ProcessKind, SessionRosterReply};
	use std::sync::Arc;

	#[test]
	fn session_unregister_succeeds_and_clears_process_id() {
		with_registered_context(|ctx| {
			assert!(ctx.process_id.is_some());
			let result = dispatch_request("session.unregister", None, ctx).unwrap();
			let reply: OkReply = serde_json::from_value(result).unwrap();
			assert!(reply.ok);
			assert!(ctx.process_id.is_none());
		});
	}

	#[test]
	fn session_unregister_then_call_returns_not_registered() {
		with_registered_context(|ctx| {
			dispatch_request("session.unregister", None, ctx).unwrap();
			let err = dispatch_request("corpus.info", None, ctx).unwrap_err();
			assert_eq!(err.code, error_codes::NOT_REGISTERED);
		});
	}

	#[test]
	fn session_update_label_succeeds() {
		with_registered_context(|ctx| {
			let params = serde_json::json!({ "label": "fr/la-parure" });
			let result = dispatch_request("session.update_label", Some(params), ctx).unwrap();
			let reply: OkReply = serde_json::from_value(result).unwrap();
			assert!(reply.ok);
		});
	}

	#[test]
	fn session_update_label_with_null_clears_label() {
		with_registered_context(|ctx| {
			let params = serde_json::json!({ "label": null });
			let result = dispatch_request("session.update_label", Some(params), ctx).unwrap();
			let reply: OkReply = serde_json::from_value(result).unwrap();
			assert!(reply.ok);
		});
	}

	#[test]
	fn session_update_label_missing_params_rejected() {
		with_registered_context(|ctx| {
			let err = dispatch_request("session.update_label", None, ctx).unwrap_err();
			assert_eq!(err.code, -32602);
		});
	}

	#[test]
	fn session_roster_returns_all_processes_when_no_filter() {
		with_state_thread(|state_tx, handle| {
			let (mut ctx_a, _rx_a) = register_context(
				state_tx.clone(),
				Arc::clone(&handle),
				ProcessKind::Reader,
				&["sentence"],
				&["sentence"],
			);
			let (_ctx_b, _rx_b) = register_context(
				state_tx,
				handle,
				ProcessKind::Kwic,
				&["hit"],
				&[],
			);

			let result = dispatch_request("session.roster", None, &mut ctx_a).unwrap();
			let reply: SessionRosterReply = serde_json::from_value(result).unwrap();
			assert_eq!(reply.processes.len(), 2);
		});
	}

	#[test]
	fn session_roster_filters_by_kind() {
		with_state_thread(|state_tx, handle| {
			let (mut ctx_a, _rx_a) = register_context(
				state_tx.clone(),
				Arc::clone(&handle),
				ProcessKind::Reader,
				&[],
				&[],
			);
			let (_ctx_b, _rx_b) = register_context(
				state_tx,
				handle,
				ProcessKind::Kwic,
				&[],
				&[],
			);

			let params = serde_json::json!({
				"filter": { "kinds": ["reader"] }
			});
			let result =
				dispatch_request("session.roster", Some(params), &mut ctx_a).unwrap();
			let reply: SessionRosterReply = serde_json::from_value(result).unwrap();
			assert_eq!(reply.processes.len(), 1);
			assert!(matches!(reply.processes[0].kind, ProcessKind::Reader));
		});
	}

	#[test]
	fn session_roster_empty_params_object_is_no_filter() {
		with_registered_context(|ctx| {
			let result =
				dispatch_request("session.roster", Some(serde_json::json!({})), ctx).unwrap();
			let reply: SessionRosterReply = serde_json::from_value(result).unwrap();
			assert_eq!(reply.processes.len(), 1);
		});
	}
}
