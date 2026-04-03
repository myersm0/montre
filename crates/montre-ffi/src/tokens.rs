use std::os::raw::c_char;
use std::ptr;

use montre_index::{Corpus, ForwardIndex};

use crate::HitList;
use crate::strings::{to_cstring, borrow_cstr, export_array};

pub(crate) fn forward_value_string(corpus: &Corpus, position: u64, layer: &str) -> Option<String> {
	if let Some(s) = corpus.forward().get_str(position, layer) {
		return Some(s.to_string());
	}
	if let Some(n) = corpus.forward().get_int(position, layer) {
		return Some(n.to_string());
	}
	None
}

pub(crate) fn forward_value_cstr(corpus: &Corpus, position: u64, layer: &str) -> *mut c_char {
	if let Some(s) = corpus.forward().get_str(position, layer) {
		return to_cstring(s);
	}
	if let Some(n) = corpus.forward().get_int(position, layer) {
		return to_cstring(&n.to_string());
	}
	ptr::null_mut()
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
	forward_value_cstr(c, position, layer_str)
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
	if layer_str == "word" {
		return to_cstring(&c.surface_text(start, end));
	}
	let words: Vec<String> = (start..end)
		.filter_map(|p| forward_value_string(c, p, layer_str))
		.collect();
	to_cstring(&words.join(" "))
}

/// Bulk-extract annotations for a range of positions [start, end).
/// Returns a string array of `out_len` entries (caller gets ownership).
/// Positions with no value for the layer produce empty strings.
/// Free with `montre_string_array_free(array, len)`.
#[no_mangle]
pub unsafe extern "C" fn montre_corpus_token_annotations(
	corpus: *const Corpus,
	start: u64,
	end: u64,
	layer: *const c_char,
	out_len: *mut u64,
) -> *mut *mut c_char {
	if corpus.is_null() || out_len.is_null() {
		if !out_len.is_null() {
			*out_len = 0;
		}
		return ptr::null_mut();
	}
	let c = &*corpus;
	let Some(layer_str) = borrow_cstr(layer) else {
		*out_len = 0;
		return ptr::null_mut();
	};

	let count = (end.saturating_sub(start)) as usize;
	if count == 0 {
		*out_len = 0;
		return ptr::null_mut();
	}

	let ptrs: Vec<*mut c_char> = (start..end)
		.map(|pos| {
			let p = forward_value_cstr(c, pos, layer_str);
			if p.is_null() { to_cstring("") } else { p }
		})
		.collect();
	export_array(&ptrs, out_len)
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
	if count == 0 {
		*out_len = 0;
		return ptr::null_mut();
	}

	let ptrs: Vec<*mut c_char> = hitlist.hits.iter().map(|hit| {
		if layer_str == "word" {
			to_cstring(&c.surface_text(hit.span.start, hit.span.end))
		} else {
			let words: Vec<String> = (hit.span.start..hit.span.end)
				.filter_map(|p| forward_value_string(c, p, layer_str))
				.collect();
			to_cstring(&words.join(" "))
		}
	}).collect();
	export_array(&ptrs, out_len)
}

/// Bulk context-token extraction for collocational analysis.
/// For each hit, extracts tokens in a ±window around the match.
/// Returns parallel arrays: relative positions (i32) and token strings.
/// Entries for different hits are concatenated; use out_offsets to find boundaries.
/// out_offsets[i] is the start index into the flat arrays for hit i.
/// out_offsets has length hitlist_len + 1 (last entry = total length).
/// Free strings with montre_string_array_free, free positions and offsets with
/// montre_i32_array_free / montre_u64_array_free.
#[no_mangle]
pub unsafe extern "C" fn montre_context_tokens(
	hits: *const HitList,
	corpus: *const Corpus,
	window: u32,
	layer: *const c_char,
	out_positions: *mut *mut i32,
	out_tokens: *mut *mut *mut c_char,
	out_offsets: *mut *mut u64,
	out_len: *mut u64,
) {
	if hits.is_null() || corpus.is_null() || out_positions.is_null()
		|| out_tokens.is_null() || out_offsets.is_null() || out_len.is_null()
	{
		if !out_len.is_null() {
			*out_len = 0;
		}
		return;
	}

	let hitlist = &*hits;
	let c = &*corpus;
	let Some(layer_str) = borrow_cstr(layer) else {
		*out_len = 0;
		return;
	};

	let window = window as u64;
	let token_total = c.token_count();
	let hit_count = hitlist.hits.len();

	let mut all_positions: Vec<i32> = Vec::new();
	let mut all_tokens: Vec<*mut c_char> = Vec::new();
	let mut offsets: Vec<u64> = Vec::with_capacity(hit_count + 1);

	for hit in &hitlist.hits {
		offsets.push(all_positions.len() as u64);

		let ctx_start = hit.span.start.saturating_sub(window);
		let ctx_end = (hit.span.end + window).min(token_total);

		for pos in ctx_start..ctx_end {
			let relative = pos as i64 - hit.span.start as i64;
			all_positions.push(relative as i32);

			let p = forward_value_cstr(c, pos, layer_str);
			all_tokens.push(if p.is_null() { to_cstring("") } else { p });
		}
	}

	offsets.push(all_positions.len() as u64);

	if all_positions.is_empty() {
		*out_positions = ptr::null_mut();
		*out_tokens = ptr::null_mut();
		*out_offsets = ptr::null_mut();
		*out_len = 0;
		return;
	}

	*out_positions = export_array(&all_positions, out_len);
	let mut tok_len: u64 = 0;
	*out_tokens = export_array(&all_tokens, &mut tok_len);
	let mut off_len: u64 = 0;
	*out_offsets = export_array(&offsets, &mut off_len);
}
