use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use montre_ffi::error::*;
use montre_ffi::strings::*;
use montre_ffi::corpus::*;
use montre_ffi::tokens::*;
use montre_ffi::query::*;
use montre_ffi::spans::*;
use montre_ffi::alignment::*;
use montre_ffi::build::*;
use montre_ffi::HitList;

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_path(prefix: &str) -> PathBuf {
	let id = COUNTER.fetch_add(1, Ordering::SeqCst);
	std::env::temp_dir().join(format!("montre_ffi_test_{}_{}_{}", std::process::id(), prefix, id))
}

fn cstr(s: &str) -> CString {
	CString::new(s).unwrap()
}

unsafe fn read_cstr(p: *mut c_char) -> String {
	assert!(!p.is_null());
	let s = CStr::from_ptr(p).to_str().unwrap().to_string();
	montre_string_free(p);
	s
}

unsafe fn read_string_array(array: *mut *mut c_char, len: u64) -> Vec<String> {
	assert!(!array.is_null());
	let mut result = Vec::new();
	for i in 0..len as usize {
		let p = *array.add(i);
		result.push(CStr::from_ptr(p).to_str().unwrap().to_string());
	}
	montre_string_array_free(array, len);
	result
}

const CONLLU_FR: &str = "\
1	Le	le	DET	_	Definite=Def|Gender=Masc|Number=Sing	2	det	_	_
2	chat	chat	NOUN	_	Gender=Masc|Number=Sing	3	nsubj	_	_
3	dort	dormir	VERB	_	Mood=Ind|Number=Sing|Person=3|Tense=Pres	0	root	_	_
4	.	.	PUNCT	_	_	3	punct	_	_

1	La	le	DET	_	Definite=Def|Gender=Fem|Number=Sing	3	det	_	_
2	vieille	vieux	ADJ	_	Gender=Fem|Number=Sing	3	amod	_	_
3	maison	maison	NOUN	_	Gender=Fem|Number=Sing	4	nsubj	_	_
4	dort	dormir	VERB	_	Mood=Ind|Number=Sing|Person=3|Tense=Pres	0	root	_	_
5	.	.	PUNCT	_	_	4	punct	_	_

";

const CONLLU_EN: &str = "\
1	The	the	DET	_	Definite=Def	2	det	_	_
2	cat	cat	NOUN	_	Number=Sing	3	nsubj	_	_
3	sleeps	sleep	VERB	_	Mood=Ind|Number=Sing|Person=3|Tense=Pres	0	root	_	_
4	.	.	PUNCT	_	_	3	punct	_	_

1	The	the	DET	_	Definite=Def	3	det	_	_
2	old	old	ADJ	_	Degree=Pos	3	amod	_	_
3	house	house	NOUN	_	Number=Sing	4	nsubj	_	_
4	sleeps	sleep	VERB	_	Mood=Ind|Number=Sing|Person=3|Tense=Pres	0	root	_	_
5	.	.	PUNCT	_	_	4	punct	_	_

";

fn write_single_component_dir() -> PathBuf {
	let dir = temp_path("conllu");
	std::fs::create_dir_all(&dir).unwrap();
	std::fs::write(dir.join("doc.conllu"), CONLLU_FR).unwrap();
	dir
}

fn write_multi_component_dirs() -> (PathBuf, PathBuf) {
	let base = temp_path("multi");
	let fr_dir = base.join("fr");
	let en_dir = base.join("en");
	let align_dir = base.join("alignments");
	std::fs::create_dir_all(&fr_dir).unwrap();
	std::fs::create_dir_all(&en_dir).unwrap();
	std::fs::create_dir_all(&align_dir).unwrap();

	std::fs::write(fr_dir.join("texte.conllu"), CONLLU_FR).unwrap();
	std::fs::write(en_dir.join("text.conllu"), CONLLU_EN).unwrap();

	// sentence alignment: fr sentence 0 → en sentence 0, fr sentence 1 → en sentence 1
	let alignment_tsv = "texte.conllu\t0\ttext.conllu\t0\ntexte.conllu\t1\ttext.conllu\t1\n";
	std::fs::write(align_dir.join("align.tsv"), alignment_tsv).unwrap();

	let manifest = format!(
		r#"[corpus]
name = "test_parallel"
decompose_feats = true

[components.fr]
path = "{}"
language = "fr"

[components.en]
path = "{}"
language = "en"

[alignments.sentence]
source = "fr"
target = "en"
edges = "{}"
source_layer = "sentence"
target_layer = "sentence"
"#,
		fr_dir.display(),
		en_dir.display(),
		align_dir.join("align.tsv").display(),
	);

	let manifest_path = base.join("corpus.toml");
	std::fs::write(&manifest_path, manifest).unwrap();

	(base, manifest_path)
}

