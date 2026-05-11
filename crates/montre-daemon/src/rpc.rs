use std::io::{self, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{sync_channel, Receiver, Sender, SyncSender};
use std::sync::{Arc, RwLock};
use std::thread;

use montre_index::{Corpus, ForwardIndex, InvertedIndex, SpanIndex};
use montre_query::QueryError;

use crate::protocol::error_codes;
use crate::protocol::{
	AlignmentInfo, AlignmentListReply, AlignmentProjectParams, AlignmentProjectReply,
	AlignmentTarget, AnnotationEntry, AnnotationRow, AnnotationValue, CorpusDocumentsParams,
	CorpusDocumentsReply, CorpusInfo, CorpusLayerInfoParams, DocumentEntry, Hit, LayerInfo,
	LayerKind, ProcessId, ProtocolError, QueryExecuteCountReply, QueryExecuteParams,
	QueryExecuteReply, QueryHitsParams, QueryHitsReply, QueryMetadataParams, RegisterParams,
	RegisterReply, SentenceEntry, Span, TextAnnotationsParams, TextAnnotationsRangeParams,
	TextAnnotationsRangeReply, TextAnnotationsReply, TextDocumentParams, TextDocumentReply,
	TextSentenceParams, TextSentenceReply, TextSentencesParams, TextSentencesReply,
	TextSurfaceParams, TextSurfaceReply,
};
use crate::state::{Command, Outbound, ResultsTable};

pub(crate) const MAX_PAYLOAD_SIZE: usize = 16 * 1024 * 1024;
const OUTBOUND_QUEUE_DEPTH: usize = 256;
const MAX_HITS_PER_PAGE: u64 = 1000;
const STATE_REPLY_FAILURE: &str = "state reply channel lost";
const STATE_DISPATCH_FAILURE: &str = "state thread closed";

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

pub(crate) struct RpcContext {
	pub process_id: Option<ProcessId>,
	pub state_tx: Sender<Command>,
	pub outbound_tx: SyncSender<Outbound>,
	pub corpus: Arc<Corpus>,
	pub corpus_id: String,
	pub canonical_path: PathBuf,
	#[allow(dead_code)]
	pub results: Arc<RwLock<ResultsTable>>,
}

pub(crate) fn run_listener(
	socket_path: &Path,
	state_tx: Sender<Command>,
	corpus: Arc<Corpus>,
	results: Arc<RwLock<ResultsTable>>,
	corpus_id: String,
	canonical_path: PathBuf,
) -> io::Result<()> {
	if socket_path.exists() {
		std::fs::remove_file(socket_path)?;
	}
	let listener = UnixListener::bind(socket_path)?;
	tracing::info!(socket = %socket_path.display(), "daemon listening");

	for stream in listener.incoming() {
		match stream {
			Ok(stream) => {
				let tx = state_tx.clone();
				let corpus = Arc::clone(&corpus);
				let results = Arc::clone(&results);
				let corpus_id = corpus_id.clone();
				let canonical_path = canonical_path.clone();
				thread::spawn(move || {
					run_connection(stream, tx, corpus, results, corpus_id, canonical_path)
				});
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
	corpus: Arc<Corpus>,
	results: Arc<RwLock<ResultsTable>>,
	corpus_id: String,
	canonical_path: PathBuf,
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

	run_reader(
		read_stream,
		state_tx,
		outbound_tx,
		corpus,
		results,
		corpus_id,
		canonical_path,
	);

	let _ = writer_handle.join();
}

fn run_writer(mut stream: UnixStream, rx: Receiver<Outbound>) {
	while let Ok(Outbound::Message(value)) = rx.recv() {
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
	corpus: Arc<Corpus>,
	results: Arc<RwLock<ResultsTable>>,
	corpus_id: String,
	canonical_path: PathBuf,
) {
	let mut ctx = RpcContext {
		process_id: None,
		state_tx: state_tx.clone(),
		outbound_tx: outbound_tx.clone(),
		corpus,
		corpus_id,
		canonical_path,
		results,
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
				let _ = outbound_tx.send(Outbound::Message(response));
			}
			Ok(Inbound::Request { id, method, params }) => {
				let response = match dispatch_request(&method, params, &mut ctx) {
					Ok(value) => build_response(id, value),
					Err(error) => build_error_response(id, error),
				};
				let _ = outbound_tx.send(Outbound::Message(response));
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
		"corpus.info" => handle_corpus_info(ctx),
		"corpus.documents" => handle_corpus_documents(params, ctx),
		"corpus.layer_info" => handle_corpus_layer_info(params, ctx),
		"text.surface" => handle_text_surface(params, ctx),
		"text.sentence" => handle_text_sentence(params, ctx),
		"text.sentences" => handle_text_sentences(params, ctx),
		"text.document" => handle_text_document(params, ctx),
		"text.annotations" => handle_text_annotations(params, ctx),
		"text.annotations_range" => handle_text_annotations_range(params, ctx),
		"alignment.list" => handle_alignment_list(ctx),
		"alignment.project" => handle_alignment_project(params, ctx),
		"query.execute" => handle_query_execute(params, ctx),
		"query.execute_count" => handle_query_execute_count(params, ctx),
		"query.hits" => handle_query_hits(params, ctx),
		"query.metadata" => handle_query_metadata(params, ctx),
		_ => Err(ProtocolError::new(
			-32601,
			format!("Method not found: {}", method),
		)),
	}
}

fn dispatch_notification(
	method: &str,
	_params: Option<serde_json::Value>,
	ctx: &RpcContext,
) {
	if ctx.process_id.is_none() {
		tracing::warn!(method = method, "notification before registration; ignored");
		return;
	}
	tracing::debug!(method = method, "notification handler not yet wired");
}

fn handle_register(
	params: Option<serde_json::Value>,
	ctx: &mut RpcContext,
) -> Result<serde_json::Value, ProtocolError> {
	if ctx.process_id.is_some() {
		return Err(ProtocolError::new(
			-32600,
			"connection is already registered",
		));
	}

	let raw = params
		.ok_or_else(|| ProtocolError::new(-32602, "session.register requires params"))?;
	let parsed: RegisterParams = serde_json::from_value(raw)
		.map_err(|e| ProtocolError::new(-32602, format!("invalid params: {}", e)))?;

	let (reply_tx, reply_rx) = sync_channel(1);
	ctx.state_tx
		.send(Command::Register {
			params: parsed,
			outbound: ctx.outbound_tx.clone(),
			reply: reply_tx,
		})
		.map_err(|_| ProtocolError::new(-32603, STATE_DISPATCH_FAILURE))?;

	let outcome: Result<RegisterReply, ProtocolError> = reply_rx
		.recv()
		.map_err(|_| ProtocolError::new(-32603, STATE_REPLY_FAILURE))?;
	let reply = outcome?;
	ctx.process_id = Some(reply.process_id);

	serde_json::to_value(reply)
		.map_err(|e| ProtocolError::new(-32603, format!("response serialization failed: {}", e)))
}

fn handle_corpus_info(ctx: &RpcContext) -> Result<serde_json::Value, ProtocolError> {
	let components = ctx
		.corpus
		.components()
		.iter()
		.map(|c| c.name.clone())
		.collect();
	let alignments = ctx
		.corpus
		.alignments()
		.iter()
		.map(|a| a.name.clone())
		.collect();
	let info = CorpusInfo {
		name: ctx.corpus.name().to_string(),
		canonical_path: ctx.canonical_path.display().to_string(),
		stable_key: ctx.corpus_id.clone(),
		components,
		layers: ctx.corpus.layers().to_vec(),
		alignments,
	};
	serde_json::to_value(info)
		.map_err(|e| ProtocolError::new(-32603, format!("response serialization failed: {}", e)))
}

fn handle_corpus_documents(
	params: Option<serde_json::Value>,
	ctx: &RpcContext,
) -> Result<serde_json::Value, ProtocolError> {
	let parsed: CorpusDocumentsParams = match params {
		None => CorpusDocumentsParams::default(),
		Some(v) => serde_json::from_value(v)
			.map_err(|e| ProtocolError::new(-32602, format!("invalid params: {}", e)))?,
	};

	let allowed_range = match parsed.component.as_deref() {
		None => None,
		Some(name) => {
			let component = ctx.corpus.component(name).ok_or_else(|| {
				ProtocolError::new(-32602, format!("unknown component '{}'", name))
			})?;
			Some(component.document_range)
		}
	};

	let document_names = ctx.corpus.document_names();
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

	let reply = CorpusDocumentsReply { documents };
	serde_json::to_value(reply)
		.map_err(|e| ProtocolError::new(-32603, format!("response serialization failed: {}", e)))
}

fn document_component(ctx: &RpcContext, document_index: usize) -> String {
	ctx.corpus
		.component_for_document(document_index)
		.map(|c| c.name.clone())
		.unwrap_or_else(|| ctx.corpus.name().to_string())
}

fn document_sentence_count(ctx: &RpcContext, document_index: usize) -> u32 {
	match (
		ctx.corpus.first_sentence_of_document(document_index),
		ctx.corpus.last_sentence_of_document(document_index),
	) {
		(Some(first), Some(last)) if last >= first => (last - first + 1) as u32,
		_ => 0,
	}
}

fn handle_corpus_layer_info(
	params: Option<serde_json::Value>,
	ctx: &RpcContext,
) -> Result<serde_json::Value, ProtocolError> {
	let raw = params
		.ok_or_else(|| ProtocolError::new(-32602, "corpus.layer_info requires params"))?;
	let parsed: CorpusLayerInfoParams = serde_json::from_value(raw)
		.map_err(|e| ProtocolError::new(-32602, format!("invalid params: {}", e)))?;

	let (kind, value_count) = classify_layer(&ctx.corpus, &parsed.layer).ok_or_else(|| {
		ProtocolError::new(-32602, format!("unknown layer '{}'", parsed.layer))
	})?;

	let info = LayerInfo {
		name: parsed.layer,
		kind,
		value_count,
	};
	serde_json::to_value(info)
		.map_err(|e| ProtocolError::new(-32603, format!("response serialization failed: {}", e)))
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

fn handle_text_surface(
	params: Option<serde_json::Value>,
	ctx: &RpcContext,
) -> Result<serde_json::Value, ProtocolError> {
	let raw = params
		.ok_or_else(|| ProtocolError::new(-32602, "text.surface requires params"))?;
	let parsed: TextSurfaceParams = serde_json::from_value(raw)
		.map_err(|e| ProtocolError::new(-32602, format!("invalid params: {}", e)))?;

	if parsed.start > parsed.end {
		return Err(ProtocolError::new(-32602, "start must be <= end"));
	}
	if parsed.end > ctx.corpus.token_count() {
		return Err(ProtocolError::new(
			-32602,
			format!("end {} exceeds token count {}", parsed.end, ctx.corpus.token_count()),
		));
	}

	let reply = TextSurfaceReply {
		surface: ctx.corpus.surface_text(parsed.start, parsed.end),
	};
	serde_json::to_value(reply)
		.map_err(|e| ProtocolError::new(-32603, format!("response serialization failed: {}", e)))
}

fn handle_text_sentence(
	params: Option<serde_json::Value>,
	ctx: &RpcContext,
) -> Result<serde_json::Value, ProtocolError> {
	let raw = params
		.ok_or_else(|| ProtocolError::new(-32602, "text.sentence requires params"))?;
	let parsed: TextSentenceParams = serde_json::from_value(raw)
		.map_err(|e| ProtocolError::new(-32602, format!("invalid params: {}", e)))?;

	let (global_idx, span) =
		sentence_span_in_doc(&ctx.corpus, parsed.doc, parsed.sent).ok_or_else(|| {
			ProtocolError::new(
				-32602,
				format!(
					"sentence {} not found in document {}",
					parsed.sent, parsed.doc
				),
			)
		})?;

	let reply = TextSentenceReply {
		surface: ctx.corpus.surface_text(span.start, span.end),
		sentence_id: ctx
			.corpus
			.sentence_id(global_idx)
			.map(String::from)
			.unwrap_or_default(),
		span,
	};
	serde_json::to_value(reply)
		.map_err(|e| ProtocolError::new(-32603, format!("response serialization failed: {}", e)))
}

fn handle_text_sentences(
	params: Option<serde_json::Value>,
	ctx: &RpcContext,
) -> Result<serde_json::Value, ProtocolError> {
	let raw = params
		.ok_or_else(|| ProtocolError::new(-32602, "text.sentences requires params"))?;
	let parsed: TextSentencesParams = serde_json::from_value(raw)
		.map_err(|e| ProtocolError::new(-32602, format!("invalid params: {}", e)))?;

	if parsed.sent_start > parsed.sent_end {
		return Err(ProtocolError::new(-32602, "sent_start must be <= sent_end"));
	}

	let doc_idx = parsed.doc as usize;
	if doc_idx >= document_count(&ctx.corpus) {
		return Err(ProtocolError::new(
			-32602,
			format!("document index {} out of range", parsed.doc),
		));
	}

	let first = ctx
		.corpus
		.first_sentence_of_document(doc_idx)
		.ok_or_else(|| ProtocolError::new(-32602, "document has no sentences"))?;
	let last = ctx
		.corpus
		.last_sentence_of_document(doc_idx)
		.ok_or_else(|| ProtocolError::new(-32602, "document has no sentences"))?;
	let doc_sentence_count = (last - first + 1) as u32;
	if parsed.sent_end > doc_sentence_count {
		return Err(ProtocolError::new(
			-32602,
			format!(
				"sent_end {} exceeds document sentence count {}",
				parsed.sent_end, doc_sentence_count
			),
		));
	}

	let sentence_spans = ctx
		.corpus
		.spans()
		.spans("sentence")
		.ok_or_else(|| ProtocolError::new(-32603, "sentence span layer missing"))?;

	let mut sentences = Vec::with_capacity((parsed.sent_end - parsed.sent_start) as usize);
	for sent in parsed.sent_start..parsed.sent_end {
		let global_idx = first + sent as usize;
		let span = sentence_spans
			.get(global_idx)
			.cloned()
			.ok_or_else(|| ProtocolError::new(-32603, "sentence span lookup failed"))?;
		sentences.push(SentenceEntry {
			sent,
			surface: ctx.corpus.surface_text(span.start, span.end),
			sentence_id: ctx
				.corpus
				.sentence_id(global_idx)
				.map(String::from)
				.unwrap_or_default(),
			span,
		});
	}

	let reply = TextSentencesReply { sentences };
	serde_json::to_value(reply)
		.map_err(|e| ProtocolError::new(-32603, format!("response serialization failed: {}", e)))
}

fn handle_text_document(
	params: Option<serde_json::Value>,
	ctx: &RpcContext,
) -> Result<serde_json::Value, ProtocolError> {
	let raw = params
		.ok_or_else(|| ProtocolError::new(-32602, "text.document requires params"))?;
	let parsed: TextDocumentParams = serde_json::from_value(raw)
		.map_err(|e| ProtocolError::new(-32602, format!("invalid params: {}", e)))?;

	let doc_idx = parsed.doc as usize;
	let names = ctx.corpus.document_names();
	let name = names.get(doc_idx).cloned().ok_or_else(|| {
		ProtocolError::new(-32602, format!("document index {} out of range", parsed.doc))
	})?;

	let document_spans = ctx
		.corpus
		.spans()
		.spans("document")
		.ok_or_else(|| ProtocolError::new(-32603, "document span layer missing"))?;
	let span = document_spans
		.get(doc_idx)
		.cloned()
		.ok_or_else(|| ProtocolError::new(-32603, "document span lookup failed"))?;

	let reply = TextDocumentReply {
		index: parsed.doc,
		name,
		component: document_component(ctx, doc_idx),
		span,
		sentence_count: document_sentence_count(ctx, doc_idx),
	};
	serde_json::to_value(reply)
		.map_err(|e| ProtocolError::new(-32603, format!("response serialization failed: {}", e)))
}

fn handle_text_annotations(
	params: Option<serde_json::Value>,
	ctx: &RpcContext,
) -> Result<serde_json::Value, ProtocolError> {
	let raw = params
		.ok_or_else(|| ProtocolError::new(-32602, "text.annotations requires params"))?;
	let parsed: TextAnnotationsParams = serde_json::from_value(raw)
		.map_err(|e| ProtocolError::new(-32602, format!("invalid params: {}", e)))?;

	let mut values = Vec::new();
	for &position in &parsed.positions {
		for layer in &parsed.layers {
			if let Some(value) = fetch_annotation(&ctx.corpus, position, layer) {
				values.push(AnnotationEntry {
					position,
					layer: layer.clone(),
					value,
				});
			}
		}
	}

	let reply = TextAnnotationsReply { values };
	serde_json::to_value(reply)
		.map_err(|e| ProtocolError::new(-32603, format!("response serialization failed: {}", e)))
}

fn handle_text_annotations_range(
	params: Option<serde_json::Value>,
	ctx: &RpcContext,
) -> Result<serde_json::Value, ProtocolError> {
	let raw = params
		.ok_or_else(|| ProtocolError::new(-32602, "text.annotations_range requires params"))?;
	let parsed: TextAnnotationsRangeParams = serde_json::from_value(raw)
		.map_err(|e| ProtocolError::new(-32602, format!("invalid params: {}", e)))?;

	if parsed.start > parsed.end {
		return Err(ProtocolError::new(-32602, "start must be <= end"));
	}
	if parsed.end > ctx.corpus.token_count() {
		return Err(ProtocolError::new(
			-32602,
			format!("end {} exceeds token count {}", parsed.end, ctx.corpus.token_count()),
		));
	}

	let layers: Vec<String> = match parsed.layers {
		Some(ls) => ls,
		None => ctx.corpus.layers().to_vec(),
	};

	let mut rows = Vec::with_capacity((parsed.end - parsed.start) as usize);
	for position in parsed.start..parsed.end {
		let mut values = std::collections::HashMap::new();
		for layer in &layers {
			if let Some(value) = fetch_annotation(&ctx.corpus, position, layer) {
				values.insert(layer.clone(), value);
			}
		}
		rows.push(AnnotationRow { position, values });
	}

	let reply = TextAnnotationsRangeReply { rows };
	serde_json::to_value(reply)
		.map_err(|e| ProtocolError::new(-32603, format!("response serialization failed: {}", e)))
}

fn handle_alignment_list(ctx: &RpcContext) -> Result<serde_json::Value, ProtocolError> {
	let alignments: Vec<AlignmentInfo> = ctx
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

fn handle_alignment_project(
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

	let alignment = ctx.corpus.alignment_meta(&parsed.alignment_name).ok_or_else(|| {
		ProtocolError::new(
			error_codes::ALIGNMENT_NOT_FOUND,
			format!("alignment '{}' not found", parsed.alignment_name),
		)
	})?;

	let source_comp = ctx
		.corpus
		.component(&alignment.source_component)
		.ok_or_else(|| {
			ProtocolError::new(
				-32603,
				format!("source component '{}' not found", alignment.source_component),
			)
		})?;
	let target_comp = ctx
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

	let first_sent = ctx
		.corpus
		.first_sentence_of_document(doc)
		.ok_or_else(|| ProtocolError::new(-32603, "source document has no sentences"))?;
	let last_sent = ctx
		.corpus
		.last_sentence_of_document(doc)
		.ok_or_else(|| ProtocolError::new(-32603, "source document has no sentences"))?;
	let sentence_spans = ctx
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

	let edges = ctx
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
			let target_first_sent = ctx
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

fn handle_query_execute(
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
		montre_query::executor::execute(&plan, &ctx.corpus).map_err(map_query_error)?;
	results.populate_context(&ctx.corpus);
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

fn handle_query_execute_count(
	params: Option<serde_json::Value>,
	ctx: &RpcContext,
) -> Result<serde_json::Value, ProtocolError> {
	let raw = params
		.ok_or_else(|| ProtocolError::new(-32602, "query.execute_count requires params"))?;
	let parsed: QueryExecuteParams = serde_json::from_value(raw)
		.map_err(|e| ProtocolError::new(-32602, format!("invalid params: {}", e)))?;

	let ast = montre_query::parse(&parsed.cql).map_err(map_query_error)?;
	let plan = montre_query::planner::plan(&ast).map_err(map_query_error)?;
	let count = montre_query::executor::execute_count(&plan, &ctx.corpus)
		.map_err(map_query_error)?;

	let reply = QueryExecuteCountReply {
		count: count as u64,
	};
	serde_json::to_value(reply)
		.map_err(|e| ProtocolError::new(-32603, format!("response serialization failed: {}", e)))
}

fn handle_query_hits(
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
		let table = ctx.results.read().expect("results lock poisoned");
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

fn handle_query_metadata(
	params: Option<serde_json::Value>,
	ctx: &RpcContext,
) -> Result<serde_json::Value, ProtocolError> {
	let raw = params
		.ok_or_else(|| ProtocolError::new(-32602, "query.metadata requires params"))?;
	let parsed: QueryMetadataParams = serde_json::from_value(raw)
		.map_err(|e| ProtocolError::new(-32602, format!("invalid params: {}", e)))?;

	let entry = {
		let table = ctx.results.read().expect("results lock poisoned");
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
	use crate::state;
	use std::collections::HashMap;
	use std::io::Cursor;
	use std::path::PathBuf;
	use std::sync::mpsc::channel;
	use std::sync::OnceLock;
	use tempfile::TempDir;

	fn corpus_fixture() -> (&'static Path, &'static Path) {
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

	fn open_test_corpus() -> (Arc<Corpus>, PathBuf) {
		let (path, canonical) = corpus_fixture();
		let corpus = Arc::new(montre_index::open(path).expect("corpus open"));
		(corpus, canonical.to_path_buf())
	}

	fn make_context(
		state_tx: Sender<Command>,
		outbound_tx: SyncSender<Outbound>,
		results: Arc<RwLock<ResultsTable>>,
	) -> RpcContext {
		let (corpus, canonical_path) = open_test_corpus();
		RpcContext {
			process_id: None,
			state_tx,
			outbound_tx,
			corpus,
			corpus_id: "test-corpus-id".to_string(),
			canonical_path,
			results,
		}
	}

	fn make_registered_context(
		state_tx: Sender<Command>,
		outbound_tx: SyncSender<Outbound>,
		results: Arc<RwLock<ResultsTable>>,
	) -> RpcContext {
		let mut ctx = make_context(state_tx, outbound_tx, results);
		let params = serde_json::json!({
			"protocol_version": 1,
			"kind": "external",
		});
		dispatch_request("session.register", Some(params), &mut ctx).expect("register");
		ctx
	}

	fn with_registered_context<F>(body: F)
	where
		F: FnOnce(&mut RpcContext),
	{
		let st = state::State::new("test".to_string(), 1);
		let results = st.results();
		let (state_tx, state_rx) = channel();
		let state_handle = thread::spawn(move || state::run(st, state_rx));
		{
			let (outbound_tx, _outbound_rx) = sync_channel(OUTBOUND_QUEUE_DEPTH);
			let mut ctx = make_registered_context(state_tx.clone(), outbound_tx, results);
			body(&mut ctx);
		}
		drop(state_tx);
		let _ = state_handle.join();
	}

	fn find_doc_index(corpus: &Corpus, needle: &str) -> u32 {
		corpus
			.document_names()
			.iter()
			.position(|n| n.contains(needle))
			.map(|i| i as u32)
			.unwrap_or_else(|| panic!("no document containing '{}'", needle))
	}

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
		let st = state::State::new("test".to_string(), 7);
		let results = st.results();
		let (state_tx, state_rx) = channel();
		let state_handle = thread::spawn(move || state::run(st, state_rx));

		{
			let (outbound_tx, _outbound_rx) = sync_channel(OUTBOUND_QUEUE_DEPTH);
			let mut ctx = make_context(state_tx.clone(), outbound_tx, results);

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
		let st = state::State::new("test".to_string(), 1);
		let results = st.results();
		let (state_tx, state_rx) = channel();
		let state_handle = thread::spawn(move || state::run(st, state_rx));

		{
			let (outbound_tx, _outbound_rx) = sync_channel(OUTBOUND_QUEUE_DEPTH);
			let mut ctx = make_context(state_tx.clone(), outbound_tx, results);

			let err = dispatch_request("query.execute", None, &mut ctx).unwrap_err();
			assert_eq!(err.code, error_codes::NOT_REGISTERED);
		}

		drop(state_tx);
		let _ = state_handle.join();
	}

	#[test]
	fn dispatch_unknown_method_returns_method_not_found() {
		let st = state::State::new("test".to_string(), 1);
		let results = st.results();
		let (state_tx, state_rx) = channel();
		let state_handle = thread::spawn(move || state::run(st, state_rx));

		{
			let (outbound_tx, _outbound_rx) = sync_channel(OUTBOUND_QUEUE_DEPTH);
			let mut ctx = make_context(state_tx.clone(), outbound_tx, results);

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
	fn corpus_info_returns_expected_shape() {
		with_registered_context(|ctx| {
			let result = dispatch_request("corpus.info", None, ctx).unwrap();
			let info: CorpusInfo = serde_json::from_value(result).unwrap();

			assert_eq!(info.name, "test-parallel");
			assert_eq!(info.stable_key, ctx.corpus_id);
			assert_eq!(info.canonical_path, ctx.canonical_path.display().to_string());

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
	fn text_sentence_returns_sentence_data() {
		with_registered_context(|ctx| {
			let doc = find_doc_index(&ctx.corpus, "la_maison");
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
	fn text_sentences_exceeding_count_rejected() {
		with_registered_context(|ctx| {
			let params = serde_json::json!({ "doc": 0, "sent_start": 0, "sent_end": 999 });
			let err = dispatch_request("text.sentences", Some(params), ctx).unwrap_err();
			assert_eq!(err.code, -32602);
		});
	}

	#[test]
	fn text_document_returns_metadata() {
		with_registered_context(|ctx| {
			let doc = find_doc_index(&ctx.corpus, "la_maison");
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
			let doc = find_doc_index(&ctx.corpus, "la_maison");
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
			let source_doc = find_doc_index(&ctx.corpus, "la_maison");
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
			let expected_target_doc = find_doc_index(&ctx.corpus, "the_house");
			assert_eq!(reply.targets[0].doc, expected_target_doc);
			assert!(reply.targets[0].end > reply.targets[0].start);
		});
	}

	#[test]
	fn alignment_project_unknown_alignment_rejected() {
		with_registered_context(|ctx| {
			let source_doc = find_doc_index(&ctx.corpus, "la_maison");
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
			let target_side_doc = find_doc_index(&ctx.corpus, "the_house");
			let params = serde_json::json!({
				"source": { "doc": target_side_doc, "start": 0, "end": 5 },
				"alignment_name": "sentence",
			});
			let err = dispatch_request("alignment.project", Some(params), ctx).unwrap_err();
			assert_eq!(err.code, error_codes::SPAN_OUTSIDE_ALIGNMENT);
		});
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
}
