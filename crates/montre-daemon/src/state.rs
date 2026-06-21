use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::Arc;

use montre_index::SpanIndex;
use montre_query::executor::Hit;
use uuid::Uuid;

use crate::handlers::alignment::project_alignment;
use crate::protocol::error_codes;
use crate::protocol::{
	Coupler, CouplerId, CouplerKind, AlignmentSource, Capabilities, Interest, InterestKind, ProcessId,
	ProcessInfo, ProtocolError, RegisterParams, RegisterReply, ResultForm, ResultHandle,
	ResultMetadata, RosterFilter, ShutdownNotificationParams, ShutdownReason, Span, Topic,
	PROTOCOL_VERSION,
};
use crate::shutdown::ShutdownCoordinator;
use crate::storage::{self, NamedResultRecord};
use crate::CorpusHandle;

const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

const SHUTDOWN_NOTIFY_BUDGET: std::time::Duration = std::time::Duration::from_millis(200);

pub(crate) type ResultsTable = HashMap<ResultHandle, Arc<ResultEntry>>;

pub(crate) struct ResultEntry {
	pub cql: String,
	pub hits: Arc<Vec<Hit>>,
	pub metadata: ResultMetadata,
}

pub(crate) type Outbound = serde_json::Value;

pub(crate) struct Connection {
	info: ProcessInfo,
	outbound: SyncSender<Outbound>,
}

pub(crate) struct State {
	daemon_epoch: u64,
	handle: Arc<CorpusHandle>,
	coordinator: Arc<ShutdownCoordinator>,
	roster: HashMap<ProcessId, Connection>,
	couplers: HashMap<CouplerId, Coupler>,
	subscriptions: HashMap<Topic, HashSet<ProcessId>>,
	named_results: HashMap<String, NamedResultRecord>,
	idle_timeout: Option<std::time::Duration>,
	roster_emptied_at: Option<std::time::Instant>,
	next_process_id: ProcessId,
	next_coupler_id: CouplerId,
}

impl State {
	pub(crate) fn new(
		daemon_epoch: u64,
		handle: Arc<CorpusHandle>,
		coordinator: Arc<ShutdownCoordinator>,
	) -> Self {
		Self {
			daemon_epoch,
			handle,
			coordinator,
			roster: HashMap::new(),
			couplers: HashMap::new(),
			subscriptions: HashMap::new(),
			named_results: HashMap::new(),
			idle_timeout: None,
			roster_emptied_at: Some(std::time::Instant::now()),
			next_process_id: 1,
			next_coupler_id: 1,
		}
	}

	pub(crate) fn set_idle_timeout(&mut self, timeout: Option<std::time::Duration>) {
		self.idle_timeout = timeout;
	}

	pub(crate) fn replay_named_results(&mut self) -> std::io::Result<()> {
		let loaded = storage::load_named_results(&self.handle.state_dir)?;
		tracing::info!(count = loaded.len(), "replayed named results from disk");
		self.named_results = loaded;
		Ok(())
	}

	fn allocate_process_id(&mut self) -> ProcessId {
		let id = self.next_process_id;
		self.next_process_id = self.next_process_id.wrapping_add(1);
		id
	}

	fn allocate_coupler_id(&mut self) -> CouplerId {
		let id = self.next_coupler_id;
		self.next_coupler_id = self.next_coupler_id.wrapping_add(1);
		id
	}

	fn register(
		&mut self,
		params: RegisterParams,
		outbound: SyncSender<Outbound>,
	) -> Result<RegisterReply, ProtocolError> {
		if params.protocol_version != PROTOCOL_VERSION {
			return Err(ProtocolError::new(
				error_codes::PROTOCOL_VERSION_MISMATCH,
				format!(
					"client requested protocol version {}, daemon supports {}",
					params.protocol_version, PROTOCOL_VERSION,
				),
			));
		}

		let process_id = self.allocate_process_id();
		let info = ProcessInfo {
			id: process_id,
			kind: params.kind,
			label: params.label,
			provides: params.provides,
			consumes: params.consumes,
			current_interest: None,
		};
		self.roster.insert(
			process_id,
			Connection {
				info: info.clone(),
				outbound,
			},
		);
		self.roster_emptied_at = None;

		self.notify_roster(RosterEvent::Registered, info);

		Ok(RegisterReply {
			process_id,
			server_version: SERVER_VERSION.to_string(),
			protocol_version: PROTOCOL_VERSION,
			daemon_epoch: self.daemon_epoch,
			capabilities: Capabilities {
				observations: false,
				workspaces: false,
				coupler_kinds: coupler_kind_names().iter().map(|k| k.to_string()).collect(),
			},
		})
	}

	fn unregister(&mut self, process_id: ProcessId) {
		let Some(connection) = self.roster.remove(&process_id) else {
			return;
		};

		let dropped: Vec<CouplerId> = self
			.couplers
			.iter()
			.filter(|(_, a)| a.master_id == process_id || a.follower_id == process_id)
			.map(|(id, _)| *id)
			.collect();
		for id in dropped {
			self.couplers.remove(&id);
		}

		for subscribers in self.subscriptions.values_mut() {
			subscribers.remove(&process_id);
		}

		if self.roster.is_empty() {
			self.roster_emptied_at = Some(std::time::Instant::now());
		}

		self.notify_roster(RosterEvent::Unregistered, connection.info);
	}

	fn update_label(
		&mut self,
		process_id: ProcessId,
		label: Option<String>,
	) -> Result<(), ProtocolError> {
		let connection = self.roster.get_mut(&process_id).ok_or_else(|| {
			ProtocolError::new(error_codes::PROCESS_NOT_FOUND, "process not found")
		})?;
		connection.info.label = label;
		let info = connection.info.clone();
		self.notify_roster(RosterEvent::LabelUpdated, info);
		Ok(())
	}

	fn roster_snapshot(&self, filter: Option<RosterFilter>) -> Vec<ProcessInfo> {
		self.roster
			.values()
			.filter(|c| match &filter {
				None => true,
				Some(f) => f.matches(&c.info),
			})
			.map(|c| c.info.clone())
			.collect()
	}

	fn publish_interest(&mut self, process_id: ProcessId, interest: Interest) {
		let Some(connection) = self.roster.get_mut(&process_id) else {
			return;
		};
		let kind = interest.kind();
		if !connection.info.provides.contains(&kind) {
			tracing::debug!(
				process_id,
				interest_kind = ?kind,
				provides = ?connection.info.provides,
				"publish_interest: interest kind not in declared provides; dropping",
			);
			return;
		}
		connection.info.current_interest = Some(interest.clone());

		let derivative_targets: Vec<(CouplerId, ProcessId, CouplerKind)> = self
			.couplers
			.values()
			.filter(|a| a.master_id == process_id)
			.map(|a| (a.id, a.follower_id, a.kind.clone()))
			.collect();

		for (coupler_id, follower_id, kind) in derivative_targets {
			let transformed = transform_interest(&self.handle, &interest, &kind);
			let Some(outbound) = self.roster.get(&follower_id).map(|c| c.outbound.clone()) else {
				continue;
			};
			for derived in transformed {
				let payload = serde_json::json!({
					"jsonrpc": "2.0",
					"method": "notification.coupler_update",
					"params": {
						"coupler_id": coupler_id,
						"interest": derived,
					},
				});
				let _ = try_send_outbound(&outbound, payload, follower_id);
			}
		}
	}

	fn insert_result(&mut self, cql: String, hits: Vec<Hit>) -> ResultMetadata {
		let handle = generate_handle();
		let metadata = ResultMetadata {
			handle: handle.clone(),
			query: cql.clone(),
			created_at: now_rfc3339(),
			materialized_at: None,
			hit_count: hits.len() as u64,
			corpus_id: self.handle.corpus_id.clone(),
			name: None,
			form: ResultForm::Session,
		};
		let reply_metadata = metadata.clone();
		let entry = Arc::new(ResultEntry {
			cql,
			hits: Arc::new(hits),
			metadata,
		});
		self.handle.results
			.write()
			.expect("results lock poisoned")
			.insert(handle.clone(), entry);
		reply_metadata
	}

