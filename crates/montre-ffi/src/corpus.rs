use std::os::raw::c_char;
use std::ptr;

use montre_index::Corpus;

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
