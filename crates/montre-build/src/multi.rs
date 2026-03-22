use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use montre_core::UnitId;
use montre_index::corpus::{AlignmentIndex, AlignmentMeta, ComponentMeta, CorpusMeta};
use montre_index::SpanIndex;

use crate::builder::{build_from_directory_streaming, IndexSink};
use crate::manifest::Manifest;
use crate::streaming_forward::StreamingForwardWriter;
use crate::{BuildError, Result};

use std::sync::atomic::{AtomicU64, Ordering};

static BUILD_COUNTER: AtomicU64 = AtomicU64::new(0);

pub struct MultiCorpusBuilder {
	manifest: Manifest,
	manifest_dir: std::path::PathBuf,
	decompose_feats: bool,
	strict: bool,
}

impl MultiCorpusBuilder {
	pub fn from_manifest(manifest_path: impl AsRef<Path>) -> Result<Self> {
		let manifest_path = manifest_path.as_ref();
		let manifest = Manifest::from_file(manifest_path)?;
		let manifest_dir = manifest_path
			.parent()
			.map(|p| p.to_path_buf())
			.unwrap_or_default();

		let decompose_feats = manifest.corpus.decompose_feats;

		Ok(Self {
			manifest,
			manifest_dir,
			decompose_feats,
			strict: false,
		})
	}

	pub fn strict(mut self, strict: bool) -> Self {
		self.strict = strict;
		self
	}

	pub fn decompose_feats(mut self, enabled: bool) -> Self {
		self.decompose_feats = enabled;
		self
	}

	pub fn build(self, output_path: impl AsRef<Path>) -> Result<()> {
		let build_id = BUILD_COUNTER.fetch_add(1, Ordering::SeqCst);
		let temp_dir = std::env::temp_dir().join(format!(
			"montre_build_{}_{}", std::process::id(), build_id
		));
		let mut streaming_forward = StreamingForwardWriter::new(&temp_dir)?;

		let mut component_names: Vec<String> =
			self.manifest.components.keys().cloned().collect();
		component_names.sort();

		let mut combined = IndexSink::new_without_forward().with_decompose_feats(self.decompose_feats);
		let mut components = Vec::new();

		for (id, name) in component_names.iter().enumerate() {
			let config = self
				.manifest
				.components
				.get(name)
				.ok_or_else(|| BuildError::UnknownComponent(name.clone()))?;

			let component_path = self.manifest_dir.join(&config.path);
			let language = config.language.clone().unwrap_or_default();

			let offset = combined.current_position;
			let sink = build_from_directory_streaming(
				&component_path,
				self.decompose_feats,
				self.strict,
				&mut streaming_forward,
				offset,
			)?;

			tracing::info!(
				"Component '{}': {} documents, {} tokens",
				name,
				sink.document_names.len(),
				sink.current_position
			);

			let doc_start = combined.document_names.len();
			combined.merge_from(sink);
			let doc_end = combined.document_names.len();

			components.push(ComponentMeta {
				id: id as u32,
				name: name.clone(),
				language,
				document_range: (doc_start, doc_end),
			});
		}

		let mut alignments = AlignmentIndex::new();
		let mut alignment_meta = Vec::new();

		let alignment_names: Vec<String> =
			self.manifest.alignments.keys().cloned().collect();
		for alignment_name in &alignment_names {
			let config = self
				.manifest
				.alignments
				.get(alignment_name)
				.ok_or_else(|| {
					BuildError::Alignment(format!("Unknown alignment: {}", alignment_name))
				})?
				.clone();

			let source_comp = components
				.iter()
				.find(|c| c.name == config.source)
				.ok_or_else(|| {
					BuildError::Alignment(format!(
						"Source component not found: {}",
						config.source
					))
				})?;

			let target_comp = components
				.iter()
				.find(|c| c.name == config.target)
				.ok_or_else(|| {
					BuildError::Alignment(format!(
						"Target component not found: {}",
						config.target
					))
				})?;

			let edges_path = self.manifest_dir.join(&config.edges);
			let edges = if edges_path.is_dir() {
				parse_alignment_dir(
					&edges_path,
					source_comp,
					target_comp,
					&combined.document_names,
					self.strict,
				)?
			} else {
				parse_alignment_file(
					&edges_path,
					source_comp,
					target_comp,
					&combined.document_names,
					self.strict,
				)?
			};

			let meta = AlignmentMeta {
				name: alignment_name.clone(),
				source_component: config.source.clone(),
				target_component: config.target.clone(),
				source_layer: config.source_layer.clone(),
				target_layer: config.target_layer.clone(),
				directed: config.directed,
				edge_count: edges.len(),
			};

			tracing::info!(
				"Alignment '{}': {} edges ({} -> {})",
				alignment_name,
				edges.len(),
				config.source,
				config.target
			);

			alignments.add(alignment_name, edges);
			alignment_meta.push(meta);
		}

		let path = output_path.as_ref();

		let alignment_bytes = if !alignments.alignments.is_empty() {
			Some(
				bincode::serialize(&alignments).map_err(|e| {
					BuildError::Io(std::io::Error::new(std::io::ErrorKind::Other, e))
				})?,
			)
		} else {
			None
		};

		let meta = CorpusMeta {
			name: self.manifest.corpus.name.clone(),
			version: montre_index::index_version,
			token_count: combined.current_position,
			layers: combined
				.layer_indices
				.iter()
				.map(|(n, _)| n.clone())
				.collect(),
			span_layers: combined
				.spans
				.layers()
				.into_iter()
				.map(String::from)
				.collect(),
			document_names: combined.document_names.clone(),
			components,
			alignments: alignment_meta,
		};

		tracing::info!(
			"Writing corpus '{}': {} tokens, {} documents, {} components, {} alignments",
			meta.name,
			meta.token_count,
			meta.document_names.len(),
			meta.components.len(),
			meta.alignments.len()
		);

		combined.write(path, meta)?;
		streaming_forward.finalize(&path.join("forward.bin"))?;

		if let Some(bytes) = alignment_bytes {
			std::fs::write(path.join("alignments.bin"), bytes)?;
		}

		Ok(())
	}
}

