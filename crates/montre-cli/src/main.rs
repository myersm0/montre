use std::fs::File;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;
use walkdir::WalkDir;

use montre_build::builder::CorpusBuilder;
use montre_build::format::conllu::ConllUReader;
use montre_build::format::{CorpusReader, ParseStats};
use montre_build::MultiCorpusBuilder;
use montre_core::Value;
use montre_index::ForwardIndex;

#[derive(Parser)]
#[command(name = "montre")]
#[command(about = "A modern corpus query engine", version)]
struct Cli {
	#[command(subcommand)]
	command: Commands,

	#[arg(short, long, global = true)]
	verbose: bool,
}

#[derive(Subcommand)]
enum Commands {
	Build {
		#[arg(short, long, conflicts_with = "manifest")]
		input: Option<PathBuf>,

		#[arg(short, long, help = "Build from manifest file")]
		manifest: Option<PathBuf>,

		#[arg(short, long)]
		output: PathBuf,

		#[arg(short, long)]
		name: Option<String>,

		#[arg(long)]
		force: bool,

		#[arg(long, help = "Fail on first parse error instead of skipping")]
		strict: bool,
	},

	Query {
		corpus: PathBuf,

		query: String,

		#[arg(short, long, default_value = "20")]
		limit: usize,

		#[arg(long)]
		count_only: bool,
	},

	Info {
		corpus: PathBuf,
	},
}

#[derive(Default)]
struct AggregateStats {
	documents: usize,
	sentences_parsed: usize,
	sentences_skipped: usize,
	tokens_parsed: usize,
}

impl AggregateStats {
	fn add(&mut self, stats: &ParseStats) {
		self.sentences_parsed += stats.sentences_parsed;
		self.sentences_skipped += stats.sentences_skipped;
		self.tokens_parsed += stats.tokens_parsed;
	}
}

fn main() -> Result<()> {
	let cli = Cli::parse();

	let filter = if cli.verbose {
		EnvFilter::new("debug")
	} else {
		EnvFilter::new("info")
	};

	tracing_subscriber::fmt()
		.with_env_filter(filter)
		.with_target(false)
		.init();

	match cli.command {
		Commands::Build {
			input,
			manifest,
			output,
			name,
			force,
			strict,
		} => {
			if let Some(manifest_path) = manifest {
				cmd_build_manifest(manifest_path, output, force, strict)
			} else if let Some(input_path) = input {
				cmd_build(input_path, output, name, force, strict)
			} else {
				anyhow::bail!("Either --input or --manifest must be specified")
			}
		}
		Commands::Query {
			corpus,
			query,
			limit,
			count_only,
		} => cmd_query(corpus, query, limit, count_only),
		Commands::Info { corpus } => cmd_info(corpus),
	}
}

fn cmd_build_manifest(
	manifest_path: PathBuf,
	output: PathBuf,
	force: bool,
	strict: bool,
) -> Result<()> {
	if output.exists() && !force {
		anyhow::bail!(
			"Output directory {} already exists. Use --force to overwrite.",
			output.display()
		);
	}

	tracing::info!("Building corpus from manifest: {}", manifest_path.display());

	MultiCorpusBuilder::from_manifest(&manifest_path)
		.with_context(|| format!("Failed to read manifest: {}", manifest_path.display()))?
		.strict(strict)
		.build(&output)
		.with_context(|| "Failed to build corpus")?;

	tracing::info!("Corpus written to {}", output.display());
	Ok(())
}

fn cmd_build(
	input: PathBuf,
	output: PathBuf,
	name: Option<String>,
	force: bool,
	strict: bool,
) -> Result<()> {
	if output.exists() && !force {
		anyhow::bail!(
			"Output directory {} already exists. Use --force to overwrite.",
			output.display()
		);
	}

	let corpus_name = name.unwrap_or_else(|| {
		output
			.file_name()
			.and_then(|n| n.to_str())
			.unwrap_or("corpus")
			.to_string()
	});

	tracing::info!("Building corpus '{}' from {}", corpus_name, input.display());

	let mut builder = CorpusBuilder::new(&corpus_name);
	let mut aggregate = AggregateStats::default();

	let entries: Vec<PathBuf> = if input.is_file() {
		vec![input.clone()]
	} else {
		WalkDir::new(&input)
			.follow_links(true)
			.into_iter()
			.filter_map(|e| e.ok())
			.filter(|e| {
				e.path()
					.extension()
					.map(|ext| ext == "conllu")
					.unwrap_or(false)
			})
			.map(|e| e.path().to_path_buf())
			.collect()
	};

	if entries.is_empty() {
		anyhow::bail!("No .conllu files found in {}", input.display());
	}

	for path in entries {
		let file =
			File::open(&path).with_context(|| format!("Failed to open: {}", path.display()))?;

		let filename = path
			.file_name()
			.and_then(|n| n.to_str())
			.unwrap_or("unknown");

		let mut reader = ConllUReader::new(file).with_source_name(filename);

		let sentences = if strict {
			reader.read_sentences_strict()?
		} else {
			reader.read_sentences()?
		};

		if !sentences.is_empty() {
			builder.add_document(filename, sentences);
			aggregate.documents += 1;
		}
		aggregate.add(reader.stats());
	}

	builder
		.build(&output)
		.with_context(|| "Failed to build corpus")?;

	if aggregate.sentences_skipped > 0 {
		tracing::info!(
			"Parsed {} documents, {} sentences ({} skipped), {} tokens",
			aggregate.documents,
			aggregate.sentences_parsed,
			aggregate.sentences_skipped,
			aggregate.tokens_parsed
		);
	} else {
		tracing::info!(
			"Parsed {} documents, {} sentences, {} tokens",
			aggregate.documents,
			aggregate.sentences_parsed,
			aggregate.tokens_parsed
		);
	}

	tracing::info!("Corpus written to {}", output.display());
	Ok(())
}

