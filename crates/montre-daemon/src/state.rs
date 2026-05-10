use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{Arc, RwLock};

use montre_index::Corpus;
use montre_query::executor::Hit;
use uuid::Uuid;

use crate::protocol::error_codes;
use crate::protocol::{
	Anchor, AnchorId, AnchorKind, Capabilities, Interest, ProcessId, ProcessInfo, ProtocolError,
	RegisterParams, RegisterReply, ResultForm, ResultHandle, ResultMetadata, RosterFilter, Topic,
	PROTOCOL_VERSION,
};

const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

const SUPPORTED_ANCHOR_KINDS: &[&str] = &[
	"SentenceMirror",
	"Alignment",
	"KwicSelection",
	"DocPickerSelection",
	"NamedResultsSelection",
	"ConlluView",
];

pub(crate) type ResultsTable = HashMap<ResultHandle, Arc<ResultEntry>>;

pub(crate) struct ResultEntry {
	pub cql: String,
	pub hits: Vec<Hit>,
	pub metadata: ResultMetadata,
}

pub(crate) enum Outbound {
	Notification(serde_json::Value),
}

pub(crate) struct Connection {
	info: ProcessInfo,
	outbound: SyncSender<Outbound>,
}

pub(crate) struct State {
	corpus: Arc<Corpus>,
	corpus_id: String,
	daemon_epoch: u64,
	results: Arc<RwLock<ResultsTable>>,
	roster: HashMap<ProcessId, Connection>,
	anchors: HashMap<AnchorId, Anchor>,
	subscriptions: HashMap<Topic, HashSet<ProcessId>>,
	named_results: HashMap<String, ResultHandle>,
	next_process_id: ProcessId,
	next_anchor_id: AnchorId,
}

impl State {
	pub(crate) fn new(corpus: Arc<Corpus>, daemon_epoch: u64) -> Self {
		let corpus_id = corpus.name().to_string();
		Self {
			corpus,
			corpus_id,
			daemon_epoch,
			results: Arc::new(RwLock::new(HashMap::new())),
			roster: HashMap::new(),
			anchors: HashMap::new(),
			subscriptions: HashMap::new(),
			named_results: HashMap::new(),
			next_process_id: 1,
			next_anchor_id: 1,
		}
	}

	pub(crate) fn corpus(&self) -> Arc<Corpus> {
		Arc::clone(&self.corpus)
	}

	pub(crate) fn results(&self) -> Arc<RwLock<ResultsTable>> {
		Arc::clone(&self.results)
	}

	pub(crate) fn corpus_id(&self) -> &str {
		&self.corpus_id
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
				anchor_kinds: SUPPORTED_ANCHOR_KINDS.iter().map(|s| s.to_string()).collect(),
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
			let Some(transformed) = transform_interest(&interest, &kind) else {
				continue;
			};
			let Some(outbound) = self.roster.get(&follower).map(|c| c.outbound.clone()) else {
				continue;
			};
			let payload = serde_json::json!({
				"jsonrpc": "2.0",
				"method": "notification.anchor_update",
				"params": {
					"anchor_id": anchor_id,
					"interest": transformed,
				},
			});
			let _ = try_send_outbound(&outbound, payload, follower);
		}
	}

	fn insert_result(&mut self, cql: String, hits: Vec<Hit>) -> ResultHandle {
		let handle = generate_handle();
		let metadata = ResultMetadata {
			handle: handle.clone(),
			query: cql.clone(),
			created_at: now_rfc3339(),
			hit_count: hits.len() as u64,
			corpus_id: self.corpus_id.clone(),
			name: None,
			form: ResultForm::Session,
		};
		let entry = Arc::new(ResultEntry {
			cql,
			hits,
			metadata,
		});
		self.results
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
			let table = self.results.read().expect("results lock poisoned");
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
		self.results
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
			let table = self.results.read().expect("results lock poisoned");
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
			next.metadata.created_at = now_rfc3339();
			Arc::new(next)
		};

		let metadata = promoted.metadata.clone();
		self.results
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
		let table = self.results.read().expect("results lock poisoned");
		let entry = table.get(handle).ok_or_else(|| {
			ProtocolError::new(error_codes::RESULT_HANDLE_INVALID, "handle not found")
		})?;
		Ok(entry.metadata.clone())
	}

	fn list_named(&self) -> Vec<ResultMetadata> {
		let table = self.results.read().expect("results lock poisoned");
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
		self.results
			.write()
			.expect("results lock poisoned")
			.remove(&handle);
		self.notify_named_results(NamedResultsEvent::Deleted, name, None);
		Ok(())
	}

	fn discard_handle(&mut self, handle: ResultHandle) {
		let mut table = self.results.write().expect("results lock poisoned");
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
		if !SUPPORTED_ANCHOR_KINDS.contains(&kind.type_name()) {
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
			if let Some(transformed) = transform_interest(&interest, &kind) {
				if let Some(outbound) =
					self.roster.get(&follower).map(|c| c.outbound.clone())
				{
					let payload = serde_json::json!({
						"jsonrpc": "2.0",
						"method": "notification.anchor_update",
						"params": {
							"anchor_id": anchor_id,
							"interest": transformed,
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

fn transform_interest(_interest: &Interest, _kind: &AnchorKind) -> Option<Interest> {
	None
}

fn generate_handle() -> ResultHandle {
	format!("r-{}", Uuid::new_v4())
}

fn now_rfc3339() -> String {
	String::new()
}

fn try_send_outbound(
	tx: &SyncSender<Outbound>,
	payload: serde_json::Value,
	process_id: ProcessId,
) -> Result<(), ()> {
	match tx.try_send(Outbound::Notification(payload)) {
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
