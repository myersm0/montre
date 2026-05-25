use std::io::{self, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::mpsc::{sync_channel, Receiver, Sender, SyncSender};
use std::sync::Arc;
use std::thread;

use crate::handlers::{alignment, corpus, coupler, daemon, query, session, subscription, text};
use crate::protocol::error_codes;
use crate::protocol::{
	ProcessId, ProtocolError, PublishInterestParams, RegisterParams,
};
use crate::state::{Command, Outbound};
use crate::CorpusHandle;
use crate::shutdown::ShutdownCoordinator;

pub(crate) const MAX_PAYLOAD_SIZE: usize = 16 * 1024 * 1024;
pub(crate) const OUTBOUND_QUEUE_DEPTH: usize = 256;
pub(crate) const STATE_REPLY_FAILURE: &str = "state reply channel lost";
pub(crate) const STATE_DISPATCH_FAILURE: &str = "state thread closed";

pub(crate) fn read_frame<R: Read>(reader: &mut R) -> io::Result<Vec<u8>> {
	let mut len_buf = [0u8; 4];
	reader.read_exact(&mut len_buf)?;
	let len = u32::from_be_bytes(len_buf) as usize;
	if len > MAX_PAYLOAD_SIZE {
		return Err(io::Error::new(
			io::ErrorKind::InvalidData,
			format!("frame size {} exceeds maximum {}", len, MAX_PAYLOAD_SIZE),
		));
	}
	let mut payload = vec![0u8; len];
	reader.read_exact(&mut payload)?;
	Ok(payload)
}

pub(crate) fn write_frame<W: Write>(writer: &mut W, payload: &[u8]) -> io::Result<()> {
	if payload.len() > MAX_PAYLOAD_SIZE {
		return Err(io::Error::new(
			io::ErrorKind::InvalidInput,
			format!("payload size {} exceeds maximum {}", payload.len(), MAX_PAYLOAD_SIZE),
		));
	}
	let len = u32::try_from(payload.len()).expect("checked above");
	writer.write_all(&len.to_be_bytes())?;
	writer.write_all(payload)?;
	writer.flush()?;
	Ok(())
}

#[derive(Debug, Clone)]
pub(crate) enum Inbound {
	Request {
		id: serde_json::Value,
		method: String,
		params: Option<serde_json::Value>,
	},
	Notification {
		method: String,
		params: Option<serde_json::Value>,
	},
}

pub(crate) fn parse_inbound(payload: &[u8]) -> Result<Inbound, ProtocolError> {
	let value: serde_json::Value = serde_json::from_slice(payload)
		.map_err(|e| ProtocolError::new(-32700, format!("Parse error: {}", e)))?;

	let object = value
		.as_object()
		.ok_or_else(|| ProtocolError::new(-32600, "Invalid Request: not a JSON object"))?;

	if object.get("jsonrpc").and_then(|v| v.as_str()) != Some("2.0") {
		return Err(ProtocolError::new(
			-32600,
			"Invalid Request: missing or invalid jsonrpc field",
		));
	}

	let method = object
		.get("method")
		.and_then(|v| v.as_str())
		.ok_or_else(|| ProtocolError::new(-32600, "Invalid Request: missing method"))?
		.to_string();

	let params = object.get("params").cloned();

	match object.get("id").cloned() {
		None => Ok(Inbound::Notification { method, params }),
		Some(id) => Ok(Inbound::Request { id, method, params }),
	}
}

pub(crate) fn build_response(id: serde_json::Value, result: serde_json::Value) -> serde_json::Value {
	serde_json::json!({
		"jsonrpc": "2.0",
		"id": id,
		"result": result,
	})
}

pub(crate) fn build_error_response(
	id: serde_json::Value,
	error: ProtocolError,
) -> serde_json::Value {
	let mut err = serde_json::json!({
		"code": error.code,
		"message": error.message,
	});
	if let Some(data) = error.data {
		err["data"] = data;
	}
	serde_json::json!({
		"jsonrpc": "2.0",
		"id": id,
		"error": err,
	})
}

pub(crate) fn parse_params<T: serde::de::DeserializeOwned>(
	method: &str,
	params: Option<serde_json::Value>,
) -> Result<T, ProtocolError> {
	let raw = params
		.ok_or_else(|| ProtocolError::new(-32602, format!("{} requires params", method)))?;
	serde_json::from_value(raw)
		.map_err(|e| ProtocolError::new(-32602, format!("invalid params: {}", e)))
}

pub(crate) fn parse_params_or_default<T: serde::de::DeserializeOwned + Default>(
	params: Option<serde_json::Value>,
) -> Result<T, ProtocolError> {
	match params {
		None => Ok(T::default()),
		Some(v) => serde_json::from_value(v)
			.map_err(|e| ProtocolError::new(-32602, format!("invalid params: {}", e))),
	}
}

pub(crate) fn serialize_reply<T: serde::Serialize>(
	value: T,
) -> Result<serde_json::Value, ProtocolError> {
	serde_json::to_value(value)
		.map_err(|e| ProtocolError::new(-32603, format!("response serialization failed: {}", e)))
}

pub(crate) fn state_roundtrip<R>(
	ctx: &RpcContext,
	build: impl FnOnce(SyncSender<R>) -> Command,
) -> Result<R, ProtocolError> {
	let (reply_tx, reply_rx) = sync_channel(1);
	ctx.state_tx
		.send(build(reply_tx))
		.map_err(|_| ProtocolError::new(-32603, STATE_DISPATCH_FAILURE))?;
	reply_rx
		.recv()
		.map_err(|_| ProtocolError::new(-32603, STATE_REPLY_FAILURE))
}

pub(crate) struct RpcContext {
	pub process_id: Option<ProcessId>,
	pub ever_registered: bool,
	pub state_tx: Sender<Command>,
	pub outbound_tx: SyncSender<Outbound>,
	pub handle: Arc<CorpusHandle>,
}

pub(crate) fn run_listener(
	socket_path: &Path,
	state_tx: Sender<Command>,
	handle: Arc<CorpusHandle>,
	coordinator: Arc<ShutdownCoordinator>,
) -> io::Result<()> {
	if socket_path.exists() {
		std::fs::remove_file(socket_path)?;
	}
	let listener = UnixListener::bind(socket_path)?;
	tracing::info!(socket = %socket_path.display(), "daemon listening");

	for stream in listener.incoming() {
		if coordinator.is_shutting_down() {
			break;
		}
		match stream {
			Ok(stream) => {
				if coordinator.is_shutting_down() {
					break;
				}
				if let Ok(clone) = stream.try_clone() {
					coordinator.register_stream(clone);
				}
				let tx = state_tx.clone();
				let handle = Arc::clone(&handle);
				thread::spawn(move || run_connection(stream, tx, handle));
			}
			Err(e) => {
				tracing::warn!(error = %e, "accept failed");
			}
		}
	}
	Ok(())
}

fn run_connection(
	stream: UnixStream,
	state_tx: Sender<Command>,
	handle: Arc<CorpusHandle>,
) {
	let read_stream = match stream.try_clone() {
		Ok(s) => s,
		Err(e) => {
			tracing::warn!(error = %e, "failed to clone connection stream");
			return;
		}
	};
	let write_stream = stream;

	let (outbound_tx, outbound_rx) = sync_channel::<Outbound>(OUTBOUND_QUEUE_DEPTH);

	let writer_handle = thread::spawn(move || run_writer(write_stream, outbound_rx));

	run_reader(read_stream, state_tx, outbound_tx, handle);

	let _ = writer_handle.join();
}

fn run_writer(mut stream: UnixStream, rx: Receiver<Outbound>) {
	while let Ok(value) = rx.recv() {
		let payload = match serde_json::to_vec(&value) {
			Ok(p) => p,
			Err(e) => {
				tracing::error!(error = %e, "failed to serialize outbound message");
				continue;
			}
		};
		if let Err(e) = write_frame(&mut stream, &payload) {
			tracing::warn!(error = %e, "writer: write failed; closing connection");
			break;
		}
	}
}

fn run_reader(
	mut stream: UnixStream,
	state_tx: Sender<Command>,
	outbound_tx: SyncSender<Outbound>,
	handle: Arc<CorpusHandle>,
) {
	let mut ctx = RpcContext {
		process_id: None,
		ever_registered: false,
		state_tx: state_tx.clone(),
		outbound_tx: outbound_tx.clone(),
		handle,
	};

	loop {
		let frame = match read_frame(&mut stream) {
			Ok(f) => f,
			Err(e) => {
				if e.kind() != io::ErrorKind::UnexpectedEof
					&& e.kind() != io::ErrorKind::ConnectionReset
				{
					tracing::warn!(error = %e, "reader: read failed");
				}
				break;
			}
		};

		match parse_inbound(&frame) {
			Err(error) => {
				let response = build_error_response(serde_json::Value::Null, error);
				let _ = outbound_tx.send(response);
			}
			Ok(Inbound::Request { id, method, params }) => {
				let response = match dispatch_request(&method, params, &mut ctx) {
					Ok(value) => build_response(id, value),
					Err(error) => build_error_response(id, error),
				};
				let _ = outbound_tx.send(response);
			}
			Ok(Inbound::Notification { method, params }) => {
				dispatch_notification(&method, params, &ctx);
			}
		}
	}

	if let Some(pid) = ctx.process_id {
		let (reply_tx, reply_rx) = sync_channel(1);
		if state_tx
			.send(Command::Unregister {
				process_id: pid,
				reply: reply_tx,
			})
			.is_ok()
		{
			let _ = reply_rx.recv();
		}
	}
}

pub(crate) fn dispatch_request(
	method: &str,
	params: Option<serde_json::Value>,
	ctx: &mut RpcContext,
) -> Result<serde_json::Value, ProtocolError> {
	if ctx.process_id.is_none() && method != "session.register" {
		return Err(ProtocolError::new(
			error_codes::NOT_REGISTERED,
			"session.register must be the first call on a connection",
		));
	}

	match method {
		"session.register" => handle_register(params, ctx),
		"session.unregister" => session::handle_session_unregister(ctx),
		"session.update_label" => session::handle_session_update_label(params, ctx),
		"session.roster" => session::handle_session_roster(params, ctx),
		"corpus.info" => corpus::handle_corpus_info(ctx),
		"corpus.documents" => corpus::handle_corpus_documents(params, ctx),
		"corpus.layer_info" => corpus::handle_corpus_layer_info(params, ctx),
		"text.surface" => text::handle_text_surface(params, ctx),
		"text.sentence" => text::handle_text_sentence(params, ctx),
		"text.sentences" => text::handle_text_sentences(params, ctx),
		"text.document" => text::handle_text_document(params, ctx),
		"text.annotations" => text::handle_text_annotations(params, ctx),
		"text.annotations_range" => text::handle_text_annotations_range(params, ctx),
		"alignment.list" => alignment::handle_alignment_list(ctx),
		"alignment.project" => alignment::handle_alignment_project(params, ctx),
		"query.execute" => query::handle_query_execute(params, ctx),
		"query.execute_count" => query::handle_query_execute_count(params, ctx),
		"query.hits" => query::handle_query_hits(params, ctx),
		"query.metadata" => query::handle_query_metadata(params, ctx),
		"query.save" => query::handle_query_save(params, ctx),
		"query.materialize" => query::handle_query_materialize(params, ctx),
		"query.load" => query::handle_query_load(params, ctx),
		"query.list_named" => query::handle_query_list_named(ctx),
		"query.delete_named" => query::handle_query_delete_named(params, ctx),
		"query.discard" => query::handle_query_discard(params, ctx),
		"coupler.create" => coupler::handle_coupler_create(params, ctx),
		"coupler.remove" => coupler::handle_coupler_remove(params, ctx),
		"coupler.list" => coupler::handle_coupler_list(params, ctx),
		"subscription.subscribe" => subscription::handle_subscription_subscribe(params, ctx),
		"subscription.unsubscribe" => subscription::handle_subscription_unsubscribe(params, ctx),
		"daemon.shutdown" => daemon::handle_daemon_shutdown(params, ctx),
		_ => Err(ProtocolError::new(
			-32601,
			format!("Method not found: {}", method),
		)),
	}
}

fn dispatch_notification(
	method: &str,
	params: Option<serde_json::Value>,
	ctx: &RpcContext,
) {
	let Some(process_id) = ctx.process_id else {
		tracing::warn!(method = method, "notification before registration; ignored");
		return;
	};
	match method {
		"session.publish_interest" => {
			let Some(raw) = params else {
				tracing::warn!(method = method, "publish_interest missing params; ignored");
				return;
			};
			let parsed: PublishInterestParams = match serde_json::from_value(raw) {
				Ok(p) => p,
				Err(e) => {
					tracing::warn!(method = method, error = %e, "publish_interest invalid params; ignored");
					return;
				}
			};
			if ctx
				.state_tx
				.send(Command::PublishInterest { process_id, interest: parsed.interest })
				.is_err()
			{
				tracing::warn!(method = method, "state dispatch failed; notification dropped");
			}
		}
		_ => {
			tracing::debug!(method = method, "unknown notification method; ignored");
		}
	}
}

fn handle_register(
	params: Option<serde_json::Value>,
	ctx: &mut RpcContext,
) -> Result<serde_json::Value, ProtocolError> {
	if ctx.ever_registered {
		return Err(ProtocolError::new(
			-32600,
			"connection has already registered; a connection accepts exactly one session.register",
		));
	}

	let parsed: RegisterParams = parse_params("session.register", params)?;

	let outbound = ctx.outbound_tx.clone();
	let reply = state_roundtrip(ctx, |tx| Command::Register {
		params: parsed,
		outbound,
		reply: tx,
	})??;
	ctx.process_id = Some(reply.process_id);
	ctx.ever_registered = true;

	serialize_reply(reply)
}

#[cfg(test)]
pub(crate) mod test_support {
	use super::{RpcContext, OUTBOUND_QUEUE_DEPTH};
	use crate::protocol::ProcessKind;
	use crate::state;
	use crate::state::{Command, Outbound};
	use crate::CorpusHandle;
	use montre_index::Corpus;
	use std::collections::HashMap;
	use std::path::{Path, PathBuf};
	use std::sync::atomic::{AtomicU64, Ordering};
	use std::sync::mpsc::{channel, sync_channel, Receiver, Sender, SyncSender};
	use std::sync::{Arc, OnceLock, RwLock};
	use std::thread;
	use tempfile::TempDir;

	pub fn corpus_fixture() -> (&'static Path, &'static Path) {
		static FIXTURE: OnceLock<(TempDir, PathBuf, PathBuf)> = OnceLock::new();
		let (_keep, path, canonical) = FIXTURE.get_or_init(|| {
			let temp = TempDir::new().expect("tempdir");
			let out = temp.path().join("corpus");
			let manifest = Path::new(env!("CARGO_MANIFEST_DIR"))
				.join("../../testdata/parallel/corpus.toml");
			montre_build::MultiCorpusBuilder::from_manifest(&manifest)
				.expect("manifest load")
				.build(&out)
				.expect("corpus build");
			let canonical = std::fs::canonicalize(&out).expect("canonicalize");
			(temp, out, canonical)
		});
		(path.as_path(), canonical.as_path())
	}

	pub fn make_handle() -> Arc<CorpusHandle> {
		static STATE_ROOT: OnceLock<TempDir> = OnceLock::new();
		static STATE_COUNTER: AtomicU64 = AtomicU64::new(0);

		let (path, canonical) = corpus_fixture();
		let corpus = Arc::new(montre_index::open(path).expect("corpus open"));

		let root = STATE_ROOT.get_or_init(|| TempDir::new().expect("state root tempdir"));
		let counter = STATE_COUNTER.fetch_add(1, Ordering::SeqCst);
		let state_dir = root.path().join(format!("test-{:08}", counter));
		std::fs::create_dir_all(&state_dir).expect("create state dir");

		Arc::new(CorpusHandle {
			corpus,
			corpus_id: "test-corpus-id".to_string(),
			canonical_path: canonical.to_path_buf(),
			state_dir,
			results: Arc::new(RwLock::new(HashMap::new())),
		})
	}

	pub fn make_context(
		state_tx: Sender<Command>,
		outbound_tx: SyncSender<Outbound>,
		handle: Arc<CorpusHandle>,
	) -> RpcContext {
		RpcContext {
			process_id: None,
			ever_registered: false,
			state_tx,
			outbound_tx,
			handle,
		}
	}

	pub fn make_registered_context(
		state_tx: Sender<Command>,
		outbound_tx: SyncSender<Outbound>,
		handle: Arc<CorpusHandle>,
	) -> RpcContext {
		let mut ctx = make_context(state_tx, outbound_tx, handle);
		let params = serde_json::json!({
			"protocol_version": 1,
			"kind": "external",
		});
		super::dispatch_request("session.register", Some(params), &mut ctx).expect("register");
		ctx
	}

	pub fn with_registered_context<F>(body: F)
	where
		F: FnOnce(&mut RpcContext),
	{
		let handle = make_handle();
		let st = state::State::new(1, Arc::clone(&handle), crate::shutdown::ShutdownCoordinator::dummy());
		let (state_tx, state_rx) = channel();
		let state_handle = thread::spawn(move || state::run(st, state_rx));
		{
			let (outbound_tx, _outbound_rx) = sync_channel(OUTBOUND_QUEUE_DEPTH);
			let mut ctx = make_registered_context(state_tx.clone(), outbound_tx, handle);
			body(&mut ctx);
		}
		drop(state_tx);
		let _ = state_handle.join();
	}

	pub fn with_state_thread<F>(body: F)
	where
		F: FnOnce(Sender<Command>, Arc<CorpusHandle>),
	{
		let handle = make_handle();
		let st = state::State::new(1, Arc::clone(&handle), crate::shutdown::ShutdownCoordinator::dummy());
		let (state_tx, state_rx) = channel();
		let state_handle = thread::spawn(move || state::run(st, state_rx));
		body(state_tx.clone(), handle);
		drop(state_tx);
		let _ = state_handle.join();
	}

	pub fn register_context(
		state_tx: Sender<Command>,
		handle: Arc<CorpusHandle>,
		kind: ProcessKind,
		provides: &[&str],
		consumes: &[&str],
	) -> (RpcContext, Receiver<Outbound>) {
		let (outbound_tx, outbound_rx) = sync_channel(OUTBOUND_QUEUE_DEPTH);
		let mut ctx = make_context(state_tx, outbound_tx, handle);
		let params = serde_json::json!({
			"protocol_version": 1,
			"kind": kind,
			"provides": provides,
			"consumes": consumes,
		});
		super::dispatch_request("session.register", Some(params), &mut ctx).expect("register");
		(ctx, outbound_rx)
	}

	pub fn find_doc_index(corpus: &Corpus, needle: &str) -> u32 {
		corpus
			.document_names()
			.iter()
			.position(|n| n.contains(needle))
			.map(|i| i as u32)
			.unwrap_or_else(|| panic!("no document containing '{}'", needle))
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::dispatch::test_support::{make_context, make_handle};
	use crate::protocol::RegisterReply;
	use crate::state;
	use std::io::Cursor;
	use std::sync::mpsc::{channel, sync_channel};

	#[test]
	fn framing_roundtrip() {
		let payload = b"hello world";
		let mut buf = Vec::new();
		write_frame(&mut buf, payload).unwrap();
		let read = read_frame(&mut Cursor::new(&buf)).unwrap();
		assert_eq!(&read[..], payload);
	}

	#[test]
	fn framing_roundtrip_empty_payload() {
		let mut buf = Vec::new();
		write_frame(&mut buf, b"").unwrap();
		let read = read_frame(&mut Cursor::new(&buf)).unwrap();
		assert_eq!(&read[..], b"");
	}

	#[test]
	fn framing_oversize_write_rejected() {
		let payload = vec![0u8; MAX_PAYLOAD_SIZE + 1];
		let mut buf = Vec::new();
		assert!(write_frame(&mut buf, &payload).is_err());
	}

	#[test]
	fn framing_oversize_length_header_rejected() {
		let mut buf = Vec::new();
		buf.extend_from_slice(&((MAX_PAYLOAD_SIZE + 1) as u32).to_be_bytes());
		assert!(read_frame(&mut Cursor::new(&buf)).is_err());
	}

	#[test]
	fn framing_eof_before_length() {
		let buf: Vec<u8> = Vec::new();
		assert!(read_frame(&mut Cursor::new(&buf)).is_err());
	}

	#[test]
	fn framing_partial_length() {
		let buf: Vec<u8> = vec![0, 0];
		assert!(read_frame(&mut Cursor::new(&buf)).is_err());
	}

	#[test]
	fn framing_truncated_payload() {
		let mut buf = Vec::new();
		buf.extend_from_slice(&10u32.to_be_bytes());
		buf.extend_from_slice(b"hi");
		assert!(read_frame(&mut Cursor::new(&buf)).is_err());
	}

	#[test]
	fn parse_request_with_numeric_id() {
		let json = br#"{"jsonrpc":"2.0","id":17,"method":"foo","params":{"x":1}}"#;
		match parse_inbound(json).unwrap() {
			Inbound::Request { id, method, params } => {
				assert_eq!(id, serde_json::json!(17));
				assert_eq!(method, "foo");
				assert_eq!(params, Some(serde_json::json!({"x": 1})));
			}
			_ => panic!("expected request"),
		}
	}

	#[test]
	fn parse_request_with_string_id() {
		let json = br#"{"jsonrpc":"2.0","id":"abc","method":"foo"}"#;
		match parse_inbound(json).unwrap() {
			Inbound::Request { id, .. } => assert_eq!(id, serde_json::json!("abc")),
			_ => panic!("expected request"),
		}
	}

	#[test]
	fn parse_request_with_null_id() {
		let json = br#"{"jsonrpc":"2.0","id":null,"method":"foo"}"#;
		match parse_inbound(json).unwrap() {
			Inbound::Request { id, .. } => assert_eq!(id, serde_json::Value::Null),
			_ => panic!("expected request when id is present even if null"),
		}
	}

	#[test]
	fn parse_notification_when_id_absent() {
		let json = br#"{"jsonrpc":"2.0","method":"foo"}"#;
		match parse_inbound(json).unwrap() {
			Inbound::Notification { method, .. } => assert_eq!(method, "foo"),
			_ => panic!("expected notification"),
		}
	}

	#[test]
	fn parse_rejects_missing_jsonrpc() {
		let json = br#"{"id":1,"method":"foo"}"#;
		assert!(parse_inbound(json).is_err());
	}

	#[test]
	fn parse_rejects_wrong_jsonrpc_version() {
		let json = br#"{"jsonrpc":"1.0","id":1,"method":"foo"}"#;
		assert!(parse_inbound(json).is_err());
	}

	#[test]
	fn parse_rejects_missing_method() {
		let json = br#"{"jsonrpc":"2.0","id":1}"#;
		assert!(parse_inbound(json).is_err());
	}

	#[test]
	fn parse_rejects_invalid_json() {
		let json = b"{not json";
		assert!(parse_inbound(json).is_err());
	}

	#[test]
	fn parse_rejects_non_object() {
		let json = b"[]";
		assert!(parse_inbound(json).is_err());
	}

	#[test]
	fn build_response_shape() {
		let response = build_response(serde_json::json!(7), serde_json::json!({"ok": true}));
		assert_eq!(response["jsonrpc"], "2.0");
		assert_eq!(response["id"], 7);
		assert_eq!(response["result"], serde_json::json!({"ok": true}));
	}

	#[test]
	fn build_error_response_omits_data_when_none() {
		let response = build_error_response(
			serde_json::json!(7),
			ProtocolError::new(1234, "boom"),
		);
		assert_eq!(response["error"]["code"], 1234);
		assert_eq!(response["error"]["message"], "boom");
		assert!(response["error"].get("data").is_none());
	}

	#[test]
	fn build_error_response_includes_data_when_set() {
		let response = build_error_response(
			serde_json::json!(7),
			ProtocolError::new(1234, "boom").with_data(serde_json::json!({"hint": "x"})),
		);
		assert_eq!(response["error"]["data"], serde_json::json!({"hint": "x"}));
	}

	#[test]
	fn dispatch_register_round_trip() {
		let handle = make_handle();
		let st = state::State::new(7, Arc::clone(&handle), crate::shutdown::ShutdownCoordinator::dummy());
		let (state_tx, state_rx) = channel();
		let state_handle = thread::spawn(move || state::run(st, state_rx));

		{
			let (outbound_tx, _outbound_rx) = sync_channel(OUTBOUND_QUEUE_DEPTH);
			let mut ctx = make_context(state_tx.clone(), outbound_tx, handle);

			let params = serde_json::json!({
				"protocol_version": 1,
				"kind": "external",
			});
			let result = dispatch_request("session.register", Some(params), &mut ctx).unwrap();
			let reply: RegisterReply = serde_json::from_value(result).unwrap();
			assert_eq!(reply.process_id, 1);
			assert_eq!(reply.daemon_epoch, 7);
			assert_eq!(ctx.process_id, Some(1));
		}

		drop(state_tx);
		let _ = state_handle.join();
	}

	#[test]
	fn dispatch_rejects_pre_register_methods() {
		let handle = make_handle();
		let st = state::State::new(1, Arc::clone(&handle), crate::shutdown::ShutdownCoordinator::dummy());
		let (state_tx, state_rx) = channel();
		let state_handle = thread::spawn(move || state::run(st, state_rx));

		{
			let (outbound_tx, _outbound_rx) = sync_channel(OUTBOUND_QUEUE_DEPTH);
			let mut ctx = make_context(state_tx.clone(), outbound_tx, handle);

			let err = dispatch_request("query.execute", None, &mut ctx).unwrap_err();
			assert_eq!(err.code, error_codes::NOT_REGISTERED);
		}

		drop(state_tx);
		let _ = state_handle.join();
	}

	#[test]
	fn dispatch_unknown_method_returns_method_not_found() {
		let handle = make_handle();
		let st = state::State::new(1, Arc::clone(&handle), crate::shutdown::ShutdownCoordinator::dummy());
		let (state_tx, state_rx) = channel();
		let state_handle = thread::spawn(move || state::run(st, state_rx));

		{
			let (outbound_tx, _outbound_rx) = sync_channel(OUTBOUND_QUEUE_DEPTH);
			let mut ctx = make_context(state_tx.clone(), outbound_tx, handle);

			let params = serde_json::json!({
				"protocol_version": 1,
				"kind": "external",
			});
			dispatch_request("session.register", Some(params), &mut ctx).unwrap();

			let err = dispatch_request("does.not.exist", None, &mut ctx).unwrap_err();
			assert_eq!(err.code, -32601);
		}

		drop(state_tx);
		let _ = state_handle.join();
	}

	#[test]
	fn dispatch_rejects_register_after_unregister() {
		let handle = make_handle();
		let st = state::State::new(1, Arc::clone(&handle), crate::shutdown::ShutdownCoordinator::dummy());
		let (state_tx, state_rx) = channel();
		let state_handle = thread::spawn(move || state::run(st, state_rx));

		{
			let (outbound_tx, _outbound_rx) = sync_channel(OUTBOUND_QUEUE_DEPTH);
			let mut ctx = make_context(state_tx.clone(), outbound_tx, handle);

			let params = serde_json::json!({
				"protocol_version": 1,
				"kind": "external",
			});
			dispatch_request("session.register", Some(params.clone()), &mut ctx)
				.expect("first register");
			assert!(ctx.process_id.is_some());
			assert!(ctx.ever_registered);

			dispatch_request("session.unregister", None, &mut ctx).expect("unregister");
			assert!(ctx.process_id.is_none(), "unregister should clear process_id");
			assert!(ctx.ever_registered, "ever_registered must remain set after unregister");

			let err = dispatch_request("session.register", Some(params), &mut ctx)
				.expect_err("second register should be rejected");
			assert_eq!(err.code, -32600);
			assert!(ctx.process_id.is_none(), "rejected register must not allocate a new process_id");
		}

		drop(state_tx);
		let _ = state_handle.join();
	}

	#[test]
	fn notification_before_registration_silently_dropped() {
		let handle = make_handle();
		let (state_tx, state_rx) = channel::<Command>();
		let (outbound_tx, _outbound_rx) = sync_channel(OUTBOUND_QUEUE_DEPTH);
		let ctx = make_context(state_tx, outbound_tx, handle);

		let params = serde_json::json!({
			"interest": { "type": "sentence", "doc": 0, "sent": 0 }
		});
		dispatch_notification("session.publish_interest", Some(params), &ctx);

		assert!(state_rx.try_recv().is_err(), "no command should reach state when unregistered");
	}

	#[test]
	fn notification_unknown_method_silently_dropped() {
		let handle = make_handle();
		let (state_tx, state_rx) = channel::<Command>();
		let (outbound_tx, _outbound_rx) = sync_channel(OUTBOUND_QUEUE_DEPTH);
		let mut ctx = make_context(state_tx, outbound_tx, handle);
		ctx.process_id = Some(42);

		dispatch_notification("session.totally_bogus", Some(serde_json::json!({})), &ctx);

		assert!(state_rx.try_recv().is_err(), "no command should be dispatched for unknown method");
	}

	#[test]
	fn publish_interest_sentence_mirror_widens_position_for_follower() {
		use crate::dispatch::test_support::{register_context, with_state_thread};
		use crate::protocol::ProcessKind;
		use std::time::Duration;

		with_state_thread(|state_tx, handle| {
			let (mut master_ctx, _master_outbound) = register_context(
				state_tx.clone(),
				Arc::clone(&handle),
				ProcessKind::External,
				&["position", "span", "sentence"],
				&[],
			);
			let (follower_ctx, follower_outbound) = register_context(
				state_tx.clone(),
				Arc::clone(&handle),
				ProcessKind::External,
				&[],
				&["sentence"],
			);

			let master_pid = master_ctx.process_id.unwrap();
			let follower_pid = follower_ctx.process_id.unwrap();

			let coupler_params = serde_json::json!({
				"master_id": master_pid,
				"follower_id": follower_pid,
				"kind": { "type": "sentence_mirror" }
			});
			let reply = dispatch_request("coupler.create", Some(coupler_params), &mut master_ctx)
				.expect("coupler.create");
			let coupler_id = reply["coupler_id"].as_u64().expect("coupler_id");

			let source_doc = crate::dispatch::test_support::find_doc_index(
				&handle.corpus,
				"la_maison",
			);
			let sent_params = serde_json::json!({ "doc": source_doc, "sent": 0 });
			let sent_reply = dispatch_request("text.sentence", Some(sent_params), &mut master_ctx)
				.expect("text.sentence");
			let position = sent_reply["span"]["start"].as_u64().expect("span.start");

			let publish_params = serde_json::json!({
				"interest": { "type": "position", "doc": source_doc, "position": position }
			});
			dispatch_notification("session.publish_interest", Some(publish_params), &master_ctx);

			let payload = follower_outbound
				.recv_timeout(Duration::from_millis(500))
				.expect("follower should receive a notification");
			assert_eq!(payload["jsonrpc"], "2.0");
			assert_eq!(payload["method"], "notification.coupler_update");
			assert_eq!(payload["params"]["coupler_id"], coupler_id);
			assert_eq!(payload["params"]["interest"]["type"], "sentence");
			assert_eq!(payload["params"]["interest"]["doc"], source_doc);
			assert_eq!(payload["params"]["interest"]["sent"], 0);
		});
	}

	#[test]
	fn publish_interest_alignment_fans_out_to_follower() {
		use crate::dispatch::test_support::{register_context, with_state_thread};
		use crate::protocol::ProcessKind;
		use std::time::Duration;

		with_state_thread(|state_tx, handle| {
			let (mut master_ctx, _master_outbound) = register_context(
				state_tx.clone(),
				Arc::clone(&handle),
				ProcessKind::External,
				&["sentence", "span"],
				&[],
			);
			let (follower_ctx, follower_outbound) = register_context(
				state_tx.clone(),
				Arc::clone(&handle),
				ProcessKind::External,
				&[],
				&["span"],
			);

			let master_pid = master_ctx.process_id.unwrap();
			let follower_pid = follower_ctx.process_id.unwrap();

			let coupler_params = serde_json::json!({
				"master_id": master_pid,
				"follower_id": follower_pid,
				"kind": { "type": "alignment", "name": "sentence" }
			});
			let reply = dispatch_request("coupler.create", Some(coupler_params), &mut master_ctx)
				.expect("coupler.create");
			let coupler_id = reply["coupler_id"].as_u64().expect("coupler_id");

			let source_doc = crate::dispatch::test_support::find_doc_index(
				&handle.corpus,
				"la_maison",
			);
			let target_doc = crate::dispatch::test_support::find_doc_index(
				&handle.corpus,
				"the_house",
			);

			let sent_params = serde_json::json!({ "doc": source_doc, "sent": 0 });
			let sent_reply = dispatch_request("text.sentence", Some(sent_params), &mut master_ctx)
				.expect("text.sentence");
			let span_start = sent_reply["span"]["start"].as_u64().expect("span.start");
			let span_end = sent_reply["span"]["end"].as_u64().expect("span.end");

			let project_params = serde_json::json!({
				"source": { "doc": source_doc, "start": span_start, "end": span_end },
				"alignment_name": "sentence",
			});
			let project_reply =
				dispatch_request("alignment.project", Some(project_params), &mut master_ctx)
					.expect("alignment.project");
			let expected_count = project_reply["targets"]
				.as_array()
				.expect("targets array")
				.len();
			assert!(expected_count > 0, "test corpus must align this sentence to at least one target");

			let publish_params = serde_json::json!({
				"interest": { "type": "sentence", "doc": source_doc, "sent": 0 }
			});
			dispatch_notification("session.publish_interest", Some(publish_params), &master_ctx);

			let mut received = Vec::new();
			let first = follower_outbound
				.recv_timeout(Duration::from_millis(500))
				.expect("follower should receive at least one notification");
			received.push(first);
			while let Ok(msg) = follower_outbound.recv_timeout(Duration::from_millis(50)) {
				received.push(msg);
			}

			assert_eq!(
				received.len(),
				expected_count,
				"one notification per projected target",
			);
			for payload in &received {
				assert_eq!(payload["jsonrpc"], "2.0");
				assert_eq!(payload["method"], "notification.coupler_update");
				assert_eq!(payload["params"]["coupler_id"], coupler_id);
				assert_eq!(payload["params"]["interest"]["type"], "span");
				assert_eq!(payload["params"]["interest"]["doc"], target_doc);
			}
		});
	}
}
