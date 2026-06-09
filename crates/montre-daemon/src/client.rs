use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use fs4::fs_std::FileExt;

use crate::dispatch::{read_frame, write_frame, OUTBOUND_QUEUE_DEPTH};
use crate::protocol::*;

pub type Result<T> = std::result::Result<T, DaemonClientError>;
type PendingMap = Arc<Mutex<HashMap<u64, SyncSender<ResponseMessage>>>>;

const CLOSE_UNREGISTER_DEADLINE: Duration = Duration::from_millis(100);
const SPAWN_POLL_TIMEOUT_DEFAULT: Duration = Duration::from_secs(10);
const DAEMON_BINARY_NAME: &str = "montre";
const MONTRE_BINARY_ENV: &str = "MONTRE_BINARY";

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DaemonClientError {
	#[error("transport error: {0}")]
	Transport(#[from] io::Error),
	#[error("framing error: {0}")]
	Framing(String),
	#[error("JSON-RPC envelope error: {0}")]
	Envelope(String),
	#[error("protocol error {0:?}")]
	Protocol(ProtocolError),
	#[error("could not locate the `montre` daemon binary: {0}")]
	DaemonBinaryNotFound(String),
	#[error("daemon failed to start within the spawn deadline\n--- daemon stderr ---\n{stderr}\n---")]
	SpawnFailed { stderr: String },
	#[error("timed out waiting for daemon response")]
	RequestTimeout,
	#[error("reader thread closed")]
	ReaderClosed,
}

impl From<ProtocolError> for DaemonClientError {
	fn from(error: ProtocolError) -> Self {
		Self::Protocol(error)
	}
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum NotificationEnvelope {
	CouplerUpdate { coupler_id: CouplerId, interest: Interest },
	RosterChanged { event: String, process: ProcessInfo },
	NamedResultsChanged { event: String, name: String, metadata: Option<ResultMetadata> },
	Shutdown { reason: String, in_seconds: u32 },
}

pub struct DaemonClient {
	writer: Arc<Mutex<UnixStream>>,
	pending: PendingMap,
	next_id: AtomicU64,
	notifications_rx: Receiver<NotificationEnvelope>,
	reader: Option<JoinHandle<()>>,
	reader_closed: Arc<AtomicBool>,
	closed: AtomicBool,
}

#[derive(Debug, Clone, Default)]
pub struct ConnectOptions {
	pub daemon_binary: Option<PathBuf>,
	pub spawn_timeout: Option<Duration>,
}

impl DaemonClient {
	pub fn connect(socket_path: &Path) -> Result<Self> {
		let stream = UnixStream::connect(socket_path)?;
		Self::connect_inner(stream)
	}

	pub fn connect_or_spawn(corpus_path: &Path) -> Result<Self> {
		Self::connect_or_spawn_with(corpus_path, ConnectOptions::default())
	}

	pub fn connect_or_spawn_with(corpus_path: &Path, options: ConnectOptions) -> Result<Self> {
		let socket = crate::socket_path_for(corpus_path)?;
		match UnixStream::connect(&socket) {
			Ok(stream) => return Self::connect_inner(stream),
			Err(e) if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused) => {}
			Err(e) => return Err(e.into()),
		}

		let state_dir = crate::state_dir_for_corpus(corpus_path)?;
		let _spawn_lock = acquire_spawn_lock(&state_dir)?;

		match UnixStream::connect(&socket) {
			Ok(stream) => return Self::connect_inner(stream),
			Err(e) if e.kind() == io::ErrorKind::NotFound => {}
			Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => {
				confirm_stale_then_unlink(&socket, &state_dir)?;
			}
			Err(e) => return Err(e.into()),
		}

		let binary = resolve_daemon_binary(options.daemon_binary.as_deref())?;
		let stderr_capture = spawn_daemon_with_stderr_capture(&binary, corpus_path)?;
		let timeout = options.spawn_timeout.unwrap_or(SPAWN_POLL_TIMEOUT_DEFAULT);
		poll_connect_with_backoff(&socket, timeout, stderr_capture)
	}

	fn connect_inner(stream: UnixStream) -> Result<Self> {
		let read_stream = stream.try_clone()?;
		let writer = Arc::new(Mutex::new(stream));
		let pending = Arc::new(Mutex::new(HashMap::new()));
		let (notifications_tx, notifications_rx) = sync_channel(OUTBOUND_QUEUE_DEPTH);
		let reader_pending = Arc::clone(&pending);
		let reader_closed = Arc::new(AtomicBool::new(false));
		let reader_closed_thread = Arc::clone(&reader_closed);
		let reader = thread::spawn(move || {
			run_reader(read_stream, reader_pending, notifications_tx, reader_closed_thread);
		});

		Ok(Self {
			writer,
			pending,
			next_id: AtomicU64::new(1),
			notifications_rx,
			reader: Some(reader),
			reader_closed,
			closed: AtomicBool::new(false),
		})
	}

	pub fn register(&mut self, params: RegisterParams) -> Result<RegisterReply> {
		self.request("session.register", Some(params))
	}

	pub fn unregister(&mut self) -> Result<OkReply> {
		self.request::<(), OkReply>("session.unregister", None)
	}

	pub fn roster(&mut self, params: SessionRosterParams) -> Result<SessionRosterReply> {
		self.request("session.roster", Some(params))
	}

	pub fn update_label(&mut self, params: SessionUpdateLabelParams) -> Result<OkReply> {
		self.request("session.update_label", Some(params))
	}

	pub fn publish_interest(&mut self, params: PublishInterestParams) -> std::result::Result<(), io::Error> {
		self.notification("session.publish_interest", params)
	}

	pub fn corpus_info(&mut self) -> Result<CorpusInfo> {
		self.request::<(), CorpusInfo>("corpus.info", None)
	}

	pub fn corpus_documents(&mut self, params: CorpusDocumentsParams) -> Result<CorpusDocumentsReply> {
		self.request("corpus.documents", Some(params))
	}

	pub fn corpus_layer_info(&mut self, params: CorpusLayerInfoParams) -> Result<LayerInfo> {
		self.request("corpus.layer_info", Some(params))
	}

	pub fn query_execute(&mut self, params: QueryExecuteParams) -> Result<QueryExecuteReply> {
		self.request("query.execute", Some(params))
	}

	pub fn query_execute_count(&mut self, params: QueryExecuteParams) -> Result<QueryExecuteCountReply> {
		self.request("query.execute_count", Some(params))
	}

	pub fn query_hits(&mut self, params: QueryHitsParams) -> Result<QueryHitsReply> {
		self.request("query.hits", Some(params))
	}

	pub fn query_metadata(&mut self, params: QueryMetadataParams) -> Result<ResultMetadata> {
		self.request("query.metadata", Some(params))
	}

	pub fn query_save(&mut self, params: QuerySaveParams) -> Result<QuerySaveReply> {
		self.request("query.save", Some(params))
	}

	pub fn query_materialize(&mut self, params: QueryMaterializeParams) -> Result<QueryMaterializeReply> {
		self.request("query.materialize", Some(params))
	}

	pub fn query_load(&mut self, params: QueryLoadParams) -> Result<QueryLoadReply> {
		self.request("query.load", Some(params))
	}

	pub fn query_list_named(&mut self) -> Result<QueryListNamedReply> {
		self.request::<(), QueryListNamedReply>("query.list_named", None)
	}

	pub fn query_delete_named(&mut self, params: QueryDeleteNamedParams) -> Result<OkReply> {
		self.request("query.delete_named", Some(params))
	}

	pub fn query_discard(&mut self, params: QueryDiscardParams) -> Result<OkReply> {
		self.request("query.discard", Some(params))
	}

	pub fn text_surface(&mut self, params: TextSurfaceParams) -> Result<TextSurfaceReply> {
		self.request("text.surface", Some(params))
	}

	pub fn text_surface_with_token_spans(
		&mut self,
		params: TextSurfaceWithTokenSpansParams,
	) -> Result<TextSurfaceWithTokenSpansReply> {
		self.request("text.surface_with_token_spans", Some(params))
	}

	pub fn text_sentence(&mut self, params: TextSentenceParams) -> Result<TextSentenceReply> {
		self.request("text.sentence", Some(params))
	}

	pub fn text_sentences(&mut self, params: TextSentencesParams) -> Result<TextSentencesReply> {
		self.request("text.sentences", Some(params))
	}

	pub fn text_document(&mut self, params: TextDocumentParams) -> Result<TextDocumentReply> {
		self.request("text.document", Some(params))
	}

	pub fn text_annotations(&mut self, params: TextAnnotationsParams) -> Result<TextAnnotationsReply> {
		self.request("text.annotations", Some(params))
	}

	pub fn text_annotations_range(&mut self, params: TextAnnotationsRangeParams) -> Result<TextAnnotationsRangeReply> {
		self.request("text.annotations_range", Some(params))
	}

	pub fn alignment_list(&mut self) -> Result<AlignmentListReply> {
		self.request::<(), AlignmentListReply>("alignment.list", None)
	}

	pub fn alignment_project(&mut self, params: AlignmentProjectParams) -> Result<AlignmentProjectReply> {
		self.request("alignment.project", Some(params))
	}

	pub fn coupler_create(&mut self, params: CouplerCreateParams) -> Result<CouplerCreateReply> {
		self.request("coupler.create", Some(params))
	}

	pub fn coupler_remove(&mut self, params: CouplerRemoveParams) -> Result<OkReply> {
		self.request("coupler.remove", Some(params))
	}

	pub fn coupler_list(&mut self, params: CouplerListParams) -> Result<CouplerListReply> {
		self.request("coupler.list", Some(params))
	}

	pub fn subscription_subscribe(&mut self, params: SubscriptionParams) -> Result<OkReply> {
		self.request("subscription.subscribe", Some(params))
	}

	pub fn subscription_unsubscribe(&mut self, params: SubscriptionParams) -> Result<OkReply> {
		self.request("subscription.unsubscribe", Some(params))
	}

	pub fn daemon_shutdown(&mut self, params: DaemonShutdownParams) -> Result<OkReply> {
		self.request("daemon.shutdown", Some(params))
	}

	pub fn notifications(&self) -> &Receiver<NotificationEnvelope> {
		&self.notifications_rx
	}

	pub fn close(mut self) -> Result<()> {
		self.close_inner()
	}

	fn request<P, R>(&mut self, method: &str, params: Option<P>) -> Result<R>
	where
		P: serde::Serialize,
		R: serde::de::DeserializeOwned,
	{
		self.request_inner(method, params, None)
	}

	fn request_with_timeout<P, R>(
		&mut self,
		method: &str,
		params: Option<P>,
		timeout: Duration,
	) -> Result<R>
	where
		P: serde::Serialize,
		R: serde::de::DeserializeOwned,
	{
		self.request_inner(method, params, Some(timeout))
	}

	fn request_inner<P, R>(
		&mut self,
		method: &str,
		params: Option<P>,
		timeout: Option<Duration>,
	) -> Result<R>
	where
		P: serde::Serialize,
		R: serde::de::DeserializeOwned,
	{
		if self.reader_closed.load(Ordering::SeqCst) {
			return Err(DaemonClientError::ReaderClosed);
		}
		let id = self.next_id.fetch_add(1, Ordering::Relaxed);
		let (reply_tx, reply_rx) = sync_channel(1);
		lock_pending(&self.pending).insert(id, reply_tx);
		if self.reader_closed.load(Ordering::SeqCst) {
			lock_pending(&self.pending).remove(&id);
			return Err(DaemonClientError::ReaderClosed);
		}

		let write_result = self.write_request(id, method, params);
		if let Err(e) = write_result {
			lock_pending(&self.pending).remove(&id);
			return Err(e);
		}

		let response = match timeout {
			Some(deadline) => match reply_rx.recv_timeout(deadline) {
				Ok(message) => message,
				Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
					lock_pending(&self.pending).remove(&id);
					return Err(DaemonClientError::RequestTimeout);
				}
				Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
					return Err(DaemonClientError::ReaderClosed);
				}
			},
			None => reply_rx.recv().map_err(|_| DaemonClientError::ReaderClosed)?,
		};
		match response {
			ResponseMessage::Result(value) => serde_json::from_value(value)
				.map_err(|e| DaemonClientError::Envelope(format!("invalid result for {}: {}", method, e))),
			ResponseMessage::Error(error) => Err(error.into()),
		}
	}

	fn write_request<P>(&self, id: u64, method: &str, params: Option<P>) -> Result<()>
	where
		P: serde::Serialize,
	{
		let mut value = serde_json::json!({
			"jsonrpc": "2.0",
			"id": id,
			"method": method,
		});
		if let Some(params) = params {
			value["params"] = serde_json::to_value(params)
				.map_err(|e| DaemonClientError::Envelope(format!("invalid params for {}: {}", method, e)))?;
		}
		self.write_value(&value)
	}

	fn notification<P>(&self, method: &str, params: P) -> std::result::Result<(), io::Error>
	where
		P: serde::Serialize,
	{
		let value = serde_json::json!({
			"jsonrpc": "2.0",
			"method": method,
			"params": params,
		});
		let payload = serde_json::to_vec(&value)
			.map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
		let mut writer = self.writer.lock().expect("writer lock poisoned");
		write_frame(&mut *writer, &payload)
	}

	fn write_value(&self, value: &serde_json::Value) -> Result<()> {
		let payload = serde_json::to_vec(value)
			.map_err(|e| DaemonClientError::Envelope(format!("request serialization failed: {}", e)))?;
		let mut writer = self.writer.lock().expect("writer lock poisoned");
		write_frame(&mut *writer, &payload).map_err(|e| {
			if e.kind() == io::ErrorKind::InvalidInput {
				DaemonClientError::Framing(e.to_string())
			} else {
				DaemonClientError::Transport(e)
			}
		})
	}

	fn close_inner(&mut self) -> Result<()> {
		if self.closed.swap(true, Ordering::SeqCst) {
			return Ok(());
		}

		if !self.reader_closed.load(Ordering::SeqCst) {
			let _ = self.request_with_timeout::<(), OkReply>(
				"session.unregister",
				None,
				CLOSE_UNREGISTER_DEADLINE,
			);
		}
		if let Ok(writer) = self.writer.lock() {
			let _ = writer.shutdown(std::net::Shutdown::Both);
		}
		if let Some(reader) = self.reader.take() {
			let _ = reader.join();
		}
		Ok(())
	}
}

