use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use montre_index::{AlignmentMeta, Corpus, InvertedIndex};
use montre_query::executor;
use montre_query::planner::{self, QueryPlan};
use roaring::RoaringBitmap;
use std::hint::black_box;
use std::sync::OnceLock;

fn corpus_path() -> String {
	std::env::var("MONTRE_BENCH_CORPUS")
		.expect("set MONTRE_BENCH_CORPUS to a built montre corpus directory")
}

fn open_corpus() -> Corpus {
	montre_index::open(&corpus_path())
		.expect("failed to open benchmark corpus; set MONTRE_BENCH_CORPUS")
}

fn alignment(corpus: &Corpus) -> Option<&AlignmentMeta> {
	match std::env::var("MONTRE_BENCH_ALIGNMENT") {
		Ok(name) => Some(
			corpus
				.alignment_meta(&name)
				.unwrap_or_else(|| panic!("MONTRE_BENCH_ALIGNMENT: no alignment named {name}")),
		),
		Err(_) => corpus.alignments().first(),
	}
}

fn source_component(corpus: &Corpus) -> String {
	std::env::var("MONTRE_BENCH_SOURCE_COMPONENT")
		.ok()
		.or_else(|| alignment(corpus).map(|meta| meta.source_component.clone()))
		.unwrap_or_else(|| corpus.components()[0].name.clone())
}

fn component_positions(corpus: &Corpus, component: &str) -> RoaringBitmap {
	let (first, end) = corpus
		.component(component)
		.unwrap_or_else(|| panic!("no component named {component}"))
		.document_range;
	let start = corpus
		.document_span(first)
		.expect("component has no documents")
		.start;
	let stop = corpus
		.document_span(end - 1)
		.expect("component has no documents")
		.end;
	let mut positions = RoaringBitmap::new();
	positions.insert_range(start as u32..stop as u32);
	positions
}

struct Slot {
	lemma: String,
	per_million: f64,
}

struct Slots {
	component: String,
	rare: Slot,
	mid: Slot,
	frequent: Slot,
}

impl Slots {
	fn prefix(&self) -> String {
		self.frequent.lemma.chars().take(2).collect()
	}
}

// noun lemmas nearest 2, 100 and 3,000 per million tokens, as in pando's engine benchmark
fn compute_slots(corpus: &Corpus) -> Slots {
	let component = source_component(corpus);
	let positions = component_positions(corpus, &component);
	let inverted = corpus.inverted();
	let nouns = inverted
		.get("upos", "NOUN")
		.expect("corpus has no upos=NOUN")
		& &positions;
	let scale = 1e6 / positions.len() as f64;
	let candidates: Vec<(&str, f64)> = inverted
		.values("lemma")
		.expect("corpus has no lemma layer")
		.into_iter()
		.filter(|lemma| lemma.chars().all(char::is_alphabetic))
		.filter_map(|lemma| {
			let occurrences = inverted.get("lemma", lemma)?;
			let total = occurrences.intersection_len(&positions);
			let as_noun = occurrences.intersection_len(&nouns);
			(as_noun * 2 > total).then_some((lemma, total as f64 * scale))
		})
		.collect();
	let nearest = |target: f64| {
		let distance = |per_million: f64| (per_million / target).ln().abs();
		let (lemma, per_million) = candidates
			.iter()
			.min_by(|left, right| {
				distance(left.1)
					.total_cmp(&distance(right.1))
					.then_with(|| left.0.cmp(right.0))
			})
			.expect("corpus has no alphabetic noun lemmas");
		Slot {
			lemma: lemma.to_string(),
			per_million: *per_million,
		}
	};
	let slots = Slots {
		rare: nearest(2.0),
		mid: nearest(100.0),
		frequent: nearest(3000.0),
		component,
	};
	eprintln!(
		"lemma slots from component {} ({} tokens): rare {} ({:.1}/M), mid {} ({:.1}/M), frequent {} ({:.1}/M)",
		slots.component,
		positions.len(),
		slots.rare.lemma,
		slots.rare.per_million,
		slots.mid.lemma,
		slots.mid.per_million,
		slots.frequent.lemma,
		slots.frequent.per_million,
	);
	slots
}