	fn save_result(
		&mut self,
		handle: ResultHandle,
		name: String,
	) -> Result<ResultMetadata, ProtocolError> {
		if self.named_results.contains_key(&name) {
			return Err(ProtocolError::new(
				error_codes::NAMED_RESULT_ALREADY_EXISTS,
				format!("name '{}' already in use", name),
			));
		}

		let promoted = {
			let table = self.handle.results.read().expect("results lock poisoned");
			let existing = table.get(&handle).cloned().ok_or_else(|| {
				ProtocolError::new(error_codes::RESULT_HANDLE_INVALID, "handle not found")
			})?;
			let mut next = ResultEntry {
				cql: existing.cql.clone(),
				hits: existing.hits.clone(),
				metadata: existing.metadata.clone(),
			};
			next.metadata.name = Some(name.clone());
			next.metadata.form = ResultForm::QueryBacked;
			Arc::new(next)
		};

		let record = NamedResultRecord {
			handle: handle.clone(),
			cql: promoted.cql.clone(),
			hit_count: promoted.metadata.hit_count,
			created_at: promoted.metadata.created_at.clone(),
		};

		let extra = std::iter::once((name.as_str(), &record));
		let existing = self
			.named_results
			.iter()
			.map(|(k, v)| (k.as_str(), v));
		storage::persist_named_results(
			&self.handle.state_dir,
			&self.handle.corpus_id,
			existing.chain(extra),
		)
		.map_err(persist_error)?;

		let metadata = promoted.metadata.clone();
		self.handle.results
			.write()
			.expect("results lock poisoned")
			.insert(handle, promoted);
		self.named_results.insert(name.clone(), record);

		self.notify_named_results(NamedResultsEvent::Saved, name, Some(metadata.clone()));
		Ok(metadata)
	}

	fn materialize_result(&mut self, name: String) -> Result<ResultMetadata, ProtocolError> {
		let handle = self.named_results.get(&name).map(|r| r.handle.clone()).ok_or_else(|| {
			ProtocolError::new(
				error_codes::NAMED_RESULT_NOT_FOUND,
				format!("name '{}' not found", name),
			)
		})?;

		let promoted = {
			let table = self.handle.results.read().expect("results lock poisoned");
			let existing = table.get(&handle).cloned().ok_or_else(|| {
				ProtocolError::new(error_codes::RESULT_HANDLE_INVALID, "handle not found")
			})?;
			if matches!(existing.metadata.form, ResultForm::Materialized) {
				return Err(ProtocolError::new(
					error_codes::RESULT_ALREADY_MATERIALIZED,
					format!("name '{}' is already materialized", name),
				));
			}
			let mut next = ResultEntry {
				cql: existing.cql.clone(),
				hits: existing.hits.clone(),
				metadata: existing.metadata.clone(),
			};
			next.metadata.form = ResultForm::Materialized;
			next.metadata.materialized_at = Some(now_rfc3339());
			Arc::new(next)
		};

		let metadata = promoted.metadata.clone();
		self.handle.results
			.write()
			.expect("results lock poisoned")
			.insert(handle, promoted);

		self.notify_named_results(NamedResultsEvent::Saved, name, Some(metadata.clone()));
		Ok(metadata)
	}

	fn load_named(&self, name: &str) -> Result<LoadOutcome, ProtocolError> {
		let record = self.named_results.get(name).ok_or_else(|| {
			ProtocolError::new(
				error_codes::NAMED_RESULT_NOT_FOUND,
				format!("name '{}' not found", name),
			)
		})?;
		let table = self.handle.results.read().expect("results lock poisoned");
		if let Some(entry) = table.get(&record.handle) {
			return Ok(LoadOutcome::Realized(entry.metadata.clone()));
		}
		Ok(LoadOutcome::Pending {
			handle: record.handle.clone(),
			cql: record.cql.clone(),
			created_at: record.created_at.clone(),
		})
	}

	fn install_replayed_result(&mut self, entry: Arc<ResultEntry>) {
		let handle = entry.metadata.handle.clone();
		self.handle.results
			.write()
			.expect("results lock poisoned")
			.insert(handle, entry);
	}

	fn list_named(&self) -> Vec<ResultMetadata> {
		let table = self.handle.results.read().expect("results lock poisoned");
		self.named_results
			.iter()
			.map(|(name, record)| {
				if let Some(entry) = table.get(&record.handle) {
					return entry.metadata.clone();
				}
				ResultMetadata {
					handle: record.handle.clone(),
					query: record.cql.clone(),
					created_at: record.created_at.clone(),
					materialized_at: None,
					hit_count: record.hit_count,
					corpus_id: self.handle.corpus_id.clone(),
					name: Some(name.clone()),
					form: ResultForm::QueryBacked,
				}
			})
			.collect()
	}

	fn delete_named(&mut self, name: String) -> Result<(), ProtocolError> {
		let record_handle = self.named_results.get(&name).map(|r| r.handle.clone()).ok_or_else(|| {
			ProtocolError::new(
				error_codes::NAMED_RESULT_NOT_FOUND,
				format!("name '{}' not found", name),
			)
		})?;

		let surviving = self
			.named_results
			.iter()
			.filter(|(key, _)| key.as_str() != name.as_str())
			.map(|(k, v)| (k.as_str(), v));
		storage::persist_named_results(
			&self.handle.state_dir,
			&self.handle.corpus_id,
			surviving,
		)
		.map_err(persist_error)?;

		self.named_results.remove(&name);
		self.handle.results
			.write()
			.expect("results lock poisoned")
			.remove(&record_handle);
		self.notify_named_results(NamedResultsEvent::Deleted, name, None);
		Ok(())
	}

	fn discard_handle(&mut self, handle: ResultHandle) {
		let mut table = self.handle.results.write().expect("results lock poisoned");
		let Some(entry) = table.get(&handle) else {
			return;
		};
		if matches!(entry.metadata.form, ResultForm::Session) {
			table.remove(&handle);
		}
	}

	fn coupler_create(
		&mut self,
		master_id: ProcessId,
		follower_id: ProcessId,
		kind: CouplerKind,
	) -> Result<CouplerId, ProtocolError> {
		if !coupler_kind_names().contains(&kind.type_name()) {
			return Err(ProtocolError::new(
				error_codes::COUPLER_KIND_UNSUPPORTED,
				format!("coupler kind '{}' not supported by this daemon", kind.type_name()),
			));
		}
		if !self.roster.contains_key(&master_id) {
			return Err(ProtocolError::new(
				error_codes::PROCESS_NOT_FOUND,
				format!("master process {} not found", master_id),
			));
		}
		if !self.roster.contains_key(&follower_id) {
			return Err(ProtocolError::new(
				error_codes::PROCESS_NOT_FOUND,
				format!("follower process {} not found", follower_id),
			));
		}
		if self.coupler_creates_cycle(master_id, follower_id) {
			return Err(ProtocolError::new(
				error_codes::COUPLER_CYCLE,
				"coupler would create a cycle in the dependency graph",
			));
		}
		let compat_ok = coupler_kind_compat(
			&kind,
			&self.roster[&master_id].info.provides,
			&self.roster[&follower_id].info.consumes,
		);
		if !compat_ok {
			let master_provides = self.roster[&master_id].info.provides.clone();
			let follower_consumes = self.roster[&follower_id].info.consumes.clone();
			return Err(ProtocolError::new(
				error_codes::COUPLER_INCOMPATIBLE,
				format!(
					"coupler kind '{}' incompatible with master provides / follower consumes",
					kind.type_name(),
				),
			)
			.with_data(serde_json::json!({
				"master_provides": master_provides,
				"follower_consumes": follower_consumes,
			})));
		}

		let coupler_id = self.allocate_coupler_id();
		let coupler = Coupler {
			id: coupler_id,
			master_id,
			follower_id,
			kind: kind.clone(),
		};
		self.couplers.insert(coupler_id, coupler);

		let initial = self
			.roster
			.get(&master_id)
			.and_then(|c| c.info.current_interest.clone());
		if let Some(interest) = initial {
			let transformed = transform_interest(&self.handle, &interest, &kind);
			if let Some(outbound) = self.roster.get(&follower_id).map(|c| c.outbound.clone()) {
				for derived in transformed {
					let payload = serde_json::json!({
						"jsonrpc": "2.0",
						"method": "notification.coupler_update",
						"params": {
							"coupler_id": coupler_id,
							"interest": derived,
						},
					});
					let _ = try_send_outbound(&outbound, payload, follower_id);
				}
			}
		}

		Ok(coupler_id)
	}