impl Drop for DaemonClient {
	fn drop(&mut self) {
		let _ = self.close_inner();
	}
}

#[derive(Debug)]
enum ResponseMessage {
	Result(serde_json::Value),
	Error(ProtocolError),
}

fn run_reader(
	mut stream: UnixStream,
	pending: PendingMap,
	notifications_tx: SyncSender<NotificationEnvelope>,
	reader_closed: Arc<AtomicBool>,
) {
	let _guard = ReaderGuard {
		reader_closed,
		pending: Arc::clone(&pending),
	};

	loop {
		let frame = match read_frame(&mut stream) {
			Ok(frame) => frame,
			Err(e) => {
				if e.kind() != io::ErrorKind::UnexpectedEof && e.kind() != io::ErrorKind::ConnectionReset {
					tracing::warn!(error = %e, "client reader failed");
				}
				break;
			}
		};

		let value: serde_json::Value = match serde_json::from_slice(&frame) {
			Ok(value) => value,
			Err(e) => {
				tracing::warn!(error = %e, "client received malformed JSON frame");
				continue;
			}
		};

		if value.get("id").is_some() {
			dispatch_response(value, &pending);
		} else {
			dispatch_notification(value, &notifications_tx);
		}
	}

}

struct ReaderGuard {
	reader_closed: Arc<AtomicBool>,
	pending: PendingMap,
}

