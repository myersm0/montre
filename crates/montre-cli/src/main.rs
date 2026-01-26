use std::fs::File;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use montre_build::builder::CorpusBuilder;
use montre_build::format::conllu::ConllUReader;
use montre_build::format::CorpusReader;

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
		#[arg(short, long)]
		input: PathBuf,

		#[arg(short, long)]
		output: PathBuf,

		#[arg(short, long)]
		name: Option<String>,

		#[arg(long)]
		force: bool,
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
		Commands::Build { input, output, name, force } => {
			cmd_build(input, output, name, force)
		}
		Commands::Query { corpus, query, limit, count_only } => {
			cmd_query(corpus, query, limit, count_only)
		}
		Commands::Info { corpus } => {
			cmd_info(corpus)
		}
	}
}

fn cmd_build(input: PathBuf, output: PathBuf, name: Option<String>, force: bool) -> Result<()> {
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

	let file = File::open(&input)
		.with_context(|| format!("Failed to open input file: {}", input.display()))?;

	let mut reader = ConllUReader::new(file);
	let sentences = reader.read_sentences()
		.with_context(|| "Failed to parse input file")?;

	let token_count: usize = sentences.iter().map(|s| s.tokens.len()).sum();
	tracing::info!("Parsed {} sentences, {} tokens", sentences.len(), token_count);

	let mut builder = CorpusBuilder::new(corpus_name);
	builder.add_sentences(sentences);
	builder.build(&output)
		.with_context(|| "Failed to build corpus")?;

	tracing::info!("Corpus written to {}", output.display());
	Ok(())
}

fn cmd_query(corpus_path: PathBuf, query: String, limit: usize, count_only: bool) -> Result<()> {
	let corpus = montre_index::open(&corpus_path)
		.with_context(|| format!("Failed to open corpus: {}", corpus_path.display()))?;

	let parsed = montre_query::parse(&query)
		.with_context(|| format!("Failed to parse query: {}", query))?;

	let plan = montre_query::planner::plan(&parsed)
		.with_context(|| "Failed to plan query")?;

	let start = std::time::Instant::now();
	let results = montre_query::executor::execute(&plan, &corpus)
		.with_context(|| "Failed to execute query")?;
	let elapsed = start.elapsed();

	let total = results.len();

	if count_only {
		println!("{}", total);
		return Ok(());
	}

	println!("Found {} matches in {:?}\n", total, elapsed);

	use montre_index::ForwardIndex;
	let context_size = 5u64;

	for (i, hit) in results.enumerate() {
		if i >= limit {
			println!("\n... and {} more results", total - limit);
			break;
		}

		let start_ctx = hit.span.start.saturating_sub(context_size);
		let end_ctx = (hit.span.end + context_size).min(corpus.token_count());

		let mut line = String::new();

		for pos in start_ctx..hit.span.start {
			if let Some(montre_core::Value::Str(w)) = corpus.forward.get(pos, "word") {
				line.push_str(w);
				line.push(' ');
			}
		}

		line.push_str(">>> ");
		for pos in hit.span.start..hit.span.end {
			if let Some(montre_core::Value::Str(w)) = corpus.forward.get(pos, "word") {
				line.push_str(w);
				line.push(' ');
			}
		}
		line.push_str("<<< ");

		for pos in hit.span.end..end_ctx {
			if let Some(montre_core::Value::Str(w)) = corpus.forward.get(pos, "word") {
				line.push_str(w);
				line.push(' ');
			}
		}

		println!("{:>8}: {}", hit.span.start, line.trim());
	}

	Ok(())
}

fn cmd_info(corpus_path: PathBuf) -> Result<()> {
	let corpus = montre_index::open(&corpus_path)
		.with_context(|| format!("Failed to open corpus: {}", corpus_path.display()))?;

	println!("Corpus: {}", corpus.name());
	println!("Path: {}", corpus.path().display());
	println!("Tokens: {}", corpus.token_count());
	println!("Layers: {}", corpus.layers().join(", "));
	println!("Span layers: {}", corpus.span_layers().join(", "));

	Ok(())
}
