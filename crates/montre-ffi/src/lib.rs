use std::cell::RefCell;
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::ptr;

use montre_core::Value;
use montre_index::{ComponentMeta, Corpus, ForwardIndex, SpanIndex};
use montre_query::executor::{self, Hit};

// ---------------------------------------------------------------------------
// Error handling (thread-local last error)
// ---------------------------------------------------------------------------

thread_local! {
	static LAST_ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
}

fn set_error(msg: String) {
	let c = CString::new(msg).unwrap_or_else(|_| CString::new("(error contained null byte)").unwrap());
	LAST_ERROR.with(|e| *e.borrow_mut() = Some(c));
}

fn clear_error() {
	LAST_ERROR.with(|e| *e.borrow_mut() = None);
}

#[no_mangle]
pub extern "C" fn montre_last_error() -> *const c_char {
	LAST_ERROR.with(|e| {
		match *e.borrow() {
			Some(ref msg) => msg.as_ptr(),
			None => ptr::null(),
		}
	})
}

// ---------------------------------------------------------------------------
// String handling
// ---------------------------------------------------------------------------

fn to_cstring(s: &str) -> *mut c_char {
	match CString::new(s) {
		Ok(c) => c.into_raw(),
		Err(_) => {
			let cleaned = s.replace('\0', "");
			CString::new(cleaned).unwrap().into_raw()
		}
	}
}

unsafe fn borrow_cstr<'a>(p: *const c_char) -> Option<&'a str> {
	if p.is_null() {
		return None;
	}
	CStr::from_ptr(p).to_str().ok()
}

#[no_mangle]
pub unsafe extern "C" fn montre_string_free(s: *mut c_char) {
	if !s.is_null() {
		drop(CString::from_raw(s));
	}
}

// ---------------------------------------------------------------------------
// Corpus
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn montre_corpus_open(path: *const c_char) -> *mut Corpus {
	clear_error();
	let Some(path_str) = borrow_cstr(path) else {
		set_error("null path".into());
		return ptr::null_mut();
	};

	match montre_index::open(path_str) {
		Ok(corpus) => Box::into_raw(Box::new(corpus)),
		Err(e) => {
			set_error(e.to_string());
			ptr::null_mut()
		}
	}
}

#[no_mangle]
pub unsafe extern "C" fn montre_corpus_close(corpus: *mut Corpus) {
	if !corpus.is_null() {
		drop(Box::from_raw(corpus));
	}
}

#[no_mangle]
pub unsafe extern "C" fn montre_corpus_token_count(corpus: *const Corpus) -> u64 {
	if corpus.is_null() {
		return 0;
	}
	let c = &*corpus;
	c.token_count()
}

#[no_mangle]
pub unsafe extern "C" fn montre_corpus_layer_count(corpus: *const Corpus) -> u32 {
	if corpus.is_null() {
		return 0;
	}
	let c = &*corpus;
	c.layers().len() as u32
}

#[no_mangle]
pub unsafe extern "C" fn montre_corpus_layer_name(
	corpus: *const Corpus,
	index: u32,
) -> *mut c_char {
	if corpus.is_null() {
		return ptr::null_mut();
	}
	let c = &*corpus;
	match c.layers().get(index as usize) {
		Some(name) => to_cstring(name),
		None => ptr::null_mut(),
	}
}

#[no_mangle]
pub unsafe extern "C" fn montre_corpus_document_count(corpus: *const Corpus) -> u32 {
	if corpus.is_null() {
		return 0;
	}
	let c = &*corpus;
	c.document_names().len() as u32
}

#[no_mangle]
pub unsafe extern "C" fn montre_corpus_document_name(
	corpus: *const Corpus,
	index: u32,
) -> *mut c_char {
	if corpus.is_null() {
		return ptr::null_mut();
	}
	let c = &*corpus;
	match c.document_names().get(index as usize) {
		Some(name) => to_cstring(name),
		None => ptr::null_mut(),
	}
}

