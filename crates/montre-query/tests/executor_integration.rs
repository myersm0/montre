use std::io::Cursor;
use std::sync::atomic::{AtomicU32, Ordering};

use montre_build::builder::CorpusBuilder;
use montre_build::format::conllu::ConllUReader;
use montre_build::format::CorpusReader;
use montre_index::Corpus;

static TEST_COUNTER: AtomicU32 = AtomicU32::new(0);

fn test_corpus_path() -> std::path::PathBuf {
	let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
	let pid = std::process::id();
	std::env::temp_dir().join(format!("montre_test_{}_{}", pid, id))
}

fn build_corpus(conllu: &str) -> Corpus {
	let path = test_corpus_path();
	let mut reader = ConllUReader::new(Cursor::new(conllu));
	let sentences = reader.read_sentences().unwrap();
	let mut builder = CorpusBuilder::new("test");
	builder.add_document("test.conllu", sentences);
	builder.build(&path).unwrap();
	montre_index::open(&path).unwrap()
}

fn build_corpus_multi_doc(docs: &[(&str, &str)]) -> Corpus {
	let path = test_corpus_path();
	let mut builder = CorpusBuilder::new("test");
	for (name, conllu) in docs {
		let mut reader = ConllUReader::new(Cursor::new(*conllu));
		let sentences = reader.read_sentences().unwrap();
		builder.add_document(*name, sentences);
	}
	builder.build(&path).unwrap();
	montre_index::open(&path).unwrap()
}

fn query_spans(corpus: &Corpus, query_str: &str) -> Vec<(u64, u64)> {
	let parsed = montre_query::parse(query_str).unwrap();
	let plan = montre_query::planner::plan(&parsed).unwrap();
	let results = montre_query::executor::execute(&plan, corpus).unwrap();
	results.hits().iter().map(|h| (h.span.start, h.span.end)).collect()
}

fn query_count(corpus: &Corpus, query_str: &str) -> usize {
	let parsed = montre_query::parse(query_str).unwrap();
	let plan = montre_query::planner::plan(&parsed).unwrap();
	let results = montre_query::executor::execute(&plan, corpus).unwrap();
	results.len()
}

// ---- Test corpus ----
// Sentence 1 (pos 0-9):  The/DET quick/ADJ brown/ADJ fox/NOUN jumps/VERB over/ADP the/DET lazy/ADJ dog/NOUN ./PUNCT
// Sentence 2 (pos 10-15): Dogs/NOUN and/CCONJ cats/NOUN are/AUX pets/NOUN ./PUNCT
// Sentence 3 (pos 16-22): The/DET cat/NOUN sat/VERB on/ADP the/DET mat/NOUN ./PUNCT

const SAMPLE: &str = "\
# sent_id = 1
1\tThe\tthe\tDET\tDT\t_\t5\tdet\t_\t_
2\tquick\tquick\tADJ\tJJ\t_\t5\tamod\t_\t_
3\tbrown\tbrown\tADJ\tJJ\t_\t5\tamod\t_\t_
4\tfox\tfox\tNOUN\tNN\t_\t5\tnsubj\t_\t_
5\tjumps\tjump\tVERB\tVBZ\t_\t0\troot\t_\t_
6\tover\tover\tADP\tIN\t_\t9\tcase\t_\t_
7\tthe\tthe\tDET\tDT\t_\t9\tdet\t_\t_
8\tlazy\tlazy\tADJ\tJJ\t_\t9\tamod\t_\t_
9\tdog\tdog\tNOUN\tNN\t_\t5\tobl\t_\t_
10\t.\t.\tPUNCT\t.\t_\t5\tpunct\t_\t_

# sent_id = 2
1\tDogs\tdog\tNOUN\tNNS\t_\t5\tnsubj\t_\t_
2\tand\tand\tCCONJ\tCC\t_\t3\tcc\t_\t_
3\tcats\tcat\tNOUN\tNNS\t_\t1\tconj\t_\t_
4\tare\tbe\tAUX\tVBP\t_\t5\tcop\t_\t_
5\tpets\tpet\tNOUN\tNNS\t_\t0\troot\t_\t_
6\t.\t.\tPUNCT\t.\t_\t5\tpunct\t_\t_