impl Drop for ReaderGuard {
	fn drop(&mut self) {
		self.reader_closed.store(true, Ordering::SeqCst);
		lock_pending(&self.pending).clear();
	}
}

fn lock_pending(pending: &PendingMap) -> MutexGuard<'_, HashMap<u64, SyncSender<ResponseMessage>>> {
	match pending.lock() {
		Ok(guard) => guard,
		Err(error) => {
			tracing::warn!("client pending map lock poisoned; recovering");
			error.into_inner()
		}
	}
}

fn dispatch_response(value: serde_json::Value, pending: &PendingMap) {
	let Some(id) = value.get("id").and_then(|v| v.as_u64()) else {
		tracing::warn!(message = %value, "client received response with non-u64 id");
		return;
	};
	let Some(tx) = lock_pending(pending).remove(&id) else {
		tracing::debug!(id = id, "client received response for unknown request id");
		return;
	};
	let message = if let Some(error) = value.get("error") {
		match serde_json::from_value::<ProtocolError>(error.clone()) {
			Ok(error) => ResponseMessage::Error(error),
			Err(e) => ResponseMessage::Error(ProtocolError::new(-32603, format!("invalid protocol error: {}", e))),
		}
	} else if let Some(result) = value.get("result") {
		ResponseMessage::Result(result.clone())
	} else {
		ResponseMessage::Error(ProtocolError::new(-32603, "response missing result and error"))
	};
	let _ = tx.send(message);
}