fn value_to_str(v: &Value) -> String {
	match v {
		Value::Str(s) => s.to_string(),
		Value::Int(n) => n.to_string(),
	}
}

fn cmd_query(corpus_path: PathBuf, query: String, limit: usize, count_only: bool) -> Result<()> {
	use std::time::Instant;

	let corpus = montre_index::open(&corpus_path)
		.with_context(|| format!("Failed to open corpus: {}", corpus_path.display()))?;

	let parse_start = Instant::now();
	let parsed =
		montre_query::parse(&query).with_context(|| format!("Failed to parse query: {}", query))?;
	let parse_time = parse_start.elapsed();

	let plan_start = Instant::now();
	let plan = montre_query::planner::plan(&parsed).with_context(|| "Failed to plan query")?;
	let plan_time = plan_start.elapsed();

	let exec_start = Instant::now();
	let results = montre_query::executor::execute(&plan, &corpus)
		.with_context(|| "Failed to execute query")?;
	let exec_time = exec_start.elapsed();

	let total = results.len();

	if count_only {
		println!("{}", total);
		eprintln!(
			"(parse: {:?}, plan: {:?}, exec: {:?})",
			parse_time, plan_time, exec_time
		);
		return Ok(());
	}

	println!("Found {} matches in {:?}\n", total, exec_time);

	let token_count = corpus.token_count();

	for (i, hit) in results.hits().iter().enumerate() {
		if i >= limit {
			if total > limit {
				println!("... ({} more results)", total - limit);
			}
			break;
		}

		let ctx_start = hit.span.start.saturating_sub(5);
		let ctx_end = (hit.span.end + 5).min(token_count);

		let left: Vec<String> = (ctx_start..hit.span.start)
			.filter_map(|p| corpus.forward.get(p, "word").map(value_to_str))
			.collect();

		let matched: Vec<String> = (hit.span.start..hit.span.end)
			.filter_map(|p| corpus.forward.get(p, "word").map(value_to_str))
			.collect();

		let right: Vec<String> = (hit.span.end..ctx_end)
			.filter_map(|p| corpus.forward.get(p, "word").map(value_to_str))
			.collect();

		let doc_name = corpus.document_at(hit.span.start).unwrap_or("?");

		println!(
			"{:>12} {:>8}: {} >>> {} <<< {}",
			doc_name,
			hit.span.start,
			left.join(" "),
			matched.join(" "),
			right.join(" ")
		);
	}

	Ok(())
}

fn cmd_info(corpus_path: PathBuf) -> Result<()> {
	let corpus = montre_index::open(&corpus_path)
		.with_context(|| format!("Failed to open corpus: {}", corpus_path.display()))?;

	println!("Corpus: {}", corpus.name());
	println!("Path: {}", corpus.path().display());
	println!("Tokens: {}", corpus.token_count());
	println!("Documents: {}", corpus.document_names().len());
	println!("Layers: {}", corpus.layers().join(", "));
	println!("Span layers: {}", corpus.span_layers().join(", "));

	if corpus.is_multi_component() {
		println!("\nComponents:");
		for comp in corpus.components() {
			let doc_count = comp.document_range.1 - comp.document_range.0;
			println!(
				"  {} ({}) - {} documents",
				comp.name, comp.language, doc_count
			);
		}
	}

	if !corpus.meta.alignments.is_empty() {
		println!("\nAlignments:");
		for align in &corpus.meta.alignments {
			println!(
				"  {} ({} -> {}, {} layer, {} edges)",
				align.name,
				align.source_component,
				align.target_component,
				align.source_layer,
				align.edge_count
			);
		}
	}

	Ok(())
}