#[no_mangle]
pub unsafe extern "C" fn montre_corpus_component_count(corpus: *const Corpus) -> u32 {
	if corpus.is_null() {
		return 0;
	}
	let c = &*corpus;
	c.components().len() as u32
}

#[no_mangle]
pub unsafe extern "C" fn montre_corpus_component_name(
	corpus: *const Corpus,
	index: u32,
) -> *mut c_char {
	if corpus.is_null() {
		return ptr::null_mut();
	}
	let c = &*corpus;
	match c.components().get(index as usize) {
		Some(comp) => to_cstring(&comp.name),
		None => ptr::null_mut(),
	}
}

#[no_mangle]
pub unsafe extern "C" fn montre_corpus_component_language(
	corpus: *const Corpus,
	index: u32,
) -> *mut c_char {
	if corpus.is_null() {
		return ptr::null_mut();
	}
	let c = &*corpus;
	match c.components().get(index as usize) {
		Some(comp) => to_cstring(&comp.language),
		None => ptr::null_mut(),
	}
}

// ---------------------------------------------------------------------------
// Token access
// ---------------------------------------------------------------------------

fn value_to_string(v: &Value) -> String {
	match v {
		Value::Str(s) => s.to_string(),
		Value::Int(n) => n.to_string(),
	}
}

#[no_mangle]
pub unsafe extern "C" fn montre_corpus_token_annotation(
	corpus: *const Corpus,
	position: u64,
	layer: *const c_char,
) -> *mut c_char {
	if corpus.is_null() {
		return ptr::null_mut();
	}
	let c = &*corpus;
	let Some(layer_str) = borrow_cstr(layer) else {
		return ptr::null_mut();
	};
	match c.forward.get(position, layer_str) {
		Some(val) => to_cstring(&value_to_string(val)),
		None => ptr::null_mut(),
	}
}

#[no_mangle]
pub unsafe extern "C" fn montre_corpus_span_text(
	corpus: *const Corpus,
	start: u64,
	end: u64,
	layer: *const c_char,
) -> *mut c_char {
	if corpus.is_null() {
		return ptr::null_mut();
	}
	let c = &*corpus;
	let Some(layer_str) = borrow_cstr(layer) else {
		return ptr::null_mut();
	};
	let words: Vec<String> = (start..end)
		.filter_map(|p| c.forward.get(p, layer_str).map(value_to_string))
		.collect();
	to_cstring(&words.join(" "))
}

/// Bulk-extract matched text for every hit in a HitList.
/// Returns an array of `len` C strings (caller gets ownership).
/// Free with `montre_string_array_free(array, len)`.
#[no_mangle]
pub unsafe extern "C" fn montre_hitlist_texts(
	hits: *const HitList,
	corpus: *const Corpus,
	layer: *const c_char,
	out_len: *mut u64,
) -> *mut *mut c_char {
	if hits.is_null() || corpus.is_null() || out_len.is_null() {
		if !out_len.is_null() {
			*out_len = 0;
		}
		return ptr::null_mut();
	}
	let hitlist = &*hits;
	let c = &*corpus;
	let Some(layer_str) = borrow_cstr(layer) else {
		*out_len = 0;
		return ptr::null_mut();
	};

	let count = hitlist.hits.len();
	let layout = std::alloc::Layout::array::<*mut c_char>(count).unwrap();
	let array = std::alloc::alloc(layout) as *mut *mut c_char;
	if array.is_null() {
		*out_len = 0;
		return ptr::null_mut();
	}

	for (i, hit) in hitlist.hits.iter().enumerate() {
		let words: Vec<String> = (hit.span.start..hit.span.end)
			.filter_map(|p| c.forward.get(p, layer_str).map(value_to_string))
			.collect();
		*array.add(i) = to_cstring(&words.join(" "));
	}

	*out_len = count as u64;
	array
}

