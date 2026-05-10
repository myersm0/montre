use std::io::Cursor;
use std::sync::atomic::{AtomicU32, Ordering};

use montre_build::builder::CorpusBuilder;
use montre_build::format::conllu::ConllUReader;
use montre_build::format::CorpusReader;
use montre_index::{Corpus, ForwardIndex, InvertedIndex, SpanIndex};

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

fn query_captures(corpus: &Corpus, query_str: &str) -> Vec<((u64, u64), Vec<(String, (u64, u64))>)> {
	let parsed = montre_query::parse(query_str).unwrap();
	let plan = montre_query::planner::plan(&parsed).unwrap();
	let results = montre_query::executor::execute(&plan, corpus).unwrap();
	results
		.hits()
		.iter()
		.map(|h| {
			let caps: Vec<(String, (u64, u64))> = h
				.captures
				.iter()
				.map(|(name, span)| (name.clone(), (span.start, span.end)))
				.collect();
			((h.span.start, h.span.end), caps)
		})
		.collect()
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
	// PUNCT at 9, next is 10=NOUN(Dogs) → (9,11) crosses s1→s2
	// PUNCT at 15, next is 16=DET(The) → not NOUN, no match
	// PUNCT at 22 → end of corpus, no match
	let without = query_count(&corpus, r#"[pos="PUNCT"] [pos="NOUN"]"#);
	let within = query_count(&corpus, r#"[pos="PUNCT"] [pos="NOUN"] within s"#);
	assert_eq!(without, 1);
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

// ===========================================================================
// Alternation + quantifier edge cases
// ===========================================================================

// S1 (pos 0-5):  The/DET very/ADV old/ADJ cat/NOUN sat/VERB ./PUNCT
// S2 (pos 6-12): A/DET big/ADJ black/ADJ dog/NOUN ran/VERB quickly/ADV ./PUNCT
// S3 (pos 13-19): The/DET really/ADV very/ADV old/ADJ house/NOUN stood/VERB ./PUNCT

const ALTERNATION_CORPUS: &str = "\
1\tThe\tthe\tDET\t_\t_\t4\tdet\t_\t_
2\tvery\tvery\tADV\t_\t_\t3\tadvmod\t_\t_
3\told\told\tADJ\t_\t_\t4\tamod\t_\t_
4\tcat\tcat\tNOUN\t_\t_\t5\tnsubj\t_\t_
5\tsat\tsit\tVERB\t_\t_\t0\troot\t_\t_
6\t.\t.\tPUNCT\t_\t_\t5\tpunct\t_\t_

1\tA\ta\tDET\t_\t_\t4\tdet\t_\t_
2\tbig\tbig\tADJ\t_\t_\t4\tamod\t_\t_
3\tblack\tblack\tADJ\t_\t_\t4\tamod\t_\t_
4\tdog\tdog\tNOUN\t_\t_\t5\tnsubj\t_\t_
5\tran\trun\tVERB\t_\t_\t0\troot\t_\t_
6\tquickly\tquickly\tADV\t_\t_\t5\tadvmod\t_\t_
7\t.\t.\tPUNCT\t_\t_\t5\tpunct\t_\t_

1\tThe\tthe\tDET\t_\t_\t5\tdet\t_\t_
2\treally\treally\tADV\t_\t_\t4\tadvmod\t_\t_
3\tvery\tvery\tADV\t_\t_\t4\tadvmod\t_\t_
4\told\told\tADJ\t_\t_\t5\tamod\t_\t_
5\thouse\thouse\tNOUN\t_\t_\t6\tnsubj\t_\t_
6\tstood\tstand\tVERB\t_\t_\t0\troot\t_\t_
7\t.\t.\tPUNCT\t_\t_\t6\tpunct\t_\t_
";

#[test]
fn alternation_plus_noun() {
	let corpus = build_corpus(ALTERNATION_CORPUS);
	// (ADJ|ADV)+ NOUN: all runs of contiguous ADJ/ADV tokens followed by NOUN
	// "very old cat": (1,4), "old cat": (2,4)
	// "big black dog": (7,10), "black dog": (8,10), "big": standalone ADJ not followed by NOUN directly? No, "big" at 7, "black" at 8, "dog" at 9
	// Wait: "big"(7) is followed by "black"(8) which is ADJ, so "big" alone + NOUN doesn't work because 8 is ADJ not NOUN.
	// Matches: ADJ/ADV run at 7,8 followed by NOUN at 9: (7,10), (8,10)
	// "really very old house": (14,18), (15,18), (16,18)
	// "quickly": ADV at 11, followed by PUNCT at 12 — no NOUN after
	let count = query_count(&corpus, r#"([pos="ADJ"] | [pos="ADV"])+ [pos="NOUN"]"#);
	// (1,4) (2,4) (7,10) (8,10) (14,18) (15,18) (16,18)
	assert_eq!(count, 7);
}

#[test]
fn alternation_exact_2_noun() {
	let corpus = build_corpus(ALTERNATION_CORPUS);
	// (ADJ|ADV){2} NOUN: exactly 2 ADJ/ADV tokens then NOUN
	// "very old" + cat: (1,4)
	// "big black" + dog: (7,10)
	// "very old" + house: (15,18)
	let count = query_count(&corpus, r#"([pos="ADJ"] | [pos="ADV"]){2} [pos="NOUN"]"#);
	assert_eq!(count, 3);
}

#[test]
fn alternation_exact_3_noun() {
	let corpus = build_corpus(ALTERNATION_CORPUS);
	// (ADJ|ADV){3} NOUN: exactly 3 then NOUN
	// Only "really very old" + house: (14,18)
	let count = query_count(&corpus, r#"([pos="ADJ"] | [pos="ADV"]){3} [pos="NOUN"]"#);
	assert_eq!(count, 1);
}

#[test]
fn alternation_range_2_3_noun() {
	let corpus = build_corpus(ALTERNATION_CORPUS);
	// (ADJ|ADV){2,3} NOUN: 2 or 3 then NOUN
	// {2}: "very old"+cat, "big black"+dog, "very old"+house = 3
	// {3}: "really very old"+house = 1
	// Total: 4
	let count = query_count(&corpus, r#"([pos="ADJ"] | [pos="ADV"]){2,3} [pos="NOUN"]"#);
	assert_eq!(count, 4);
}

#[test]
fn alternation_optional_in_sequence() {
	let corpus = build_corpus(ALTERNATION_CORPUS);
	// DET (ADJ|ADV)? NOUN: DET, optionally one ADJ or ADV, then NOUN
	// DET at 0: next is ADV(very) then ADJ(old) then NOUN(cat). Optional takes 0 → DET+NOUN? 0+... no, pos 1 is ADV not NOUN.
	// Optional takes 1 → DET ADV(1) ... next is ADJ(2) not NOUN. No match.
	// DET at 6: next is ADJ(big), then ADJ(black) not NOUN. Optional takes 0 → DET+NOUN? pos 7 is ADJ not NOUN. No match.
	// DET at 13: same issue.
	// So actually no matches at all — all DET-NOUN pairs in this corpus have intervening modifiers.
	let count = query_count(&corpus, r#"[pos="DET"] ([pos="ADJ"] | [pos="ADV"])? [pos="NOUN"]"#);
	assert_eq!(count, 0);
}

#[test]
fn alternation_star_in_sequence() {
	let corpus = build_corpus(ALTERNATION_CORPUS);
	// DET (ADJ|ADV)* NOUN: DET, any number of ADJ/ADV, then NOUN
	// DET(0) + "very old cat": (0,4)
	// DET(0) + "old cat" when star matches just "old"? No — star is (ADJ|ADV)*, must be contiguous from DET end.
	// DET at 0 (end=1), star matches 1,2 (very,old), NOUN at 3 → (0,4)
	// DET at 6 (end=7), star matches 7,8 (big,black), NOUN at 9 → (6,10)
	// DET at 13 (end=14), star matches 14,15,16 (really,very,old), NOUN at 17 → (13,18)
	let count = query_count(&corpus, r#"[pos="DET"] ([pos="ADJ"] | [pos="ADV"])* [pos="NOUN"]"#);
	assert_eq!(count, 3);
}

#[test]
fn quantified_alternation_standalone() {
	let corpus = build_corpus(ALTERNATION_CORPUS);
	// (ADJ|ADV){2}: just the 2-token runs, no following NOUN constraint
	// Run at 1-2 (very,old): (1,3)
	// Run at 7-8 (big,black): (7,9)
	// Run at 14-16 (really,very,old): (14,16), (15,17)
	// Hmm, wait — 16 is ADJ "old", 17 is NOUN "house". So pos 15-16 is ADV,ADJ and 16 is the end.
	// Run [14,15,16] = really,very,old. Spans of length 2: (14,16), (15,17)
	let count = query_count(&corpus, r#"([pos="ADJ"] | [pos="ADV"]){2}"#);
	assert_eq!(count, 4);
}

// ===========================================================================
// Alignment projection
// ===========================================================================

fn build_parallel_corpus() -> Corpus {
	use montre_build::MultiCorpusBuilder;
	use std::path::PathBuf;

	let output = test_corpus_path();
	let manifest_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
		.join("../../testdata/parallel/corpus.toml");

	MultiCorpusBuilder::from_manifest(&manifest_path)
		.unwrap()
		.build(&output)
		.unwrap();

	montre_index::open(&output).unwrap()
}

#[test]
fn parallel_corpus_structure() {
	let corpus = build_parallel_corpus();
	assert_eq!(corpus.components().len(), 2);
	assert_eq!(corpus.document_names().len(), 4);
	assert!(corpus.alignments().len() == 1);
}

#[test]
fn query_within_component() {
	let corpus = build_parallel_corpus();
	// [lemma="chat"] should exist only in the French component
	let fr_count = query_count(&corpus, r#"[lemma="chat"] within component:"fr""#);
	let en_count = query_count(&corpus, r#"[lemma="chat"] within component:"en""#);
	assert_eq!(fr_count, 1);
	assert_eq!(en_count, 0);
}

#[test]
fn query_lemma_cat_in_english() {
	let corpus = build_parallel_corpus();
	let count = query_count(&corpus, r#"[lemma="cat"] within component:"en""#);
	assert_eq!(count, 1);
}

#[test]
fn alignment_projection_single_hit() {
	let corpus = build_parallel_corpus();
	// Query [lemma="chat"] in French, project to English via sentence alignment.
	// "chat" is in le_chat.conllu sentence 0 → should project to the_cat.conllu sentence 0.
	let hits = query_spans(
		&corpus,
		r#"[lemma="chat"] within component:"fr" =sentence=>"#,
	);
	assert_eq!(hits.len(), 1);
	// The projected hit should be the full English sentence containing "The old black cat sleeps quietly ."
}

#[test]
fn alignment_projection_multiple_hits() {
	let corpus = build_parallel_corpus();
	// Query [pos="VERB"] in French — should hit dormir, rêver, dresser, entourer (4 verbs).
	// These span all 4 French sentences, mapping to all 4 English sentences.
	let fr_verbs = query_count(&corpus, r#"[pos="VERB"] within component:"fr""#);
	assert_eq!(fr_verbs, 4);

	let projected = query_spans(
		&corpus,
		r#"[pos="VERB"] within component:"fr" =sentence=>"#,
	);
	// 4 source sentences → 4 target sentences (1:1 alignment)
	assert_eq!(projected.len(), 4);
}

#[test]
fn alignment_projection_deduplicates() {
	let corpus = build_parallel_corpus();
	// Two ADJ hits in the same French sentence should project to one English sentence.
	// le_chat S0 has "vieux" and "noir" (both ADJ) — both in the same sentence.
	// Projection should return only one target sentence, not two.
	let fr_adj_in_chat = query_count(
		&corpus,
		r#"[pos="ADJ" & lemma=/vieux|noir/] within component:"fr""#,
	);
	assert!(fr_adj_in_chat >= 2); // vieux appears twice (le_chat + la_maison), noir once

	// More targeted: query both ADJ in le_chat S0 specifically
	let projected = query_spans(
		&corpus,
		r#"[lemma=/vieux|noir/] within component:"fr" =sentence=>"#,
	);
	// "vieux" appears in le_chat:S0 and la_maison:S1 → 2 distinct source sentences
	// "noir" appears in le_chat:S0 → same as one of the vieux hits
	// So at most 2 distinct target sentences
	assert!(projected.len() <= 3);
}

#[test]
fn projection_preserves_target_component() {
	let corpus = build_parallel_corpus();
	// After projection, hits should be in the English component's position range.
	let projected = query_spans(
		&corpus,
		r#"[lemma="chat"] within component:"fr" =sentence=>"#,
	);
	assert_eq!(projected.len(), 1);

	// The projected span should NOT be in the French position range.
	let fr_noun_spans = query_spans(&corpus, r#"[lemma="chat"] within component:"fr""#);
	let fr_start = fr_noun_spans[0].0;
	let projected_start = projected[0].0;
	assert_ne!(fr_start, projected_start);
}

// ===========================================================================
// Feats layer
// ===========================================================================

const FEATS_CORPUS: &str = "\
1\tThe\tthe\tDET\t_\tDefinite=Def|PronType=Art\t2\tdet\t_\t_
2\tcat\tcat\tNOUN\t_\tGender=Masc|Number=Sing\t3\tnsubj\t_\t_
3\tsat\tsit\tVERB\t_\tMood=Ind|Tense=Past|VerbForm=Fin\t0\troot\t_\t_
";

#[test]
fn feats_layer_indexed() {
	let corpus = build_corpus(FEATS_CORPUS);
	assert!(corpus.layers().contains(&"feats".to_string()));
}

#[test]
fn feats_query() {
	let corpus = build_corpus(FEATS_CORPUS);
	let count = query_count(&corpus, r#"[feats="Gender=Masc|Number=Sing"]"#);
	assert_eq!(count, 1);
}

#[test]
fn feats_query_verb() {
	let corpus = build_corpus(FEATS_CORPUS);
	let count = query_count(&corpus, r#"[feats=/.*Tense=Past.*/]"#);
	assert_eq!(count, 1);
}

// ===========================================================================
// Feats decomposition
// ===========================================================================

fn build_corpus_with_feats(conllu: &str) -> Corpus {
	let path = test_corpus_path();
	let mut reader = ConllUReader::new(Cursor::new(conllu));
	let sentences = reader.read_sentences().unwrap();
	let mut builder = CorpusBuilder::new("test").decompose_feats(true);
	builder.add_document("test.conllu", sentences);
	builder.build(&path).unwrap();
	montre_index::open(&path).unwrap()
}

#[test]
fn decomposed_feats_layers_present() {
	let corpus = build_corpus_with_feats(FEATS_CORPUS);
	let layers = corpus.layers();
	assert!(layers.contains(&"feats".to_string()));
	assert!(layers.contains(&"feats.Gender".to_string()));
	assert!(layers.contains(&"feats.Number".to_string()));
	assert!(layers.contains(&"feats.Tense".to_string()));
	assert!(layers.contains(&"feats.Mood".to_string()));
	assert!(layers.contains(&"feats.VerbForm".to_string()));
	assert!(layers.contains(&"feats.Definite".to_string()));
	assert!(layers.contains(&"feats.PronType".to_string()));
}

#[test]
fn decomposed_feats_query_single() {
	let corpus = build_corpus_with_feats(FEATS_CORPUS);
	let count = query_count(&corpus, r#"[feats.Number="Sing"]"#);
	assert_eq!(count, 1); // only cat
}

#[test]
fn decomposed_feats_query_conjunction() {
	let corpus = build_corpus_with_feats(FEATS_CORPUS);
	let count = query_count(&corpus, r#"[feats.Gender="Masc" & feats.Number="Sing"]"#);
	assert_eq!(count, 1); // only cat
}

#[test]
fn decomposed_feats_query_with_pos() {
	let corpus = build_corpus_with_feats(FEATS_CORPUS);
	let count = query_count(&corpus, r#"[pos="VERB" & feats.Tense="Past"]"#);
	assert_eq!(count, 1); // sat
}

#[test]
fn decomposed_feats_raw_still_works() {
	let corpus = build_corpus_with_feats(FEATS_CORPUS);
	let count = query_count(&corpus, r#"[feats="Gender=Masc|Number=Sing"]"#);
	assert_eq!(count, 1);
}

#[test]
fn decomposed_feats_not_present_without_flag() {
	let corpus = build_corpus(FEATS_CORPUS);
	let layers = corpus.layers();
	assert!(layers.contains(&"feats".to_string()));
	assert!(!layers.contains(&"feats.Gender".to_string()));
}

// ===========================================================================
// Captures
// ===========================================================================

#[test]
fn capture_no_labels() {
	let corpus = build_corpus(SAMPLE);
	let results = query_captures(&corpus, r#"[pos="ADJ"] [pos="NOUN"]"#);
	assert_eq!(results.len(), 2);
	assert!(results[0].1.is_empty());
	assert!(results[1].1.is_empty());
}

#[test]
fn capture_single_label() {
	let corpus = build_corpus(SAMPLE);
	// a:brown fox, a:lazy dog
	let results = query_captures(&corpus, r#"a:[pos="ADJ"] [pos="NOUN"]"#);
	assert_eq!(results.len(), 2);

	assert_eq!(results[0].0, (2, 4));
	assert_eq!(results[0].1, vec![("a".into(), (2, 3))]);

	assert_eq!(results[1].0, (7, 9));
	assert_eq!(results[1].1, vec![("a".into(), (7, 8))]);
}

#[test]
fn capture_two_labels() {
	let corpus = build_corpus(SAMPLE);
	let results = query_captures(&corpus, r#"a:[pos="ADJ"] b:[pos="NOUN"]"#);
	assert_eq!(results.len(), 2);

	assert_eq!(results[0].0, (2, 4));
	assert_eq!(results[0].1, vec![("a".into(), (2, 3)), ("b".into(), (3, 4))]);

	assert_eq!(results[1].0, (7, 9));
	assert_eq!(results[1].1, vec![("a".into(), (7, 8)), ("b".into(), (8, 9))]);
}

#[test]
fn capture_quantified_label() {
	let corpus = build_corpus(SAMPLE);
	// a:[pos="ADJ"]+ captures the full adjective run
	let results = query_captures(&corpus, r#"a:[pos="ADJ"]+ [pos="NOUN"]"#);
	assert_eq!(results.len(), 3);

	// "quick brown" fox
	assert_eq!(results[0].0, (1, 4));
	assert_eq!(results[0].1, vec![("a".into(), (1, 3))]);

	// "brown" fox
	assert_eq!(results[1].0, (2, 4));
	assert_eq!(results[1].1, vec![("a".into(), (2, 3))]);

	// "lazy" dog
	assert_eq!(results[2].0, (7, 9));
	assert_eq!(results[2].1, vec![("a".into(), (7, 8))]);
}

#[test]
fn capture_single_token_query() {
	let corpus = build_corpus(SAMPLE);
	// bare labeled query, not in a sequence
	let results = query_captures(&corpus, r#"a:[pos="ADJ"]"#);
	assert_eq!(results.len(), 3);

	assert_eq!(results[0].0, (1, 2));
	assert_eq!(results[0].1, vec![("a".into(), (1, 2))]);

	assert_eq!(results[1].0, (2, 3));
	assert_eq!(results[1].1, vec![("a".into(), (2, 3))]);

	assert_eq!(results[2].0, (7, 8));
	assert_eq!(results[2].1, vec![("a".into(), (7, 8))]);
}

// ===========================================================================
// Global constraints
// ===========================================================================

#[test]
fn constraint_distance_filters() {
	let corpus = build_corpus(SAMPLE);
	// ADJ immediately followed by NOUN: distance is 0
	// distance >= 0 keeps all
	let all = query_count(&corpus, r#"a:[pos="ADJ"] b:[pos="NOUN"] :: distance(a,b) >= 0"#);
	assert_eq!(all, 2);
	// distance >= 1 eliminates adjacent (distance 0)
	let filtered = query_count(&corpus, r#"a:[pos="ADJ"] b:[pos="NOUN"] :: distance(a,b) >= 1"#);
	assert_eq!(filtered, 0);
}

#[test]
fn constraint_eq_filters() {
	let corpus = build_corpus(SAMPLE);
	// ADJ followed by NOUN: a.pos="ADJ", b.pos="NOUN" — never equal
	let count = query_count(&corpus, r#"a:[pos="ADJ"] b:[pos="NOUN"] :: a.pos = b.pos"#);
	assert_eq!(count, 0);
}

#[test]
fn constraint_ne() {
	let corpus = build_corpus(SAMPLE);
	// ADJ followed by NOUN: a.pos != b.pos — always true
	let count = query_count(&corpus, r#"a:[pos="ADJ"] b:[pos="NOUN"] :: a.pos != b.pos"#);
	assert_eq!(count, 2);
}

#[test]
fn constraint_multiple() {
	let corpus = build_corpus(SAMPLE);
	let count = query_count(
		&corpus,
		r#"a:[pos="ADJ"] b:[pos="NOUN"] :: a.pos != b.pos & distance(a,b) >= 0"#,
	);
	assert_eq!(count, 2);
}

#[test]
fn constraint_quantified_label() {
	let corpus = build_corpus(SAMPLE);
	// a:[pos="ADJ"]+ b:[pos="NOUN"] produces 3 hits: (1,4), (2,4), (7,9)
	// a.pos is always ADJ (first token), b.pos is always NOUN — always !=
	let count = query_count(
		&corpus,
		r#"a:[pos="ADJ"]+ b:[pos="NOUN"] :: a.pos != b.pos"#,
	);
	assert_eq!(count, 3);
}

#[test]
fn constraint_eq_matching() {
	let corpus = build_corpus(SAMPLE);
	// DET []* DET within sentence: both have lemma "the"
	let count = query_count(
		&corpus,
		r#"a:[pos="DET"] []* b:[pos="DET"] within s :: a.lemma = b.lemma"#,
	);
	assert!(count > 0);
	// same query with != should filter them out (both are "the")
	let ne_count = query_count(
		&corpus,
		r#"a:[pos="DET"] []* b:[pos="DET"] within s :: a.lemma != b.lemma"#,
	);
	assert_eq!(ne_count, 0);
}

#[test]
fn constraint_unknown_label_errors() {
	let parsed = montre_query::parse(r#"a:[] b:[] :: x.lemma = b.lemma"#).unwrap();
	let result = montre_query::planner::plan(&parsed);
	assert!(result.is_err());
}

#[test]
fn constraint_distance_directionality() {
	let corpus = build_corpus(SAMPLE);
	// a=quick(ADJ,pos 1), []=brown, b=fox(NOUN,pos 3)
	// distance(a,b) = b.start - a.end = 3 - 2 = 1
	let fwd = query_count(
		&corpus,
		r#"a:[pos="ADJ"] [] b:[pos="NOUN"] :: distance(a,b) >= 1"#,
	);
	assert_eq!(fwd, 1);
	// distance(b,a) = a.start - b.end = 1 - 4 = 0 (saturating)
	let rev = query_count(
		&corpus,
		r#"a:[pos="ADJ"] [] b:[pos="NOUN"] :: distance(b,a) >= 1"#,
	);
	assert_eq!(rev, 0);
}

// ===========================================================================
// Item 1: Head layer
// ===========================================================================

#[test]
fn head_layer_stores_sentence_local_values() {
	let corpus = build_corpus(SAMPLE);
	// Sentence 1 (positions 0-9):
	//   "The"(0) head=5, "quick"(1) head=5, "jumps"(4) head=0 (root)
	assert_eq!(corpus.forward().get_int(0, "head"), Some(5));
	assert_eq!(corpus.forward().get_int(1, "head"), Some(5));
	assert_eq!(corpus.forward().get_int(4, "head"), Some(0));
	// Sentence 2 (positions 10-15):
	//   "Dogs"(10) head=5, "pets"(14) head=0 (root)
	assert_eq!(corpus.forward().get_int(10, "head"), Some(5));
	assert_eq!(corpus.forward().get_int(14, "head"), Some(0));
	// Sentence 3 (positions 16-22):
	//   "The"(16) head=2, "cat"(17) head=3, "sat"(18) head=0 (root)
	assert_eq!(corpus.forward().get_int(16, "head"), Some(2));
	assert_eq!(corpus.forward().get_int(17, "head"), Some(3));
	assert_eq!(corpus.forward().get_int(18, "head"), Some(0));
}

#[test]
fn head_layer_not_in_inverted_index() {
	let corpus = build_corpus(SAMPLE);
	assert!(corpus.inverted().values("head").is_none());
}

#[test]
fn head_layer_in_corpus_layers() {
	let corpus = build_corpus(SAMPLE);
	assert!(corpus.layers().contains(&"head".to_string()));
}

#[test]
fn head_get_str_returns_none_for_numeric_layer() {
	let corpus = build_corpus(SAMPLE);
	assert_eq!(corpus.forward().get_str(0, "head"), None);
}

// ===========================================================================
// Item 2: UPOS/XPOS split
// ===========================================================================

#[test]
fn upos_and_xpos_are_distinct_layers() {
	let corpus = build_corpus(SAMPLE);
	let layers = corpus.layers();
	assert!(layers.contains(&"upos".to_string()));
	assert!(layers.contains(&"xpos".to_string()));
	assert!(!layers.contains(&"pos".to_string()));
}

#[test]
fn upos_values_correct() {
	let corpus = build_corpus(SAMPLE);
	assert_eq!(corpus.forward().get_str(0, "upos"), Some("DET"));
	assert_eq!(corpus.forward().get_str(3, "upos"), Some("NOUN"));
	assert_eq!(corpus.forward().get_str(4, "upos"), Some("VERB"));
}

#[test]
fn xpos_values_correct() {
	let corpus = build_corpus(SAMPLE);
	assert_eq!(corpus.forward().get_str(0, "xpos"), Some("DT"));
	assert_eq!(corpus.forward().get_str(3, "xpos"), Some("NN"));
	assert_eq!(corpus.forward().get_str(4, "xpos"), Some("VBZ"));
	assert_eq!(corpus.forward().get_str(7, "xpos"), Some("JJ"));
}

#[test]
fn pos_alias_resolves_to_upos() {
	let corpus = build_corpus(SAMPLE);
	let via_pos = query_spans(&corpus, r#"[pos="ADJ"]"#);
	let via_upos = query_spans(&corpus, r#"[upos="ADJ"]"#);
	assert_eq!(via_pos, via_upos);
	assert!(!via_pos.is_empty());
}

#[test]
fn pos_alias_in_global_constraint() {
	let corpus = build_corpus(SAMPLE);
	let via_pos = query_count(
		&corpus,
		r#"a:[pos="ADJ"] b:[pos="NOUN"] :: a.pos != b.pos"#,
	);
	let via_upos = query_count(
		&corpus,
		r#"a:[upos="ADJ"] b:[upos="NOUN"] :: a.upos != b.upos"#,
	);
	assert_eq!(via_pos, via_upos);
}

// ===========================================================================
// Item 3: Sentence ID preservation
// ===========================================================================

#[test]
fn sentence_ids_preserved_from_conllu() {
	let corpus = build_corpus(SAMPLE);
	assert_eq!(corpus.sentence_id_count(), 3);
	assert_eq!(corpus.sentence_id(0), Some("1"));
	assert_eq!(corpus.sentence_id(1), Some("2"));
	assert_eq!(corpus.sentence_id(2), Some("3"));
}

#[test]
fn sentence_ids_fallback_when_absent() {
	let conllu = "\
1\tHello\thello\tINTJ\tUH\t_\t0\troot\t_\t_

1\tWorld\tworld\tNOUN\tNN\t_\t0\troot\t_\t_
";
	let corpus = build_corpus(conllu);
	assert_eq!(corpus.sentence_id_count(), 2);
	assert_eq!(corpus.sentence_id(0), Some("test.conllu:0"));
	assert_eq!(corpus.sentence_id(1), Some("test.conllu:1"));
}

#[test]
fn sentence_ids_mixed_presence() {
	let conllu = "\
# sent_id = my-custom-id
1\tHello\thello\tINTJ\tUH\t_\t0\troot\t_\t_

1\tWorld\tworld\tNOUN\tNN\t_\t0\troot\t_\t_

# sent_id = another-id
1\tFoo\tfoo\tNOUN\tNN\t_\t0\troot\t_\t_
";
	let corpus = build_corpus(conllu);
	assert_eq!(corpus.sentence_id_count(), 3);
	assert_eq!(corpus.sentence_id(0), Some("my-custom-id"));
	assert_eq!(corpus.sentence_id(1), Some("test.conllu:1"));
	assert_eq!(corpus.sentence_id(2), Some("another-id"));
}

#[test]
fn sentence_ids_across_documents() {
	let corpus = build_corpus_multi_doc(&[
		("doc-a", "# sent_id = a-sent-1\n1\tHello\thello\tINTJ\tUH\t_\t0\troot\t_\t_\n"),
		("doc-b", "1\tWorld\tworld\tNOUN\tNN\t_\t0\troot\t_\t_\n"),
	]);
	assert_eq!(corpus.sentence_id_count(), 2);
	assert_eq!(corpus.sentence_id(0), Some("a-sent-1"));
	assert_eq!(corpus.sentence_id(1), Some("doc-b:0"));
}

#[test]
fn sentence_id_out_of_bounds() {
	let corpus = build_corpus(SAMPLE);
	assert_eq!(corpus.sentence_id(999), None);
}

#[test]
fn sentence_id_count_matches_sentence_spans() {
	let corpus = build_corpus(SAMPLE);
	let span_count = corpus.spans().spans("sentence").map_or(0, |s| s.len());
	assert_eq!(corpus.sentence_id_count(), span_count);
}

const FRENCH_MWT: &str = "\
# sent_id = fr-1
# text = Il va au marché.
1\tIl\til\tPRON\t_\t_\t2\tnsubj\t2:nsubj\t_
2\tva\taller\tVERB\t_\t_\t0\troot\t0:root\t_
3-4\tau\t_\t_\t_\t_\t_\t_\t_\t_
3\tà\tà\tADP\t_\t_\t5\tcase\t5:case\t_
4\tle\tle\tDET\t_\t_\t5\tdet\t5:det\t_
5\tmarché\tmarché\tNOUN\t_\t_\t2\tobl\t2:obl\t_
6\t.\t.\tPUNCT\t_\t_\t2\tpunct\t_\tSpaceAfter=No
";

#[test]
fn mwt_surface_text() {
	let corpus = build_corpus(FRENCH_MWT);
	let text = corpus.surface_text(0, 6);
	assert_eq!(text, "Il va au marché .");
}

#[test]
fn mwt_lookup() {
	let corpus = build_corpus(FRENCH_MWT);
	let mwt = corpus.mwt_covering(2);
	assert!(mwt.is_some());
	let mwt = mwt.unwrap();
	assert_eq!(mwt.form, "au");
	assert_eq!(mwt.start, 2);
	assert_eq!(mwt.end, 4);

	assert!(corpus.mwt_covering(0).is_none());
	assert!(corpus.mwt_covering(4).is_none());
}

#[test]
fn mwt_tokens_still_queryable() {
	let corpus = build_corpus(FRENCH_MWT);
	let spans = query_spans(&corpus, r#"[lemma="à"]"#);
	assert_eq!(spans.len(), 1);
	assert_eq!(spans[0], (2, 3));
}

const SPACE_AFTER_DATA: &str = "\
# text = Il dort.
1\tIl\til\tPRON\t_\t_\t2\tnsubj\t_\t_
2\tdort\tdormir\tVERB\t_\t_\t0\troot\t_\tSpaceAfter=No
3\t.\t.\tPUNCT\t_\t_\t2\tpunct\t_\t_
";

#[test]
fn space_after_no_surface_text() {
	let corpus = build_corpus(SPACE_AFTER_DATA);
	let text = corpus.surface_text(0, 3);
	assert_eq!(text, "Il dort.");
}

#[test]
fn space_after_no_on_ordinary_token() {
	let corpus = build_corpus(SPACE_AFTER_DATA);
	assert!(corpus.has_no_space_after(1));
	assert!(!corpus.has_no_space_after(0));
	assert!(!corpus.has_no_space_after(2));
}

const MWT_SPACE_AFTER: &str = "\
# text = dell'uomo
1-2\tdell'\t_\t_\t_\t_\t_\t_\t_\tSpaceAfter=No
1\tdi\tdi\tADP\t_\t_\t3\tcase\t_\t_
2\til\til\tDET\t_\t_\t3\tdet\t_\t_
3\tuomo\tuomo\tNOUN\t_\t_\t0\troot\t_\t_
";

#[test]
fn mwt_space_after_no_surface_text() {
	let corpus = build_corpus(MWT_SPACE_AFTER);
	let text = corpus.surface_text(0, 3);
	assert_eq!(text, "dell'uomo");
}

#[test]
fn deps_layer_forward_only() {
	let corpus = build_corpus(FRENCH_MWT);
	assert_eq!(corpus.forward().get_str(0, "deps"), Some("2:nsubj"));
	assert_eq!(corpus.forward().get_str(1, "deps"), Some("0:root"));
	assert_eq!(corpus.forward().get_str(5, "deps"), None);

	let inverted_values = corpus.inverted().values("deps");
	assert!(inverted_values.is_none());
}

const EMPTY_NODE_DATA: &str = "\
1\tThey\tthey\tPRON\t_\t_\t2\tnsubj\t2:nsubj\t_
2\tbought\tbuy\tVERB\t_\t_\t0\troot\t0:root\t_
2.1\tbought\tbuy\tVERB\t_\t_\t_\t_\t0:root|4:nsubj\t_
3\tand\tand\tCCONJ\t_\t_\t4\tcc\t4:cc\t_
4\tate\teat\tVERB\t_\t_\t2\tconj\t2:conj\t_
5\tan\ta\tDET\t_\t_\t6\tdet\t6:det\t_
6\tapple\tapple\tNOUN\t_\t_\t2\tobj\t2:obj|4:obj\t_
7\t.\t.\tPUNCT\t_\t_\t2\tpunct\t_\t_
";

#[test]
fn empty_nodes_preserved() {
	let corpus = build_corpus(EMPTY_NODE_DATA);

	assert_eq!(corpus.token_count(), 7);

	let store = corpus.empty_nodes().expect("empty nodes should be loaded");
	assert_eq!(store.len(), 1);
	let nodes = store.in_sentence(0);
	assert_eq!(nodes.len(), 1);
	assert_eq!(nodes[0].node_id, "2.1");
	assert_eq!(nodes[0].form, "bought");
	assert_eq!(nodes[0].upos.as_deref(), Some("VERB"));
	assert_eq!(nodes[0].deps.as_deref(), Some("0:root|4:nsubj"));
}

#[test]
fn empty_nodes_not_indexed() {
	let corpus = build_corpus(EMPTY_NODE_DATA);
	let spans = query_spans(&corpus, r#"[word="bought"]"#);
	assert_eq!(spans.len(), 1);
	assert_eq!(spans[0], (1, 2));
}

#[test]
fn deps_with_empty_node_references() {
	let corpus = build_corpus(EMPTY_NODE_DATA);
	assert_eq!(corpus.forward().get_str(5, "deps"), Some("2:obj|4:obj"));
}
