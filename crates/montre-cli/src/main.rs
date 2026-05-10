use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use montre_build::builder::CorpusBuilder;
use montre_build::MultiCorpusBuilder;
use montre_index::{Corpus, InvertedIndex, SpanIndex};

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

	/// Count matches for a CQL query
	Count {
		corpus: PathBuf,

		query: String,

		#[arg(long, value_delimiter = ',', help = "Restrict to named component(s)")]
		component: Vec<String>,

		#[arg(long, value_delimiter = ',', help = "Restrict to named document(s)")]
		document: Vec<String>,

		#[arg(long, help = "Group counts by document")]
		by_document: bool,

		#[arg(long, help = "Group counts by component")]
		by_component: bool,
	},

	Info {
		corpus: PathBuf,
	},

	/// List documents in the corpus
	#[command(alias = "documents")]
	Docs {
		corpus: PathBuf,

		#[arg(long, help = "Filter by component")]
		component: Option<String>,
	},

	/// List components in the corpus
	Components {
		corpus: PathBuf,
	},

	/// List annotation layers in the corpus
	Layers {
		corpus: PathBuf,
	},

	/// List distinct values for a layer
	#[command(alias = "vocabulary")]
	Vocab {
		corpus: PathBuf,

		layer: String,

		#[arg(long, value_delimiter = ',', help = "Restrict to named component(s)")]
		component: Vec<String>,

		#[arg(long, value_delimiter = ',', help = "Restrict to named document(s)")]
		document: Vec<String>,
	},

	/// Run the session daemon for a corpus
	Serve {
		corpus: PathBuf,

		#[arg(long, help = "Override the auto-derived socket path")]
		socket_path: Option<PathBuf>,

		#[arg(
			long,
			default_value = "600",
			help = "Idle shutdown timeout in seconds (0 disables)",
		)]
		idle_timeout: u64,
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
		Commands::Count {
			corpus,
			query,
			component,
			document,
			by_document,
			by_component,
		} => cmd_count(corpus, query, component, document, by_document, by_component),
		Commands::Info { corpus } => cmd_info(corpus),
		Commands::Docs { corpus, component } => cmd_docs(corpus, component),
		Commands::Components { corpus } => cmd_components(corpus),
		Commands::Layers { corpus } => cmd_layers(corpus),
		Commands::Vocab {
			corpus,
			layer,
			component,
			document,
		} => cmd_vocab(corpus, layer, component, document),
		Commands::Serve {
			corpus,
			socket_path,
			idle_timeout,
		} => cmd_serve(corpus, socket_path, idle_timeout),
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

fn component_name_for_document(corpus: &Corpus, doc_index: usize) -> &str {
	corpus
		.component_for_document(doc_index)
		.map(|c| c.name.as_str())
		.unwrap_or_else(|| corpus.name())
}

fn build_full_query(
	query: &str,
	components: &[String],
	documents: &[String],
	corpus: &Corpus,
) -> Result<String> {
	let mut full_query = query.to_string();
	if !components.is_empty() && !corpus.components().is_empty() {
		let names: Vec<String> = components.iter().map(|c| format!("\"{}\"", c)).collect();
		full_query = format!("{} within component:{}", full_query, names.join(","));
	}
	if !documents.is_empty() {
		let resolved: Vec<String> = documents
			.iter()
			.map(|d| resolve_document_name(d, corpus.document_names()))
			.collect::<Result<_>>()?;
		warn_document_collision(&resolved, corpus, components.is_empty());
		let names: Vec<String> = resolved.iter().map(|d| format!("\"{}\"", d)).collect();
		full_query = format!("{} within doc:{}", full_query, names.join(","));
	}
	Ok(full_query)
}

fn warn_document_collision(
	resolved_names: &[String],
	corpus: &Corpus,
	no_component_filter: bool,
) {
	if !no_component_filter || !corpus.is_multi_component() {
		return;
	}
	for name in resolved_names {
		let indices = corpus.document_indices_by_name(&[name.clone()]);
		if indices.len() > 1 {
			let comp_names: Vec<&str> = indices
				.iter()
				.filter_map(|&idx| {
					corpus.component_for_document(idx).map(|c| c.name.as_str())
				})
				.collect();
			eprintln!(
				"warning: document name '{}' matches {} documents across components {}; use --component to disambiguate",
				name,
				indices.len(),
				comp_names.join(", "),
			);
		}
	}
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

	let full_query = build_full_query(&query, &components, &documents, &corpus)?;

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

		let left = corpus.surface_text(ctx_start, hit.span.start);
		let matched = corpus.surface_text(hit.span.start, hit.span.end);
		let right = corpus.surface_text(hit.span.end, ctx_end);

		let doc_name = corpus.document_at(hit.span.start).unwrap_or("?");

		println!(
			"{:>12} {:>8}: {} >>> {} <<< {}",
			doc_name,
			hit.span.start,
			left,
			matched,
			right
		);
	}

	Ok(())
}

