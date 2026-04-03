use std::os::raw::c_char;
use std::ptr;

use montre_index::{Corpus, SpanIndex};
use montre_query::executor::{self, Hit};

use crate::HitList;
use crate::error::{set_error, clear_error};
use crate::strings::{to_cstring, borrow_cstr};

#[no_mangle]
pub unsafe extern "C" fn montre_corpus_alignment_count(corpus: *const Corpus) -> u32 {
	if corpus.is_null() {
		return 0;
	}
	let c = &*corpus;
	c.alignment_metas().len() as u32
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
	match c.alignment_metas().get(index as usize) {
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
	match c.alignment_metas().get(index as usize) {
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
	match c.alignment_metas().get(index as usize) {
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
	match c.alignment_metas().get(index as usize) {
		Some(a) => a.edge_count as u64,
		None => 0,
	}
}

#[no_mangle]
pub unsafe extern "C" fn montre_corpus_alignment_source_layer(
	corpus: *const Corpus,
	index: u32,
) -> *mut c_char {
	if corpus.is_null() {
		return ptr::null_mut();
	}
	let c = &*corpus;
	match c.alignment_metas().get(index as usize) {
		Some(a) => to_cstring(&a.source_layer),
		None => ptr::null_mut(),
	}
}

#[no_mangle]
pub unsafe extern "C" fn montre_corpus_alignment_target_layer(
	corpus: *const Corpus,
	index: u32,
) -> *mut c_char {
	if corpus.is_null() {
		return ptr::null_mut();
	}
	let c = &*corpus;
	match c.alignment_metas().get(index as usize) {
		Some(a) => to_cstring(&a.target_layer),
		None => ptr::null_mut(),
	}
}

/// Returns 1 if directed, 0 if undirected, -1 if index is invalid.
#[no_mangle]
pub unsafe extern "C" fn montre_corpus_alignment_directed(
	corpus: *const Corpus,
	index: u32,
) -> i32 {
	if corpus.is_null() {
		return -1;
	}
	let c = &*corpus;
	match c.alignment_metas().get(index as usize) {
		Some(a) => a.directed as i32,
		None => -1,
	}
}

/// Return all alignment edges as a flat u32 array of quads:
/// [src_doc, src_sent, tgt_doc, tgt_sent, ...].
/// out_len receives the number of edges (array length is out_len * 4).
/// Free with `montre_u32_array_free(array, out_len * 4)`.
/// Returns null if the alignment does not exist.
#[no_mangle]
pub unsafe extern "C" fn montre_corpus_alignment_edges(
	corpus: *const Corpus,
	name: *const c_char,
	out_len: *mut u64,
) -> *mut u32 {
	if corpus.is_null() || out_len.is_null() {
		if !out_len.is_null() { *out_len = 0; }
		return ptr::null_mut();
	}
	let c = &*corpus;
	let Some(name_str) = borrow_cstr(name) else {
		*out_len = 0;
		return ptr::null_mut();
	};
	let Some(edges) = c.alignment_edges(name_str) else {
		*out_len = 0;
		return ptr::null_mut();
	};

	let edge_count = edges.len();
	if edge_count == 0 {
		*out_len = 0;
		return ptr::null_mut();
	}

	let flat_len = edge_count * 4;
	let layout = std::alloc::Layout::array::<u32>(flat_len).unwrap();
	let array = std::alloc::alloc(layout) as *mut u32;
	if array.is_null() {
		*out_len = 0;
		return ptr::null_mut();
	}

	for (i, &((src_doc, src_sent), (tgt_doc, tgt_sent))) in edges.iter().enumerate() {
		let base = i * 4;
		*array.add(base) = src_doc;
		*array.add(base + 1) = src_sent;
		*array.add(base + 2) = tgt_doc;
		*array.add(base + 3) = tgt_sent;
	}

	*out_len = edge_count as u64;
	array
}

/// Project a HitList through a named alignment, returning a new HitList
/// of target-side sentence spans.
/// out_unmapped: number of source hits not locatable in the source component (nullable).
/// out_no_alignment: number of source hits with no alignment edge (nullable).
/// out_projected: number of unique target sentences produced (nullable).
#[no_mangle]
pub unsafe extern "C" fn montre_project(
	corpus: *const Corpus,
	source_hits: *const HitList,
	alignment_name: *const c_char,
	out_unmapped: *mut u64,
	out_no_alignment: *mut u64,
	out_projected: *mut u64,
) -> *mut HitList {
	clear_error();

	if !out_unmapped.is_null() { *out_unmapped = 0; }
	if !out_no_alignment.is_null() { *out_no_alignment = 0; }
	if !out_projected.is_null() { *out_projected = 0; }

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

	let Some(doc_spans) = c.spans().spans("document") else {
		set_error("no document spans".into());
		return ptr::null_mut();
	};

	let Some(source_sent_spans) = c.spans().spans(&align_meta.source_layer) else {
		set_error(format!("source span layer not found: {}", align_meta.source_layer));
		return ptr::null_mut();
	};

	let Some(target_sent_spans) = c.spans().spans(&align_meta.target_layer) else {
		set_error(format!("target span layer not found: {}", align_meta.target_layer));
		return ptr::null_mut();
	};

	let edge_map = executor::build_edge_map(edges);

	let mut result_hits = Vec::new();
	let mut seen_targets = std::collections::HashSet::new();
	let mut unmapped = 0u64;
	let mut no_alignment = 0u64;

	for hit in &source.hits {
		let Some((src_doc, src_sent)) = executor::find_doc_and_sent(
			hit, doc_spans, source_sent_spans, source_comp,
		) else {
			unmapped += 1;
			continue;
		};

		if let Some(targets) = edge_map.get(&(src_doc, src_sent)) {
			for &(tgt_doc, tgt_sent) in targets {
				if seen_targets.insert((tgt_doc, tgt_sent)) {
					if let Some(span) = executor::resolve_target_span(
						tgt_doc, tgt_sent, doc_spans, target_sent_spans, target_comp,
					) {
						result_hits.push(Hit {
							span,
							document_index: Hit::UNPOPULATED,
							sentence_index: Hit::UNPOPULATED,
							captures: Vec::new(),
						});
					}
				}
			}
		} else {
			no_alignment += 1;
		}
	}

	result_hits.sort_by_key(|h| h.span.start);

	if !out_unmapped.is_null() { *out_unmapped = unmapped; }
	if !out_no_alignment.is_null() { *out_no_alignment = no_alignment; }
	if !out_projected.is_null() { *out_projected = result_hits.len() as u64; }

	Box::into_raw(Box::new(HitList { hits: result_hits }))
}
