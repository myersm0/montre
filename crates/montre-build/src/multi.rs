use std::fs::File;
use std::io::BufRead;
use std::io::BufReader;
use std::path::Path;

use montre_core::UnitId;
use montre_index::corpus::{AlignmentIndex, AlignmentMeta, ComponentMeta, CorpusMeta};
use montre_index::ForwardIndex;
use montre_index::SpanIndex;
use walkdir::WalkDir;

use crate::builder::IndexSink;
use crate::format::conllu::ConllUReader;
use crate::format::CorpusReader;
use crate::manifest::Manifest;
use crate::{BuildError, Result};

pub struct MultiCorpusBuilder {
	manifest: Manifest,
	manifest_dir: std::path::PathBuf,
	sink: IndexSink,
	components: Vec<ComponentMeta>,
	alignments: AlignmentIndex,
	alignment_meta: Vec<AlignmentMeta>,
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

		Ok(Self {
			manifest,
			manifest_dir,
			sink: IndexSink::new(),
			components: Vec::new(),
			alignments: AlignmentIndex::new(),
			alignment_meta: Vec::new(),
			strict: false,
		})
	}

	pub fn strict(mut self, strict: bool) -> Self {
		self.strict = strict;
		self
	}

	pub fn build(mut self, output_path: impl AsRef<Path>) -> Result<()> {
		let component_names: Vec<String> = self.manifest.components.keys().cloned().collect();

		for component_name in &component_names {
			self.build_component(component_name)?;
		}

		let alignment_names: Vec<String> = self.manifest.alignments.keys().cloned().collect();
		for alignment_name in &alignment_names {
			self.ingest_alignment(alignment_name)?;
		}

		self.write(output_path)
	}

	fn build_component(&mut self, name: &str) -> Result<()> {
		let config = self
			.manifest
			.components
			.get(name)
			.ok_or_else(|| BuildError::UnknownComponent(name.to_string()))?;

		let component_path = self.manifest_dir.join(&config.path);
		let language = config.language.clone().unwrap_or_default();
		let doc_start = self.sink.document_names.len();

		let mut files: Vec<_> = WalkDir::new(&component_path)
			.into_iter()
			.filter_map(|e| e.ok())
			.filter(|e| {
				e.path()
					.extension()
					.map(|ext| ext == "conllu")
					.unwrap_or(false)
			})
			.collect();

		files.sort_by_key(|e| e.path().to_path_buf());

		for entry in files {
			let file = File::open(entry.path())?;
			let reader = BufReader::new(file);
			let mut conllu = ConllUReader::new(reader);

			let result = if self.strict {
				conllu.read_sentences_strict()
			} else {
				conllu.read_sentences()
			};

			match result {
				Ok(sentences) => {
					let doc_name = entry
						.path()
						.file_name()
						.map(|s| s.to_string_lossy().to_string())
						.unwrap_or_else(|| "unknown".to_string());

					self.sink.add_document(&doc_name, sentences);
				}
				Err(e) => {
					if self.strict {
						return Err(e);
					} else {
						tracing::warn!("Skipping {:?}: {}", entry.path(), e);
					}
				}
			}
		}

		let doc_end = self.sink.document_names.len();

		let component_meta = ComponentMeta {
			id: self.components.len() as u32,
			name: name.to_string(),
			language,
			document_range: (doc_start, doc_end),
		};

		tracing::info!(
			"Component '{}': {} documents, {} tokens",
			name,
			doc_end - doc_start,
			self.sink.current_position
		);

		self.components.push(component_meta);
		Ok(())
	}

	fn ingest_alignment(&mut self, name: &str) -> Result<()> {
		let config = self
			.manifest
			.alignments
			.get(name)
			.ok_or_else(|| BuildError::Alignment(format!("Unknown alignment: {}", name)))?
			.clone();

		let source_comp = self
			.components
			.iter()
			.find(|c| c.name == config.source)
			.ok_or_else(|| {
				BuildError::Alignment(format!("Source component not found: {}", config.source))
			})?;

		let target_comp = self
			.components
			.iter()
			.find(|c| c.name == config.target)
			.ok_or_else(|| {
				BuildError::Alignment(format!("Target component not found: {}", config.target))
			})?;

		let edges_path = self.manifest_dir.join(&config.edges);
		let edges = if edges_path.is_dir() {
			self.parse_alignment_dir(&edges_path, source_comp, target_comp)?
		} else {
			self.parse_alignment_file(&edges_path, source_comp, target_comp)?
		};

		let meta = AlignmentMeta {
			name: name.to_string(),
			source_component: config.source.clone(),
			target_component: config.target.clone(),
			source_layer: config.source_layer.clone(),
			target_layer: config.target_layer.clone(),
			directed: config.directed,
			edge_count: edges.len(),
		};

		tracing::info!(
			"Alignment '{}': {} edges ({} -> {})",
			name,
			edges.len(),
			config.source,
			config.target
		);

		self.alignments.add(name, edges);
		self.alignment_meta.push(meta);

		Ok(())
	}

	fn parse_alignment_dir(
		&self,
		dir: &Path,
		source_comp: &ComponentMeta,
		target_comp: &ComponentMeta,
	) -> Result<Vec<(UnitId, UnitId)>> {
		let mut all_edges = Vec::new();

		let mut files: Vec<_> = std::fs::read_dir(dir)
			.map_err(|e| BuildError::Alignment(format!("Failed to read directory {:?}: {}", dir, e)))?
			.filter_map(|e| e.ok())
			.filter(|e| {
				e.path()
					.extension()
					.map(|ext| ext == "tsv")
					.unwrap_or(false)
			})
			.collect();

		files.sort_by_key(|e| e.path());

		for entry in files {
			let edges = self.parse_alignment_file(&entry.path(), source_comp, target_comp)?;
			all_edges.extend(edges);
		}

		Ok(all_edges)
	}

	fn parse_alignment_file(
		&self,
		path: &Path,
		source_comp: &ComponentMeta,
		target_comp: &ComponentMeta,
	) -> Result<Vec<(UnitId, UnitId)>> {
		let file = File::open(path).map_err(|e| {
			BuildError::Alignment(format!("Failed to open alignment file {:?}: {}", path, e))
		})?;

		let source_doc_map = self.build_doc_name_map(source_comp);
		let target_doc_map = self.build_doc_name_map(target_comp);

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
				if self.strict {
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
					if self.strict {
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
					if self.strict {
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

	fn build_doc_name_map(&self, comp: &ComponentMeta) -> std::collections::HashMap<String, u32> {
		let mut map = std::collections::HashMap::new();
		for (i, doc_idx) in (comp.document_range.0..comp.document_range.1).enumerate() {
			if let Some(name) = self.sink.document_names.get(doc_idx) {
				map.insert(name.clone(), i as u32);
			}
		}
		map
	}

	fn write(self, output_path: impl AsRef<Path>) -> Result<()> {
		let path = output_path.as_ref();

		let alignment_bytes = if !self.alignments.alignments.is_empty() {
			Some(
				bincode::serialize(&self.alignments)
					.map_err(|e| BuildError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?,
			)
		} else {
			None
		};

		let meta = CorpusMeta {
			name: self.manifest.corpus.name.clone(),
			version: montre_index::index_version,
			token_count: self.sink.forward.token_count(),
			layers: self.sink.layer_indices.iter().map(|(n, _)| n.clone()).collect(),
			span_layers: self.sink.spans.layers().into_iter().map(String::from).collect(),
			document_names: self.sink.document_names.clone(),
			components: self.components,
			alignments: self.alignment_meta,
		};

		tracing::info!(
			"Wrote corpus '{}': {} tokens, {} documents, {} components, {} alignments",
			meta.name,
			meta.token_count,
			meta.document_names.len(),
			meta.components.len(),
			meta.alignments.len()
		);

		self.sink.write(path, meta)?;

		if let Some(bytes) = alignment_bytes {
			std::fs::write(path.join("alignments.bin"), bytes)?;
		}

		Ok(())
	}
}