	fn coupler_remove(&mut self, coupler_id: CouplerId) -> Result<(), ProtocolError> {
		self.couplers
			.remove(&coupler_id)
			.map(|_| ())
			.ok_or_else(|| {
				ProtocolError::new(error_codes::COUPLER_NOT_FOUND, "coupler not found")
			})
	}

	fn coupler_list(&self, filter: Option<ProcessId>) -> Vec<Coupler> {
		match filter {
			None => self.couplers.values().cloned().collect(),
			Some(pid) => self
				.couplers
				.values()
				.filter(|a| a.master_id == pid || a.follower_id == pid)
				.cloned()
				.collect(),
		}
	}

	fn coupler_creates_cycle(&self, master_id: ProcessId, follower_id: ProcessId) -> bool {
		if master_id == follower_id {
			return true;
		}
		let mut stack = vec![follower_id];
		let mut visited = HashSet::new();
		while let Some(current) = stack.pop() {
			if !visited.insert(current) {
				continue;
			}
			if current == master_id {
				return true;
			}
			for coupler in self.couplers.values() {
				if coupler.master_id == current {
					stack.push(coupler.follower_id);
				}
			}
		}
		false
	}

	fn subscribe(
		&mut self,
		process_id: ProcessId,
		topic: Topic,
	) -> Result<(), ProtocolError> {
		if !self.roster.contains_key(&process_id) {
			return Err(ProtocolError::new(
				error_codes::PROCESS_NOT_FOUND,
				"process not found",
			));
		}
		self.subscriptions.entry(topic).or_default().insert(process_id);
		Ok(())
	}

	fn unsubscribe(&mut self, process_id: ProcessId, topic: Topic) {
		if let Some(set) = self.subscriptions.get_mut(&topic) {
			set.remove(&process_id);
		}
	}

	fn notify_roster(&self, event: RosterEvent, info: ProcessInfo) {
		let payload = serde_json::json!({
			"jsonrpc": "2.0",
			"method": "notification.roster_changed",
			"params": {
				"event": event.as_str(),
				"process": info,
			},
		});
		self.broadcast(Topic::RosterChanged, payload);
	}

	fn notify_named_results(
		&self,
		event: NamedResultsEvent,
		name: String,
		metadata: Option<ResultMetadata>,
	) {
		let mut params = serde_json::json!({
			"event": event.as_str(),
			"name": name,
		});
		if let Some(m) = metadata {
			params["metadata"] = serde_json::to_value(m).unwrap_or(serde_json::Value::Null);
		}
		let payload = serde_json::json!({
			"jsonrpc": "2.0",
			"method": "notification.named_results_changed",
			"params": params,
		});
		self.broadcast(Topic::NamedResultsChanged, payload);
	}

	fn broadcast(&self, topic: Topic, payload: serde_json::Value) {
		let recipients: Vec<(ProcessId, SyncSender<Outbound>)> = self
			.subscriptions
			.get(&topic)
			.map(|set| {
				set.iter()
					.filter_map(|pid| self.roster.get(pid).map(|c| (*pid, c.outbound.clone())))
					.collect()
			})
			.unwrap_or_default();
		for (pid, outbound) in recipients {
			let _ = try_send_outbound(&outbound, payload.clone(), pid);
		}
	}

	fn initiate_shutdown(&self, reason: ShutdownReason) {
		if !self.coordinator.mark_shutting_down() {
			tracing::debug!("shutdown already in progress; ignoring duplicate initiate");
			return;
		}
		tracing::info!(reason = ?reason, "daemon shutdown initiated");
		self.notify_shutdown(reason);

		let coordinator = Arc::clone(&self.coordinator);
		std::thread::spawn(move || {
			std::thread::sleep(std::time::Duration::from_millis(500));
			coordinator.close_all_streams();
			if let Err(error) = coordinator.wake_listener() {
				tracing::warn!(error = %error, "self-connect to wake listener failed");
			}
		});
	}

	fn check_idle_timeout(&self) {
		let Some(timeout) = self.idle_timeout else { return };
		let Some(emptied_at) = self.roster_emptied_at else { return };
		let elapsed = emptied_at.elapsed();
		if elapsed >= timeout {
			tracing::info!(
				elapsed_seconds = elapsed.as_secs(),
				"idle timeout reached; initiating shutdown",
			);
			self.initiate_shutdown(ShutdownReason::IdleTimeout);
		}
	}

	fn notify_shutdown(&self, reason: ShutdownReason) {
		let params = ShutdownNotificationParams {
			reason,
			in_seconds: 0,
		};
		let payload = serde_json::json!({
			"jsonrpc": "2.0",
			"method": "notification.shutdown",
			"params": serde_json::to_value(params).unwrap_or(serde_json::Value::Null),
		});
		let recipients: Vec<(ProcessId, SyncSender<Outbound>)> = self
			.roster
			.iter()
			.map(|(pid, c)| (*pid, c.outbound.clone()))
			.collect();
		for (pid, outbound) in recipients {
			let _ = send_outbound_with_timeout(
				&outbound,
				payload.clone(),
				SHUTDOWN_NOTIFY_BUDGET,
				pid,
			);
		}
	}
}

#[derive(Debug, Clone, Copy)]
enum RosterEvent {
	Registered,
	Unregistered,
	LabelUpdated,
}

impl RosterEvent {
	fn as_str(self) -> &'static str {
		match self {
			RosterEvent::Registered => "registered",
			RosterEvent::Unregistered => "unregistered",
			RosterEvent::LabelUpdated => "label_updated",
		}
	}
}

#[derive(Debug, Clone, Copy)]
enum NamedResultsEvent {
	Saved,
	Deleted,
}

impl NamedResultsEvent {
	fn as_str(self) -> &'static str {
		match self {
			NamedResultsEvent::Saved => "saved",
			NamedResultsEvent::Deleted => "deleted",
		}
	}
}

pub(crate) fn coupler_kind_compat(
	kind: &CouplerKind,
	master_provides: &[InterestKind],
	follower_consumes: &[InterestKind],
) -> bool {
	let (accepted_in, produced_out) = coupler_kind_signature(kind);
	accepted_in.iter().any(|k| master_provides.contains(k))
		&& produced_out.iter().any(|k| follower_consumes.contains(k))
}

fn coupler_kind_signature(
	kind: &CouplerKind,
) -> (&'static [InterestKind], &'static [InterestKind]) {
	use InterestKind::*;
	match kind {
		CouplerKind::SentenceMirror => (&[Position, Span, Sentence], &[Sentence]),
		CouplerKind::Alignment { .. } => (&[Sentence, Span], &[Span]),
		CouplerKind::KwicSelection => (&[Hit], &[Span]),
		CouplerKind::DocPickerSelection => (&[Document], &[Document]),
		CouplerKind::NamedResultsSelection => (&[Results], &[Results]),
		CouplerKind::ConlluView => (&[Sentence], &[Sentence]),
	}
}

fn coupler_kind_names() -> &'static [&'static str] {
	#[allow(dead_code)]
	fn drift_guard(kind: &CouplerKind) {
		// adding a variant fails this match; keep the name list below in sync
		match kind {
			CouplerKind::SentenceMirror
			| CouplerKind::Alignment { .. }
			| CouplerKind::KwicSelection
			| CouplerKind::DocPickerSelection
			| CouplerKind::NamedResultsSelection
			| CouplerKind::ConlluView => {}
		}
	}
	&[
		"sentence_mirror",
		"alignment",
		"kwic_selection",
		"doc_picker_selection",
		"named_results_selection",
		"conllu_view",
	]
}

