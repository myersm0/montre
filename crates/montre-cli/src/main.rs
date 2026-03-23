use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use montre_build::builder::CorpusBuilder;
use montre_build::MultiCorpusBuilder;
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

		#[arg(long, help = "Index individual morphological features as separate layers")]
		decompose_feats: bool,
	},

	Query {
		corpus: PathBuf,

		query: String,

		#[arg(short, long, default_value = "20")]
		limit: usize,

		#[arg(long)]
		count_only: bool,

		#[arg(long, value_delimiter = ',', help = "Restrict to named component(s)")]
		component: Vec<String>,

		#[arg(long, value_delimiter = ',', help = "Restrict to named document(s)")]
		document: Vec<String>,
	},

	Info {
		corpus: PathBuf,
	},

	Docs {
		corpus: PathBuf,

		#[arg(long, help = "Filter by component")]
		component: Option<String>,
	},
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
			decompose_feats,
		} => {
			if let Some(manifest_path) = manifest {
				cmd_build_manifest(manifest_path, output, force, strict, decompose_feats)
			} else if let Some(input_path) = input {
				cmd_build(input_path, output, name, force, strict, decompose_feats)
			} else {
				anyhow::bail!("Either --input or --manifest must be specified")
			}
		}
		Commands::Query {
			corpus,
			query,
			limit,
			count_only,
			component,
			document,
		} => cmd_query(corpus, query, limit, count_only, component, document),
		Commands::Info { corpus } => cmd_info(corpus),
		Commands::Docs { corpus, component } => cmd_docs(corpus, component),
	}
}

fn cmd_build_manifest(
	manifest_path: PathBuf,
	output: PathBuf,
	force: bool,
	strict: bool,
	decompose_feats: bool,
) -> Result<()> {
	if output.exists() && !force {
		anyhow::bail!(
			"Output directory {} already exists. Use --force to overwrite.",
			output.display()
		);
	}

	tracing::info!("Building corpus from manifest: {}", manifest_path.display());

	let mut builder = MultiCorpusBuilder::from_manifest(&manifest_path)
		.with_context(|| format!("Failed to read manifest: {}", manifest_path.display()))?
		.strict(strict);

	if decompose_feats {
		builder = builder.decompose_feats(true);
	}

	builder
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
	decompose_feats: bool,
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

	let builder = CorpusBuilder::from_directory(&corpus_name, &input, decompose_feats, strict)
		.with_context(|| format!("Failed to build from {}", input.display()))?;

	tracing::info!(
		"Indexed {} documents, {} tokens",
		builder.document_count(),
		builder.current_position()
	);

	builder
		.build(&output)
		.with_context(|| "Failed to write corpus")?;

	tracing::info!("Corpus written to {}", output.display());
	Ok(())
}

fn cmd_query(
	corpus_path: PathBuf,
	query: String,
	limit: usize,
	count_only: bool,
	components: Vec<String>,
	documents: Vec<String>,
) -> Result<()> {
	use std::time::Instant;

	let corpus = montre_index::open(&corpus_path)
		.with_context(|| format!("Failed to open corpus: {}", corpus_path.display()))?;

	let mut full_query = query.clone();
	if !components.is_empty() {
		let names: Vec<String> = components.iter().map(|c| format!("\"{}\"", c)).collect();
		full_query = format!("{} within component:{}", full_query, names.join(","));
	}
	if !documents.is_empty() {
		let resolved: Vec<String> = documents
			.iter()
			.map(|d| resolve_document_name(d, corpus.document_names()))
			.collect::<Result<_>>()?;
		let names: Vec<String> = resolved.iter().map(|d| format!("\"{}\"", d)).collect();
		full_query = format!("{} within doc:{}", full_query, names.join(","));
	}

	let parse_start = Instant::now();
	let parsed = montre_query::parse(&full_query)
		.with_context(|| format!("Failed to parse query: {}", full_query))?;
	let parse_time = parse_start.elapsed();

	let plan_start = Instant::now();
	let plan = montre_query::planner::plan(&parsed).with_context(|| "Failed to plan query")?;
	let plan_time = plan_start.elapsed();

	if count_only {
		let exec_start = Instant::now();
		let total = montre_query::executor::execute_count(&plan, &corpus)
			.with_context(|| "Failed to execute query")?;
		let exec_time = exec_start.elapsed();
		println!("{}", total);
		eprintln!(
			"(parse: {:?}, plan: {:?}, exec: {:?})",
			parse_time, plan_time, exec_time
		);
		return Ok(());
	}

	let exec_start = Instant::now();
	let results = montre_query::executor::execute(&plan, &corpus)
		.with_context(|| "Failed to execute query")?;
	let exec_time = exec_start.elapsed();

	let total = results.len();

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
			.filter_map(|p| corpus.forward.get_str(p, "word").map(str::to_string))
			.collect();

		let matched: Vec<String> = (hit.span.start..hit.span.end)
			.filter_map(|p| corpus.forward.get_str(p, "word").map(str::to_string))
			.collect();

		let right: Vec<String> = (hit.span.end..ctx_end)
			.filter_map(|p| corpus.forward.get_str(p, "word").map(str::to_string))
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

fn resolve_document_name(input: &str, doc_names: &[String]) -> Result<String> {
	if doc_names.iter().any(|n| n == input) {
		return Ok(input.to_string());
	}

	let stem_matches: Vec<&String> = doc_names
		.iter()
		.filter(|n| {
			std::path::Path::new(n.as_str())
				.file_stem()
				.and_then(|s| s.to_str())
				== Some(input)
		})
		.collect();

	match stem_matches.len() {
		1 => Ok(stem_matches[0].clone()),
		0 => anyhow::bail!(
			"Document '{}' not found. Use `montre docs` to list available documents.",
			input
		),
		_ => anyhow::bail!(
			"Document '{}' is ambiguous, matches: {}",
			input,
			stem_matches.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
		),
	}
}

fn cmd_docs(corpus_path: PathBuf, component: Option<String>) -> Result<()> {
	let corpus = montre_index::open(&corpus_path)
		.with_context(|| format!("Failed to open corpus: {}", corpus_path.display()))?;

	let doc_names = corpus.document_names();

	let range = match component {
		Some(ref name) => {
			let comp = corpus.component(name).with_context(|| {
				let available: Vec<&str> =
					corpus.components().iter().map(|c| c.name.as_str()).collect();
				format!(
					"Component '{}' not found. Available: {}",
					name,
					available.join(", ")
				)
			})?;
			comp.document_range.0..comp.document_range.1
		}
		None => 0..doc_names.len(),
	};

	for idx in range {
		if let Some(name) = doc_names.get(idx) {
			println!("{:>4}  {}", idx, name);
		}
	}

	Ok(())
}