fn dispatch_notification(value: serde_json::Value, tx: &SyncSender<NotificationEnvelope>) {
	let Some(method) = value.get("method").and_then(|v| v.as_str()) else {
		tracing::warn!(message = %value, "client received notification without method");
		return;
	};
	let params = value.get("params").cloned().unwrap_or(serde_json::Value::Null);
	let envelope = match method {
		"notification.coupler_update" => parse_coupler_update(params),
		"notification.roster_changed" => parse_roster_changed(params),
		"notification.named_results_changed" => parse_named_results_changed(params),
		"notification.shutdown" => parse_shutdown(params),
		_ => {
			tracing::debug!(method = method, "unknown notification method; dropped");
			return;
		}
	};
	match envelope {
		Ok(envelope) => {
			if let Err(std::sync::mpsc::TrySendError::Full(_)) = tx.try_send(envelope) {
				tracing::warn!("client notification queue full; dropping notification");
			}
		}
		Err(e) => tracing::warn!(method = method, error = %e, "invalid notification payload"),
	}
}

fn parse_coupler_update(params: serde_json::Value) -> std::result::Result<NotificationEnvelope, serde_json::Error> {
	#[derive(serde::Deserialize)]
	struct Params {
		coupler_id: CouplerId,
		interest: Interest,
	}
	let p: Params = serde_json::from_value(params)?;
	Ok(NotificationEnvelope::CouplerUpdate { coupler_id: p.coupler_id, interest: p.interest })
}