fn transform_interest(
	handle: &CorpusHandle,
	interest: &Interest,
	kind: &CouplerKind,
) -> Vec<Interest> {
	let (accepts, _) = coupler_kind_signature(kind);
	if !accepts.contains(&interest.kind()) {
		return Vec::new();
	}
	match kind {
		CouplerKind::SentenceMirror => match interest {
			Interest::Sentence { .. } => vec![interest.clone()],
			Interest::Position { doc, position } => {
				widen_position_to_sentence(handle, *doc, *position)
			}
			Interest::Span { doc, start, .. } => {
				widen_position_to_sentence(handle, *doc, *start)
			}
			_ => Vec::new(),
		},
		CouplerKind::Alignment { name } => {
			let source = match interest {
				Interest::Sentence { doc, sent } => {
					let Some(span) = sentence_to_span(handle, *doc, *sent) else {
						return Vec::new();
					};
					AlignmentSource { doc: *doc, start: span.start, end: span.end }
				}
				Interest::Span { doc, start, end } => {
					AlignmentSource { doc: *doc, start: *start, end: *end }
				}
				_ => return Vec::new(),
			};
			match project_alignment(&handle.corpus, name, source) {
				Ok(targets) => targets
					.into_iter()
					.map(|t| Interest::Span { doc: t.doc, start: t.start, end: t.end })
					.collect(),
				Err(_) => Vec::new(),
			}
		}
		CouplerKind::KwicSelection => match interest {
			Interest::Hit { result, hit_idx } => {
				let table = handle.results.read().expect("results lock poisoned");
				let Some(entry) = table.get(result) else { return Vec::new(); };
				let Some(hit) = entry.hits.get(*hit_idx as usize) else { return Vec::new(); };
				vec![Interest::Span {
					doc: hit.document_index,
					start: hit.span.start,
					end: hit.span.end,
				}]
			}
			_ => Vec::new(),
		},
		CouplerKind::DocPickerSelection
		| CouplerKind::NamedResultsSelection
		| CouplerKind::ConlluView => Vec::new(),
	}
}

fn widen_position_to_sentence(handle: &CorpusHandle, doc: u32, position: u64) -> Vec<Interest> {
	let corpus = &handle.corpus;
	if corpus.document_for_position(position) != Some(doc as usize) {
		return Vec::new();
	}
	let Some((global_idx, _)) = corpus.spans().containing_with_index("sentence", position) else {
		return Vec::new();
	};
	let Some(first) = corpus.first_sentence_of_document(doc as usize) else {
		return Vec::new();
	};
	vec![Interest::Sentence { doc, sent: (global_idx - first) as u32 }]
}

fn sentence_to_span(handle: &CorpusHandle, doc: u32, sent: u32) -> Option<Span> {
	let corpus = &handle.corpus;
	let first = corpus.first_sentence_of_document(doc as usize)?;
	let last = corpus.last_sentence_of_document(doc as usize)?;
	let global_idx = first.checked_add(sent as usize)?;
	if global_idx > last {
		return None;
	}
	corpus.spans().spans("sentence")?.get(global_idx).cloned()
}

fn generate_handle() -> ResultHandle {
	format!("r-{}", Uuid::new_v4())
}

fn now_rfc3339() -> String {
	time::OffsetDateTime::now_utc()
		.format(&time::format_description::well_known::Rfc3339)
		.expect("RFC 3339 format of OffsetDateTime::now_utc() is total")
}

fn try_send_outbound(
	tx: &SyncSender<Outbound>,
	payload: serde_json::Value,
	process_id: ProcessId,
) -> Result<(), ()> {
	match tx.try_send(payload) {
		Ok(()) => Ok(()),
		Err(std::sync::mpsc::TrySendError::Full(_)) => {
			tracing::warn!(
				process_id = process_id,
				"outbound queue full; dropping notification",
			);
			Err(())
		}
		Err(std::sync::mpsc::TrySendError::Disconnected(_)) => Err(()),
	}
}

fn send_outbound_with_timeout(
	tx: &SyncSender<Outbound>,
	mut payload: serde_json::Value,
	timeout: std::time::Duration,
	process_id: ProcessId,
) -> Result<(), ()> {
	let deadline = std::time::Instant::now() + timeout;
	loop {
		match tx.try_send(payload) {
			Ok(()) => return Ok(()),
			Err(std::sync::mpsc::TrySendError::Disconnected(_)) => return Err(()),
			Err(std::sync::mpsc::TrySendError::Full(returned)) => {
				payload = returned;
			}
		}
		if std::time::Instant::now() >= deadline {
			tracing::warn!(
				process_id = process_id,
				timeout_ms = timeout.as_millis() as u64,
				"outbound send timed out; dropping critical notification",
			);
			return Err(());
		}
		std::thread::sleep(std::time::Duration::from_millis(5));
	}
}

fn persist_error(error: std::io::Error) -> ProtocolError {
	tracing::error!(error = %error, "named-results persistence failed");
	ProtocolError::new(-32603, format!("persistence failure: {}", error))
}

pub(crate) enum LoadOutcome {
	Realized(ResultMetadata),
	Pending {
		handle: ResultHandle,
		cql: String,
		created_at: String,
	},
}

pub(crate) enum Command {
	Register {
		params: RegisterParams,
		outbound: SyncSender<Outbound>,
		reply: SyncSender<Result<RegisterReply, ProtocolError>>,
	},
	Unregister {
		process_id: ProcessId,
		reply: SyncSender<()>,
	},
	UpdateLabel {
		process_id: ProcessId,
		label: Option<String>,
		reply: SyncSender<Result<(), ProtocolError>>,
	},
	Roster {
		filter: Option<RosterFilter>,
		reply: SyncSender<Vec<ProcessInfo>>,
	},
	PublishInterest {
		process_id: ProcessId,
		interest: Interest,
	},
	#[cfg(test)]
	TestPanic,
	InsertResult {
		cql: String,
		hits: Vec<Hit>,
		reply: SyncSender<ResultMetadata>,
	},
	SaveResult {
		handle: ResultHandle,
		name: String,
		reply: SyncSender<Result<ResultMetadata, ProtocolError>>,
	},
	MaterializeResult {
		name: String,
		reply: SyncSender<Result<ResultMetadata, ProtocolError>>,
	},
	LoadNamed {
		name: String,
		reply: SyncSender<Result<LoadOutcome, ProtocolError>>,
	},
	InstallReplayedResult {
		entry: Arc<ResultEntry>,
		reply: SyncSender<()>,
	},
	ListNamed {
		reply: SyncSender<Vec<ResultMetadata>>,
	},
	DeleteNamed {
		name: String,
		reply: SyncSender<Result<(), ProtocolError>>,
	},
	DiscardHandle {
		handle: ResultHandle,
		reply: SyncSender<()>,
	},
	CouplerCreate {
		master_id: ProcessId,
		follower_id: ProcessId,
		kind: CouplerKind,
		reply: SyncSender<Result<CouplerId, ProtocolError>>,
	},
	CouplerRemove {
		coupler_id: CouplerId,
		reply: SyncSender<Result<(), ProtocolError>>,
	},
	CouplerList {
		process_id: Option<ProcessId>,
		reply: SyncSender<Vec<Coupler>>,
	},
	Subscribe {
		process_id: ProcessId,
		topic: Topic,
		reply: SyncSender<Result<(), ProtocolError>>,
	},
	Unsubscribe {
		process_id: ProcessId,
		topic: Topic,
		reply: SyncSender<()>,
	},
	InitiateShutdown {
		reason: ShutdownReason,
		reply: SyncSender<()>,
	},
}

