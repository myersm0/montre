use std::os::raw::c_char;
use std::ptr;

use montre_index::{Corpus, InvertedIndex, SpanIndex};

use crate::error::{set_error, clear_error};
use crate::strings::{to_cstring, borrow_cstr, export_array, export_string_array};

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

/// Get the half-open document index range [start, end) for a component.
/// Returns 1 on success, 0 if the corpus or component index is invalid.
#[no_mangle]
pub unsafe extern "C" fn montre_corpus_component_document_range(
	corpus: *const Corpus,
	index: u32,
	out_start: *mut u32,
	out_end: *mut u32,
) -> i32 {
	if corpus.is_null() || out_start.is_null() || out_end.is_null() {
		return 0;
	}
	let c = &*corpus;
	let Some(comp) = c.components().get(index as usize) else {
		return 0;
	};
	*out_start = comp.document_range.0 as u32;
	*out_end = comp.document_range.1 as u32;
	1
}

/// Find the component index for a given document index.
/// Returns -1 if the document index does not belong to any component.
#[no_mangle]
pub unsafe extern "C" fn montre_corpus_component_for_document(
	corpus: *const Corpus,
	doc_index: u32,
) -> i32 {
	if corpus.is_null() {
		return -1;
	}
	let c = &*corpus;
	for (i, comp) in c.components().iter().enumerate() {
		if (doc_index as usize) >= comp.document_range.0
			&& (doc_index as usize) < comp.document_range.1
		{
			return i as i32;
		}
	}
	-1
}

/// Returns the total token count for a component.
/// Returns -1 if the component index is invalid or the corpus has no document spans.
#[no_mangle]
pub unsafe extern "C" fn montre_corpus_component_token_count(
	corpus: *const Corpus,
	index: u32,
) -> i64 {
	if corpus.is_null() {
		return -1;
	}
	let c = &*corpus;
	let Some(comp) = c.components().get(index as usize) else {
		return -1;
	};
	let (doc_start, doc_end) = comp.document_range;
	if doc_start >= doc_end {
		return 0;
	}
	let Some(doc_spans) = c.spans().spans("document") else {
		return -1;
	};
	let Some(first) = doc_spans.get(doc_start) else {
		return -1;
	};
	let Some(last) = doc_spans.get(doc_end - 1) else {
		return -1;
	};
	(last.end - first.start) as i64
}

/// Return all distinct values for a layer from the inverted index.
/// Returns a string array of `out_len` entries (caller gets ownership).
/// Free with `montre_string_array_free(array, len)`.
/// Returns null if the layer does not exist.
#[no_mangle]
pub unsafe extern "C" fn montre_corpus_inverted_values(
	corpus: *const Corpus,
	layer: *const c_char,
	out_len: *mut u64,
) -> *mut *mut c_char {
	if corpus.is_null() {
		if !out_len.is_null() { *out_len = 0; }
		return ptr::null_mut();
	}
	let c = &*corpus;
	let Some(layer_str) = borrow_cstr(layer) else {
		if !out_len.is_null() { *out_len = 0; }
		return ptr::null_mut();
	};
	let Some(values) = c.inverted().values(layer_str) else {
		if !out_len.is_null() { *out_len = 0; }
		return ptr::null_mut();
	};
	export_string_array(&values, out_len)
}

/// Returns the document index for a given name, or -1 if not found.
#[no_mangle]
pub unsafe extern "C" fn montre_corpus_document_index_by_name(
	corpus: *const Corpus,
	name: *const c_char,
) -> i64 {
	if corpus.is_null() {
		return -1;
	}
	let c = &*corpus;
	let Some(name_str) = borrow_cstr(name) else {
		return -1;
	};
	match c.document_index_by_name(name_str) {
		Some(idx) => idx as i64,
		None => -1,
	}
}

/// Returns the number of positions matching a layer/value pair
/// via inverted index bitmap cardinality. Returns -1 if not found.
#[no_mangle]
pub unsafe extern "C" fn montre_corpus_inverted_count(
	corpus: *const Corpus,
	layer: *const c_char,
	value: *const c_char,
) -> i64 {
	if corpus.is_null() {
		return -1;
	}
	let c = &*corpus;
	let Some(layer_str) = borrow_cstr(layer) else {
		return -1;
	};
	let Some(value_str) = borrow_cstr(value) else {
		return -1;
	};
	match c.inverted().get(layer_str, value_str) {
		Some(bitmap) => bitmap.len() as i64,
		None => -1,
	}
}

/// Returns the component index for a given name, or -1 if not found.
#[no_mangle]
pub unsafe extern "C" fn montre_corpus_component_index_by_name(
	corpus: *const Corpus,
	name: *const c_char,
) -> i32 {
	if corpus.is_null() {
		return -1;
	}
	let c = &*corpus;
	let Some(name_str) = borrow_cstr(name) else {
		return -1;
	};
	match c.components().iter().position(|comp| comp.name == name_str) {
		Some(idx) => idx as i32,
		None => -1,
	}
}