fn slots() -> &'static Slots {
	static SLOTS: OnceLock<Slots> = OnceLock::new();
	SLOTS.get_or_init(|| compute_slots(&open_corpus()))
}

struct BenchQuery {
	label: &'static str,
	cql: String,
}

fn standard_queries() -> Vec<BenchQuery> {
	let slots = slots();
	vec![
		BenchQuery {
			label: "pos_noun",
			cql: r#"[pos="NOUN"]"#.into(),
		},
		BenchQuery {
			label: "adj_noun",
			cql: r#"[pos="ADJ"] [pos="NOUN"]"#.into(),
		},
		BenchQuery {
			label: "optional_adj_noun",
			cql: r#"[pos="ADJ"]? [pos="NOUN"]"#.into(),
		},
		BenchQuery {
			label: "alt_plus_noun",
			cql: r#"([pos="ADJ"] | [pos="ADV"])+ [pos="NOUN"]"#.into(),
		},
		BenchQuery {
			label: "alt_det_plus_noun",
			cql: r#"([pos="ADJ"] | [pos="DET"])+ [pos="NOUN"]"#.into(),
		},
		BenchQuery {
			label: "lemma_rare",
			cql: format!(r#"[lemma="{}"]"#, slots.rare.lemma),
		},
		BenchQuery {
			label: "lemma_mid",
			cql: format!(r#"[lemma="{}"]"#, slots.mid.lemma),
		},
		BenchQuery {
			label: "lemma_frequent",
			cql: format!(r#"[lemma="{}"]"#, slots.frequent.lemma),
		},
		BenchQuery {
			label: "lemma_prefix",
			cql: format!(r#"[lemma=/^{}/]"#, slots.prefix()),
		},
		BenchQuery {
			label: "negation",
			cql: r#"[pos!="PUNCT"]"#.into(),
		},
		BenchQuery {
			label: "scanall_then_mid",
			cql: format!(r#"[] [lemma="{}"]"#, slots.mid.lemma),
		},
		BenchQuery {
			label: "within_sentence",
			cql: r#"[pos="DET"] [pos="ADJ"]* [pos="NOUN"] within s"#.into(),
		},
	]
}

fn projection_queries(corpus: &Corpus) -> Vec<BenchQuery> {
	let Some(alignment) = alignment(corpus) else {
		eprintln!("corpus has no alignments; skipping alignment_projection");
		return Vec::new();
	};
	let alignment = &alignment.name;
	let slots = slots();
	let component = &slots.component;
	vec![
		BenchQuery {
			label: "project_lemma_mid",
			cql: format!(
				r#"[lemma="{}"] within component:"{component}" ={alignment}=>"#,
				slots.mid.lemma
			),
		},
		BenchQuery {
			label: "project_adj_noun",
			cql: format!(
				r#"[pos="ADJ"] [pos="NOUN"] within component:"{component}" ={alignment}=>"#
			),
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
	let cases = standard_queries()
		.into_iter()
		.filter(|q| ["pos_noun", "adj_noun", "lemma_prefix"].contains(&q.label));
	let mut group = c.benchmark_group("count_vs_execute");
	for q in cases {
		let plan = parse_and_plan(&q.cql);
		group.bench_with_input(BenchmarkId::new("execute", q.label), &plan, |b, plan| {
			b.iter(|| executor::execute(black_box(plan), black_box(&corpus)).unwrap())
		});
		group.bench_with_input(BenchmarkId::new("count", q.label), &plan, |b, plan| {
			b.iter(|| executor::execute_count(black_box(plan), black_box(&corpus)).unwrap())
		});
	}
	group.finish();
}

fn bench_alignment_projection(c: &mut Criterion) {
	let corpus = open_corpus();
	let queries = projection_queries(&corpus);
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
	let layers = ["upos", "lemma", "word"];
	let mut group = c.benchmark_group("vocab");
	for layer in &layers {
		group.bench_function(*layer, |b| {
			b.iter(|| {
				let values = corpus.inverted().values(black_box(layer)).unwrap();
				let mut entries: Vec<(&str, u64)> = values
					.iter()
					.map(|value| {
						let count = corpus
							.inverted()
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
				let mut results = executor::execute(black_box(plan), black_box(&corpus)).unwrap();
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