pub(crate) fn run(mut state: State, commands: Receiver<Command>) {
	loop {
		let command = match state.idle_timeout {
			Some(timeout) => match commands.recv_timeout(timeout) {
				Ok(c) => c,
				Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
					state.check_idle_timeout();
					continue;
				}
				Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
			},
			None => match commands.recv() {
				Ok(c) => c,
				Err(_) => return,
			},
		};

		match command {
			Command::Register { params, outbound, reply } => {
				let _ = reply.send(state.register(params, outbound));
			}
			Command::Unregister { process_id, reply } => {
				state.unregister(process_id);
				let _ = reply.send(());
			}
			Command::UpdateLabel { process_id, label, reply } => {
				let _ = reply.send(state.update_label(process_id, label));
			}
			Command::Roster { filter, reply } => {
				let _ = reply.send(state.roster_snapshot(filter));
			}
			Command::PublishInterest { process_id, interest } => {
				state.publish_interest(process_id, interest);
			}
			#[cfg(test)]
			Command::TestPanic => panic!("test-induced state thread panic"),
			Command::InsertResult { cql, hits, reply } => {
				let _ = reply.send(state.insert_result(cql, hits));
			}
			Command::SaveResult { handle, name, reply } => {
				let _ = reply.send(state.save_result(handle, name));
			}
			Command::MaterializeResult { name, reply } => {
				let _ = reply.send(state.materialize_result(name));
			}
			Command::LoadNamed { name, reply } => {
				let _ = reply.send(state.load_named(&name));
			}
			Command::InstallReplayedResult { entry, reply } => {
				state.install_replayed_result(entry);
				let _ = reply.send(());
			}
			Command::ListNamed { reply } => {
				let _ = reply.send(state.list_named());
			}
			Command::DeleteNamed { name, reply } => {
				let _ = reply.send(state.delete_named(name));
			}
			Command::DiscardHandle { handle, reply } => {
				state.discard_handle(handle);
				let _ = reply.send(());
			}
			Command::CouplerCreate { master_id, follower_id, kind, reply } => {
				let _ = reply.send(state.coupler_create(master_id, follower_id, kind));
			}
			Command::CouplerRemove { coupler_id, reply } => {
				let _ = reply.send(state.coupler_remove(coupler_id));
			}
			Command::CouplerList { process_id, reply } => {
				let _ = reply.send(state.coupler_list(process_id));
			}
			Command::Subscribe { process_id, topic, reply } => {
				let _ = reply.send(state.subscribe(process_id, topic));
			}
			Command::Unsubscribe { process_id, topic, reply } => {
				state.unsubscribe(process_id, topic);
				let _ = reply.send(());
			}
			Command::InitiateShutdown { reason, reply } => {
				state.initiate_shutdown(reason);
				let _ = reply.send(());
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::dispatch::test_support::make_handle;
	use crate::protocol::{InterestKind, ProcessKind};
	use std::sync::mpsc::sync_channel;

	fn make_state() -> State {
		State::new(1, make_handle(), crate::shutdown::ShutdownCoordinator::dummy())
	}

	fn dummy_outbound() -> SyncSender<Outbound> {
		let (tx, _rx) = sync_channel(256);
		tx
	}

	fn make_register_params(kind: ProcessKind) -> RegisterParams {
		RegisterParams {
			protocol_version: PROTOCOL_VERSION,
			kind,
			label: None,
			provides: vec![],
			consumes: vec![],
		}
	}

	fn register(state: &mut State) -> ProcessId {
		state
			.register(make_register_params(ProcessKind::External), dummy_outbound())
			.unwrap()
			.process_id
	}

	fn register_compatible(state: &mut State) -> ProcessId {
		let params = RegisterParams {
			protocol_version: PROTOCOL_VERSION,
			kind: ProcessKind::External,
			label: None,
			provides: vec![InterestKind::Position, InterestKind::Span, InterestKind::Sentence],
			consumes: vec![InterestKind::Sentence],
		};
		state.register(params, dummy_outbound()).unwrap().process_id
	}

	#[test]
	fn register_happy_path() {
		let mut state = make_state();
		let reply = state
			.register(make_register_params(ProcessKind::External), dummy_outbound())
			.unwrap();
		assert_eq!(reply.process_id, 1);
		assert_eq!(reply.daemon_epoch, 1);
		assert_eq!(reply.protocol_version, PROTOCOL_VERSION);
	}

	#[test]
	fn register_assigns_increasing_ids() {
		let mut state = make_state();
		let r1 = register(&mut state);
		let r2 = register(&mut state);
		assert_eq!(r1, 1);
		assert_eq!(r2, 2);
	}

	#[test]
	fn register_version_mismatch() {
		let mut state = make_state();
		let mut params = make_register_params(ProcessKind::External);
		params.protocol_version = 999;
		let err = state.register(params, dummy_outbound()).unwrap_err();
		assert_eq!(err.code, error_codes::PROTOCOL_VERSION_MISMATCH);
	}

	#[test]
	fn register_capabilities_includes_all_coupler_kinds() {
		let mut state = make_state();
		let reply = state
			.register(make_register_params(ProcessKind::External), dummy_outbound())
			.unwrap();
		for &name in coupler_kind_names() {
			assert!(
				reply.capabilities.coupler_kinds.iter().any(|k| k == name),
				"expected capability '{}' present",
				name,
			);
		}
	}

	#[test]
	fn unregister_drops_couplers_involving_process() {
		let mut state = make_state();
		let p1 = register_compatible(&mut state);
		let p2 = register_compatible(&mut state);
		let coupler_id = state
			.coupler_create(p1, p2, CouplerKind::SentenceMirror)
			.unwrap();
		assert!(state.couplers.contains_key(&coupler_id));

		state.unregister(p1);
		assert!(!state.couplers.contains_key(&coupler_id));
	}

	#[test]
	fn unregister_removes_from_subscriptions() {
		let mut state = make_state();
		let pid = register(&mut state);
		state.subscribe(pid, Topic::RosterChanged).unwrap();
		assert!(state.subscriptions[&Topic::RosterChanged].contains(&pid));

		state.unregister(pid);
		assert!(!state.subscriptions[&Topic::RosterChanged].contains(&pid));
	}

	#[test]
	fn coupler_create_unknown_master_rejected() {
		let mut state = make_state();
		let p = register(&mut state);
		let err = state
			.coupler_create(999, p, CouplerKind::SentenceMirror)
			.unwrap_err();
		assert_eq!(err.code, error_codes::PROCESS_NOT_FOUND);
	}

	#[test]
	fn coupler_create_unknown_follower_rejected() {
		let mut state = make_state();
		let p = register(&mut state);
		let err = state
			.coupler_create(p, 999, CouplerKind::SentenceMirror)
			.unwrap_err();
		assert_eq!(err.code, error_codes::PROCESS_NOT_FOUND);
	}

	#[test]
	fn coupler_create_self_loop_rejected_as_cycle() {
		let mut state = make_state();
		let p = register(&mut state);
		let err = state
			.coupler_create(p, p, CouplerKind::SentenceMirror)
			.unwrap_err();
		assert_eq!(err.code, error_codes::COUPLER_CYCLE);
	}

	#[test]
	fn coupler_create_two_node_cycle_rejected() {
		let mut state = make_state();
		let p1 = register_compatible(&mut state);
		let p2 = register_compatible(&mut state);
		state
			.coupler_create(p1, p2, CouplerKind::SentenceMirror)
			.unwrap();
		let err = state
			.coupler_create(p2, p1, CouplerKind::SentenceMirror)
			.unwrap_err();
		assert_eq!(err.code, error_codes::COUPLER_CYCLE);
	}

	#[test]
	fn coupler_create_three_node_cycle_rejected() {
		let mut state = make_state();
		let p1 = register_compatible(&mut state);
		let p2 = register_compatible(&mut state);
		let p3 = register_compatible(&mut state);
		state
			.coupler_create(p1, p2, CouplerKind::SentenceMirror)
			.unwrap();
		state
			.coupler_create(p2, p3, CouplerKind::SentenceMirror)
			.unwrap();
		let err = state
			.coupler_create(p3, p1, CouplerKind::SentenceMirror)
			.unwrap_err();
		assert_eq!(err.code, error_codes::COUPLER_CYCLE);
	}

	#[test]
	fn coupler_create_incompatible_rejected() {
		let mut state = make_state();
		let p_master = state
			.register(
				RegisterParams {
					protocol_version: PROTOCOL_VERSION,
					kind: ProcessKind::External,
					label: None,
					provides: vec![InterestKind::Hit],
					consumes: vec![],
				},
				dummy_outbound(),
			)
			.unwrap()
			.process_id;
		let p_follower = state
			.register(
				RegisterParams {
					protocol_version: PROTOCOL_VERSION,
					kind: ProcessKind::External,
					label: None,
					provides: vec![],
					consumes: vec![InterestKind::Sentence],
				},
				dummy_outbound(),
			)
			.unwrap()
			.process_id;
		let err = state
			.coupler_create(p_master, p_follower, CouplerKind::SentenceMirror)
			.unwrap_err();
		assert_eq!(err.code, error_codes::COUPLER_INCOMPATIBLE);
		assert!(err.data.is_some());
	}

	#[test]
	fn coupler_remove_unknown_rejected() {
		let mut state = make_state();
		let err = state.coupler_remove(999).unwrap_err();
		assert_eq!(err.code, error_codes::COUPLER_NOT_FOUND);
	}

	#[test]
	fn insert_result_returns_prefixed_handle() {
		let mut state = make_state();
		let handle = state.insert_result("foo".to_string(), vec![]).handle;
		assert!(handle.starts_with("r-"), "handle = {}", handle);
		assert!(handle.len() > "r-".len());
	}

	#[test]
	fn insert_result_distinct_handles() {
		let mut state = make_state();
		let h1 = state.insert_result("foo".to_string(), vec![]).handle;
		let h2 = state.insert_result("foo".to_string(), vec![]).handle;
		assert_ne!(h1, h2);
	}

	#[test]
	fn save_result_duplicate_name_rejected() {
		let mut state = make_state();
		let h1 = state.insert_result("foo".to_string(), vec![]).handle;
		let h2 = state.insert_result("foo".to_string(), vec![]).handle;
		state.save_result(h1, "named".to_string()).unwrap();
		let err = state.save_result(h2, "named".to_string()).unwrap_err();
		assert_eq!(err.code, error_codes::NAMED_RESULT_ALREADY_EXISTS);
	}

	#[test]
	fn save_result_invalid_handle_rejected() {
		let mut state = make_state();
		let err = state
			.save_result("r-nonexistent".to_string(), "named".to_string())
			.unwrap_err();
		assert_eq!(err.code, error_codes::RESULT_HANDLE_INVALID);
	}

	#[test]
	fn materialize_unknown_name_rejected() {
		let mut state = make_state();
		let err = state.materialize_result("absent".to_string()).unwrap_err();
		assert_eq!(err.code, error_codes::NAMED_RESULT_NOT_FOUND);
	}

	#[test]
	fn materialize_already_materialized_rejected() {
		let mut state = make_state();
		let h = state.insert_result("foo".to_string(), vec![]).handle;
		state.save_result(h, "named".to_string()).unwrap();
		state.materialize_result("named".to_string()).unwrap();
		let err = state.materialize_result("named".to_string()).unwrap_err();
		assert_eq!(err.code, error_codes::RESULT_ALREADY_MATERIALIZED);
	}

	#[test]
	fn materialize_sets_materialized_at_and_preserves_created_at() {
		let mut state = make_state();
		let h = state.insert_result("foo".to_string(), vec![]).handle;
		state.save_result(h.clone(), "named".to_string()).unwrap();

		let created_at_before = state.handle
			.results
			.read()
			.unwrap()
			.get(&h)
			.unwrap()
			.metadata
			.created_at
			.clone();
		assert!(state.handle.results.read().unwrap().get(&h).unwrap().metadata.materialized_at.is_none());

		let metadata = state.materialize_result("named".to_string()).unwrap();
		assert_eq!(metadata.created_at, created_at_before);
		assert!(metadata.materialized_at.is_some());
		assert!(!metadata.materialized_at.as_ref().unwrap().is_empty());
	}

	#[test]
	fn delete_named_removes_handle_and_table_entry() {
		let mut state = make_state();
		let h = state.insert_result("foo".to_string(), vec![]).handle;
		state.save_result(h.clone(), "named".to_string()).unwrap();
		assert!(state.handle.results.read().unwrap().contains_key(&h));

		state.delete_named("named".to_string()).unwrap();
		assert!(!state.handle.results.read().unwrap().contains_key(&h));
		assert!(!state.named_results.contains_key("named"));
	}

	#[test]
	fn delete_named_unknown_rejected() {
		let mut state = make_state();
		let err = state.delete_named("absent".to_string()).unwrap_err();
		assert_eq!(err.code, error_codes::NAMED_RESULT_NOT_FOUND);
	}

	#[test]
	fn discard_handle_session_form_removed() {
		let mut state = make_state();
		let h = state.insert_result("foo".to_string(), vec![]).handle;
		assert!(state.handle.results.read().unwrap().contains_key(&h));

		state.discard_handle(h.clone());
		assert!(!state.handle.results.read().unwrap().contains_key(&h));
	}

	#[test]
	fn discard_handle_named_is_no_op() {
		let mut state = make_state();
		let h = state.insert_result("foo".to_string(), vec![]).handle;
		state.save_result(h.clone(), "named".to_string()).unwrap();

		state.discard_handle(h.clone());
		assert!(state.handle.results.read().unwrap().contains_key(&h));
		assert!(state.named_results.contains_key("named"));
	}

	fn make_info(kind: ProcessKind, provides: Vec<InterestKind>) -> ProcessInfo {
		ProcessInfo {
			id: 1,
			kind,
			label: None,
			provides,
			consumes: vec![],
			current_interest: None,
		}
	}

	#[test]
	fn roster_filter_kind_match_and_mismatch() {
		let info = make_info(ProcessKind::Reader, vec![]);
		let filter = RosterFilter {
			kinds: vec![ProcessKind::Reader],
			..Default::default()
		};
		assert!(filter.matches(&info));

		let filter = RosterFilter {
			kinds: vec![ProcessKind::Kwic],
			..Default::default()
		};
		assert!(!filter.matches(&info));
	}

	#[test]
	fn roster_filter_provides_any_of() {
		let info = make_info(
			ProcessKind::Reader,
			vec![InterestKind::Sentence, InterestKind::Document],
		);
		let filter = RosterFilter {
			provides_any_of: vec![InterestKind::Sentence],
			..Default::default()
		};
		assert!(filter.matches(&info));

		let filter = RosterFilter {
			provides_any_of: vec![InterestKind::Hit],
			..Default::default()
		};
		assert!(!filter.matches(&info));
	}

	#[test]
	fn roster_filter_combines_with_and() {
		let info = make_info(ProcessKind::Reader, vec![InterestKind::Sentence]);
		let filter = RosterFilter {
			kinds: vec![ProcessKind::Reader],
			provides_any_of: vec![InterestKind::Sentence],
			..Default::default()
		};
		assert!(filter.matches(&info));

		let filter = RosterFilter {
			kinds: vec![ProcessKind::Reader],
			provides_any_of: vec![InterestKind::Hit],
			..Default::default()
		};
		assert!(!filter.matches(&info));
	}

	#[test]
	fn roster_filter_within_field_is_or() {
		let info = make_info(ProcessKind::Reader, vec![InterestKind::Sentence]);
		let filter = RosterFilter {
			provides_any_of: vec![InterestKind::Sentence, InterestKind::Hit],
			..Default::default()
		};
		assert!(filter.matches(&info));

		let filter = RosterFilter {
			provides_any_of: vec![InterestKind::Document, InterestKind::Hit],
			..Default::default()
		};
		assert!(!filter.matches(&info));
	}

	#[test]
	fn roster_filter_consumes_any_of() {
		let info = ProcessInfo {
			id: 1,
			kind: ProcessKind::Reader,
			label: None,
			provides: vec![],
			consumes: vec![InterestKind::Sentence],
			current_interest: None,
		};
		let filter = RosterFilter {
			consumes_any_of: vec![InterestKind::Sentence, InterestKind::Span],
			..Default::default()
		};
		assert!(filter.matches(&info));

		let filter = RosterFilter {
			consumes_any_of: vec![InterestKind::Hit],
			..Default::default()
		};
		assert!(!filter.matches(&info));
	}

	#[test]
	fn coupler_kind_compat_sentence_mirror_accepts_position_to_sentence() {
		assert!(coupler_kind_compat(
			&CouplerKind::SentenceMirror,
			&[InterestKind::Position],
			&[InterestKind::Sentence],
		));
	}

	#[test]
	fn coupler_kind_compat_sentence_mirror_rejects_hit_provider() {
		assert!(!coupler_kind_compat(
			&CouplerKind::SentenceMirror,
			&[InterestKind::Hit],
			&[InterestKind::Sentence],
		));
	}

	#[test]
	fn coupler_kind_compat_alignment_accepts_span_to_span() {
		assert!(coupler_kind_compat(
			&CouplerKind::Alignment { name: "labse".to_string() },
			&[InterestKind::Span],
			&[InterestKind::Span],
		));
	}

	#[test]
	fn coupler_kind_compat_kwic_requires_hit_provider() {
		assert!(coupler_kind_compat(
			&CouplerKind::KwicSelection,
			&[InterestKind::Hit],
			&[InterestKind::Span],
		));
		assert!(!coupler_kind_compat(
			&CouplerKind::KwicSelection,
			&[InterestKind::Sentence],
			&[InterestKind::Span],
		));
	}

	#[test]
	fn coupler_kind_compat_rejects_empty_master_provides() {
		assert!(!coupler_kind_compat(
			&CouplerKind::SentenceMirror,
			&[],
			&[InterestKind::Sentence],
		));
	}

	#[test]
	fn coupler_kind_compat_rejects_empty_follower_consumes() {
		assert!(!coupler_kind_compat(
			&CouplerKind::SentenceMirror,
			&[InterestKind::Position],
			&[],
		));
	}

	#[test]
	fn coupler_kind_compat_tbd_kinds_self_to_self() {
		assert!(coupler_kind_compat(
			&CouplerKind::DocPickerSelection,
			&[InterestKind::Document],
			&[InterestKind::Document],
		));
		assert!(coupler_kind_compat(
			&CouplerKind::NamedResultsSelection,
			&[InterestKind::Results],
			&[InterestKind::Results],
		));
		assert!(coupler_kind_compat(
			&CouplerKind::ConlluView,
			&[InterestKind::Sentence],
			&[InterestKind::Sentence],
		));
	}

	fn find_doc(corpus: &montre_index::Corpus, needle: &str) -> u32 {
		corpus
			.document_names()
			.iter()
			.position(|n| n.contains(needle))
			.map(|i| i as u32)
			.unwrap_or_else(|| panic!("no document containing '{}'", needle))
	}

	#[test]
	fn transform_sentence_mirror_passes_sentence_through() {
		let handle = make_handle();
		let doc = find_doc(&handle.corpus, "la_maison");
		let interest = Interest::Sentence { doc, sent: 0 };
		let out = transform_interest(&handle, &interest, &CouplerKind::SentenceMirror);
		assert_eq!(out.len(), 1);
		match &out[0] {
			Interest::Sentence { doc: d, sent: s } => {
				assert_eq!(*d, doc);
				assert_eq!(*s, 0);
			}
			other => panic!("expected Sentence, got {:?}", other),
		}
	}

	#[test]
	fn transform_sentence_mirror_widens_position_to_containing_sentence() {
		let handle = make_handle();
		let doc = find_doc(&handle.corpus, "la_maison");
		let span = sentence_to_span(&handle, doc, 0).expect("sentence span");
		let interest = Interest::Position { doc, position: span.start };
		let out = transform_interest(&handle, &interest, &CouplerKind::SentenceMirror);
		assert_eq!(out.len(), 1);
		match &out[0] {
			Interest::Sentence { doc: d, sent: s } => {
				assert_eq!(*d, doc);
				assert_eq!(*s, 0);
			}
			other => panic!("expected Sentence, got {:?}", other),
		}
	}

	#[test]
	fn transform_sentence_mirror_widens_span_to_sentence_containing_start() {
		let handle = make_handle();
		let doc = find_doc(&handle.corpus, "la_maison");
		let span = sentence_to_span(&handle, doc, 0).expect("sentence span");
		let interest = Interest::Span { doc, start: span.start, end: span.end };
		let out = transform_interest(&handle, &interest, &CouplerKind::SentenceMirror);
		assert_eq!(out.len(), 1);
		match &out[0] {
			Interest::Sentence { doc: d, sent: s } => {
				assert_eq!(*d, doc);
				assert_eq!(*s, 0);
			}
			other => panic!("expected Sentence, got {:?}", other),
		}
	}

	#[test]
	fn transform_sentence_mirror_position_outside_any_sentence_returns_empty() {
		let handle = make_handle();
		let doc = find_doc(&handle.corpus, "la_maison");
		let interest = Interest::Position { doc, position: u64::MAX };
		let out = transform_interest(&handle, &interest, &CouplerKind::SentenceMirror);
		assert!(out.is_empty());
	}

	#[test]
	fn transform_sentence_mirror_defensively_rejects_hit_input() {
		let handle = make_handle();
		let interest = Interest::Hit { result: "r-x".to_string(), hit_idx: 0 };
		let out = transform_interest(&handle, &interest, &CouplerKind::SentenceMirror);
		assert!(out.is_empty());
	}

	#[test]
	fn transform_alignment_projects_sentence_to_target_doc() {
		let handle = make_handle();
		let source_doc = find_doc(&handle.corpus, "la_maison");
		let target_doc = find_doc(&handle.corpus, "the_house");
		let interest = Interest::Sentence { doc: source_doc, sent: 0 };
		let kind = CouplerKind::Alignment { name: "sentence".to_string() };
		let out = transform_interest(&handle, &interest, &kind);
		assert!(!out.is_empty());
		for derived in &out {
			match derived {
				Interest::Span { doc, start, end } => {
					assert_eq!(*doc, target_doc);
					assert!(end > start);
				}
				other => panic!("expected Span, got {:?}", other),
			}
		}
	}

	#[test]
	fn transform_alignment_projects_span_to_target_doc() {
		let handle = make_handle();
		let source_doc = find_doc(&handle.corpus, "la_maison");
		let target_doc = find_doc(&handle.corpus, "the_house");
		let span = sentence_to_span(&handle, source_doc, 0).expect("sentence span");
		let interest = Interest::Span { doc: source_doc, start: span.start, end: span.end };
		let kind = CouplerKind::Alignment { name: "sentence".to_string() };
		let out = transform_interest(&handle, &interest, &kind);
		assert!(!out.is_empty());
		for derived in &out {
			match derived {
				Interest::Span { doc, .. } => assert_eq!(*doc, target_doc),
				other => panic!("expected Span, got {:?}", other),
			}
		}
	}

	#[test]
	fn transform_alignment_unknown_name_returns_empty() {
		let handle = make_handle();
		let doc = find_doc(&handle.corpus, "la_maison");
		let interest = Interest::Span { doc, start: 0, end: 5 };
		let kind = CouplerKind::Alignment { name: "totally-not-an-alignment".to_string() };
		let out = transform_interest(&handle, &interest, &kind);
		assert!(out.is_empty());
	}

	#[test]
	fn transform_alignment_defensively_rejects_position_input() {
		let handle = make_handle();
		let doc = find_doc(&handle.corpus, "la_maison");
		let interest = Interest::Position { doc, position: 0 };
		let kind = CouplerKind::Alignment { name: "sentence".to_string() };
		let out = transform_interest(&handle, &interest, &kind);
		assert!(out.is_empty());
	}

	#[test]
	fn transform_kwic_projects_hit_to_span() {
		let mut state = make_state();
		let doc = find_doc(&state.handle.corpus, "la_maison");
		let executor_hit = Hit {
			span: Span { start: 3, end: 7 },
			document_index: doc,
			sentence_index: 0,
			captures: vec![],
		};
		let result_handle = state.insert_result("test".to_string(), vec![executor_hit]).handle;
		let interest = Interest::Hit { result: result_handle, hit_idx: 0 };
		let out = transform_interest(&state.handle, &interest, &CouplerKind::KwicSelection);
		assert_eq!(out.len(), 1);
		match &out[0] {
			Interest::Span { doc: d, start, end } => {
				assert_eq!(*d, doc);
				assert_eq!(*start, 3);
				assert_eq!(*end, 7);
			}
			other => panic!("expected Span, got {:?}", other),
		}
	}

	#[test]
	fn transform_kwic_unknown_result_returns_empty() {
		let handle = make_handle();
		let interest = Interest::Hit { result: "r-bogus".to_string(), hit_idx: 0 };
		let out = transform_interest(&handle, &interest, &CouplerKind::KwicSelection);
		assert!(out.is_empty());
	}

	#[test]
	fn transform_kwic_hit_idx_out_of_range_returns_empty() {
		let mut state = make_state();
		let result_handle = state.insert_result("test".to_string(), vec![]).handle;
		let interest = Interest::Hit { result: result_handle, hit_idx: 99 };
		let out = transform_interest(&state.handle, &interest, &CouplerKind::KwicSelection);
		assert!(out.is_empty());
	}

	#[test]
	fn transform_inert_kinds_return_empty() {
		let handle = make_handle();
		let doc = find_doc(&handle.corpus, "la_maison");
		assert!(transform_interest(
			&handle,
			&Interest::Document { doc },
			&CouplerKind::DocPickerSelection,
		)
		.is_empty());
		assert!(transform_interest(
			&handle,
			&Interest::Results { handle: "r-x".to_string() },
			&CouplerKind::NamedResultsSelection,
		)
		.is_empty());
		assert!(transform_interest(
			&handle,
			&Interest::Sentence { doc, sent: 0 },
			&CouplerKind::ConlluView,
		)
		.is_empty());
	}

	fn state_sharing_dir(state_dir: std::path::PathBuf) -> State {
		use crate::dispatch::test_support::corpus_fixture;
		use std::sync::RwLock;
		let (path, canonical) = corpus_fixture();
		let corpus = Arc::new(montre_index::open(path).expect("corpus open"));
		let handle = Arc::new(CorpusHandle {
			corpus,
			corpus_id: "persist-test".to_string(),
			canonical_path: canonical.to_path_buf(),
			state_dir,
			results: Arc::new(RwLock::new(HashMap::new())),
		});
		State::new(1, handle, crate::shutdown::ShutdownCoordinator::dummy())
	}

	#[test]
	fn save_persists_and_replay_restores_named_record() {
		let temp = tempfile::TempDir::new().expect("tempdir");
		let state_dir = temp.path().to_path_buf();

		let mut first = state_sharing_dir(state_dir.clone());
		let handle = first.insert_result("[pos=\"NOUN\"]".to_string(), vec![]).handle;
		first.save_result(handle.clone(), "saved".to_string()).unwrap();
		drop(first);

		let mut second = state_sharing_dir(state_dir);
		second.replay_named_results().expect("replay");

		let record = second.named_results.get("saved").expect("name present after replay");
		assert_eq!(record.handle, handle);
		assert_eq!(record.cql, "[pos=\"NOUN\"]");
	}

	#[test]
	fn delete_persists_removal_and_replay_omits_it() {
		let temp = tempfile::TempDir::new().expect("tempdir");
		let state_dir = temp.path().to_path_buf();

		let mut first = state_sharing_dir(state_dir.clone());
		let kept = first.insert_result("[pos=\"NOUN\"]".to_string(), vec![]).handle;
		let doomed = first.insert_result("[pos=\"ADJ\"]".to_string(), vec![]).handle;
		first.save_result(kept, "kept".to_string()).unwrap();
		first.save_result(doomed, "doomed".to_string()).unwrap();
		first.delete_named("doomed".to_string()).unwrap();
		drop(first);

		let mut second = state_sharing_dir(state_dir);
		second.replay_named_results().expect("replay");

		assert!(second.named_results.contains_key("kept"));
		assert!(!second.named_results.contains_key("doomed"));
	}

	#[test]
	fn initiate_shutdown_marks_coordinator_and_is_idempotent() {
		let coordinator = ShutdownCoordinator::dummy();
		let state = State::new(1, make_handle(), Arc::clone(&coordinator));
		assert!(!coordinator.is_shutting_down());
		state.initiate_shutdown(ShutdownReason::Requested);
		assert!(coordinator.is_shutting_down());
		state.initiate_shutdown(ShutdownReason::Signal);
		assert!(coordinator.is_shutting_down());
	}

	#[test]
	fn initiate_shutdown_broadcasts_notification_to_all_roster_members() {
		let coordinator = ShutdownCoordinator::dummy();
		let mut state = State::new(1, make_handle(), Arc::clone(&coordinator));

		let (tx_a, rx_a) = sync_channel::<Outbound>(16);
		let (tx_b, rx_b) = sync_channel::<Outbound>(16);
		state.register(make_register_params(ProcessKind::Reader), tx_a).unwrap();
		state.register(make_register_params(ProcessKind::Kwic), tx_b).unwrap();

		state.initiate_shutdown(ShutdownReason::Requested);

		let message_a = rx_a.try_recv().expect("a should receive shutdown");
		let message_b = rx_b.try_recv().expect("b should receive shutdown");
		assert_eq!(message_a["method"], "notification.shutdown");
		assert_eq!(message_b["method"], "notification.shutdown");
		assert_eq!(message_a["params"]["reason"], "requested");
		assert_eq!(message_a["params"]["in_seconds"], 0);
	}

	#[test]
	fn notify_shutdown_waits_for_outbound_drain_under_backpressure() {
		use std::time::Duration;

		let coordinator = ShutdownCoordinator::dummy();
		let mut state = State::new(1, make_handle(), Arc::clone(&coordinator));

		let (tx, rx) = sync_channel::<Outbound>(1);
		state.register(make_register_params(ProcessKind::Reader), tx.clone()).unwrap();

		tx.send(serde_json::json!({"filler": true}))
			.expect("filler fits in the fresh empty channel");

		let drainer = std::thread::spawn(move || {
			std::thread::sleep(Duration::from_millis(60));
			let mut messages = Vec::new();
			while let Ok(message) = rx.recv_timeout(Duration::from_millis(150)) {
				messages.push(message);
			}
			messages
		});

		state.initiate_shutdown(ShutdownReason::Requested);

		let drained = drainer.join().expect("drainer join");
		assert!(
			drained.iter().any(|m| m["method"] == "notification.shutdown"),
			"shutdown notification must arrive after the channel drains; got {:?}",
			drained,
		);
	}

	#[test]
	fn notify_shutdown_bounded_when_outbound_never_drains() {
		use std::time::{Duration, Instant};

		let coordinator = ShutdownCoordinator::dummy();
		let mut state = State::new(1, make_handle(), Arc::clone(&coordinator));

		let (tx, rx) = sync_channel::<Outbound>(1);
		state.register(make_register_params(ProcessKind::Reader), tx.clone()).unwrap();

		tx.send(serde_json::json!({"filler": true}))
			.expect("filler fits in the fresh empty channel");

		let _block = rx;

		let start = Instant::now();
		state.initiate_shutdown(ShutdownReason::Requested);
		let elapsed = start.elapsed();

		assert!(
			elapsed < Duration::from_secs(1),
			"initiate_shutdown must be bounded even when a recipient is wedged; took {:?}",
			elapsed,
		);
	}

	#[test]
	fn roster_emptied_at_initially_set_at_startup() {
		let state = make_state();
		assert!(state.roster_emptied_at.is_some());
	}

	#[test]
	fn register_clears_roster_emptied_at() {
		let mut state = make_state();
		state
			.register(make_register_params(ProcessKind::Reader), dummy_outbound())
			.unwrap();
		assert!(state.roster_emptied_at.is_none());
	}

	#[test]
	fn unregister_to_empty_sets_roster_emptied_at() {
		let mut state = make_state();
		let pid = state
			.register(make_register_params(ProcessKind::Reader), dummy_outbound())
			.unwrap()
			.process_id;
		assert!(state.roster_emptied_at.is_none());
		state.unregister(pid);
		assert!(state.roster_emptied_at.is_some());
	}

	#[test]
	fn unregister_with_remaining_roster_does_not_set_roster_emptied_at() {
		let mut state = make_state();
		let pid_a = state
			.register(make_register_params(ProcessKind::Reader), dummy_outbound())
			.unwrap()
			.process_id;
		let _pid_b = state
			.register(make_register_params(ProcessKind::Kwic), dummy_outbound())
			.unwrap()
			.process_id;
		state.unregister(pid_a);
		assert!(state.roster_emptied_at.is_none());
	}

	#[test]
	fn check_idle_timeout_fires_when_threshold_exceeded() {
		use std::time::{Duration, Instant};
		let coordinator = ShutdownCoordinator::dummy();
		let mut state = State::new(1, make_handle(), Arc::clone(&coordinator));
		state.set_idle_timeout(Some(Duration::from_secs(1)));
		state.roster_emptied_at = Some(
			Instant::now()
				.checked_sub(Duration::from_secs(2))
				.expect("instant subtract"),
		);
		assert!(!coordinator.is_shutting_down());
		state.check_idle_timeout();
		assert!(coordinator.is_shutting_down());
	}

	#[test]
	fn check_idle_timeout_skips_when_roster_emptied_at_is_none() {
		use std::time::Duration;
		let coordinator = ShutdownCoordinator::dummy();
		let mut state = State::new(1, make_handle(), Arc::clone(&coordinator));
		state.set_idle_timeout(Some(Duration::from_millis(1)));
		state.roster_emptied_at = None;
		state.check_idle_timeout();
		assert!(!coordinator.is_shutting_down());
	}

	#[test]
	fn check_idle_timeout_skips_when_idle_timeout_disabled() {
		use std::time::{Duration, Instant};
		let coordinator = ShutdownCoordinator::dummy();
		let mut state = State::new(1, make_handle(), Arc::clone(&coordinator));
		state.set_idle_timeout(None);
		state.roster_emptied_at = Some(
			Instant::now()
				.checked_sub(Duration::from_secs(2))
				.expect("instant subtract"),
		);
		state.check_idle_timeout();
		assert!(!coordinator.is_shutting_down());
	}
}
