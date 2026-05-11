use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::Arc;

use montre_index::SpanIndex;
use montre_query::executor::Hit;
use uuid::Uuid;

use crate::handlers::alignment::project_alignment;
use crate::protocol::error_codes;
use crate::protocol::{
	Anchor, AnchorId, AnchorKind, AlignmentSource, Capabilities, Interest, InterestKind, ProcessId,
	ProcessInfo, ProtocolError, RegisterParams, RegisterReply, ResultForm, ResultHandle,
	ResultMetadata, RosterFilter, Span, Topic, PROTOCOL_VERSION,
};
use crate::CorpusHandle;

const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

pub(crate) type ResultsTable = HashMap<ResultHandle, Arc<ResultEntry>>;

pub(crate) struct ResultEntry {
	pub cql: String,
	pub hits: Vec<Hit>,
	pub metadata: ResultMetadata,
}

pub(crate) enum Outbound {
	Message(serde_json::Value),
}

pub(crate) struct Connection {
	info: ProcessInfo,
	outbound: SyncSender<Outbound>,
}

pub(crate) struct State {
	daemon_epoch: u64,
	handle: Arc<CorpusHandle>,
	roster: HashMap<ProcessId, Connection>,
	anchors: HashMap<AnchorId, Anchor>,
	subscriptions: HashMap<Topic, HashSet<ProcessId>>,
	named_results: HashMap<String, ResultHandle>,
	next_process_id: ProcessId,
	next_anchor_id: AnchorId,
}

impl State {
	pub(crate) fn new(daemon_epoch: u64, handle: Arc<CorpusHandle>) -> Self {
		Self {
			daemon_epoch,
			handle,
			roster: HashMap::new(),
			anchors: HashMap::new(),
			subscriptions: HashMap::new(),
			named_results: HashMap::new(),
			next_process_id: 1,
			next_anchor_id: 1,
		}
	}

	fn allocate_process_id(&mut self) -> ProcessId {
		let id = self.next_process_id;
		self.next_process_id = self.next_process_id.wrapping_add(1);
		id
	}

	fn allocate_anchor_id(&mut self) -> AnchorId {
		let id = self.next_anchor_id;
		self.next_anchor_id = self.next_anchor_id.wrapping_add(1);
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

		self.notify_roster(RosterEvent::Registered, info);

		Ok(RegisterReply {
			process_id,
			server_version: SERVER_VERSION.to_string(),
			protocol_version: PROTOCOL_VERSION,
			daemon_epoch: self.daemon_epoch,
			capabilities: Capabilities {
				observations: false,
				workspaces: false,
				anchor_kinds: all_anchor_kinds().map(|k| k.type_name().to_string()).collect(),
			},
		})
	}