fn parse_roster_changed(params: serde_json::Value) -> std::result::Result<NotificationEnvelope, serde_json::Error> {
	#[derive(serde::Deserialize)]
	struct Params {
		event: String,
		process: ProcessInfo,
	}
	let p: Params = serde_json::from_value(params)?;
	Ok(NotificationEnvelope::RosterChanged { event: p.event, process: p.process })
}

fn parse_named_results_changed(params: serde_json::Value) -> std::result::Result<NotificationEnvelope, serde_json::Error> {
	#[derive(serde::Deserialize)]
	struct Params {
		event: String,
		name: String,
		#[serde(default)]
		metadata: Option<ResultMetadata>,
	}
	let p: Params = serde_json::from_value(params)?;
	Ok(NotificationEnvelope::NamedResultsChanged { event: p.event, name: p.name, metadata: p.metadata })
}

fn parse_shutdown(params: serde_json::Value) -> std::result::Result<NotificationEnvelope, serde_json::Error> {
	#[derive(serde::Deserialize)]
	struct Params {
		reason: String,
		in_seconds: u32,
	}
	let p: Params = serde_json::from_value(params)?;
	Ok(NotificationEnvelope::Shutdown { reason: p.reason, in_seconds: p.in_seconds })
}

fn spawn_daemon_with_stderr_capture(
	binary: &Path,
	corpus_path: &Path,
) -> io::Result<tempfile::NamedTempFile> {
	let stderr_capture = tempfile::Builder::new()
		.prefix("montre-spawn-")
		.suffix(".log")
		.tempfile()?;
	let stderr_handle = stderr_capture.reopen()?;
	Command::new(binary)
		.arg("serve")
		.arg(corpus_path)
		.stdin(std::process::Stdio::null())
		.stdout(std::process::Stdio::null())
		.stderr(std::process::Stdio::from(stderr_handle))
		.spawn()?;
	Ok(stderr_capture)
}

