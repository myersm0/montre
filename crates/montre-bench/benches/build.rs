use std::io::BufReader;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::Duration;

use criterion::{criterion_group, criterion_main, Criterion};
use walkdir::WalkDir;

use montre_build::builder::CorpusBuilder;
use montre_build::format::conllu::ConllUReader;
use montre_build::format::CorpusReader;
use montre_build::MultiCorpusBuilder;

fn conllu_path() -> Option<PathBuf> {
	match std::env::var("MONTRE_BENCH_CONLLU") {
		Ok(p) => {
			let path = PathBuf::from(&p);
			if path.exists() {
				Some(path)
			} else {
				eprintln!("MONTRE_BENCH_CONLLU={p} does not exist; skipping");
				None
			}
		}
		Err(_) => {
			eprintln!("MONTRE_BENCH_CONLLU not set; skipping conllu/single-component benchmarks");
			None
		}
	}
}

fn manifest_path() -> Option<PathBuf> {
	match std::env::var("MONTRE_BENCH_MANIFEST") {
		Ok(p) => {
			let path = PathBuf::from(&p);
			if path.exists() {
				Some(path)
			} else {
				eprintln!("MONTRE_BENCH_MANIFEST={p} does not exist; skipping");
				None
			}
		}
		Err(_) => {
			eprintln!("MONTRE_BENCH_MANIFEST not set; skipping multi-component build benchmark");
			None
		}
	}
}

fn conllu_files(dir: &Path) -> Vec<PathBuf> {
	let mut files: Vec<PathBuf> = WalkDir::new(dir)
		.into_iter()
		.filter_map(|e| e.ok())
		.filter(|e| {
			e.path()
				.extension()
				.map(|ext| ext == "conllu")
				.unwrap_or(false)
		})
		.map(|e| e.path().to_path_buf())
		.collect();
	files.sort();
	files
}

fn bench_conllu_parse_only(c: &mut Criterion) {
	let Some(dir) = conllu_path() else { return };
	let files = conllu_files(&dir);
	if files.is_empty() {
		eprintln!("no .conllu files in {}; skipping", dir.display());
		return;
	}

	c.bench_function("conllu_parse_only", |b| {
		b.iter(|| {
			let mut total_sentences = 0usize;
			for path in &files {
				let file = File::open(path).unwrap();
				let reader = BufReader::new(file);
				let mut conllu = ConllUReader::new(reader);
				let sentences = conllu.read_sentences().unwrap();
				total_sentences += sentences.len();
			}
			total_sentences
		})
	});
}

fn bench_build_single_component(c: &mut Criterion) {
	let Some(dir) = conllu_path() else { return };

	c.bench_function("build_single_component", |b| {
		b.iter(|| {
			let output = tempfile::tempdir().unwrap();
			CorpusBuilder::from_directory("bench-single", &dir, false, false)
				.unwrap()
				.build(output.path())
				.unwrap();
		})
	});
}

fn bench_build_multi_component(c: &mut Criterion) {
	let Some(manifest) = manifest_path() else { return };

	c.bench_function("build_multi_component", |b| {
		b.iter(|| {
			let output = tempfile::tempdir().unwrap();
			MultiCorpusBuilder::from_manifest(&manifest)
				.unwrap()
				.build(output.path())
				.unwrap();
		})
	});
}

criterion_group! {
	name = benches;
	config = Criterion::default()
		.sample_size(10)
		.measurement_time(Duration::from_secs(30));
	targets =
		bench_conllu_parse_only,
		bench_build_single_component,
		bench_build_multi_component,
}
criterion_main!(benches);
