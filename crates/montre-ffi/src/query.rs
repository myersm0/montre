use std::os::raw::c_char;
use std::ptr;

use montre_index::{Corpus, SpanIndex};
use montre_query::{Query, executor};

use crate::HitList;
use crate::error::{set_error, clear_error};
use crate::strings::{borrow_cstr, export_array, to_cstring};

fn wrap_in_component(parsed: Query, component: &str) -> Query {
	let components = vec![component.to_string()];
	match parsed {
		Query::Constrained { inner, constraints } => {
			Query::Constrained {
				inner: Box::new(Query::WithinComponent {
					inner,
					components,
				}),
				constraints,
			}
		}
		other => Query::WithinComponent {
			inner: Box::new(other),
			components,
		},
	}
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
	match executor::execute_count(&plan, c) {
		Ok(n) => n as i64,
		Err(e) => {
			set_error(e.to_string());
			-1
		}
	}
}

#[no_mangle]
pub unsafe extern "C" fn montre_query_in_component(
	corpus: *const Corpus,
	cql: *const c_char,
	component: *const c_char,
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
	let Some(comp_str) = borrow_cstr(component) else {
		set_error("null component name".into());
		return ptr::null_mut();
	};

	let parsed = match montre_query::parse(cql_str) {
		Ok(q) => q,
		Err(e) => {
			set_error(e.to_string());
			return ptr::null_mut();
		}
	};

	let wrapped = wrap_in_component(parsed, comp_str);

	let plan = match montre_query::planner::plan(&wrapped) {
		Ok(p) => p,
		Err(e) => {
			set_error(e.to_string());
			return ptr::null_mut();
		}
	};

	let c = &*corpus;
	let results = match executor::execute(&plan, c) {
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
pub unsafe extern "C" fn montre_query_count_in_component(
	corpus: *const Corpus,
	cql: *const c_char,
	component: *const c_char,
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
	let Some(comp_str) = borrow_cstr(component) else {
		set_error("null component name".into());
		return -1;
	};

	let parsed = match montre_query::parse(cql_str) {
		Ok(q) => q,
		Err(e) => {
			set_error(e.to_string());
			return -1;
		}
	};

	let wrapped = wrap_in_component(parsed, comp_str);

	let plan = match montre_query::planner::plan(&wrapped) {
		Ok(p) => p,
		Err(e) => {
			set_error(e.to_string());
			return -1;
		}
	};

	let c = &*corpus;
	match executor::execute_count(&plan, c) {
		Ok(n) => n as i64,
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

/// Bulk-extract all hit start positions as a flat u64 array.
/// Free with `montre_u64_array_free(array, len)`.
#[no_mangle]
pub unsafe extern "C" fn montre_hitlist_starts(
	hits: *const HitList,
	out_len: *mut u64,
) -> *mut u64 {
	if hits.is_null() {
		if !out_len.is_null() { *out_len = 0; }
		return ptr::null_mut();
	}
	let data: Vec<u64> = (*hits).hits.iter().map(|h| h.span.start).collect();
	export_array(&data, out_len)
}

/// Bulk-extract all hit end positions as a flat u64 array.
/// Free with `montre_u64_array_free(array, len)`.
#[no_mangle]
pub unsafe extern "C" fn montre_hitlist_ends(
	hits: *const HitList,
	out_len: *mut u64,
) -> *mut u64 {
	if hits.is_null() {
		if !out_len.is_null() { *out_len = 0; }
		return ptr::null_mut();
	}
	let data: Vec<u64> = (*hits).hits.iter().map(|h| h.span.end).collect();
	export_array(&data, out_len)
}

/// Bulk-extract all hit document indices as a flat u64 array.
/// Call montre_hitlist_populate_context first if you need document indices.
/// Free with `montre_u64_array_free(array, len)`.
#[no_mangle]
pub unsafe extern "C" fn montre_hitlist_document_indices(
	hits: *const HitList,
	out_len: *mut u64,
) -> *mut u64 {
	if hits.is_null() {
		if !out_len.is_null() { *out_len = 0; }
		return ptr::null_mut();
	}
	let data: Vec<u64> = (*hits).hits.iter().map(|h| h.document_index as u64).collect();
	export_array(&data, out_len)
}

/// Bulk-extract all hit sentence indices as a flat u64 array.
/// Call montre_hitlist_populate_context first if you need sentence indices.
/// Free with `montre_u64_array_free(array, len)`.
#[no_mangle]
pub unsafe extern "C" fn montre_hitlist_sentence_indices(
	hits: *const HitList,
	out_len: *mut u64,
) -> *mut u64 {
	if hits.is_null() {
		if !out_len.is_null() { *out_len = 0; }
		return ptr::null_mut();
	}
	let data: Vec<u64> = (*hits).hits.iter().map(|h| h.sentence_index as u64).collect();
	export_array(&data, out_len)
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

	let doc_spans = corpus.spans().spans("document");
	let sent_spans = corpus.spans().spans("sentence");

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

#[no_mangle]
pub unsafe extern "C" fn montre_hit_capture_count(hits: *const HitList, index: u64) -> u32 {
	if hits.is_null() {
		return 0;
	}
	let hitlist = &*hits;
	match hitlist.hits.get(index as usize) {
		Some(hit) => hit.captures.len() as u32,
		None => 0,
	}
}

#[no_mangle]
pub unsafe extern "C" fn montre_hit_capture_name(
	hits: *const HitList,
	hit_index: u64,
	capture_index: u32,
) -> *mut c_char {
	if hits.is_null() {
		return ptr::null_mut();
	}
	let hitlist = &*hits;
	let Some(hit) = hitlist.hits.get(hit_index as usize) else {
		return ptr::null_mut();
	};
	match hit.captures.get(capture_index as usize) {
		Some((name, _)) => to_cstring(name),
		None => ptr::null_mut(),
	}
}

#[no_mangle]
pub unsafe extern "C" fn montre_hit_capture_start(
	hits: *const HitList,
	hit_index: u64,
	capture_index: u32,
) -> u64 {
	if hits.is_null() {
		return 0;
	}
	let hitlist = &*hits;
	let Some(hit) = hitlist.hits.get(hit_index as usize) else {
		return 0;
	};
	match hit.captures.get(capture_index as usize) {
		Some((_, span)) => span.start,
		None => 0,
	}
}

#[no_mangle]
pub unsafe extern "C" fn montre_hit_capture_end(
	hits: *const HitList,
	hit_index: u64,
	capture_index: u32,
) -> u64 {
	if hits.is_null() {
		return 0;
	}
	let hitlist = &*hits;
	let Some(hit) = hitlist.hits.get(hit_index as usize) else {
		return 0;
	};
	match hit.captures.get(capture_index as usize) {
		Some((_, span)) => span.end,
		None => 0,
	}
}