fn poll_connect_with_backoff(
	socket: &Path,
	timeout: Duration,
	stderr_capture: tempfile::NamedTempFile,
) -> Result<DaemonClient> {
	let deadline = Instant::now() + timeout;
	let mut sleep = Duration::from_millis(50);
	loop {
		match UnixStream::connect(socket) {
			Ok(stream) => {
				drop(stderr_capture);
				return DaemonClient::connect_inner(stream);
			}
			Err(e) if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused) => {}
			Err(e) => {
				drop(stderr_capture);
				return Err(e.into());
			}
		}
		if Instant::now() >= deadline {
			let stderr = std::fs::read_to_string(stderr_capture.path()).unwrap_or_default();
			return Err(DaemonClientError::SpawnFailed { stderr });
		}
		thread::sleep(sleep);
		sleep = std::cmp::min(sleep * 2, Duration::from_millis(250));
	}
}

fn resolve_daemon_binary(explicit: Option<&Path>) -> Result<PathBuf> {
	resolve_daemon_binary_inner(
		explicit,
		|k| std::env::var(k).ok().filter(|v| !v.is_empty()),
		std::env::current_exe,
	)
}

fn resolve_daemon_binary_inner(
	explicit: Option<&Path>,
	env_lookup: impl Fn(&str) -> Option<String>,
	current_exe: impl FnOnce() -> io::Result<PathBuf>,
) -> Result<PathBuf> {
	if let Some(path) = explicit {
		if path.is_file() {
			return Ok(path.to_path_buf());
		}
		return Err(DaemonClientError::DaemonBinaryNotFound(format!(
			"explicit binary path does not exist: {}",
			path.display(),
		)));
	}

	if let Some(value) = env_lookup(MONTRE_BINARY_ENV) {
		let path = PathBuf::from(&value);
		if path.is_file() {
			return Ok(path);
		}
		return Err(DaemonClientError::DaemonBinaryNotFound(format!(
			"{} points to a missing file: {}",
			MONTRE_BINARY_ENV,
			path.display(),
		)));
	}

	if let Some(found) = find_on_path(&env_lookup, DAEMON_BINARY_NAME) {
		return Ok(found);
	}

	if let Ok(exe) = current_exe() {
		if let Some(parent) = exe.parent() {
			let sibling = parent.join(DAEMON_BINARY_NAME);
			if sibling.is_file() {
				return Ok(sibling);
			}
		}
	}

	Err(DaemonClientError::DaemonBinaryNotFound(format!(
		"`{}` not found on PATH and no sibling exists next to the current executable; \
		 set {} to specify a path explicitly",
		DAEMON_BINARY_NAME, MONTRE_BINARY_ENV,
	)))
}

fn find_on_path(
	env_lookup: &impl Fn(&str) -> Option<String>,
	name: &str,
) -> Option<PathBuf> {
	let path = env_lookup("PATH")?;
	for directory in path.split(':').filter(|d| !d.is_empty()) {
		let candidate = Path::new(directory).join(name);
		if candidate.is_file() {
			return Some(candidate);
		}
	}
	None
}

struct SpawnLockGuard {
	_file: File,
}

