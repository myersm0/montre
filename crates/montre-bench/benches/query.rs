use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use montre_index::{Corpus, InvertedIndex};
use montre_query::executor;
use montre_query::planner::{self, QueryPlan};
use std::hint::black_box;

fn corpus_path() -> String {
	std::env::var("MONTRE_BENCH_CORPUS")
		.expect("set MONTRE_BENCH_CORPUS to a built montre corpus directory")
}

fn open_corpus() -> Corpus {
	montre_index::open(&corpus_path())
		.expect("failed to open benchmark corpus; set MONTRE_BENCH_CORPUS")
}

fn alignment_name() -> String {
	std::env::var("MONTRE_BENCH_ALIGNMENT")
		.unwrap_or_else(|_| "labse".into())
}

fn source_component() -> String {
	std::env::var("MONTRE_BENCH_SOURCE_COMPONENT")
		.unwrap_or_else(|_| "maupassant-fr".into())
}

struct BenchQuery {
	label: &'static str,
	cql: String,
}

fn standard_queries() -> Vec<BenchQuery> {
	vec![
		BenchQuery { label: "pos_noun", cql: r#"[pos="NOUN"]"#.into() },
		BenchQuery { label: "adj_noun", cql: r#"[pos="ADJ"] [pos="NOUN"]"#.into() },
		BenchQuery { label: "optional_adj_noun", cql: r#"[pos="ADJ"]? [pos="NOUN"]"#.into() },
		BenchQuery { label: "alt_plus_noun", cql: r#"([pos="ADJ"] | [pos="ADV"])+ [pos="NOUN"]"#.into() },
		BenchQuery { label: "alt_det_plus_noun", cql: r#"([pos="ADJ"] | [pos="DET"])+ [pos="NOUN"]"#.into() },
		BenchQuery { label: "lemma_literal", cql: r#"[lemma="maison"]"#.into() },
		BenchQuery { label: "lemma_regex", cql: r#"[lemma=/^un.*/]"#.into() },
		BenchQuery { label: "negation", cql: r#"[pos!="PUNCT"]"#.into() },
		BenchQuery { label: "scanall_first", cql: r#"[] [lemma="chat"]"#.into() },
		BenchQuery { label: "within_sentence", cql: r#"[pos="DET"] [pos="ADJ"]* [pos="NOUN"] within s"#.into() },
	]
}

fn projection_queries() -> Vec<BenchQuery> {
	let alignment = alignment_name();
	let component = source_component();
	vec![
		BenchQuery {
			label: "project_maison",
			cql: format!(r#"[lemma="maison"] within component:"{component}" ={alignment}=>"#),
		},
		BenchQuery {
			label: "project_adj_noun",
			cql: format!(r#"[pos="ADJ"] [pos="NOUN"] within component:"{component}" ={alignment}=>"#),
		},
	]
}

fn parse_and_plan(cql: &str) -> QueryPlan {
	let parsed = montre_query::parse(cql).unwrap();
	planner::plan(&parsed).unwrap()
}

fn bench_corpus_open(c: &mut Criterion) {
	let path = corpus_path();
	c.bench_function("corpus_open", |b| {
		b.iter(|| montre_index::open(black_box(&path)).unwrap())
	});
}

fn bench_query_execute(c: &mut Criterion) {
	let corpus = open_corpus();
	let queries = standard_queries();
	let mut group = c.benchmark_group("query_execute");
	for q in &queries {
		let plan = parse_and_plan(&q.cql);
		group.bench_with_input(BenchmarkId::new("execute", q.label), &plan, |b, plan| {
			b.iter(|| executor::execute(black_box(plan), black_box(&corpus)).unwrap())
		});
	}
	group.finish();
}

fn bench_query_full_pipeline(c: &mut Criterion) {
	let corpus = open_corpus();
	let queries = standard_queries();
	let mut group = c.benchmark_group("query_full_pipeline");
	for q in &queries {
		group.bench_function(q.label, |b| {
			b.iter(|| {
				let parsed = montre_query::parse(black_box(&q.cql)).unwrap();
				let plan = planner::plan(&parsed).unwrap();
				executor::execute(&plan, &corpus).unwrap()
			})
		});
	}
	group.finish();
}

fn bench_query_count_only(c: &mut Criterion) {
	let corpus = open_corpus();
	let queries = standard_queries();
	let mut group = c.benchmark_group("query_count_only");
	for q in &queries {
		let plan = parse_and_plan(&q.cql);
		group.bench_with_input(BenchmarkId::new("count", q.label), &plan, |b, plan| {
			b.iter(|| executor::execute_count(black_box(plan), black_box(&corpus)).unwrap())
		});
	}
	group.finish();
}

fn bench_count_vs_execute(c: &mut Criterion) {
	let corpus = open_corpus();
	let cases: &[(&str, &str)] = &[
		("pos_noun", r#"[pos="NOUN"]"#),
		("adj_noun", r#"[pos="ADJ"] [pos="NOUN"]"#),
		("lemma_regex", r#"[lemma=/^un.*/]"#),
	];
	let mut group = c.benchmark_group("count_vs_execute");
	for &(label, cql) in cases {
		let plan = parse_and_plan(cql);
		group.bench_with_input(BenchmarkId::new("execute", label), &plan, |b, plan| {
			b.iter(|| executor::execute(black_box(plan), black_box(&corpus)).unwrap())
		});
		group.bench_with_input(BenchmarkId::new("count", label), &plan, |b, plan| {
			b.iter(|| executor::execute_count(black_box(plan), black_box(&corpus)).unwrap())
		});
	}
	group.finish();
}

fn bench_alignment_projection(c: &mut Criterion) {
	let corpus = open_corpus();
	let queries = projection_queries();
	let mut group = c.benchmark_group("alignment_projection");
	for q in &queries {
		let plan = parse_and_plan(&q.cql);
		group.bench_with_input(BenchmarkId::new("project", q.label), &plan, |b, plan| {
			b.iter(|| executor::execute(black_box(plan), black_box(&corpus)).unwrap())
		});
	}
	group.finish();
}

fn bench_vocab(c: &mut Criterion) {
	let corpus = open_corpus();
	let layers = ["pos", "lemma", "word"];
	let mut group = c.benchmark_group("vocab");
	for layer in &layers {
		group.bench_function(*layer, |b| {
			b.iter(|| {
				let values = corpus.inverted.values(black_box(layer)).unwrap();
				let mut entries: Vec<(&str, u64)> = values
					.iter()
					.map(|value| {
						let count = corpus
							.inverted
							.get(layer, value)
							.map(|bm| bm.len())
							.unwrap_or(0);
						(*value, count)
					})
					.collect();
				entries.sort_by(|a, b| b.1.cmp(&a.1));
				entries
			})
		});
	}
	group.finish();
}

fn bench_count_by_document(c: &mut Criterion) {
	let corpus = open_corpus();
	let queries = &[
		("pos_noun", r#"[pos="NOUN"]"#),
		("adj_noun", r#"[pos="ADJ"] [pos="NOUN"]"#),
	];
	let doc_count = corpus.document_names().len();
	let mut group = c.benchmark_group("count_by_document");
	for &(label, cql) in queries {
		let plan = parse_and_plan(cql);
		group.bench_with_input(BenchmarkId::new("group", label), &plan, |b, plan| {
			b.iter(|| {
				let mut results =
					executor::execute(black_box(plan), black_box(&corpus)).unwrap();
				results.populate_context(&corpus);
				let mut counts: Vec<usize> = vec![0; doc_count];
				for hit in results.hits() {
					counts[hit.document_index as usize] += 1;
				}
				counts
			})
		});
	}
	group.finish();
}

criterion_group!(
	benches,
	bench_corpus_open,
	bench_query_execute,
	bench_query_full_pipeline,
	bench_query_count_only,
	bench_count_vs_execute,
	bench_alignment_projection,
	bench_vocab,
	bench_count_by_document,
);
criterion_main!(benches);
