use std::os::raw::c_char;
use std::ptr;

use montre_index::{Corpus, SpanIndex};
use montre_query::executor::{self, Hit, Results};
use montre_query::planner::QueryPlan;
use montre_query::{Limits, Query, QueryError};

use crate::error::{clear_error, set_error, set_query_error};
use crate::strings::{borrow_cstr, export_array, to_cstring};
use crate::HitList;

fn wrap_in_component(parsed: Query, component: &str) -> Query {
	let components = vec![component.to_string()];
	match parsed {
		Query::Constrained { inner, constraints } => Query::Constrained {
			inner: Box::new(Query::WithinComponent { inner, components }),
			constraints,
		},
		other => Query::WithinComponent {
			inner: Box::new(other),
			components,
		},
	}
}

// parse, scope to `component` if given, plan and run; failures go to montre_last_error
unsafe fn run<T>(
	corpus: *const Corpus,
	cql: *const c_char,
	component: Option<*const c_char>,
	execute: impl FnOnce(&QueryPlan, &Corpus) -> Result<T, QueryError>,
) -> Option<T> {
	clear_error();
	if corpus.is_null() {
		set_error("null corpus".into());
		return None;
	}
	let Some(cql_str) = borrow_cstr(cql) else {
		set_error("null query string".into());
		return None;
	};
	let Ok(component) = component
		.map(|name| borrow_cstr(name).ok_or(()))
		.transpose()
	else {
		set_error("null component name".into());
		return None;
	};
	let planned = montre_query::parse(cql_str)
		.map(|parsed| match component {
			Some(name) => wrap_in_component(parsed, name),
			None => parsed,
		})
		.and_then(|query| montre_query::planner::plan(&query));
	match planned.and_then(|plan| execute(&plan, &*corpus)) {
		Ok(value) => Some(value),
		Err(error) => {
			set_query_error(error);
			None
		}
	}
}

fn limits(max_intermediate_matches: u64, max_expansions: u64, max_results: u64) -> Limits {
	Limits {
		max_intermediate_matches,
		max_expansions,
		max_results,
	}
}

fn hit_list(results: Option<Results>) -> *mut HitList {
	match results {
		Some(results) => Box::into_raw(Box::new(HitList {
			hits: results.into_hits(),
		})),
		None => ptr::null_mut(),
	}
}

fn count(counted: Option<usize>) -> i64 {
	counted.map_or(-1, |count| count as i64)
}

#[no_mangle]
pub unsafe extern "C" fn montre_query(corpus: *const Corpus, cql: *const c_char) -> *mut HitList {
	hit_list(run(corpus, cql, None, executor::execute))
}

#[no_mangle]
pub unsafe extern "C" fn montre_query_count(corpus: *const Corpus, cql: *const c_char) -> i64 {
	count(run(corpus, cql, None, executor::execute_count))
}

#[no_mangle]
pub unsafe extern "C" fn montre_query_in_component(
	corpus: *const Corpus,
	cql: *const c_char,
	component: *const c_char,
) -> *mut HitList {
	hit_list(run(corpus, cql, Some(component), executor::execute))
}

#[no_mangle]
pub unsafe extern "C" fn montre_query_count_in_component(
	corpus: *const Corpus,
	cql: *const c_char,
	component: *const c_char,
) -> i64 {
	count(run(corpus, cql, Some(component), executor::execute_count))
}

/// Writes the limits the functions without them use. Each pointer is nullable.
#[no_mangle]
pub unsafe extern "C" fn montre_default_limits(
	out_max_intermediate_matches: *mut u64,
	out_max_expansions: *mut u64,
	out_max_results: *mut u64,
) {
	let defaults = Limits::default();
	for (out, value) in [
		(
			out_max_intermediate_matches,
			defaults.max_intermediate_matches,
		),
		(out_max_expansions, defaults.max_expansions),
		(out_max_results, defaults.max_results),
	] {
		if !out.is_null() {
			*out = value;
		}
	}
}

#[no_mangle]
pub unsafe extern "C" fn montre_query_with_limits(
	corpus: *const Corpus,
	cql: *const c_char,
	max_intermediate_matches: u64,
	max_expansions: u64,
	max_results: u64,
) -> *mut HitList {
	let limits = limits(max_intermediate_matches, max_expansions, max_results);
	hit_list(run(corpus, cql, None, |plan, corpus| {
		executor::execute_with(plan, corpus, &limits)
	}))
}

#[no_mangle]
pub unsafe extern "C" fn montre_query_count_with_limits(
	corpus: *const Corpus,
	cql: *const c_char,
	max_intermediate_matches: u64,
	max_expansions: u64,
	max_results: u64,
) -> i64 {
	let limits = limits(max_intermediate_matches, max_expansions, max_results);
	count(run(corpus, cql, None, |plan, corpus| {
		executor::execute_count_with(plan, corpus, &limits)
	}))
}

#[no_mangle]
pub unsafe extern "C" fn montre_query_in_component_with_limits(
	corpus: *const Corpus,
	cql: *const c_char,
	component: *const c_char,
	max_intermediate_matches: u64,
	max_expansions: u64,
	max_results: u64,
) -> *mut HitList {
	let limits = limits(max_intermediate_matches, max_expansions, max_results);
	hit_list(run(corpus, cql, Some(component), |plan, corpus| {
		executor::execute_with(plan, corpus, &limits)
	}))
}

#[no_mangle]
pub unsafe extern "C" fn montre_query_count_in_component_with_limits(
	corpus: *const Corpus,
	cql: *const c_char,
	component: *const c_char,
	max_intermediate_matches: u64,
	max_expansions: u64,
	max_results: u64,
) -> i64 {
	let limits = limits(max_intermediate_matches, max_expansions, max_results);
	count(run(corpus, cql, Some(component), |plan, corpus| {
		executor::execute_count_with(plan, corpus, &limits)
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

/// Bulk-extract all hit start positions as a flat u64 array.
/// Free with `montre_u64_array_free(array, len)`.
#[no_mangle]
pub unsafe extern "C" fn montre_hitlist_starts(
	hits: *const HitList,
	out_len: *mut u64,
) -> *mut u64 {
	if hits.is_null() {
		if !out_len.is_null() {
			*out_len = 0;
		}
		return ptr::null_mut();
	}
	let data: Vec<u64> = (*hits).hits.iter().map(|h| h.span.start).collect();
	export_array(&data, out_len)
}

/// Bulk-extract all hit end positions as a flat u64 array.
/// Free with `montre_u64_array_free(array, len)`.
#[no_mangle]
pub unsafe extern "C" fn montre_hitlist_ends(hits: *const HitList, out_len: *mut u64) -> *mut u64 {
	if hits.is_null() {
		if !out_len.is_null() {
			*out_len = 0;
		}
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
		if !out_len.is_null() {
			*out_len = 0;
		}
		return ptr::null_mut();
	}
	let data: Vec<u64> = (*hits)
		.hits
		.iter()
		.map(|h| h.document_index as u64)
		.collect();
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
		if !out_len.is_null() {
			*out_len = 0;
		}
		return ptr::null_mut();
	}
	let data: Vec<u64> = (*hits)
		.hits
		.iter()
		.map(|h| h.sentence_index as u64)
		.collect();
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
			hit.document_index = montre_core::span_containing(spans, hit.span.start)
				.map_or(Hit::UNPOPULATED, |i| i as u32);
		}
		if let Some(spans) = sent_spans {
			hit.sentence_index = montre_core::span_containing(spans, hit.span.start)
				.map_or(Hit::UNPOPULATED, |i| i as u32);
		}
	}
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