fn acquire_spawn_lock(state_dir: &Path) -> io::Result<SpawnLockGuard> {
	let path = state_dir.join("spawn.lock");
	let file = OpenOptions::new()
		.create(true)
		.write(true)
		.read(true)
		.truncate(false)
		.open(&path)?;
	file.lock_exclusive()?;
	Ok(SpawnLockGuard { _file: file })
}

fn confirm_stale_then_unlink(socket: &Path, state_dir: &Path) -> Result<()> {
	if crate::storage::daemon_lock_held(state_dir)? {
		return Err(DaemonClientError::Transport(io::Error::new(
			io::ErrorKind::ConnectionRefused,
			format!(
				"daemon appears alive at {} (daemon.lock held) but the socket is \
				 refusing connections, likely a saturated accept queue or a daemon \
				 mid-startup. Retry shortly.",
				socket.display(),
			),
		)));
	}
	let _ = std::fs::remove_file(socket);
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn spawn_lock_serializes_within_process() {
		let temp = tempfile::TempDir::new().expect("tempdir");
		let state_dir = temp.path();

		let first = acquire_spawn_lock(state_dir).expect("first acquire");

		let second_path = state_dir.join("spawn.lock");
		let second_file = OpenOptions::new()
			.create(true)
			.write(true)
			.read(true)
			.truncate(false)
			.open(&second_path)
			.expect("open second");
		let acquired = second_file.try_lock_exclusive().expect("try lock");
		assert!(!acquired, "second lock should not be acquired while first held");

		drop(first);

		let acquired = second_file.try_lock_exclusive().expect("try lock after release");
		assert!(acquired, "second lock should succeed after first released");
	}

	fn no_env(_: &str) -> Option<String> {
		None
	}

	fn no_exe() -> io::Result<PathBuf> {
		Err(io::Error::new(io::ErrorKind::NotFound, "test: no current_exe"))
	}

	fn create_executable_stub(path: &Path, body: &str) {
		use std::os::unix::fs::PermissionsExt;
		std::fs::write(path, body).expect("write stub");
		let mut permissions = std::fs::metadata(path).expect("stat stub").permissions();
		permissions.set_mode(0o755);
		std::fs::set_permissions(path, permissions).expect("chmod stub");
	}

	#[test]
	fn resolve_uses_explicit_path_when_it_exists() {
		let temp = tempfile::TempDir::new().expect("tempdir");
		let binary = temp.path().join("montre");
		std::fs::write(&binary, "").unwrap();

		let result = resolve_daemon_binary_inner(Some(&binary), no_env, no_exe).unwrap();
		assert_eq!(result, binary);
	}

	#[test]
	fn resolve_rejects_explicit_path_that_does_not_exist() {
		let temp = tempfile::TempDir::new().expect("tempdir");
		let missing = temp.path().join("missing-montre");

		let result = resolve_daemon_binary_inner(Some(&missing), no_env, no_exe);
		assert!(matches!(result, Err(DaemonClientError::DaemonBinaryNotFound(_))));
	}

	#[test]
	fn resolve_uses_env_when_no_explicit() {
		let temp = tempfile::TempDir::new().expect("tempdir");
		let binary = temp.path().join("montre");
		std::fs::write(&binary, "").unwrap();
		let binary_string = binary.to_string_lossy().into_owned();

		let result = resolve_daemon_binary_inner(
			None,
			|key| if key == MONTRE_BINARY_ENV { Some(binary_string.clone()) } else { None },
			no_exe,
		)
		.unwrap();
		assert_eq!(result, binary);
	}

	#[test]
	fn resolve_rejects_env_pointing_at_missing_file() {
		let temp = tempfile::TempDir::new().expect("tempdir");
		let missing = temp.path().join("nope");
		let missing_string = missing.to_string_lossy().into_owned();

		let result = resolve_daemon_binary_inner(
			None,
			|key| if key == MONTRE_BINARY_ENV { Some(missing_string.clone()) } else { None },
			no_exe,
		);
		assert!(matches!(result, Err(DaemonClientError::DaemonBinaryNotFound(_))));
	}

	#[test]
	fn resolve_falls_back_to_path() {
		let temp = tempfile::TempDir::new().expect("tempdir");
		let binary = temp.path().join("montre");
		std::fs::write(&binary, "").unwrap();
		let path_string = temp.path().to_string_lossy().into_owned();

		let result = resolve_daemon_binary_inner(
			None,
			|key| if key == "PATH" { Some(path_string.clone()) } else { None },
			no_exe,
		)
		.unwrap();
		assert_eq!(result, binary);
	}

	#[test]
	fn resolve_falls_back_to_current_exe_sibling() {
		let temp = tempfile::TempDir::new().expect("tempdir");
		let sibling_binary = temp.path().join("montre");
		std::fs::write(&sibling_binary, "").unwrap();
		let exe_path = temp.path().join("some-other-binary");
		std::fs::write(&exe_path, "").unwrap();

		let result = resolve_daemon_binary_inner(
			None,
			no_env,
			|| Ok(exe_path.clone()),
		)
		.unwrap();
		assert_eq!(result, sibling_binary);
	}

	#[test]
	fn resolve_errors_when_nothing_found() {
		let result = resolve_daemon_binary_inner(None, no_env, no_exe);
		assert!(matches!(result, Err(DaemonClientError::DaemonBinaryNotFound(_))));
	}

	#[test]
	fn spawn_with_stderr_capture_surfaces_child_output_on_timeout() {
		let temp = tempfile::TempDir::new().expect("tempdir");
		let binary = temp.path().join("fake-daemon");
		create_executable_stub(
			&binary,
			"#!/bin/sh\necho 'synthetic spawn failure' >&2\nexit 1\n",
		);

		let stderr_capture = spawn_daemon_with_stderr_capture(&binary, temp.path())
			.expect("spawn fake daemon");

		let unreachable_socket = temp.path().join("never-bound.sock");
		let result = poll_connect_with_backoff(
			&unreachable_socket,
			Duration::from_millis(500),
			stderr_capture,
		);
		match result {
			Err(DaemonClientError::SpawnFailed { stderr }) => {
				assert!(
					stderr.contains("synthetic spawn failure"),
					"captured stderr should contain the fake message: {:?}",
					stderr,
				);
			}
			Err(other) => panic!("expected SpawnFailed, got {:?}", other),
			Ok(_) => panic!("expected SpawnFailed, got Ok(_)"),
		}
	}

	#[test]
	fn confirm_stale_refuses_to_unlink_when_daemon_lock_is_held() {
		let temp = tempfile::TempDir::new().expect("tempdir");
		let state_dir = temp.path();
		let socket = state_dir.join("stand-in.sock");
		std::fs::write(&socket, b"placeholder").expect("create placeholder socket file");

		let live_lock = crate::storage::acquire_daemon_lock(state_dir).expect("acquire");

		let result = confirm_stale_then_unlink(&socket, state_dir);
		match result {
			Err(DaemonClientError::Transport(e)) => {
				assert_eq!(e.kind(), io::ErrorKind::ConnectionRefused);
			}
			Err(other) => panic!("expected Transport(ConnectionRefused), got {:?}", other),
			Ok(()) => panic!("expected error, got Ok"),
		}
		assert!(
			socket.exists(),
			"socket placeholder must not be unlinked while daemon.lock is held",
		);

		drop(live_lock);
	}

	#[test]
	fn confirm_stale_unlinks_when_daemon_lock_is_free() {
		let temp = tempfile::TempDir::new().expect("tempdir");
		let state_dir = temp.path();
		let socket = state_dir.join("stand-in.sock");
		std::fs::write(&socket, b"placeholder").expect("create placeholder socket file");

		confirm_stale_then_unlink(&socket, state_dir).expect("should succeed");
		assert!(!socket.exists(), "stale socket should be unlinked");
	}

	#[test]
	fn confirm_stale_is_a_noop_when_socket_already_absent() {
		let temp = tempfile::TempDir::new().expect("tempdir");
		let state_dir = temp.path();
		let socket = state_dir.join("never-existed.sock");

		confirm_stale_then_unlink(&socket, state_dir).expect("should succeed");
		assert!(!socket.exists());
	}
}