	fn unregister(&mut self, process_id: ProcessId) {
		let Some(connection) = self.roster.remove(&process_id) else {
			return;
		};

		let dropped: Vec<AnchorId> = self
			.anchors
			.iter()
			.filter(|(_, a)| a.master == process_id || a.follower == process_id)
			.map(|(id, _)| *id)
			.collect();
		for id in dropped {
			self.anchors.remove(&id);
		}

		for subscribers in self.subscriptions.values_mut() {
			subscribers.remove(&process_id);
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
				Some(f) => roster_filter_matches(f, &c.info),
			})
			.map(|c| c.info.clone())
			.collect()
	}

	fn publish_interest(&mut self, process_id: ProcessId, interest: Interest) {
		let Some(connection) = self.roster.get_mut(&process_id) else {
			return;
		};
		connection.info.current_interest = Some(interest.clone());

		let derivative_targets: Vec<(AnchorId, ProcessId, AnchorKind)> = self
			.anchors
			.values()
			.filter(|a| a.master == process_id)
			.map(|a| (a.id, a.follower, a.kind.clone()))
			.collect();

		for (anchor_id, follower, kind) in derivative_targets {
			let transformed = transform_interest(&self.handle, &interest, &kind);
			let Some(outbound) = self.roster.get(&follower).map(|c| c.outbound.clone()) else {
				continue;
			};
			for derived in transformed {
				let payload = serde_json::json!({
					"jsonrpc": "2.0",
					"method": "notification.anchor_update",
					"params": {
						"anchor_id": anchor_id,
						"interest": derived,
					},
				});
				let _ = try_send_outbound(&outbound, payload, follower);
			}
		}
	}

	fn insert_result(&mut self, cql: String, hits: Vec<Hit>) -> ResultHandle {
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
		let entry = Arc::new(ResultEntry {
			cql,
			hits,
			metadata,
		});
		self.handle.results
			.write()
			.expect("results lock poisoned")
			.insert(handle.clone(), entry);
		handle
	}

	fn save_result(
		&mut self,
		handle: ResultHandle,
		name: String,
	) -> Result<(), ProtocolError> {
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

		let metadata = promoted.metadata.clone();
		self.handle.results
			.write()
			.expect("results lock poisoned")
			.insert(handle.clone(), promoted);
		self.named_results.insert(name.clone(), handle);

		self.notify_named_results(NamedResultsEvent::Saved, name, Some(metadata));
		Ok(())
	}

	fn materialize_result(&mut self, name: String) -> Result<ResultMetadata, ProtocolError> {
		let handle = self.named_results.get(&name).cloned().ok_or_else(|| {
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

	fn load_named(&self, name: &str) -> Result<ResultMetadata, ProtocolError> {
		let handle = self.named_results.get(name).ok_or_else(|| {
			ProtocolError::new(
				error_codes::NAMED_RESULT_NOT_FOUND,
				format!("name '{}' not found", name),
			)
		})?;
		let table = self.handle.results.read().expect("results lock poisoned");
		let entry = table.get(handle).ok_or_else(|| {
			ProtocolError::new(error_codes::RESULT_HANDLE_INVALID, "handle not found")
		})?;
		Ok(entry.metadata.clone())
	}

	fn list_named(&self) -> Vec<ResultMetadata> {
		let table = self.handle.results.read().expect("results lock poisoned");
		self.named_results
			.values()
			.filter_map(|h| table.get(h).map(|e| e.metadata.clone()))
			.collect()
	}

	fn delete_named(&mut self, name: String) -> Result<(), ProtocolError> {
		let handle = self.named_results.remove(&name).ok_or_else(|| {
			ProtocolError::new(
				error_codes::NAMED_RESULT_NOT_FOUND,
				format!("name '{}' not found", name),
			)
		})?;
		self.handle.results
			.write()
			.expect("results lock poisoned")
			.remove(&handle);
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

	fn anchor_create(
		&mut self,
		master: ProcessId,
		follower: ProcessId,
		kind: AnchorKind,
	) -> Result<AnchorId, ProtocolError> {
		if !all_anchor_kinds().any(|k| k.type_name() == kind.type_name()) {
			return Err(ProtocolError::new(
				error_codes::ANCHOR_KIND_UNSUPPORTED,
				format!("anchor kind '{}' not supported by this daemon", kind.type_name()),
			));
		}
		if !self.roster.contains_key(&master) {
			return Err(ProtocolError::new(
				error_codes::PROCESS_NOT_FOUND,
				format!("master process {} not found", master),
			));
		}
		if !self.roster.contains_key(&follower) {
			return Err(ProtocolError::new(
				error_codes::PROCESS_NOT_FOUND,
				format!("follower process {} not found", follower),
			));
		}
		if self.anchor_creates_cycle(master, follower) {
			return Err(ProtocolError::new(
				error_codes::ANCHOR_CYCLE,
				"anchor would create a cycle in the dependency graph",
			));
		}
		let compat_ok = anchor_kind_compat(
			&kind,
			&self.roster[&master].info.provides,
			&self.roster[&follower].info.consumes,
		);
		if !compat_ok {
			let master_provides = self.roster[&master].info.provides.clone();
			let follower_consumes = self.roster[&follower].info.consumes.clone();
			return Err(ProtocolError::new(
				error_codes::ANCHOR_INCOMPATIBLE,
				format!(
					"anchor kind '{}' incompatible with master provides / follower consumes",
					kind.type_name(),
				),
			)
			.with_data(serde_json::json!({
				"master_provides": master_provides,
				"follower_consumes": follower_consumes,
			})));
		}

		let anchor_id = self.allocate_anchor_id();
		let anchor = Anchor {
			id: anchor_id,
			master,
			follower,
			kind: kind.clone(),
		};
		self.anchors.insert(anchor_id, anchor);

		let initial = self
			.roster
			.get(&master)
			.and_then(|c| c.info.current_interest.clone());
		if let Some(interest) = initial {
			let transformed = transform_interest(&self.handle, &interest, &kind);
			if let Some(outbound) = self.roster.get(&follower).map(|c| c.outbound.clone()) {
				for derived in transformed {
					let payload = serde_json::json!({
						"jsonrpc": "2.0",
						"method": "notification.anchor_update",
						"params": {
							"anchor_id": anchor_id,
							"interest": derived,
						},
					});
					let _ = try_send_outbound(&outbound, payload, follower);
				}
			}
		}

		Ok(anchor_id)
	}

	fn anchor_remove(&mut self, anchor_id: AnchorId) -> Result<(), ProtocolError> {
		self.anchors
			.remove(&anchor_id)
			.map(|_| ())
			.ok_or_else(|| {
				ProtocolError::new(error_codes::ANCHOR_NOT_FOUND, "anchor not found")
			})
	}

	fn anchor_list(&self, filter: Option<ProcessId>) -> Vec<Anchor> {
		match filter {
			None => self.anchors.values().cloned().collect(),
			Some(pid) => self
				.anchors
				.values()
				.filter(|a| a.master == pid || a.follower == pid)
				.cloned()
				.collect(),
		}
	}

	fn anchor_creates_cycle(&self, master: ProcessId, follower: ProcessId) -> bool {
		if master == follower {
			return true;
		}
		let mut stack = vec![follower];
		let mut visited = HashSet::new();
		while let Some(current) = stack.pop() {
			if !visited.insert(current) {
				continue;
			}
			if current == master {
				return true;
			}
			for anchor in self.anchors.values() {
				if anchor.master == current {
					stack.push(anchor.follower);
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

fn roster_filter_matches(filter: &RosterFilter, info: &ProcessInfo) -> bool {
	if !filter.kinds.is_empty() && !filter.kinds.contains(&info.kind) {
		return false;
	}
	if !filter.provides_any_of.is_empty()
		&& !filter
			.provides_any_of
			.iter()
			.any(|k| info.provides.contains(k))
	{
		return false;
	}
	if !filter.consumes_any_of.is_empty()
		&& !filter
			.consumes_any_of
			.iter()
			.any(|k| info.consumes.contains(k))
	{
		return false;
	}
	true
}

pub(crate) fn anchor_kind_compat(
	kind: &AnchorKind,
	master_provides: &[InterestKind],
	follower_consumes: &[InterestKind],
) -> bool {
	let (accepted_in, produced_out) = anchor_kind_signature(kind);
	accepted_in.iter().any(|k| master_provides.contains(k))
		&& produced_out.iter().any(|k| follower_consumes.contains(k))
}

fn anchor_kind_signature(
	kind: &AnchorKind,
) -> (&'static [InterestKind], &'static [InterestKind]) {
	use InterestKind::*;
	match kind {
		AnchorKind::SentenceMirror => (&[Position, Span, Sentence], &[Sentence]),
		AnchorKind::Alignment { .. } => (&[Sentence, Span], &[Sentence, Span]),
		AnchorKind::KwicSelection => (&[Hit], &[Sentence]),
		AnchorKind::DocPickerSelection => (&[Document], &[Document]),
		AnchorKind::NamedResultsSelection => (&[Results], &[Results]),
		AnchorKind::ConlluView => (&[Sentence], &[Sentence]),
	}
}

fn all_anchor_kinds() -> impl Iterator<Item = AnchorKind> {
	[
		AnchorKind::SentenceMirror,
		AnchorKind::Alignment { name: String::new() },
		AnchorKind::KwicSelection,
		AnchorKind::DocPickerSelection,
		AnchorKind::NamedResultsSelection,
		AnchorKind::ConlluView,
	]
	.into_iter()
}

// Drift guard for all_anchor_kinds: forces a compile error if AnchorKind grows.
#[allow(dead_code)]
fn assert_anchor_kinds_covered(kind: &AnchorKind) {
	match kind {
		AnchorKind::SentenceMirror => {}
		AnchorKind::Alignment { .. } => {}
		AnchorKind::KwicSelection => {}
		AnchorKind::DocPickerSelection => {}
		AnchorKind::NamedResultsSelection => {}
		AnchorKind::ConlluView => {}
	}
}

fn transform_interest(
	handle: &CorpusHandle,
	interest: &Interest,
	kind: &AnchorKind,
) -> Vec<Interest> {
	let (accepts, _) = anchor_kind_signature(kind);
	if !accepts.contains(&interest.kind()) {
		return Vec::new();
	}
	match kind {
		AnchorKind::SentenceMirror => match interest {
			Interest::Sentence { .. } => vec![interest.clone()],
			Interest::Position { doc, position } => {
				widen_position_to_sentence(handle, *doc, *position)
			}
			Interest::Span { doc, start, .. } => {
				widen_position_to_sentence(handle, *doc, *start)
			}
			_ => Vec::new(),
		},
		AnchorKind::Alignment { name } => {
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
		AnchorKind::KwicSelection => match interest {
			Interest::Hit { result, hit_idx } => {
				let table = handle.results.read().expect("results lock poisoned");
				let Some(entry) = table.get(result) else { return Vec::new(); };
				let Some(hit) = entry.hits.get(*hit_idx as usize) else { return Vec::new(); };
				let doc = hit.document_index;
				let Some(first) = handle.corpus.first_sentence_of_document(doc as usize) else {
					return Vec::new();
				};
				let global = hit.sentence_index as usize;
				if global < first {
					return Vec::new();
				}
				vec![Interest::Sentence { doc, sent: (global - first) as u32 }]
			}
			_ => Vec::new(),
		},
		AnchorKind::DocPickerSelection
		| AnchorKind::NamedResultsSelection
		| AnchorKind::ConlluView => Vec::new(),
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
	match tx.try_send(Outbound::Message(payload)) {
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

#[allow(dead_code)]
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
	InsertResult {
		cql: String,
		hits: Vec<Hit>,
		reply: SyncSender<ResultHandle>,
	},
	SaveResult {
		handle: ResultHandle,
		name: String,
		reply: SyncSender<Result<(), ProtocolError>>,
	},
	MaterializeResult {
		name: String,
		reply: SyncSender<Result<ResultMetadata, ProtocolError>>,
	},
	LoadNamed {
		name: String,
		reply: SyncSender<Result<ResultMetadata, ProtocolError>>,
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
	AnchorCreate {
		master: ProcessId,
		follower: ProcessId,
		kind: AnchorKind,
		reply: SyncSender<Result<AnchorId, ProtocolError>>,
	},
	AnchorRemove {
		anchor_id: AnchorId,
		reply: SyncSender<Result<(), ProtocolError>>,
	},
	AnchorList {
		process_id: Option<ProcessId>,
		reply: SyncSender<Vec<Anchor>>,
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
}

pub(crate) fn run(mut state: State, commands: Receiver<Command>) {
	while let Ok(command) = commands.recv() {
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
			Command::AnchorCreate { master, follower, kind, reply } => {
				let _ = reply.send(state.anchor_create(master, follower, kind));
			}
			Command::AnchorRemove { anchor_id, reply } => {
				let _ = reply.send(state.anchor_remove(anchor_id));
			}
			Command::AnchorList { process_id, reply } => {
				let _ = reply.send(state.anchor_list(process_id));
			}
			Command::Subscribe { process_id, topic, reply } => {
				let _ = reply.send(state.subscribe(process_id, topic));
			}
			Command::Unsubscribe { process_id, topic, reply } => {
				state.unsubscribe(process_id, topic);
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
		State::new(1, make_handle())
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
	fn register_capabilities_includes_all_anchor_kinds() {
		let mut state = make_state();
		let reply = state
			.register(make_register_params(ProcessKind::External), dummy_outbound())
			.unwrap();
		for kind in all_anchor_kinds() {
			let name = kind.type_name();
			assert!(
				reply.capabilities.anchor_kinds.iter().any(|k| k == name),
				"expected capability '{}' present",
				name,
			);
		}
	}

	#[test]
	fn unregister_drops_anchors_involving_process() {
		let mut state = make_state();
		let p1 = register_compatible(&mut state);
		let p2 = register_compatible(&mut state);
		let anchor_id = state
			.anchor_create(p1, p2, AnchorKind::SentenceMirror)
			.unwrap();
		assert!(state.anchors.contains_key(&anchor_id));

		state.unregister(p1);
		assert!(!state.anchors.contains_key(&anchor_id));
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
	fn anchor_create_unknown_master_rejected() {
		let mut state = make_state();
		let p = register(&mut state);
		let err = state
			.anchor_create(999, p, AnchorKind::SentenceMirror)
			.unwrap_err();
		assert_eq!(err.code, error_codes::PROCESS_NOT_FOUND);
	}

	#[test]
	fn anchor_create_unknown_follower_rejected() {
		let mut state = make_state();
		let p = register(&mut state);
		let err = state
			.anchor_create(p, 999, AnchorKind::SentenceMirror)
			.unwrap_err();
		assert_eq!(err.code, error_codes::PROCESS_NOT_FOUND);
	}

	#[test]
	fn anchor_create_self_loop_rejected_as_cycle() {
		let mut state = make_state();
		let p = register(&mut state);
		let err = state
			.anchor_create(p, p, AnchorKind::SentenceMirror)
			.unwrap_err();
		assert_eq!(err.code, error_codes::ANCHOR_CYCLE);
	}

	#[test]
	fn anchor_create_two_node_cycle_rejected() {
		let mut state = make_state();
		let p1 = register_compatible(&mut state);
		let p2 = register_compatible(&mut state);
		state
			.anchor_create(p1, p2, AnchorKind::SentenceMirror)
			.unwrap();
		let err = state
			.anchor_create(p2, p1, AnchorKind::SentenceMirror)
			.unwrap_err();
		assert_eq!(err.code, error_codes::ANCHOR_CYCLE);
	}

	#[test]
	fn anchor_create_three_node_cycle_rejected() {
		let mut state = make_state();
		let p1 = register_compatible(&mut state);
		let p2 = register_compatible(&mut state);
		let p3 = register_compatible(&mut state);
		state
			.anchor_create(p1, p2, AnchorKind::SentenceMirror)
			.unwrap();
		state
			.anchor_create(p2, p3, AnchorKind::SentenceMirror)
			.unwrap();
		let err = state
			.anchor_create(p3, p1, AnchorKind::SentenceMirror)
			.unwrap_err();
		assert_eq!(err.code, error_codes::ANCHOR_CYCLE);
	}

	#[test]
	fn anchor_create_incompatible_rejected() {
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
			.anchor_create(p_master, p_follower, AnchorKind::SentenceMirror)
			.unwrap_err();
		assert_eq!(err.code, error_codes::ANCHOR_INCOMPATIBLE);
		assert!(err.data.is_some());
	}

	#[test]
	fn anchor_remove_unknown_rejected() {
		let mut state = make_state();
		let err = state.anchor_remove(999).unwrap_err();
		assert_eq!(err.code, error_codes::ANCHOR_NOT_FOUND);
	}

	#[test]
	fn insert_result_returns_prefixed_handle() {
		let mut state = make_state();
		let handle = state.insert_result("foo".to_string(), vec![]);
		assert!(handle.starts_with("r-"), "handle = {}", handle);
		assert!(handle.len() > "r-".len());
	}

	#[test]
	fn insert_result_distinct_handles() {
		let mut state = make_state();
		let h1 = state.insert_result("foo".to_string(), vec![]);
		let h2 = state.insert_result("foo".to_string(), vec![]);
		assert_ne!(h1, h2);
	}

	#[test]
	fn save_result_duplicate_name_rejected() {
		let mut state = make_state();
		let h1 = state.insert_result("foo".to_string(), vec![]);
		let h2 = state.insert_result("foo".to_string(), vec![]);
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
		let h = state.insert_result("foo".to_string(), vec![]);
		state.save_result(h, "named".to_string()).unwrap();
		state.materialize_result("named".to_string()).unwrap();
		let err = state.materialize_result("named".to_string()).unwrap_err();
		assert_eq!(err.code, error_codes::RESULT_ALREADY_MATERIALIZED);
	}

	#[test]
	fn materialize_sets_materialized_at_and_preserves_created_at() {
		let mut state = make_state();
		let h = state.insert_result("foo".to_string(), vec![]);
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
		let h = state.insert_result("foo".to_string(), vec![]);
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
		let h = state.insert_result("foo".to_string(), vec![]);
		assert!(state.handle.results.read().unwrap().contains_key(&h));

		state.discard_handle(h.clone());
		assert!(!state.handle.results.read().unwrap().contains_key(&h));
	}

	#[test]
	fn discard_handle_named_is_no_op() {
		let mut state = make_state();
		let h = state.insert_result("foo".to_string(), vec![]);
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
		assert!(roster_filter_matches(&filter, &info));

		let filter = RosterFilter {
			kinds: vec![ProcessKind::Kwic],
			..Default::default()
		};
		assert!(!roster_filter_matches(&filter, &info));
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
		assert!(roster_filter_matches(&filter, &info));

		let filter = RosterFilter {
			provides_any_of: vec![InterestKind::Hit],
			..Default::default()
		};
		assert!(!roster_filter_matches(&filter, &info));
	}

	#[test]
	fn roster_filter_combines_with_and() {
		let info = make_info(ProcessKind::Reader, vec![InterestKind::Sentence]);
		let filter = RosterFilter {
			kinds: vec![ProcessKind::Reader],
			provides_any_of: vec![InterestKind::Sentence],
			..Default::default()
		};
		assert!(roster_filter_matches(&filter, &info));

		let filter = RosterFilter {
			kinds: vec![ProcessKind::Reader],
			provides_any_of: vec![InterestKind::Hit],
			..Default::default()
		};
		assert!(!roster_filter_matches(&filter, &info));
	}

	#[test]
	fn anchor_kind_compat_sentence_mirror_accepts_position_to_sentence() {
		assert!(anchor_kind_compat(
			&AnchorKind::SentenceMirror,
			&[InterestKind::Position],
			&[InterestKind::Sentence],
		));
	}

	#[test]
	fn anchor_kind_compat_sentence_mirror_rejects_hit_provider() {
		assert!(!anchor_kind_compat(
			&AnchorKind::SentenceMirror,
			&[InterestKind::Hit],
			&[InterestKind::Sentence],
		));
	}

	#[test]
	fn anchor_kind_compat_alignment_accepts_span_to_span() {
		assert!(anchor_kind_compat(
			&AnchorKind::Alignment { name: "labse".to_string() },
			&[InterestKind::Span],
			&[InterestKind::Span],
		));
	}

	#[test]
	fn anchor_kind_compat_kwic_requires_hit_provider() {
		assert!(anchor_kind_compat(
			&AnchorKind::KwicSelection,
			&[InterestKind::Hit],
			&[InterestKind::Sentence],
		));
		assert!(!anchor_kind_compat(
			&AnchorKind::KwicSelection,
			&[InterestKind::Sentence],
			&[InterestKind::Sentence],
		));
	}

	#[test]
	fn anchor_kind_compat_rejects_empty_master_provides() {
		assert!(!anchor_kind_compat(
			&AnchorKind::SentenceMirror,
			&[],
			&[InterestKind::Sentence],
		));
	}

	#[test]
	fn anchor_kind_compat_rejects_empty_follower_consumes() {
		assert!(!anchor_kind_compat(
			&AnchorKind::SentenceMirror,
			&[InterestKind::Position],
			&[],
		));
	}

	#[test]
	fn anchor_kind_compat_tbd_kinds_self_to_self() {
		assert!(anchor_kind_compat(
			&AnchorKind::DocPickerSelection,
			&[InterestKind::Document],
			&[InterestKind::Document],
		));
		assert!(anchor_kind_compat(
			&AnchorKind::NamedResultsSelection,
			&[InterestKind::Results],
			&[InterestKind::Results],
		));
		assert!(anchor_kind_compat(
			&AnchorKind::ConlluView,
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
		let out = transform_interest(&handle, &interest, &AnchorKind::SentenceMirror);
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
		let out = transform_interest(&handle, &interest, &AnchorKind::SentenceMirror);
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
		let out = transform_interest(&handle, &interest, &AnchorKind::SentenceMirror);
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
		let out = transform_interest(&handle, &interest, &AnchorKind::SentenceMirror);
		assert!(out.is_empty());
	}

	#[test]
	fn transform_sentence_mirror_defensively_rejects_hit_input() {
		let handle = make_handle();
		let interest = Interest::Hit { result: "r-x".to_string(), hit_idx: 0 };
		let out = transform_interest(&handle, &interest, &AnchorKind::SentenceMirror);
		assert!(out.is_empty());
	}

	#[test]
	fn transform_alignment_projects_sentence_to_target_doc() {
		let handle = make_handle();
		let source_doc = find_doc(&handle.corpus, "la_maison");
		let target_doc = find_doc(&handle.corpus, "the_house");
		let interest = Interest::Sentence { doc: source_doc, sent: 0 };
		let kind = AnchorKind::Alignment { name: "sentence".to_string() };
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
		let kind = AnchorKind::Alignment { name: "sentence".to_string() };
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
		let kind = AnchorKind::Alignment { name: "totally-not-an-alignment".to_string() };
		let out = transform_interest(&handle, &interest, &kind);
		assert!(out.is_empty());
	}

	#[test]
	fn transform_alignment_defensively_rejects_position_input() {
		let handle = make_handle();
		let doc = find_doc(&handle.corpus, "la_maison");
		let interest = Interest::Position { doc, position: 0 };
		let kind = AnchorKind::Alignment { name: "sentence".to_string() };
		let out = transform_interest(&handle, &interest, &kind);
		assert!(out.is_empty());
	}

	#[test]
	fn transform_kwic_extracts_containing_sentence_from_hit() {
		let mut state = make_state();
		let doc = find_doc(&state.handle.corpus, "la_maison");
		let first_global =
			state.handle.corpus.first_sentence_of_document(doc as usize).expect("first sent");
		let executor_hit = Hit {
			span: Span { start: 0, end: 1 },
			document_index: doc,
			sentence_index: first_global as u32,
			captures: vec![],
		};
		let result_handle = state.insert_result("test".to_string(), vec![executor_hit]);
		let interest = Interest::Hit { result: result_handle, hit_idx: 0 };
		let out = transform_interest(&state.handle, &interest, &AnchorKind::KwicSelection);
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
	fn transform_kwic_unknown_result_returns_empty() {
		let handle = make_handle();
		let interest = Interest::Hit { result: "r-bogus".to_string(), hit_idx: 0 };
		let out = transform_interest(&handle, &interest, &AnchorKind::KwicSelection);
		assert!(out.is_empty());
	}

	#[test]
	fn transform_kwic_hit_idx_out_of_range_returns_empty() {
		let mut state = make_state();
		let result_handle = state.insert_result("test".to_string(), vec![]);
		let interest = Interest::Hit { result: result_handle, hit_idx: 99 };
		let out = transform_interest(&state.handle, &interest, &AnchorKind::KwicSelection);
		assert!(out.is_empty());
	}

	#[test]
	fn transform_inert_kinds_return_empty() {
		let handle = make_handle();
		let doc = find_doc(&handle.corpus, "la_maison");
		assert!(transform_interest(
			&handle,
			&Interest::Document { doc },
			&AnchorKind::DocPickerSelection,
		)
		.is_empty());
		assert!(transform_interest(
			&handle,
			&Interest::Results { handle: "r-x".to_string() },
			&AnchorKind::NamedResultsSelection,
		)
		.is_empty());
		assert!(transform_interest(
			&handle,
			&Interest::Sentence { doc, sent: 0 },
			&AnchorKind::ConlluView,
		)
		.is_empty());
	}
}