fn build_doc_name_map(
	comp: &ComponentMeta,
	document_names: &[String],
) -> HashMap<String, u32> {
	let mut map = HashMap::new();
	for (i, doc_idx) in (comp.document_range.0..comp.document_range.1).enumerate() {
		if let Some(name) = document_names.get(doc_idx) {
			map.insert(name.clone(), i as u32);
		}
	}
	map
}

fn parse_alignment_dir(
	dir: &Path,
	source_comp: &ComponentMeta,
	target_comp: &ComponentMeta,
	document_names: &[String],
	strict: bool,
) -> Result<Vec<(UnitId, UnitId)>> {
	let mut files: Vec<_> = std::fs::read_dir(dir)
		.map_err(|e| {
			BuildError::Alignment(format!("Failed to read directory {:?}: {}", dir, e))
		})?
		.filter_map(|e| e.ok())
		.filter(|e| {
			e.path()
				.extension()
				.map(|ext| ext == "tsv")
				.unwrap_or(false)
		})
		.collect();

	files.sort_by_key(|e| e.path());

	let mut all_edges = Vec::new();
	for entry in files {
		let edges = parse_alignment_file(
			&entry.path(),
			source_comp,
			target_comp,
			document_names,
			strict,
		)?;
		all_edges.extend(edges);
	}

	Ok(all_edges)
}

fn parse_alignment_file(
	path: &Path,
	source_comp: &ComponentMeta,
	target_comp: &ComponentMeta,
	document_names: &[String],
	strict: bool,
) -> Result<Vec<(UnitId, UnitId)>> {
	let file = File::open(path).map_err(|e| {
		BuildError::Alignment(format!(
			"Failed to open alignment file {:?}: {}",
			path, e
		))
	})?;

	let source_doc_map = build_doc_name_map(source_comp, document_names);
	let target_doc_map = build_doc_name_map(target_comp, document_names);

	let reader = BufReader::new(file);
	let mut edges = Vec::new();

	for (line_num, line) in reader.lines().enumerate() {
		let line = line?;
		let line = line.trim();

		if line.is_empty() || line.starts_with('#') {
			continue;
		}

		let parts: Vec<&str> = line.split('\t').collect();
		if parts.len() < 4 {
			if strict {
				return Err(BuildError::Alignment(format!(
					"Invalid alignment line {} in {:?}: expected 4 tab-separated fields",
					line_num + 1,
					path
				)));
			}
			continue;
		}

		let source_doc_name = parts[0];
		let source_doc = match source_doc_map.get(source_doc_name) {
			Some(&idx) => idx,
			None => {
				if strict {
					return Err(BuildError::Alignment(format!(
						"Unknown source document '{}' at line {} (not in component '{}')",
						source_doc_name,
						line_num + 1,
						source_comp.name
					)));
				}
				continue;
			}
		};

		let source_unit: u32 = parts[1].parse().map_err(|_| {
			BuildError::Alignment(format!(
				"Invalid source sentence index at line {}: {}",
				line_num + 1,
				parts[1]
			))
		})?;

		let target_doc_name = parts[2];
		let target_doc = match target_doc_map.get(target_doc_name) {
			Some(&idx) => idx,
			None => {
				if strict {
					return Err(BuildError::Alignment(format!(
						"Unknown target document '{}' at line {} (not in component '{}')",
						target_doc_name,
						line_num + 1,
						target_comp.name
					)));
				}
				continue;
			}
		};

		let target_unit: u32 = parts[3].parse().map_err(|_| {
			BuildError::Alignment(format!(
				"Invalid target sentence index at line {}: {}",
				line_num + 1,
				parts[3]
			))
		})?;

		edges.push(((source_doc, source_unit), (target_doc, target_unit)));
	}

	Ok(edges)
}
