use std::os::raw::c_char;
use std::ptr;

use montre_index::{Corpus, ForwardIndex};

use crate::HitList;
use crate::strings::{to_cstring, borrow_cstr};

pub(crate) fn forward_value_string(corpus: &Corpus, position: u64, layer: &str) -> Option<String> {
	if let Some(s) = corpus.forward.get_str(position, layer) {
		return Some(s.to_string());
	}
	if let Some(n) = corpus.forward.get_int(position, layer) {
		return Some(n.to_string());
	}
	None
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
	match forward_value_string(c, position, layer_str) {
		Some(val) => to_cstring(&val),
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

	let layout = std::alloc::Layout::array::<*mut c_char>(count).unwrap();
	let array = std::alloc::alloc(layout) as *mut *mut c_char;
	if array.is_null() {
		*out_len = 0;
		return ptr::null_mut();
	}

	for (i, pos) in (start..end).enumerate() {
		let val = forward_value_string(c, pos, layer_str).unwrap_or_default();
		*array.add(i) = to_cstring(&val);
	}

	*out_len = count as u64;
	array
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

	let layout = std::alloc::Layout::array::<*mut c_char>(count).unwrap();
	let array = std::alloc::alloc(layout) as *mut *mut c_char;
	if array.is_null() {
		*out_len = 0;
		return ptr::null_mut();
	}

	for (i, hit) in hitlist.hits.iter().enumerate() {
		let words: Vec<String> = (hit.span.start..hit.span.end)
			.filter_map(|p| forward_value_string(c, p, layer_str))
			.collect();
		*array.add(i) = to_cstring(&words.join(" "));
	}

	*out_len = count as u64;
	array
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
	let mut all_tokens: Vec<String> = Vec::new();
	let mut offsets: Vec<u64> = Vec::with_capacity(hit_count + 1);

	for hit in &hitlist.hits {
		offsets.push(all_positions.len() as u64);

		let ctx_start = hit.span.start.saturating_sub(window);
		let ctx_end = (hit.span.end + window).min(token_total);

		for pos in ctx_start..ctx_end {
			let relative = pos as i64 - hit.span.start as i64;
			all_positions.push(relative as i32);

			let token_str = forward_value_string(c, pos, layer_str)
				.unwrap_or_default();
			all_tokens.push(token_str);
		}
	}

	offsets.push(all_positions.len() as u64);
	let total = all_positions.len();

	if total == 0 {
		*out_positions = ptr::null_mut();
		*out_tokens = ptr::null_mut();
		*out_offsets = ptr::null_mut();
		*out_len = 0;
		return;
	}

	let pos_layout = std::alloc::Layout::array::<i32>(total).unwrap();
	let pos_array = std::alloc::alloc(pos_layout) as *mut i32;
	for (i, &p) in all_positions.iter().enumerate() {
		*pos_array.add(i) = p;
	}

	let tok_layout = std::alloc::Layout::array::<*mut c_char>(total).unwrap();
	let tok_array = std::alloc::alloc(tok_layout) as *mut *mut c_char;
	for (i, s) in all_tokens.iter().enumerate() {
		*tok_array.add(i) = to_cstring(s);
	}

	let off_layout = std::alloc::Layout::array::<u64>(offsets.len()).unwrap();
	let off_array = std::alloc::alloc(off_layout) as *mut u64;
	for (i, &o) in offsets.iter().enumerate() {
		*off_array.add(i) = o;
	}

	*out_positions = pos_array;
	*out_tokens = tok_array;
	*out_offsets = off_array;
	*out_len = total as u64;
}