// ===========================================================================
// Single-component: build, open, introspection, query, tokens
// ===========================================================================

#[test]
fn single_component_lifecycle() {
	unsafe {
		let input_dir = write_single_component_dir();
		let output_dir = temp_path("corpus_single");

		let name = cstr("test_single");
		let input = cstr(input_dir.to_str().unwrap());
		let output = cstr(output_dir.to_str().unwrap());

		// build
		let rc = montre_build_directory(
			name.as_ptr(), input.as_ptr(), output.as_ptr(), 1, 0,
		);
		assert_eq!(rc, 1, "build failed: {:?}", read_last_error());

		// open
		let corpus = montre_corpus_open(output.as_ptr());
		assert!(!corpus.is_null(), "open failed: {:?}", read_last_error());

		// token count: 4 + 5 = 9
		assert_eq!(montre_corpus_token_count(corpus), 9);

		// layers should include the core set plus decomposed feats
		let layer_count = montre_corpus_layer_count(corpus);
		assert!(layer_count >= 6); // word, lemma, pos, xpos, feats, deprel + feats.*
		let mut layer_names = Vec::new();
		for i in 0..layer_count {
			layer_names.push(read_cstr(montre_corpus_layer_name(corpus, i)));
		}
		assert!(layer_names.contains(&"word".to_string()));
		assert!(layer_names.contains(&"lemma".to_string()));
		assert!(layer_names.contains(&"pos".to_string()));

		// documents
		assert_eq!(montre_corpus_document_count(corpus), 1);
		let doc_name = read_cstr(montre_corpus_document_name(corpus, 0));
		assert_eq!(doc_name, "doc.conllu");

		// components: single-component corpus has 0 components
		assert_eq!(montre_corpus_component_count(corpus), 0);

		// span layers
		let span_layer_count = montre_corpus_span_layer_count(corpus);
		assert!(span_layer_count >= 2); // sentence, document
		let mut span_layer_names = Vec::new();
		for i in 0..span_layer_count {
			span_layer_names.push(read_cstr(montre_corpus_span_layer_name(corpus, i)));
		}
		assert!(span_layer_names.contains(&"sentence".to_string()));
		assert!(span_layer_names.contains(&"document".to_string()));

		// span count
		let sent = cstr("sentence");
		let sent_count = montre_corpus_span_count(corpus, sent.as_ptr());
		assert_eq!(sent_count, 2);

		// span_at: sentence 0 should be [0, 4)
		let mut start: u64 = 0;
		let mut end: u64 = 0;
		let rc = montre_corpus_span_at(corpus, sent.as_ptr(), 0, &mut start, &mut end);
		assert_eq!(rc, 1);
		assert_eq!(start, 0);
		assert_eq!(end, 4);

		// span_at: sentence 1 should be [4, 9)
		let rc = montre_corpus_span_at(corpus, sent.as_ptr(), 1, &mut start, &mut end);
		assert_eq!(rc, 1);
		assert_eq!(start, 4);
		assert_eq!(end, 9);

		// span_containing: position 5 is in sentence 1
		let idx = montre_corpus_span_containing(corpus, sent.as_ptr(), 5, &mut start, &mut end);
		assert_eq!(idx, 1);
		assert_eq!(start, 4);
		assert_eq!(end, 9);

		// span_containing with null out-params (just need the index)
		let idx = montre_corpus_span_containing(
			corpus, sent.as_ptr(), 0, std::ptr::null_mut(), std::ptr::null_mut(),
		);
		assert_eq!(idx, 0);

		// token annotation: position 1 word = "chat"
		let word = cstr("word");
		let val = read_cstr(montre_corpus_token_annotation(corpus, 1, word.as_ptr()));
		assert_eq!(val, "chat");

		// token annotation: position 1 lemma = "chat"
		let lemma = cstr("lemma");
		let val = read_cstr(montre_corpus_token_annotation(corpus, 1, lemma.as_ptr()));
		assert_eq!(val, "chat");

		// token annotations (bulk): positions [0, 4) words
		let mut len: u64 = 0;
		let arr = montre_corpus_token_annotations(corpus, 0, 4, word.as_ptr(), &mut len);
		assert_eq!(len, 4);
		let words = read_string_array(arr, len);
		assert_eq!(words, vec!["Le", "chat", "dort", "."]);

		// span_text: sentence 0 by word
		let text = read_cstr(montre_corpus_span_text(corpus, 0, 4, word.as_ptr()));
		assert_eq!(text, "Le chat dort .");

		// inverted values: pos layer
		let pos = cstr("pos");
		let arr = montre_corpus_inverted_values(corpus, pos.as_ptr(), &mut len);
		let pos_values = read_string_array(arr, len);
		assert!(pos_values.contains(&"NOUN".to_string()));
		assert!(pos_values.contains(&"VERB".to_string()));
		assert!(pos_values.contains(&"DET".to_string()));
		assert!(pos_values.contains(&"ADJ".to_string()));

		// query: [pos="NOUN"] should match 2 (chat, maison)
		let q = cstr(r#"[pos="NOUN"]"#);
		let hits = montre_query(corpus, q.as_ptr());
		assert!(!hits.is_null());
		assert_eq!(montre_hitlist_len(hits), 2);

		// hit accessors
		let h0_start = montre_hit_start(hits, 0);
		let h0_end = montre_hit_end(hits, 0);
		assert_eq!(h0_end - h0_start, 1); // single token

		// populate context
		montre_hitlist_populate_context(hits, corpus);
		let doc_idx = montre_hit_document_index(hits, 0);
		assert_eq!(doc_idx, 0);
		let sent_idx_0 = montre_hit_sentence_index(hits, 0);
		let sent_idx_1 = montre_hit_sentence_index(hits, 1);
		assert!(sent_idx_0 != sent_idx_1); // two nouns in different sentences

		// hitlist_texts
		let arr = montre_hitlist_texts(hits, corpus, lemma.as_ptr(), &mut len);
		assert_eq!(len, 2);
		let lemmas = read_string_array(arr, len);
		assert!(lemmas.contains(&"chat".to_string()));
		assert!(lemmas.contains(&"maison".to_string()));

		// context_tokens: window=1 around the first noun hit
		let single_q = cstr(r#"[lemma="chat"]"#);
		let single_hits = montre_query(corpus, single_q.as_ptr());
		assert_eq!(montre_hitlist_len(single_hits), 1);

		let mut positions: *mut i32 = std::ptr::null_mut();
		let mut tokens: *mut *mut c_char = std::ptr::null_mut();
		let mut offsets: *mut u64 = std::ptr::null_mut();
		let mut ctx_len: u64 = 0;
		montre_context_tokens(
			single_hits, corpus, 1, word.as_ptr(),
			&mut positions, &mut tokens, &mut offsets, &mut ctx_len,
		);
		// window=1 around "chat" (pos 1): positions 0,1,2 = "Le","chat","dort"
		assert_eq!(ctx_len, 3);
		let ctx_words = read_string_array(tokens, ctx_len);
		assert_eq!(ctx_words, vec!["Le", "chat", "dort"]);
		montre_i32_array_free(positions, ctx_len);
		montre_u64_array_free(offsets, 2); // hitlist_len + 1

		// query_count
		let count = montre_query_count(corpus, q.as_ptr());
		assert_eq!(count, 2);

		// query_count: [pos="ADJ"] should be 1 (vieille)
		let adj_q = cstr(r#"[pos="ADJ"]"#);
		assert_eq!(montre_query_count(corpus, adj_q.as_ptr()), 1);

		// bulk hit field extraction: hits has 2 NOUN hits with context populated
		let mut bulk_len: u64 = 0;
		let starts = montre_hitlist_starts(hits, &mut bulk_len);
		assert_eq!(bulk_len, 2);
		let s0 = *starts.add(0);
		let s1 = *starts.add(1);
		assert_ne!(s0, s1); // two different positions
		montre_u64_array_free(starts, bulk_len);

		let ends = montre_hitlist_ends(hits, &mut bulk_len);
		assert_eq!(bulk_len, 2);
		// each NOUN hit is a single token, so end = start + 1
		assert_eq!(*ends.add(0), s0 + 1);
		assert_eq!(*ends.add(1), s1 + 1);
		montre_u64_array_free(ends, bulk_len);

		let doc_indices = montre_hitlist_document_indices(hits, &mut bulk_len);
		assert_eq!(bulk_len, 2);
		assert_eq!(*doc_indices.add(0), 0); // single-doc corpus
		assert_eq!(*doc_indices.add(1), 0);
		montre_u64_array_free(doc_indices, bulk_len);

		let sent_indices = montre_hitlist_sentence_indices(hits, &mut bulk_len);
		assert_eq!(bulk_len, 2);
		// two nouns in different sentences
		assert_ne!(*sent_indices.add(0), *sent_indices.add(1));
		montre_u64_array_free(sent_indices, bulk_len);

		montre_hitlist_free(single_hits);
		montre_hitlist_free(hits);
		montre_corpus_close(corpus);

		// cleanup
		let _ = std::fs::remove_dir_all(&input_dir);
		let _ = std::fs::remove_dir_all(&output_dir);
	}
}

// ===========================================================================
// Multi-component: build from manifest, components, alignments, projection
// ===========================================================================

#[test]
fn multi_component_lifecycle() {
	unsafe {
		let (base_dir, manifest_path) = write_multi_component_dirs();
		let output_dir = temp_path("corpus_multi");

		let manifest = cstr(manifest_path.to_str().unwrap());
		let output = cstr(output_dir.to_str().unwrap());

		// build from manifest
		let rc = montre_build_manifest(manifest.as_ptr(), output.as_ptr(), 0, 0);
		assert_eq!(rc, 1, "manifest build failed: {:?}", read_last_error());

		let corpus = montre_corpus_open(output.as_ptr());
		assert!(!corpus.is_null(), "open failed: {:?}", read_last_error());

		// 9 fr + 9 en = 18 tokens
		assert_eq!(montre_corpus_token_count(corpus), 18);

		// 2 components
		assert_eq!(montre_corpus_component_count(corpus), 2);

		// component names and languages (sorted: en, fr)
		let c0_name = read_cstr(montre_corpus_component_name(corpus, 0));
		let c1_name = read_cstr(montre_corpus_component_name(corpus, 1));
		let c0_lang = read_cstr(montre_corpus_component_language(corpus, 0));
		let c1_lang = read_cstr(montre_corpus_component_language(corpus, 1));
		// components are sorted alphabetically: "en" < "fr"
		assert_eq!(c0_name, "en");
		assert_eq!(c0_lang, "en");
		assert_eq!(c1_name, "fr");
		assert_eq!(c1_lang, "fr");

		// component document range
		let mut doc_start: u32 = 0;
		let mut doc_end: u32 = 0;
		let rc = montre_corpus_component_document_range(corpus, 0, &mut doc_start, &mut doc_end);
		assert_eq!(rc, 1);
		assert_eq!(doc_end - doc_start, 1); // 1 document in en component

		let rc = montre_corpus_component_document_range(corpus, 1, &mut doc_start, &mut doc_end);
		assert_eq!(rc, 1);
		assert_eq!(doc_end - doc_start, 1); // 1 document in fr component

		// component_for_document
		let comp_0 = montre_corpus_component_for_document(corpus, 0);
		let comp_1 = montre_corpus_component_for_document(corpus, 1);
		assert!(comp_0 >= 0);
		assert!(comp_1 >= 0);
		assert_ne!(comp_0, comp_1);

		// invalid document
		assert_eq!(montre_corpus_component_for_document(corpus, 99), -1);

		// component token counts
		let en_tokens = montre_corpus_component_token_count(corpus, 0);
		let fr_tokens = montre_corpus_component_token_count(corpus, 1);
		assert_eq!(en_tokens, 9); // 4 + 5
		assert_eq!(fr_tokens, 9); // 4 + 5
		assert_eq!(montre_corpus_component_token_count(corpus, 99), -1);

		// query in component
		let q = cstr(r#"[lemma="chat"]"#);
		let fr = cstr("fr");
		let en = cstr("en");
		let fr_hits = montre_query_in_component(corpus, q.as_ptr(), fr.as_ptr());
		let en_hits = montre_query_in_component(corpus, q.as_ptr(), en.as_ptr());
		assert_eq!(montre_hitlist_len(fr_hits), 1); // "chat" in French
		assert_eq!(montre_hitlist_len(en_hits), 0); // no "chat" in English

		let cat_q = cstr(r#"[lemma="cat"]"#);
		let en_cat = montre_query_in_component(corpus, cat_q.as_ptr(), en.as_ptr());
		assert_eq!(montre_hitlist_len(en_cat), 1);

		// query_count_in_component
		let noun_q = cstr(r#"[pos="NOUN"]"#);
		let fr_noun_count = montre_query_count_in_component(corpus, noun_q.as_ptr(), fr.as_ptr());
		let en_noun_count = montre_query_count_in_component(corpus, noun_q.as_ptr(), en.as_ptr());
		assert_eq!(fr_noun_count, 2); // chat, maison
		assert_eq!(en_noun_count, 2); // cat, house

		// alignment metadata
		assert_eq!(montre_corpus_alignment_count(corpus), 1);
		let align_name = read_cstr(montre_corpus_alignment_name(corpus, 0));
		assert_eq!(align_name, "sentence");
		let align = cstr("sentence");

		let source = read_cstr(montre_corpus_alignment_source(corpus, 0));
		let target = read_cstr(montre_corpus_alignment_target(corpus, 0));
		assert_eq!(source, "fr");
		assert_eq!(target, "en");

		assert!(montre_corpus_alignment_edge_count(corpus, 0) >= 2);

		let source_layer = read_cstr(montre_corpus_alignment_source_layer(corpus, 0));
		let target_layer = read_cstr(montre_corpus_alignment_target_layer(corpus, 0));
		assert_eq!(source_layer, "sentence");
		assert_eq!(target_layer, "sentence");

		let directed = montre_corpus_alignment_directed(corpus, 0);
		assert_eq!(directed, 1); // default is directed

		// invalid alignment index
		assert_eq!(montre_corpus_alignment_directed(corpus, 99), -1);

		// alignment edge access
		let mut edge_count: u64 = 0;
		let edges = montre_corpus_alignment_edges(corpus, align.as_ptr(), &mut edge_count);
		assert!(!edges.is_null());
		assert_eq!(edge_count, 2); // 2 sentence pairs
		// each edge is [src_doc, src_sent, tgt_doc, tgt_sent]
		let e0_src_doc = *edges.add(0);
		let e0_src_sent = *edges.add(1);
		let e0_tgt_doc = *edges.add(2);
		let e0_tgt_sent = *edges.add(3);
		// first edge maps sentence 0 → sentence 0
		assert_eq!(e0_src_sent, 0);
		assert_eq!(e0_tgt_sent, 0);
		// src and tgt docs should be 0 (first doc within each component)
		assert_eq!(e0_src_doc, 0);
		assert_eq!(e0_tgt_doc, 0);
		montre_u32_array_free(edges, edge_count * 4);

		// nonexistent alignment returns null
		let bad_align = cstr("nonexistent");
		let null_edges = montre_corpus_alignment_edges(corpus, bad_align.as_ptr(), &mut edge_count);
		assert!(null_edges.is_null());
		assert_eq!(edge_count, 0);

		// projection: French [lemma="chat"] → English sentence
		let mut unmapped: u64 = 0;
		let mut no_align: u64 = 0;
		let mut projected: u64 = 0;
		let target_hits = montre_project(
			corpus, fr_hits, align.as_ptr(),
			&mut unmapped, &mut no_align, &mut projected,
		);
		assert!(!target_hits.is_null(), "projection failed: {:?}", read_last_error());
		assert_eq!(projected, 1);
		assert_eq!(unmapped, 0);
		assert_eq!(montre_hitlist_len(target_hits), 1);

		// the projected hit should be the English sentence with "cat"
		let word = cstr("word");
		let mut len: u64 = 0;
		let arr = montre_hitlist_texts(target_hits, corpus, word.as_ptr(), &mut len);
		assert_eq!(len, 1);
		let texts = read_string_array(arr, len);
		assert!(texts[0].contains("cat"));

		montre_hitlist_free(target_hits);
		montre_hitlist_free(fr_hits);
		montre_hitlist_free(en_hits);
		montre_hitlist_free(en_cat);
		montre_corpus_close(corpus);

		let _ = std::fs::remove_dir_all(&base_dir);
		let _ = std::fs::remove_dir_all(&output_dir);
	}
}

// ===========================================================================
// Error handling
// ===========================================================================

#[test]
fn error_handling() {
	unsafe {
		// open nonexistent corpus
		let bad_path = cstr("/nonexistent/corpus");
		let corpus = montre_corpus_open(bad_path.as_ptr());
		assert!(corpus.is_null());
		let err = read_last_error();
		assert!(err.is_some());

		// null arguments
		assert_eq!(montre_corpus_token_count(std::ptr::null()), 0);
		assert_eq!(montre_hitlist_len(std::ptr::null()), 0);
		assert_eq!(montre_query_count(std::ptr::null(), std::ptr::null()), -1);

		// build with null args
		assert_eq!(montre_build_directory(
			std::ptr::null(), std::ptr::null(), std::ptr::null(), 0, 0,
		), 0);
	}
}

// ===========================================================================
// Helpers
// ===========================================================================

unsafe fn read_last_error() -> Option<String> {
	let p = montre_last_error();
	if p.is_null() {
		None
	} else {
		Some(CStr::from_ptr(p).to_str().unwrap().to_string())
	}
}