# sent_id = 3
1\tThe\tthe\tDET\tDT\t_\t2\tdet\t_\t_
2\tcat\tcat\tNOUN\tNN\t_\t3\tnsubj\t_\t_
3\tsat\tsit\tVERB\tVBD\t_\t0\troot\t_\t_
4\ton\ton\tADP\tIN\t_\t6\tcase\t_\t_
5\tthe\tthe\tDET\tDT\t_\t6\tdet\t_\t_
6\tmat\tmat\tNOUN\tNN\t_\t3\tobl\t_\t_
7\t.\t.\tPUNCT\t.\t_\t3\tpunct\t_\t_
";

// ===========================================================================
// Single token queries
// ===========================================================================

#[test]
fn literal_match() {
	let corpus = build_corpus(SAMPLE);
	let spans = query_spans(&corpus, r#"[pos="ADJ"]"#);
	assert_eq!(spans, vec![(1, 2), (2, 3), (7, 8)]);
}

#[test]
fn literal_match_word() {
	let corpus = build_corpus(SAMPLE);
	let spans = query_spans(&corpus, r#"[word="fox"]"#);
	assert_eq!(spans, vec![(3, 4)]);
}

#[test]
fn literal_match_lemma() {
	let corpus = build_corpus(SAMPLE);
	// lemma "dog" appears at positions 8 (dog) and 10 (Dogs)
	let spans = query_spans(&corpus, r#"[lemma="dog"]"#);
	assert_eq!(spans, vec![(8, 9), (10, 11)]);
}

#[test]
fn quoted_word_shorthand() {
	let corpus = build_corpus(SAMPLE);
	let spans = query_spans(&corpus, r#""fox""#);
	assert_eq!(spans, vec![(3, 4)]);
}

#[test]
fn negation() {
	let corpus = build_corpus(SAMPLE);
	let count = query_count(&corpus, r#"[pos!="PUNCT"]"#);
	// 23 tokens total, 3 PUNCT (positions 9, 15, 22)
	assert_eq!(count, 20);
}

#[test]
fn matchall() {
	let corpus = build_corpus(SAMPLE);
	assert_eq!(query_count(&corpus, r#"[]"#), 23);
}

#[test]
fn conjunction() {
	let corpus = build_corpus(SAMPLE);
	// word="cat" AND pos="NOUN": position 17 only ("cats" at 12 has word="cats")
	let spans = query_spans(&corpus, r#"[word="cat" & pos="NOUN"]"#);
	assert_eq!(spans, vec![(17, 18)]);
}

#[test]
fn regex_match() {
	let corpus = build_corpus(SAMPLE);
	// lemma starting with 'c': cat (12, 17)
	let spans = query_spans(&corpus, r#"[lemma=/^c/]"#);
	assert_eq!(spans.len(), 2);
	assert!(spans.contains(&(12, 13)));
	assert!(spans.contains(&(17, 18)));
}

#[test]
fn no_matches() {
	let corpus = build_corpus(SAMPLE);
	let spans = query_spans(&corpus, r#"[word="elephant"]"#);
	assert!(spans.is_empty());
}

// ===========================================================================
// Sequences
// ===========================================================================

#[test]
fn two_token_sequence() {
	let corpus = build_corpus(SAMPLE);
	// ADJ NOUN: (2,4) = "brown fox", (7,9) = "lazy dog"
	let spans = query_spans(&corpus, r#"[pos="ADJ"] [pos="NOUN"]"#);
	assert_eq!(spans, vec![(2, 4), (7, 9)]);
}

#[test]
fn three_token_sequence() {
	let corpus = build_corpus(SAMPLE);
	// ADJ ADJ NOUN: only "quick brown fox" at (1, 4)
	let spans = query_spans(&corpus, r#"[pos="ADJ"] [pos="ADJ"] [pos="NOUN"]"#);
	assert_eq!(spans, vec![(1, 4)]);
}

#[test]
fn sequence_with_quoted_words() {
	let corpus = build_corpus(SAMPLE);
	let spans = query_spans(&corpus, r#""The" "cat""#);
	assert_eq!(spans, vec![(16, 18)]);
}

#[test]
fn sequence_det_noun() {
	let corpus = build_corpus(SAMPLE);
	// DET at {0, 6, 16, 20}. Following NOUNs: 16→17(cat), 20→21(mat)
	let spans = query_spans(&corpus, r#"[pos="DET"] [pos="NOUN"]"#);
	assert_eq!(spans, vec![(16, 18), (20, 22)]);
}

// ===========================================================================
// Quantifiers
// ===========================================================================

#[test]
fn quantifier_plus() {
	let corpus = build_corpus(SAMPLE);
	// ADJ+: runs of contiguous ADJs
	// positions 1,2 (contiguous run) → (1,2), (2,3), (1,3)
	// position 7 (single) → (7,8)
	let spans = query_spans(&corpus, r#"[pos="ADJ"]+"#);
	assert_eq!(spans, vec![(1, 2), (1, 3), (2, 3), (7, 8)]);
}

#[test]
fn quantifier_star() {
	let corpus = build_corpus(SAMPLE);
	// ADJ* alone: all ADJ spans plus... actually this is a standalone repetition.
	// min=0 means it can match zero tokens, but the executor filters out empty spans.
	// So effectively same as ADJ+ for non-zero matches.
	let count = query_count(&corpus, r#"[pos="ADJ"]*"#);
	assert_eq!(count, query_count(&corpus, r#"[pos="ADJ"]+"#));
}

#[test]
fn quantifier_optional() {
	let corpus = build_corpus(SAMPLE);
	// ADJ? NOUN: optional ADJ before NOUN
	// Without ADJ: all NOUNs at 3, 8, 10, 12, 14, 17, 21
	// With ADJ: (2,4)="brown fox", (7,9)="lazy dog"
	// So total = 7 bare NOUNs + 2 ADJ-NOUN = 9
	let count = query_count(&corpus, r#"[pos="ADJ"]? [pos="NOUN"]"#);
	assert_eq!(count, 9);
}

#[test]
fn quantifier_exact() {
	let corpus = build_corpus(SAMPLE);
	// ADJ{2}: exactly 2 contiguous ADJs → only (1, 3) = "quick brown"
	let spans = query_spans(&corpus, r#"[pos="ADJ"]{2}"#);
	assert_eq!(spans, vec![(1, 3)]);
}

#[test]
fn quantifier_range() {
	let corpus = build_corpus(SAMPLE);
	// ADJ{1,2}: 1 or 2 contiguous ADJs
	let spans = query_spans(&corpus, r#"[pos="ADJ"]{1,2}"#);
	assert_eq!(spans, vec![(1, 2), (1, 3), (2, 3), (7, 8)]);
}

#[test]
fn quantifier_min_only() {
	let corpus = build_corpus(SAMPLE);
	// ADJ{2,}: 2 or more. Only the run at 1-2 qualifies → (1, 3)
	let spans = query_spans(&corpus, r#"[pos="ADJ"]{2,}"#);
	assert_eq!(spans, vec![(1, 3)]);
}

#[test]
fn det_adj_star_noun() {
	let corpus = build_corpus(SAMPLE);
	// DET ADJ* NOUN: (0,4)="The quick brown fox", (6,9)="the lazy dog",
	// (16,18)="The cat", (20,22)="the mat"
	let spans = query_spans(&corpus, r#"[pos="DET"] [pos="ADJ"]* [pos="NOUN"]"#);
	assert_eq!(spans, vec![(0, 4), (6, 9), (16, 18), (20, 22)]);
}

#[test]
fn matchall_with_quantifier() {
	let corpus = build_corpus(SAMPLE);
	// []{2}: every pair of adjacent tokens
	let count = query_count(&corpus, r#"[]{2}"#);
	assert_eq!(count, 22); // positions 0-21, each starting a 2-token span
}

#[test]
fn sequence_with_matchall_gap() {
	let corpus = build_corpus(SAMPLE);
	// [pos="DET"] [] [pos="NOUN"]: DET, any token, NOUN
	// 0: DET, 1: ADJ, 2: ADJ (not NOUN) — no
	// 6: DET, 7: ADJ, 8: NOUN — yes → (6, 9)
	// 16: DET, 17: NOUN, 18: VERB — no (NOUN is at offset 1, not 2)
	// 20: DET, 21: NOUN, 22: PUNCT — no
	let spans = query_spans(&corpus, r#"[pos="DET"] [] [pos="NOUN"]"#);
	assert_eq!(spans, vec![(6, 9)]);
}

// ===========================================================================
// Alternation
// ===========================================================================

#[test]
fn simple_alternation() {
	let corpus = build_corpus(SAMPLE);
	// NOUN | VERB: union of both
	let count = query_count(&corpus, r#"[pos="NOUN"] | [pos="VERB"]"#);
	let noun_count = query_count(&corpus, r#"[pos="NOUN"]"#);
	let verb_count = query_count(&corpus, r#"[pos="VERB"]"#);
	assert_eq!(count, noun_count + verb_count);
}

#[test]
fn alternation_in_sequence() {
	let corpus = build_corpus(SAMPLE);
	// DET (ADJ | NOUN): DET followed by ADJ or NOUN
	// DET at 0: next is 1=ADJ → match (0, 2)
	// DET at 6: next is 7=ADJ → match (6, 8)
	// DET at 16: next is 17=NOUN → match (16, 18)
	// DET at 20: next is 21=NOUN → match (20, 22)
	let spans = query_spans(&corpus, r#"[pos="DET"] ([pos="ADJ"] | [pos="NOUN"])"#);
	assert_eq!(spans, vec![(0, 2), (6, 8), (16, 18), (20, 22)]);
}

#[test]
fn three_way_alternation() {
	let corpus = build_corpus(SAMPLE);
	let count = query_count(&corpus, r#"[pos="DET"] | [pos="ADJ"] | [pos="VERB"]"#);
	let det = query_count(&corpus, r#"[pos="DET"]"#);
	let adj = query_count(&corpus, r#"[pos="ADJ"]"#);
	let verb = query_count(&corpus, r#"[pos="VERB"]"#);
	assert_eq!(count, det + adj + verb);
}

// ===========================================================================
// Within constraints
// ===========================================================================

#[test]
fn within_sentence() {
	let corpus = build_corpus(SAMPLE);
	// NOUN that appears in any sentence (all do, trivially)
	let without = query_count(&corpus, r#"[pos="NOUN"]"#);
	let within = query_count(&corpus, r#"[pos="NOUN"] within s"#);
	assert_eq!(without, within);
}

#[test]
fn within_sentence_blocks_cross_boundary() {
	let corpus = build_corpus(SAMPLE);
	// Sentence 1 ends at position 10, sentence 2 starts at 10.
	// A span crossing the boundary should be filtered.
	// PUNCT followed by NOUN: (9,11) crosses s1→s2, (15,17) crosses s2→s3
	// Without within: both match
	// With within s: neither should match (they cross sentence boundaries)
	let without = query_count(&corpus, r#"[pos="PUNCT"] [pos="NOUN"]"#);
	let within = query_count(&corpus, r#"[pos="PUNCT"] [pos="NOUN"] within s"#);
	assert_eq!(without, 2);
	assert_eq!(within, 0);
}

#[test]
fn within_document() {
	let corpus = build_corpus(SAMPLE);
	let count = query_count(&corpus, r#"[pos="ADJ"] within doc"#);
	// Single document, so all ADJs pass
	assert_eq!(count, 3);
}

// ===========================================================================
// Multi-document
// ===========================================================================

const DOC_FR: &str = "\
1\tLe\tle\tDET\t_\t_\t2\tdet\t_\t_
2\tchat\tchat\tNOUN\t_\t_\t3\tnsubj\t_\t_
3\tdort\tdormir\tVERB\t_\t_\t0\troot\t_\t_
";

const DOC_EN: &str = "\
1\tThe\tthe\tDET\t_\t_\t2\tdet\t_\t_
2\tcat\tcat\tNOUN\t_\t_\t3\tnsubj\t_\t_
3\tsleeps\tsleep\tVERB\t_\t_\t0\troot\t_\t_
";

#[test]
fn multi_document_query() {
	let corpus = build_corpus_multi_doc(&[("fr.conllu", DOC_FR), ("en.conllu", DOC_EN)]);
	// 6 tokens total across 2 documents
	assert_eq!(corpus.token_count(), 6);
	let count = query_count(&corpus, r#"[pos="NOUN"]"#);
	assert_eq!(count, 2);
}

#[test]
fn multi_document_within_doc() {
	let corpus = build_corpus_multi_doc(&[("fr.conllu", DOC_FR), ("en.conllu", DOC_EN)]);
	// DET NOUN within doc: both (0,2) and (3,5) are within their docs
	let spans = query_spans(&corpus, r#"[pos="DET"] [pos="NOUN"] within doc"#);
	assert_eq!(spans, vec![(0, 2), (3, 5)]);
}

#[test]
fn sequence_cannot_cross_documents() {
	let corpus = build_corpus_multi_doc(&[("fr.conllu", DOC_FR), ("en.conllu", DOC_EN)]);
	// VERB DET: position 2=VERB(dort), 3=DET(The) — crosses doc boundary
	// Without within doc, the sequence engine still finds it because positions are contiguous
	let without = query_count(&corpus, r#"[pos="VERB"] [pos="DET"]"#);
	let within = query_count(&corpus, r#"[pos="VERB"] [pos="DET"] within doc"#);
	assert_eq!(without, 1);
	assert_eq!(within, 0);
}

// ===========================================================================
// Edge cases
// ===========================================================================

#[test]
fn single_token_corpus() {
	let conllu = "1\tHello\thello\tINTJ\tUH\t_\t0\troot\t_\t_\n";
	let corpus = build_corpus(conllu);
	assert_eq!(query_count(&corpus, r#"[]"#), 1);
	assert_eq!(query_count(&corpus, r#"[pos="INTJ"]"#), 1);
	assert_eq!(query_count(&corpus, r#"[pos="NOUN"]"#), 0);
}

#[test]
fn quantifier_on_empty_result() {
	let corpus = build_corpus(SAMPLE);
	// No PROPN in corpus
	assert_eq!(query_count(&corpus, r#"[pos="PROPN"]+"#), 0);
}

#[test]
fn sequence_where_first_step_empty() {
	let corpus = build_corpus(SAMPLE);
	assert_eq!(query_count(&corpus, r#"[word="zzz"] [pos="NOUN"]"#), 0);
}

#[test]
fn sequence_where_second_step_empty() {
	let corpus = build_corpus(SAMPLE);
	assert_eq!(query_count(&corpus, r#"[pos="NOUN"] [word="zzz"]"#), 0);
}

#[test]
fn regex_no_matches() {
	let corpus = build_corpus(SAMPLE);
	assert_eq!(query_count(&corpus, r#"[lemma=/^zzz/]"#), 0);
}

#[test]
fn negation_conjunction() {
	let corpus = build_corpus(SAMPLE);
	// NOUN but not lemma="dog"
	// NOUNs: fox(3), dog(8), Dogs(10), cats(12), pets(14), cat(17), mat(21) = 7
	// lemma="dog": 8, 10 → subtract 2 = 5
	let count = query_count(&corpus, r#"[pos="NOUN" & lemma!="dog"]"#);
	assert_eq!(count, 5);
}

// ===========================================================================
// Results API
// ===========================================================================

#[test]
fn results_populate_context() {
	let corpus = build_corpus(SAMPLE);
	let parsed = montre_query::parse(r#"[pos="VERB"]"#).unwrap();
	let plan = montre_query::planner::plan(&parsed).unwrap();
	let mut results = montre_query::executor::execute(&plan, &corpus).unwrap();
	results.populate_context(&corpus);

	let hits = results.hits();
	// VERB at position 4 (sentence 0, doc 0) and 18 (sentence 2, doc 0)
	assert_eq!(hits.len(), 2);
	assert_eq!(hits[0].sentence_index, 0);
	assert_eq!(hits[0].document_index, 0);
	assert_eq!(hits[1].sentence_index, 2);
	assert_eq!(hits[1].document_index, 0);
}

#[test]
fn results_into_iter() {
	let corpus = build_corpus(SAMPLE);
	let parsed = montre_query::parse(r#"[pos="VERB"]"#).unwrap();
	let plan = montre_query::planner::plan(&parsed).unwrap();
	let results = montre_query::executor::execute(&plan, &corpus).unwrap();

	let collected: Vec<_> = results.into_iter().collect();
	assert_eq!(collected.len(), 2);
}

#[test]
fn results_ref_iter() {
	let corpus = build_corpus(SAMPLE);
	let parsed = montre_query::parse(r#"[pos="VERB"]"#).unwrap();
	let plan = montre_query::planner::plan(&parsed).unwrap();
	let results = montre_query::executor::execute(&plan, &corpus).unwrap();

	let mut count = 0;
	for _hit in &results {
		count += 1;
	}
	assert_eq!(count, 2);
	// results is still usable
	assert_eq!(results.len(), 2);
}
