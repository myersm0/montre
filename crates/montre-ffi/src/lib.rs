use std::cell::RefCell;
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::ptr;

use montre_core::Value;
use montre_index::{Corpus, ForwardIndex, SpanIndex};
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