fn cmd_count(
	corpus_path: PathBuf,
	query: String,
	components: Vec<String>,
	documents: Vec<String>,
	by_document: bool,
	by_component: bool,
) -> Result<()> {
	let corpus = montre_index::open(&corpus_path)
		.with_context(|| format!("Failed to open corpus: {}", corpus_path.display()))?;

	if by_document {
		return cmd_count_by_document(&corpus, &query, &components, &documents);
	}
	if by_component {
		return cmd_count_by_component(&corpus, &query, &components, &documents);
	}

	let full_query = build_full_query(&query, &components, &documents, &corpus)?;
	let parsed = montre_query::parse(&full_query)
		.with_context(|| format!("Failed to parse query: {}", full_query))?;
	let plan = montre_query::planner::plan(&parsed).with_context(|| "Failed to plan query")?;
	let total = montre_query::executor::execute_count(&plan, &corpus)
		.with_context(|| "Failed to execute query")?;
	println!("{}", total);
	Ok(())
}

fn cmd_count_by_document(
	corpus: &Corpus,
	query: &str,
	components: &[String],
	documents: &[String],
) -> Result<()> {
	let full_query = build_full_query(query, components, documents, corpus)?;
	let parsed = montre_query::parse(&full_query)
		.with_context(|| format!("Failed to parse query: {}", full_query))?;
	let plan = montre_query::planner::plan(&parsed).with_context(|| "Failed to plan query")?;
	let mut results = montre_query::executor::execute(&plan, corpus)
		.with_context(|| "Failed to execute query")?;

	results.populate_context(corpus);

	let doc_names = corpus.document_names();
	let mut counts: Vec<usize> = vec![0; doc_names.len()];
	for hit in results.hits() {
		let idx = hit.document_index as usize;
		if idx < counts.len() {
			counts[idx] += 1;
		}
	}

	let doc_indices: Vec<usize> = if documents.is_empty() {
		(0..doc_names.len()).collect()
	} else {
		let resolved: Vec<String> = documents
			.iter()
			.map(|d| resolve_document_name(d, doc_names))
			.collect::<Result<_>>()?;
		corpus.document_indices_by_name(&resolved)
	};

	for idx in doc_indices {
		let comp_name = component_name_for_document(corpus, idx);
		if !components.is_empty() && !components.iter().any(|c| c == comp_name) {
			continue;
		}
		println!("{}\t{}\t{}", comp_name, &doc_names[idx], counts[idx]);
	}

	Ok(())
}

