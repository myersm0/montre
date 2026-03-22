use std::os::raw::c_char;
use std::ptr;

use montre_index::{Corpus, InvertedIndex};

use crate::error::{set_error, clear_error};
use crate::strings::{to_cstring, borrow_cstr};

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

	let Some(values) = c.inverted.values(layer_str) else {
		*out_len = 0;
		return ptr::null_mut();
	};

	let count = values.len();
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

	for (i, val) in values.iter().enumerate() {
		*array.add(i) = to_cstring(val);
	}

	*out_len = count as u64;
	array
}
