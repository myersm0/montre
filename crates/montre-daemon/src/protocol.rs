use std::collections::HashMap;

use serde::{Deserialize, Serialize};

pub use montre_core::Span;

pub type ProcessId = u32;
pub type AnchorId = u32;
pub type ResultHandle = String;

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessKind {
	Reader,
	Kwic,
	Conllu,
	Docs,
	Vocab,
	Results,
	External,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum InterestKind {
	Position,
	Span,
	Sentence,
	Hit,
	Results,
	Document,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Interest {
	Position { doc: u32, position: u64 },
	Span { doc: u32, start: u64, end: u64 },
	Sentence { doc: u32, sent: u32 },
	Hit { result: ResultHandle, hit_idx: u64 },
	Results { handle: ResultHandle },
	Document { doc: u32 },
}

impl Interest {
	pub fn kind(&self) -> InterestKind {
		match self {
			Interest::Position { .. } => InterestKind::Position,
			Interest::Span { .. } => InterestKind::Span,
			Interest::Sentence { .. } => InterestKind::Sentence,
			Interest::Hit { .. } => InterestKind::Hit,
			Interest::Results { .. } => InterestKind::Results,
			Interest::Document { .. } => InterestKind::Document,
		}
	}
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessInfo {
	pub id: ProcessId,
	pub kind: ProcessKind,
	pub label: Option<String>,
	pub provides: Vec<InterestKind>,
	pub consumes: Vec<InterestKind>,
	pub current_interest: Option<Interest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AnchorKind {
	SentenceMirror,
	Alignment { name: String },
	KwicSelection,
	DocPickerSelection,
	NamedResultsSelection,
	ConlluView,
}

impl AnchorKind {
	pub fn type_name(&self) -> &'static str {
		match self {
			AnchorKind::SentenceMirror => "SentenceMirror",
			AnchorKind::Alignment { .. } => "Alignment",
			AnchorKind::KwicSelection => "KwicSelection",
			AnchorKind::DocPickerSelection => "DocPickerSelection",
			AnchorKind::NamedResultsSelection => "NamedResultsSelection",
			AnchorKind::ConlluView => "ConlluView",
		}
	}
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Anchor {
	pub id: AnchorId,
	pub master: ProcessId,
	pub follower: ProcessId,
	pub kind: AnchorKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hit {
	pub span: Span,
	pub document_index: u32,
	pub sentence_index: u32,
	pub captures: Option<HashMap<String, Span>>,
}

impl From<montre_query::executor::Hit> for Hit {
	fn from(hit: montre_query::executor::Hit) -> Self {
		let captures = if hit.captures.is_empty() {
			None
		} else {
			Some(hit.captures.into_iter().collect())
		};
		Self {
			span: hit.span,
			document_index: hit.document_index,
			sentence_index: hit.sentence_index,
			captures,
		}
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultForm {
	Session,
	QueryBacked,
	Materialized,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResultMetadata {
	pub handle: ResultHandle,
	pub query: String,
	pub created_at: String,
	pub materialized_at: Option<String>,
	pub hit_count: u64,
	pub corpus_id: String,
	pub name: Option<String>,
	pub form: ResultForm,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterParams {
	pub protocol_version: u32,
	pub kind: ProcessKind,
	#[serde(default)]
	pub label: Option<String>,
	#[serde(default)]
	pub provides: Vec<InterestKind>,
	#[serde(default)]
	pub consumes: Vec<InterestKind>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterReply {
	pub process_id: ProcessId,
	pub server_version: String,
	pub protocol_version: u32,
	pub daemon_epoch: u64,
	pub capabilities: Capabilities,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capabilities {
	pub observations: bool,
	pub workspaces: bool,
	pub anchor_kinds: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OkReply {
	pub ok: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionUpdateLabelParams {
	#[serde(default)]
	pub label: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionRosterParams {
	#[serde(default)]
	pub filter: Option<RosterFilter>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRosterReply {
	pub processes: Vec<ProcessInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpusInfo {
	pub name: String,
	pub canonical_path: String,
	pub stable_key: String,
	pub components: Vec<String>,
	pub layers: Vec<String>,
	pub alignments: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CorpusDocumentsParams {
	#[serde(default)]
	pub component: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentEntry {
	pub index: u32,
	pub name: String,
	pub component: String,
	pub sentence_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpusDocumentsReply {
	pub documents: Vec<DocumentEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpusLayerInfoParams {
	pub layer: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayerKind {
	String,
	Int,
	#[serde(other)]
	Unknown,
}

impl From<montre_index::LayerKind> for LayerKind {
	fn from(kind: montre_index::LayerKind) -> Self {
		match kind {
			montre_index::LayerKind::String => Self::String,
			montre_index::LayerKind::Int => Self::Int,
			_ => {
				tracing::error!(
					?kind,
					"unknown montre_index::LayerKind variant; mapping to wire Unknown",
				);
				debug_assert!(
					false,
					"unknown montre_index::LayerKind variant: {:?}",
					kind,
				);
				Self::Unknown
			}
		}
	}
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayerInfo {
	pub name: String,
	pub kind: LayerKind,
	pub value_count: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AnnotationValue {
	Int(i64),
	String(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextSurfaceParams {
	pub start: u64,
	pub end: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextSurfaceReply {
	pub surface: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextSentenceParams {
	pub doc: u32,
	pub sent: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextSentenceReply {
	pub span: Span,
	pub surface: String,
	pub sentence_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextSentencesParams {
	pub doc: u32,
	pub sent_start: u32,
	pub sent_end: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SentenceEntry {
	pub sent: u32,
	pub span: Span,
	pub surface: String,
	pub sentence_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextSentencesReply {
	pub sentences: Vec<SentenceEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextDocumentParams {
	pub doc: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextDocumentReply {
	pub index: u32,
	pub name: String,
	pub component: String,
	pub span: Span,
	pub sentence_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextAnnotationsParams {
	pub positions: Vec<u64>,
	pub layers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnotationEntry {
	pub position: u64,
	pub layer: String,
	pub value: AnnotationValue,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextAnnotationsReply {
	pub values: Vec<AnnotationEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextAnnotationsRangeParams {
	pub start: u64,
	pub end: u64,
	#[serde(default)]
	pub layers: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnotationRow {
	pub position: u64,
	pub values: HashMap<String, AnnotationValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextAnnotationsRangeReply {
	pub rows: Vec<AnnotationRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlignmentInfo {
	pub name: String,
	pub source_component: String,
	pub target_component: String,
	pub source_layer: String,
	pub target_layer: String,
	pub edge_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlignmentListReply {
	pub alignments: Vec<AlignmentInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlignmentSource {
	pub doc: u32,
	pub start: u64,
	pub end: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlignmentProjectParams {
	pub source: AlignmentSource,
	pub alignment_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlignmentTarget {
	pub doc: u32,
	pub start: u64,
	pub end: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlignmentProjectReply {
	pub targets: Vec<AlignmentTarget>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryExecuteParams {
	pub cql: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryExecuteReply {
	pub handle: ResultHandle,
	pub hit_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryExecuteCountReply {
	pub count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryHitsParams {
	pub handle: ResultHandle,
	pub offset: u64,
	pub limit: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryHitsReply {
	pub hits: Vec<Hit>,
	pub offset: u64,
	pub limit: u64,
	pub total_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryMetadataParams {
	pub handle: ResultHandle,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuerySaveParams {
	pub handle: ResultHandle,
	pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuerySaveReply {
	pub ok: bool,
	pub form: ResultForm,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryMaterializeParams {
	pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryMaterializeReply {
	pub ok: bool,
	pub hit_count: u64,
	pub materialized_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryLoadParams {
	pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryLoadReply {
	pub handle: ResultHandle,
	pub hit_count: u64,
	pub form: ResultForm,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NamedResultEntry {
	pub name: String,
	pub hit_count: u64,
	pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryListNamedReply {
	pub names: Vec<NamedResultEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryDeleteNamedParams {
	pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryDiscardParams {
	pub handle: ResultHandle,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnchorCreateParams {
	pub master_id: ProcessId,
	pub follower_id: ProcessId,
	pub kind: AnchorKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnchorCreateReply {
	pub anchor_id: AnchorId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnchorRemoveParams {
	pub anchor_id: AnchorId,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AnchorListParams {
	#[serde(default)]
	pub process_id: Option<ProcessId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnchorListReply {
	pub anchors: Vec<Anchor>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscriptionParams {
	pub topic: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Topic {
	RosterChanged,
	NamedResultsChanged,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RosterFilter {
	#[serde(default)]
	pub provides_any_of: Vec<InterestKind>,
	#[serde(default)]
	pub consumes_any_of: Vec<InterestKind>,
	#[serde(default)]
	pub kinds: Vec<ProcessKind>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProtocolError {
	pub code: i32,
	pub message: String,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub data: Option<serde_json::Value>,
}

impl ProtocolError {
	pub fn new(code: i32, message: impl Into<String>) -> Self {
		Self {
			code,
			message: message.into(),
			data: None,
		}
	}

	pub fn with_data(mut self, data: serde_json::Value) -> Self {
		self.data = Some(data);
		self
	}
}

pub mod error_codes {
	pub const PROTOCOL_VERSION_MISMATCH: i32 = 1000;
	pub const CORPUS_LOAD_FAILURE: i32 = 1001;
	pub const NOT_REGISTERED: i32 = 1002;
	pub const CQL_PARSE_ERROR: i32 = 1100;
	pub const PLAN_ERROR: i32 = 1101;
	pub const EXECUTION_ERROR: i32 = 1102;
	pub const RESULT_HANDLE_INVALID: i32 = 1200;
	pub const NAMED_RESULT_ALREADY_EXISTS: i32 = 1201;
	pub const NAMED_RESULT_NOT_FOUND: i32 = 1202;
	pub const PAGE_LIMIT_EXCEEDED: i32 = 1203;
	pub const STORED_QUERY_INVALID: i32 = 1204;
	pub const RESULT_ALREADY_MATERIALIZED: i32 = 1205;
	pub const ALIGNMENT_NOT_FOUND: i32 = 1300;
	pub const SPAN_OUTSIDE_ALIGNMENT: i32 = 1301;
	pub const ANCHOR_INCOMPATIBLE: i32 = 1400;
	pub const ANCHOR_NOT_FOUND: i32 = 1401;
	pub const ANCHOR_CYCLE: i32 = 1402;
	pub const ANCHOR_KIND_UNSUPPORTED: i32 = 1403;
	pub const PROCESS_NOT_FOUND: i32 = 1500;
	pub const UNKNOWN_TOPIC: i32 = 1600;
}