fn cmd_count_by_component(
	corpus: &Corpus,
	query: &str,
	components: &[String],
	documents: &[String],
) -> Result<()> {
	let comp_list: Vec<(&str, (usize, usize))> = if corpus.components().is_empty() {
		vec![(corpus.name(), (0, corpus.document_names().len()))]
	} else {
		corpus
			.components()
			.iter()
			.map(|c| (c.name.as_str(), c.document_range))
			.collect()
	};

	for (comp_name, _range) in &comp_list {
		if !components.is_empty() && !components.iter().any(|c| c == comp_name) {
			continue;
		}

		let mut full = if corpus.components().is_empty() {
			query.to_string()
		} else {
			format!("{} within component:\"{}\"", query, comp_name)
		};
		if !documents.is_empty() {
			let resolved: Vec<String> = documents
				.iter()
				.map(|d| resolve_document_name(d, corpus.document_names()))
				.collect::<Result<_>>()?;
			let names: Vec<String> = resolved.iter().map(|d| format!("\"{}\"", d)).collect();
			full = format!("{} within doc:{}", full, names.join(","));
		}

		let parsed = montre_query::parse(&full)
			.with_context(|| format!("Failed to parse query: {}", full))?;
		let plan =
			montre_query::planner::plan(&parsed).with_context(|| "Failed to plan query")?;
		let count = montre_query::executor::execute_count(&plan, corpus)
			.with_context(|| "Failed to execute query")?;

		println!("{}\t{}", comp_name, count);
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

	if !corpus.alignments().is_empty() {
		println!("\nAlignments:");
		for align in corpus.alignments() {
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
			let comp_name = component_name_for_document(&corpus, idx);
			println!("{}\t{}", comp_name, name);
		}
	}

	Ok(())
}

fn cmd_components(corpus_path: PathBuf) -> Result<()> {
	let corpus = montre_index::open(&corpus_path)
		.with_context(|| format!("Failed to open corpus: {}", corpus_path.display()))?;

	if corpus.components().is_empty() {
		println!("{}\t{}", corpus.name(), "");
	} else {
		for comp in corpus.components() {
			println!("{}\t{}", comp.name, comp.language);
		}
	}

	Ok(())
}

fn cmd_layers(corpus_path: PathBuf) -> Result<()> {
	let corpus = montre_index::open(&corpus_path)
		.with_context(|| format!("Failed to open corpus: {}", corpus_path.display()))?;

	for layer in corpus.layers() {
		println!("{}", layer);
	}

	Ok(())
}

fn cmd_serve(
	corpus_path: PathBuf,
	socket_path: Option<PathBuf>,
	idle_timeout_secs: u64,
) -> Result<()> {
	let idle_timeout = if idle_timeout_secs == 0 {
		None
	} else {
		Some(std::time::Duration::from_secs(idle_timeout_secs))
	};

	let options = montre_daemon::ServeOptions {
		corpus_path,
		socket_path,
		idle_timeout,
	};

	montre_daemon::serve(options).map_err(anyhow::Error::from)
}

fn resolve_cli_layer_name(name: &str) -> &str {
	match name {
		"pos" => "upos",
		other => other,
	}
}

fn cmd_vocab(
	corpus_path: PathBuf,
	layer: String,
	components: Vec<String>,
	documents: Vec<String>,
) -> Result<()> {
	let corpus = montre_index::open(&corpus_path)
		.with_context(|| format!("Failed to open corpus: {}", corpus_path.display()))?;

	let layer = resolve_cli_layer_name(&layer);

	let values = corpus
		.inverted()
		.values(layer)
		.with_context(|| format!("Layer '{}' not found", layer))?;

	let position_mask = build_position_mask(&corpus, &components, &documents)?;

	let mut vals: Vec<&str> = if let Some(ref mask) = position_mask {
		values
			.iter()
			.filter(|value| {
				corpus
					.inverted()
					.get(layer, value)
					.map(|bitmap| !(bitmap & mask).is_empty())
					.unwrap_or(false)
			})
			.copied()
			.collect()
	} else {
		values
	};
	vals.sort_unstable();
	for value in vals {
		println!("{}", value);
	}

	Ok(())
}

fn build_position_mask(
	corpus: &Corpus,
	components: &[String],
	documents: &[String],
) -> Result<Option<roaring::RoaringBitmap>> {
	let doc_spans = corpus.spans().spans("document").unwrap_or(&[]);
	let doc_names = corpus.document_names();

	let mut doc_indices: Option<Vec<usize>> = None;

	if !components.is_empty() {
		let mut indices = Vec::new();
		for comp_name in components {
			let comp = corpus.component(comp_name).with_context(|| {
				format!("Component '{}' not found", comp_name)
			})?;
			indices.extend(comp.document_range.0..comp.document_range.1);
		}
		doc_indices = Some(indices);
	}

	if !documents.is_empty() {
		let resolved: Vec<String> = documents
			.iter()
			.map(|d| resolve_document_name(d, doc_names))
			.collect::<Result<_>>()?;
		let matched = corpus.document_indices_by_name(&resolved);
		doc_indices = Some(match doc_indices {
			Some(existing) => existing
				.into_iter()
				.filter(|idx| matched.contains(idx))
				.collect(),
			None => matched,
		});
	}

	match doc_indices {
		Some(indices) => {
			let mut mask = roaring::RoaringBitmap::new();
			for doc_idx in indices {
				if let Some(span) = doc_spans.get(doc_idx) {
					mask.insert_range(span.start as u32..span.end as u32);
				}
			}
			Ok(Some(mask))
		}
		None => Ok(None),
	}
}
