use crate::dispatch::{parse_params_or_default, serialize_reply, state_roundtrip, RpcContext};
use crate::protocol::{DaemonShutdownParams, OkReply, ProtocolError, ShutdownReason};
use crate::state::Command;

pub(crate) fn handle_daemon_shutdown(
	params: Option<serde_json::Value>,
	ctx: &RpcContext,
) -> Result<serde_json::Value, ProtocolError> {
	let parsed: DaemonShutdownParams = parse_params_or_default(params)?;
	let reason = parsed.reason.unwrap_or(ShutdownReason::Requested);

	state_roundtrip(ctx, |reply| Command::InitiateShutdown { reason, reply })?;

	serialize_reply(OkReply { ok: true })
}

#[cfg(test)]
mod tests {
	use crate::dispatch::dispatch_request;
	use crate::dispatch::test_support::with_registered_context;
	use crate::protocol::OkReply;

	#[test]
	fn daemon_shutdown_returns_ok_with_default_reason() {
		with_registered_context(|ctx| {
			let result = dispatch_request("daemon.shutdown", None, ctx).unwrap();
			let reply: OkReply = serde_json::from_value(result).unwrap();
			assert!(reply.ok);
		});
	}

	#[test]
	fn daemon_shutdown_accepts_explicit_reason() {
		with_registered_context(|ctx| {
			let params = serde_json::json!({ "reason": "requested" });
			let result = dispatch_request("daemon.shutdown", Some(params), ctx).unwrap();
			let reply: OkReply = serde_json::from_value(result).unwrap();
			assert!(reply.ok);
		});
	}
}