#[no_mangle]
pub unsafe extern "C" fn montre_string_array_free(array: *mut *mut c_char, len: u64) {
	if array.is_null() {
		return;
	}
	for i in 0..len as usize {
		let s = *array.add(i);
		if !s.is_null() {
			drop(CString::from_raw(s));
		}
	}
	let layout = std::alloc::Layout::array::<*mut c_char>(len as usize).unwrap();
	std::alloc::dealloc(array as *mut u8, layout);
}

// ---------------------------------------------------------------------------
// Query
// ---------------------------------------------------------------------------

pub struct HitList {
	hits: Vec<Hit>,
}

#[no_mangle]
pub unsafe extern "C" fn montre_query(
	corpus: *const Corpus,
	cql: *const c_char,
) -> *mut HitList {
	clear_error();
	if corpus.is_null() {
		set_error("null corpus".into());
		return ptr::null_mut();
	}
	let Some(cql_str) = borrow_cstr(cql) else {
		set_error("null query string".into());
		return ptr::null_mut();
	};

	let parsed = match montre_query::parse(cql_str) {
		Ok(q) => q,
		Err(e) => {
			set_error(e.to_string());
			return ptr::null_mut();
		}
	};

	let plan = match montre_query::planner::plan(&parsed) {
		Ok(p) => p,
		Err(e) => {
			set_error(e.to_string());
			return ptr::null_mut();
		}
	};

	let results = match executor::execute(&plan, &*corpus) {
		Ok(r) => r,
		Err(e) => {
			set_error(e.to_string());
			return ptr::null_mut();
		}
	};

	Box::into_raw(Box::new(HitList {
		hits: results.into_hits(),
	}))
}

#[no_mangle]
pub unsafe extern "C" fn montre_query_count(
	corpus: *const Corpus,
	cql: *const c_char,
) -> i64 {
	clear_error();
	if corpus.is_null() {
		set_error("null corpus".into());
		return -1;
	}
	let Some(cql_str) = borrow_cstr(cql) else {
		set_error("null query string".into());
		return -1;
	};

	let parsed = match montre_query::parse(cql_str) {
		Ok(q) => q,
		Err(e) => {
			set_error(e.to_string());
			return -1;
		}
	};

	let plan = match montre_query::planner::plan(&parsed) {
		Ok(p) => p,
		Err(e) => {
			set_error(e.to_string());
			return -1;
		}
	};

	let c = &*corpus;
	match executor::execute(&plan, c) {
		Ok(r) => r.len() as i64,
		Err(e) => {
			set_error(e.to_string());
			-1
		}
	}
}

#[no_mangle]
pub unsafe extern "C" fn montre_hitlist_free(hits: *mut HitList) {
	if !hits.is_null() {
		drop(Box::from_raw(hits));
	}
}

#[no_mangle]
pub unsafe extern "C" fn montre_hitlist_len(hits: *const HitList) -> u64 {
	if hits.is_null() {
		return 0;
	}
	let hitlist = &*hits;
	hitlist.hits.len() as u64
}

#[no_mangle]
pub unsafe extern "C" fn montre_hit_start(hits: *const HitList, index: u64) -> u64 {
	if hits.is_null() {
		return 0;
	}
	let hitlist = &*hits;
	match hitlist.hits.get(index as usize) {
		Some(hit) => hit.span.start,
		None => 0,
	}
}

#[no_mangle]
pub unsafe extern "C" fn montre_hit_end(hits: *const HitList, index: u64) -> u64 {
	if hits.is_null() {
		return 0;
	}
	let hitlist = &*hits;
	match hitlist.hits.get(index as usize) {
		Some(hit) => hit.span.end,
		None => 0,
	}
}

#[no_mangle]
pub unsafe extern "C" fn montre_hit_document_index(hits: *const HitList, index: u64) -> u32 {
	if hits.is_null() {
		return 0;
	}
	let hitlist = &*hits;
	match hitlist.hits.get(index as usize) {
		Some(hit) => hit.document_index,
		None => 0,
	}
}

