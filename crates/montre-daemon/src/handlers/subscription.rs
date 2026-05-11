use std::sync::mpsc::sync_channel;

use crate::dispatch::{RpcContext, STATE_DISPATCH_FAILURE, STATE_REPLY_FAILURE};
use crate::protocol::error_codes;
use crate::protocol::{OkReply, ProcessId, ProtocolError, SubscriptionParams, Topic};
use crate::state::Command;

pub(crate) fn handle_subscription_subscribe(
	params: Option<serde_json::Value>,
	ctx: &RpcContext,
) -> Result<serde_json::Value, ProtocolError> {
	let (process_id, topic) = parse_subscription(params, ctx, "subscription.subscribe")?;

	let (reply_tx, reply_rx) = sync_channel(1);
	ctx.state_tx
		.send(Command::Subscribe {
			process_id,
			topic,
			reply: reply_tx,
		})
		.map_err(|_| ProtocolError::new(-32603, STATE_DISPATCH_FAILURE))?;
	reply_rx
		.recv()
		.map_err(|_| ProtocolError::new(-32603, STATE_REPLY_FAILURE))??;

	serde_json::to_value(OkReply { ok: true })
		.map_err(|e| ProtocolError::new(-32603, format!("response serialization failed: {}", e)))
}

pub(crate) fn handle_subscription_unsubscribe(
	params: Option<serde_json::Value>,
	ctx: &RpcContext,
) -> Result<serde_json::Value, ProtocolError> {
	let (process_id, topic) = parse_subscription(params, ctx, "subscription.unsubscribe")?;

	let (reply_tx, reply_rx) = sync_channel(1);
	ctx.state_tx
		.send(Command::Unsubscribe {
			process_id,
			topic,
			reply: reply_tx,
		})
		.map_err(|_| ProtocolError::new(-32603, STATE_DISPATCH_FAILURE))?;
	reply_rx
		.recv()
		.map_err(|_| ProtocolError::new(-32603, STATE_REPLY_FAILURE))?;

	serde_json::to_value(OkReply { ok: true })
		.map_err(|e| ProtocolError::new(-32603, format!("response serialization failed: {}", e)))
}

fn parse_subscription(
	params: Option<serde_json::Value>,
	ctx: &RpcContext,
	method: &str,
) -> Result<(ProcessId, Topic), ProtocolError> {
	let raw = params
		.ok_or_else(|| ProtocolError::new(-32602, format!("{} requires params", method)))?;
	let parsed: SubscriptionParams = serde_json::from_value(raw)
		.map_err(|e| ProtocolError::new(-32602, format!("invalid params: {}", e)))?;

	let topic = parse_topic(&parsed.topic)?;
	let process_id = ctx.process_id.ok_or_else(|| {
		ProtocolError::new(-32603, format!("internal: {} without process_id", method))
	})?;
	Ok((process_id, topic))
}

fn parse_topic(name: &str) -> Result<Topic, ProtocolError> {
	let value = serde_json::Value::String(name.to_string());
	serde_json::from_value(value).map_err(|_| {
		ProtocolError::new(
			error_codes::UNKNOWN_TOPIC,
			format!("unknown subscription topic '{}'", name),
		)
	})
}

#[cfg(test)]
mod tests {
	use crate::dispatch::dispatch_request;
	use crate::dispatch::test_support::with_registered_context;
	use crate::protocol::error_codes;
	use crate::protocol::OkReply;

	#[test]
	fn subscribe_known_topic_succeeds() {
		with_registered_context(|ctx| {
			let params = serde_json::json!({ "topic": "roster_changed" });
			let result =
				dispatch_request("subscription.subscribe", Some(params), ctx).unwrap();
			let reply: OkReply = serde_json::from_value(result).unwrap();
			assert!(reply.ok);
		});
	}

	#[test]
	fn subscribe_named_results_topic_succeeds() {
		with_registered_context(|ctx| {
			let params = serde_json::json!({ "topic": "named_results_changed" });
			let result =
				dispatch_request("subscription.subscribe", Some(params), ctx).unwrap();
			let reply: OkReply = serde_json::from_value(result).unwrap();
			assert!(reply.ok);
		});
	}

	#[test]
	fn subscribe_unknown_topic_returns_1600() {
		with_registered_context(|ctx| {
			let params = serde_json::json!({ "topic": "totally_unknown" });
			let err =
				dispatch_request("subscription.subscribe", Some(params), ctx).unwrap_err();
			assert_eq!(err.code, error_codes::UNKNOWN_TOPIC);
		});
	}

	#[test]
	fn subscribe_missing_params_rejected() {
		with_registered_context(|ctx| {
			let err = dispatch_request("subscription.subscribe", None, ctx).unwrap_err();
			assert_eq!(err.code, -32602);
		});
	}

	#[test]
	fn unsubscribe_known_topic_succeeds() {
		with_registered_context(|ctx| {
			let params = serde_json::json!({ "topic": "roster_changed" });
			dispatch_request("subscription.subscribe", Some(params.clone()), ctx).unwrap();

			let result =
				dispatch_request("subscription.unsubscribe", Some(params), ctx).unwrap();
			let reply: OkReply = serde_json::from_value(result).unwrap();
			assert!(reply.ok);
		});
	}

	#[test]
	fn unsubscribe_unknown_topic_returns_1600() {
		with_registered_context(|ctx| {
			let params = serde_json::json!({ "topic": "absolutely_made_up" });
			let err =
				dispatch_request("subscription.unsubscribe", Some(params), ctx).unwrap_err();
			assert_eq!(err.code, error_codes::UNKNOWN_TOPIC);
		});
	}

	#[test]
	fn unsubscribe_never_subscribed_is_no_op() {
		with_registered_context(|ctx| {
			let params = serde_json::json!({ "topic": "roster_changed" });
			let result =
				dispatch_request("subscription.unsubscribe", Some(params), ctx).unwrap();
			let reply: OkReply = serde_json::from_value(result).unwrap();
			assert!(reply.ok);
		});
	}
}
