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

fn collect_conllu_files(dir: &PathBuf) -> Result<Vec<PathBuf>> {
	let mut files = Vec::new();
	collect_conllu_recursive(dir, &mut files)?;
	files.sort();
	Ok(files)
}

fn collect_conllu_recursive(dir: &PathBuf, files: &mut Vec<PathBuf>) -> Result<()> {
	if dir.is_file() {
		if matches!(dir.extension().and_then(|e| e.to_str()), Some("conllu" | "conll")) {
			files.push(dir.clone());
		}
		return Ok(());
	}

	for entry in std::fs::read_dir(dir)
		.with_context(|| format!("Failed to read directory: {}", dir.display()))?
	{
		let entry = entry?;
		let path = entry.path();

		if path.is_dir() {
			collect_conllu_recursive(&path, files)?;
		} else if matches!(path.extension().and_then(|e| e.to_str()), Some("conllu" | "conll")) {
			files.push(path);
		}
	}

	Ok(())
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

	let files = collect_conllu_files(&input)?;

	if files.is_empty() {
		anyhow::bail!("No .conllu files found in {}", input.display());
	}

	tracing::info!(
		"Building corpus '{}' from {} files in {}",
		corpus_name,
		files.len(),
		input.display()
	);

	let mut builder = CorpusBuilder::new(&corpus_name);
	let mut total_sentences = 0usize;

	for file_path in &files {
		let doc_name = file_path
			.file_name()
			.and_then(|n| n.to_str())
			.unwrap_or("unknown")
			.to_string();

		let file = File::open(file_path)
			.with_context(|| format!("Failed to open: {}", file_path.display()))?;

		let mut reader = ConllUReader::new(file);
		let sentences = reader.read_sentences()
			.with_context(|| format!("Failed to parse: {}", file_path.display()))?;

		let sentence_count = sentences.len();
		builder.add_document(&doc_name, sentences);
		total_sentences += sentence_count;

		tracing::debug!("  {} sentences from {}", sentence_count, doc_name);
	}

	tracing::info!(
		"Parsed {} documents, {} sentences, {} tokens",
		builder.document_count(),
		total_sentences,
		builder.current_position()
	);

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

		let doc_name = corpus.document_at(hit.span.start).unwrap_or("?");

		let mut left = String::new();
		for pos in start_ctx..hit.span.start {
			if let Some(montre_core::Value::Str(w)) = corpus.forward.get(pos, "word") {
				left.push_str(w);
				left.push(' ');
			}
		}

		let mut match_text = String::new();
		for pos in hit.span.start..hit.span.end {
			if let Some(montre_core::Value::Str(w)) = corpus.forward.get(pos, "word") {
				match_text.push_str(w);
				match_text.push(' ');
			}
		}

		let mut right = String::new();
		for pos in hit.span.end..end_ctx {
			if let Some(montre_core::Value::Str(w)) = corpus.forward.get(pos, "word") {
				right.push_str(w);
				right.push(' ');
			}
		}

		println!(
			"{:<30} {:>20} >>> {} <<< {}",
			doc_name,
			left.trim(),
			match_text.trim(),
			right.trim()
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

	Ok(())
}