#[no_mangle]
pub unsafe extern "C" fn montre_hit_sentence_index(hits: *const HitList, index: u64) -> u32 {
	if hits.is_null() {
		return 0;
	}
	let hitlist = &*hits;
	match hitlist.hits.get(index as usize) {
		Some(hit) => hit.sentence_index,
		None => 0,
	}
}

#[no_mangle]
pub unsafe extern "C" fn montre_hitlist_populate_context(
	hits: *mut HitList,
	corpus: *const Corpus,
) {
	if hits.is_null() || corpus.is_null() {
		return;
	}
	let hit_list = &mut *hits;
	let corpus = &*corpus;

	let doc_spans = corpus.spans.spans("document");
	let sent_spans = corpus.spans.spans("sentence");

	for hit in &mut hit_list.hits {
		if let Some(spans) = doc_spans {
			hit.document_index = binary_search_span_index(spans, hit.span.start);
		}
		if let Some(spans) = sent_spans {
			hit.sentence_index = binary_search_span_index(spans, hit.span.start);
		}
	}
}

fn binary_search_span_index(spans: &[montre_core::Span], position: u64) -> u32 {
	let mut lo = 0usize;
	let mut hi = spans.len();

	while lo < hi {
		let mid = lo + (hi - lo) / 2;
		let span = &spans[mid];
		if position < span.start {
			hi = mid;
		} else if position >= span.end {
			lo = mid + 1;
		} else {
			return mid as u32;
		}
	}

	0
}

// ---------------------------------------------------------------------------
// Alignments
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn montre_corpus_alignment_count(corpus: *const Corpus) -> u32 {
	if corpus.is_null() {
		return 0;
	}
	let c = &*corpus;
	c.meta.alignments.len() as u32
}

#[no_mangle]
pub unsafe extern "C" fn montre_corpus_alignment_name(
	corpus: *const Corpus,
	index: u32,
) -> *mut c_char {
	if corpus.is_null() {
		return ptr::null_mut();
	}
	let c = &*corpus;
	match c.meta.alignments.get(index as usize) {
		Some(a) => to_cstring(&a.name),
		None => ptr::null_mut(),
	}
}

#[no_mangle]
pub unsafe extern "C" fn montre_corpus_alignment_source(
	corpus: *const Corpus,
	index: u32,
) -> *mut c_char {
	if corpus.is_null() {
		return ptr::null_mut();
	}
	let c = &*corpus;
	match c.meta.alignments.get(index as usize) {
		Some(a) => to_cstring(&a.source_component),
		None => ptr::null_mut(),
	}
}

#[no_mangle]
pub unsafe extern "C" fn montre_corpus_alignment_target(
	corpus: *const Corpus,
	index: u32,
) -> *mut c_char {
	if corpus.is_null() {
		return ptr::null_mut();
	}
	let c = &*corpus;
	match c.meta.alignments.get(index as usize) {
		Some(a) => to_cstring(&a.target_component),
		None => ptr::null_mut(),
	}
}

#[no_mangle]
pub unsafe extern "C" fn montre_corpus_alignment_edge_count(
	corpus: *const Corpus,
	index: u32,
) -> u64 {
	if corpus.is_null() {
		return 0;
	}
	let c = &*corpus;
	match c.meta.alignments.get(index as usize) {
		Some(a) => a.edge_count as u64,
		None => 0,
	}
}