/// Bulk-extract all values and their bitmap cardinalities for a layer.
/// Returns parallel arrays: out_values (string array) and out_counts (u64 array).
/// Free values with montre_string_array_free, counts with montre_u64_array_free.
/// Returns 1 on success, 0 if the layer does not exist.
#[no_mangle]
pub unsafe extern "C" fn montre_corpus_inverted_counts(
	corpus: *const Corpus,
	layer: *const c_char,
	out_values: *mut *mut *mut c_char,
	out_counts: *mut *mut u64,
	out_len: *mut u64,
) -> i32 {
	if corpus.is_null() || out_values.is_null() || out_counts.is_null() || out_len.is_null() {
		if !out_len.is_null() { *out_len = 0; }
		return 0;
	}
	let c = &*corpus;
	let Some(layer_str) = borrow_cstr(layer) else {
		*out_len = 0;
		return 0;
	};
	let Some(values) = c.inverted().values(layer_str) else {
		*out_len = 0;
		return 0;
	};

	let counts: Vec<u64> = values.iter().map(|v| {
		c.inverted().get(layer_str, v).map_or(0, |b| b.len() as u64)
	}).collect();

	*out_values = export_string_array(&values, out_len);
	let mut count_len: u64 = 0;
	*out_counts = export_array(&counts, &mut count_len);
	1
}

#[no_mangle]
pub unsafe extern "C" fn montre_corpus_mwt_form(
	corpus: *const Corpus,
	position: u64,
) -> *mut c_char {
	if corpus.is_null() {
		return ptr::null_mut();
	}
	let c = &*corpus;
	match c.mwt_covering(position) {
		Some(mwt) => to_cstring(&mwt.form),
		None => ptr::null_mut(),
	}
}

#[no_mangle]
pub unsafe extern "C" fn montre_corpus_mwt_at(
	corpus: *const Corpus,
	position: u64,
	out_start: *mut u64,
	out_end: *mut u64,
	out_form: *mut *mut c_char,
) -> i32 {
	if corpus.is_null() {
		return 0;
	}
	let c = &*corpus;
	match c.mwt_covering(position) {
		Some(mwt) => {
			if !out_start.is_null() { *out_start = mwt.start; }
			if !out_end.is_null() { *out_end = mwt.end; }
			if !out_form.is_null() { *out_form = to_cstring(&mwt.form); }
			1
		}
		None => 0,
	}
}

#[no_mangle]
pub unsafe extern "C" fn montre_corpus_surface_text(
	corpus: *const Corpus,
	start: u64,
	end: u64,
) -> *mut c_char {
	if corpus.is_null() {
		return ptr::null_mut();
	}
	let c = &*corpus;
	to_cstring(&c.surface_text(start, end))
}

#[no_mangle]
pub unsafe extern "C" fn montre_corpus_has_no_space_after(
	corpus: *const Corpus,
	position: u64,
) -> i32 {
	if corpus.is_null() {
		return 0;
	}
	let c = &*corpus;
	if c.has_no_space_after(position) { 1 } else { 0 }
}

#[no_mangle]
pub unsafe extern "C" fn montre_corpus_empty_node_count(
	corpus: *const Corpus,
) -> u64 {
	if corpus.is_null() {
		return 0;
	}
	let c = &*corpus;
	c.empty_nodes().map_or(0, |s| s.len() as u64)
}

#[no_mangle]
pub unsafe extern "C" fn montre_corpus_empty_node_count_in_sentence(
	corpus: *const Corpus,
	sentence_index: u32,
) -> u64 {
	if corpus.is_null() {
		return 0;
	}
	let c = &*corpus;
	c.empty_nodes().map_or(0, |s| s.in_sentence(sentence_index).len() as u64)
}

#[no_mangle]
pub unsafe extern "C" fn montre_corpus_empty_node_field(
	corpus: *const Corpus,
	sentence_index: u32,
	node_index: u32,
	field: *const c_char,
) -> *mut c_char {
	if corpus.is_null() {
		return ptr::null_mut();
	}
	let c = &*corpus;
	let Some(store) = c.empty_nodes() else {
		return ptr::null_mut();
	};
	let nodes = store.in_sentence(sentence_index);
	let Some(node) = nodes.get(node_index as usize) else {
		return ptr::null_mut();
	};
	let Some(field_str) = borrow_cstr(field) else {
		return ptr::null_mut();
	};
	let value = match field_str {
		"node_id" => Some(node.node_id.as_str()),
		"form" => Some(node.form.as_str()),
		"lemma" => node.lemma.as_deref(),
		"upos" => node.upos.as_deref(),
		"xpos" => node.xpos.as_deref(),
		"feats" => node.feats.as_deref(),
		"deps" => node.deps.as_deref(),
		"misc" => node.misc.as_deref(),
		_ => None,
	};
	match value {
		Some(s) => to_cstring(s),
		None => ptr::null_mut(),
	}
}
