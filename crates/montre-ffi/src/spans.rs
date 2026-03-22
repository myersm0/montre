use std::os::raw::c_char;
use std::ptr;

use montre_index::{Corpus, SpanIndex};

use crate::strings::{to_cstring, borrow_cstr};

#[no_mangle]
pub unsafe extern "C" fn montre_corpus_span_layer_count(corpus: *const Corpus) -> u32 {
	if corpus.is_null() {
		return 0;
	}
	let c = &*corpus;
	c.span_layers().len() as u32
}

#[no_mangle]
pub unsafe extern "C" fn montre_corpus_span_layer_name(
	corpus: *const Corpus,
	index: u32,
) -> *mut c_char {
	if corpus.is_null() {
		return ptr::null_mut();
	}
	let c = &*corpus;
	match c.span_layers().get(index as usize) {
		Some(name) => to_cstring(name),
		None => ptr::null_mut(),
	}
}

/// Returns the number of spans in a layer, or -1 if the layer does not exist.
#[no_mangle]
pub unsafe extern "C" fn montre_corpus_span_count(
	corpus: *const Corpus,
	layer: *const c_char,
) -> i64 {
	if corpus.is_null() {
		return -1;
	}
	let c = &*corpus;
	let Some(layer_str) = borrow_cstr(layer) else {
		return -1;
	};
	match c.spans.spans(layer_str) {
		Some(spans) => spans.len() as i64,
		None => -1,
	}
}

/// Get the start and end of a span by layer name and index.
/// Returns 1 on success, 0 if the layer or index is invalid.
#[no_mangle]
pub unsafe extern "C" fn montre_corpus_span_at(
	corpus: *const Corpus,
	layer: *const c_char,
	index: u64,
	out_start: *mut u64,
	out_end: *mut u64,
) -> i32 {
	if corpus.is_null() || out_start.is_null() || out_end.is_null() {
		return 0;
	}
	let c = &*corpus;
	let Some(layer_str) = borrow_cstr(layer) else {
		return 0;
	};
	let Some(spans) = c.spans.spans(layer_str) else {
		return 0;
	};
	let Some(span) = spans.get(index as usize) else {
		return 0;
	};
	*out_start = span.start;
	*out_end = span.end;
	1
}

/// Find the span containing a position. Returns the span index and writes
/// the span boundaries to out_start and out_end.
/// Returns -1 if the layer does not exist or no span contains the position.
/// out_start and out_end are nullable (pass NULL if you only need the index).
#[no_mangle]
pub unsafe extern "C" fn montre_corpus_span_containing(
	corpus: *const Corpus,
	layer: *const c_char,
	position: u64,
	out_start: *mut u64,
	out_end: *mut u64,
) -> i64 {
	if corpus.is_null() {
		return -1;
	}
	let c = &*corpus;
	let Some(layer_str) = borrow_cstr(layer) else {
		return -1;
	};
	let Some(spans) = c.spans.spans(layer_str) else {
		return -1;
	};

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
			if !out_start.is_null() { *out_start = span.start; }
			if !out_end.is_null() { *out_end = span.end; }
			return mid as i64;
		}
	}

	-1
}