/// Project a HitList through a named alignment, returning a new HitList
/// of target-side sentence spans.
#[no_mangle]
pub unsafe extern "C" fn montre_project(
	corpus: *const Corpus,
	source_hits: *const HitList,
	alignment_name: *const c_char,
) -> *mut HitList {
	clear_error();
	if corpus.is_null() || source_hits.is_null() {
		set_error("null corpus or hits".into());
		return ptr::null_mut();
	}
	let Some(align_str) = borrow_cstr(alignment_name) else {
		set_error("null alignment name".into());
		return ptr::null_mut();
	};

	let c = &*corpus;
	let source = &*source_hits;

	let Some(edges) = c.alignment_edges(align_str) else {
		set_error(format!("alignment not found: {}", align_str));
		return ptr::null_mut();
	};

	let Some(align_meta) = c.alignment_meta(align_str) else {
		set_error(format!("alignment metadata not found: {}", align_str));
		return ptr::null_mut();
	};

	let Some(source_comp) = c.component(&align_meta.source_component) else {
		set_error(format!("source component not found: {}", align_meta.source_component));
		return ptr::null_mut();
	};

	let Some(target_comp) = c.component(&align_meta.target_component) else {
		set_error(format!("target component not found: {}", align_meta.target_component));
		return ptr::null_mut();
	};

	let Some(doc_spans) = c.spans.spans("document") else {
		set_error("no document spans".into());
		return ptr::null_mut();
	};

	let Some(source_sent_spans) = c.spans.spans(&align_meta.source_layer) else {
		set_error(format!("source span layer not found: {}", align_meta.source_layer));
		return ptr::null_mut();
	};

	let Some(target_sent_spans) = c.spans.spans(&align_meta.target_layer) else {
		set_error(format!("target span layer not found: {}", align_meta.target_layer));
		return ptr::null_mut();
	};

	let mut result_hits = Vec::new();
	let mut seen_targets = std::collections::HashSet::new();

	for hit in &source.hits {
		let Some((src_doc, src_sent)) = find_doc_and_sent(
			hit, doc_spans, source_sent_spans, source_comp,
		) else {
			continue;
		};

		for &((edge_src_doc, edge_src_sent), (tgt_doc, tgt_sent)) in edges {
			if edge_src_doc == src_doc && edge_src_sent == src_sent {
				let target_key = (tgt_doc, tgt_sent);
				if seen_targets.insert(target_key) {
					if let Some(span) = resolve_target_span(
						tgt_doc, tgt_sent, doc_spans, target_sent_spans, target_comp,
					) {
						result_hits.push(Hit {
							span,
							document_index: 0,
							sentence_index: 0,
							captures: Vec::new(),
						});
					}
				}
			}
		}
	}

	result_hits.sort_by_key(|h| h.span.start);
	Box::into_raw(Box::new(HitList { hits: result_hits }))
}

fn find_doc_and_sent(
	hit: &Hit,
	doc_spans: &[montre_core::Span],
	sent_spans: &[montre_core::Span],
	comp: &ComponentMeta,
) -> Option<(u32, u32)> {
	for doc_idx in comp.document_range.0..comp.document_range.1 {
		let doc_span = doc_spans.get(doc_idx)?;
		if hit.span.start >= doc_span.start && hit.span.end <= doc_span.end {
			let doc_within_comp = (doc_idx - comp.document_range.0) as u32;
			let mut sent_within_doc = 0u32;
			for sent_span in sent_spans.iter() {
				if sent_span.start >= doc_span.start && sent_span.end <= doc_span.end {
					if hit.span.start >= sent_span.start && hit.span.end <= sent_span.end {
						return Some((doc_within_comp, sent_within_doc));
					}
					sent_within_doc += 1;
				} else if sent_span.start >= doc_span.end {
					break;
				}
			}
		}
	}
	None
}

fn resolve_target_span(
	tgt_doc: u32,
	tgt_sent: u32,
	doc_spans: &[montre_core::Span],
	target_sent_spans: &[montre_core::Span],
	target_comp: &ComponentMeta,
) -> Option<montre_core::Span> {
	let abs_doc_idx = target_comp.document_range.0 + tgt_doc as usize;
	let doc_span = doc_spans.get(abs_doc_idx)?;
	let mut sent_count = 0u32;
	for sent_span in target_sent_spans.iter() {
		if sent_span.start >= doc_span.start && sent_span.end <= doc_span.end {
			if sent_count == tgt_sent {
				return Some(*sent_span);
			}
			sent_count += 1;
		} else if sent_span.start >= doc_span.end {
			break;
		}
	}
	None
}
