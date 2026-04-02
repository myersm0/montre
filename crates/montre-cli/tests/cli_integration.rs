use std::path::PathBuf;
use std::sync::OnceLock;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

struct TestCorpora {
	_multi_dir: TempDir,
	multi_path: PathBuf,
	_single_dir: TempDir,
	single_path: PathBuf,
}

fn testdata() -> PathBuf {
	PathBuf::from(env!("CARGO_MANIFEST_DIR"))
		.join("../../testdata")
}

fn corpora() -> &'static TestCorpora {
	static INSTANCE: OnceLock<TestCorpora> = OnceLock::new();
	INSTANCE.get_or_init(|| {
		let multi_dir = TempDir::new().unwrap();
		let multi_path = multi_dir.path().to_path_buf();
		Command::cargo_bin("montre")
			.unwrap()
			.args([
				"build",
				"-m",
				testdata().join("parallel/corpus.toml").to_str().unwrap(),
				"-o",
				multi_path.to_str().unwrap(),
				"--force",
			])
			.assert()
			.success();

		let single_dir = TempDir::new().unwrap();
		let single_path = single_dir.path().to_path_buf();
		Command::cargo_bin("montre")
			.unwrap()
			.args([
				"build",
				"-i",
				testdata().join("parallel/fr").to_str().unwrap(),
				"-o",
				single_path.to_str().unwrap(),
				"-n",
				"french-only",
				"--force",
			])
			.assert()
			.success();

		TestCorpora {
			_multi_dir: multi_dir,
			multi_path,
			_single_dir: single_dir,
			single_path,
		}
	})
}

fn montre() -> Command {
	Command::cargo_bin("montre").unwrap()
}

// ── count ──

#[test]
fn count_bare() {
	let c = corpora();
	montre()
		.args(["count", c.multi_path.to_str().unwrap(), r#"[pos="NOUN"]"#])
		.assert()
		.success()
		.stdout("12\n");
}

#[test]
fn count_with_component() {
	let c = corpora();
	montre()
		.args([
			"count",
			c.multi_path.to_str().unwrap(),
			r#"[pos="NOUN"]"#,
			"--component",
			"fr",
		])
		.assert()
		.success()
		.stdout("6\n");
}

#[test]
fn count_with_document() {
	let c = corpora();
	montre()
		.args([
			"count",
			c.multi_path.to_str().unwrap(),
			r#"[pos="NOUN"]"#,
			"--document",
			"le_chat.conllu",
		])
		.assert()
		.success()
		.stdout("2\n");
}

#[test]
fn count_single_component() {
	let c = corpora();
	montre()
		.args(["count", c.single_path.to_str().unwrap(), r#"[pos="NOUN"]"#])
		.assert()
		.success()
		.stdout("6\n");
}

#[test]
fn count_by_component() {
	let c = corpora();
	let output = montre()
		.args([
			"count",
			c.multi_path.to_str().unwrap(),
			r#"[pos="NOUN"]"#,
			"--by-component",
		])
		.assert()
		.success();

	let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
	let lines: Vec<&str> = stdout.lines().collect();
	assert_eq!(lines.len(), 2);
	for line in &lines {
		let cols: Vec<&str> = line.split('\t').collect();
		assert_eq!(cols.len(), 2);
	}
	assert!(stdout.contains("fr\t6"));
	assert!(stdout.contains("en\t6"));
}

#[test]
fn count_by_component_single() {
	let c = corpora();
	let output = montre()
		.args([
			"count",
			c.single_path.to_str().unwrap(),
			r#"[pos="NOUN"]"#,
			"--by-component",
		])
		.assert()
		.success();

	let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
	let lines: Vec<&str> = stdout.lines().collect();
	assert_eq!(lines.len(), 1);
	assert!(stdout.contains("french-only\t6"));
}

#[test]
fn count_by_document() {
	let c = corpora();
	let output = montre()
		.args([
			"count",
			c.multi_path.to_str().unwrap(),
			r#"[pos="NOUN"]"#,
			"--by-document",
		])
		.assert()
		.success();

	let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
	let lines: Vec<&str> = stdout.lines().collect();
	assert_eq!(lines.len(), 4);
	for line in &lines {
		let cols: Vec<&str> = line.split('\t').collect();
		assert_eq!(cols.len(), 3, "expected 3 columns: {}", line);
	}
	assert!(stdout.contains("fr\tle_chat.conllu\t2"));
	assert!(stdout.contains("fr\tla_maison.conllu\t4"));
	assert!(stdout.contains("en\tthe_cat.conllu\t2"));
	assert!(stdout.contains("en\tthe_house.conllu\t4"));
}

#[test]
fn count_by_document_single() {
	let c = corpora();
	let output = montre()
		.args([
			"count",
			c.single_path.to_str().unwrap(),
			r#"[pos="NOUN"]"#,
			"--by-document",
		])
		.assert()
		.success();

	let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
	let lines: Vec<&str> = stdout.lines().collect();
	assert_eq!(lines.len(), 2);
	for line in &lines {
		let cols: Vec<&str> = line.split('\t').collect();
		assert_eq!(cols.len(), 3, "expected 3 columns: {}", line);
	}
	assert!(stdout.contains("french-only\t"));
}

#[test]
fn count_by_document_with_component_filter() {
	let c = corpora();
	let output = montre()
		.args([
			"count",
			c.multi_path.to_str().unwrap(),
			r#"[pos="NOUN"]"#,
			"--by-document",
			"--component",
			"en",
		])
		.assert()
		.success();

	let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
	let lines: Vec<&str> = stdout.lines().collect();
	assert_eq!(lines.len(), 2);
	assert!(!stdout.contains("fr\t"));
	assert!(stdout.contains("en\tthe_cat.conllu\t2"));
	assert!(stdout.contains("en\tthe_house.conllu\t4"));
}

// ── components ──

#[test]
fn components_multi() {
	let c = corpora();
	let output = montre()
		.args(["components", c.multi_path.to_str().unwrap()])
		.assert()
		.success();

	let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
	let lines: Vec<&str> = stdout.lines().collect();
	assert_eq!(lines.len(), 2);
	assert!(stdout.contains("fr\tfr"));
	assert!(stdout.contains("en\ten"));
}

#[test]
fn components_single() {
	let c = corpora();
	let output = montre()
		.args(["components", c.single_path.to_str().unwrap()])
		.assert()
		.success();

	let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
	let lines: Vec<&str> = stdout.lines().collect();
	assert_eq!(lines.len(), 1);
	assert!(stdout.contains("french-only"));
}

// ── layers ──

#[test]
fn layers_lists_all() {
	let c = corpora();
	let output = montre()
		.args(["layers", c.multi_path.to_str().unwrap()])
		.assert()
		.success();

	let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
	let lines: Vec<&str> = stdout.lines().collect();
	assert!(lines.contains(&"word"));
	assert!(lines.contains(&"lemma"));
	assert!(lines.contains(&"upos"));
}

// ── vocab ──

#[test]
fn vocab_bare_lists_values() {
	let c = corpora();
	let output = montre()
		.args(["vocab", c.multi_path.to_str().unwrap(), "pos"])
		.assert()
		.success();

	let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
	let lines: Vec<&str> = stdout.lines().collect();
	assert!(lines.contains(&"NOUN"));
	assert!(lines.contains(&"DET"));
	assert!(lines.contains(&"VERB"));
	assert!(lines.contains(&"ADJ"));
	for line in &lines {
		assert!(!line.contains('\t'), "bare vocab should not have counts: {}", line);
	}
}

#[test]
fn vocab_sorted_alphabetically() {
	let c = corpora();
	let output = montre()
		.args(["vocab", c.multi_path.to_str().unwrap(), "pos"])
		.assert()
		.success();

	let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
	let lines: Vec<&str> = stdout.lines().collect();
	let mut sorted = lines.clone();
	sorted.sort();
	assert_eq!(lines, sorted);
}

#[test]
fn vocab_component_filter() {
	let c = corpora();
	let fr_output = montre()
		.args([
			"vocab",
			c.multi_path.to_str().unwrap(),
			"lemma",
			"--component",
			"fr",
		])
		.assert()
		.success();

	let en_output = montre()
		.args([
			"vocab",
			c.multi_path.to_str().unwrap(),
			"lemma",
			"--component",
			"en",
		])
		.assert()
		.success();

	let fr_stdout = String::from_utf8(fr_output.get_output().stdout.clone()).unwrap();
	let en_stdout = String::from_utf8(en_output.get_output().stdout.clone()).unwrap();

	let fr_lines: Vec<&str> = fr_stdout.lines().collect();
	let en_lines: Vec<&str> = en_stdout.lines().collect();

	assert!(fr_lines.contains(&"maison"));
	assert!(!en_lines.contains(&"maison"));
	assert!(en_lines.contains(&"house"));
	assert!(!fr_lines.contains(&"house"));
}

#[test]
fn vocab_document_filter() {
	let c = corpora();
	let output = montre()
		.args([
			"vocab",
			c.multi_path.to_str().unwrap(),
			"lemma",
			"--document",
			"le_chat.conllu",
		])
		.assert()
		.success();

	let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
	let lines: Vec<&str> = stdout.lines().collect();
	assert!(lines.contains(&"chat"));
	assert!(!lines.contains(&"maison"));
}

#[test]
fn vocab_unknown_layer_fails() {
	let c = corpora();
	montre()
		.args(["vocab", c.multi_path.to_str().unwrap(), "nonexistent"])
		.assert()
		.failure()
		.stderr(predicate::str::contains("not found"));
}

// ── docs ──

#[test]
fn docs_multi_component_format() {
	let c = corpora();
	let output = montre()
		.args(["docs", c.multi_path.to_str().unwrap()])
		.assert()
		.success();

	let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
	let lines: Vec<&str> = stdout.lines().collect();
	assert_eq!(lines.len(), 4);
	for line in &lines {
		let cols: Vec<&str> = line.split('\t').collect();
		assert_eq!(cols.len(), 2, "expected component\\tdocument: {}", line);
	}
}

#[test]
fn docs_single_component_format() {
	let c = corpora();
	let output = montre()
		.args(["docs", c.single_path.to_str().unwrap()])
		.assert()
		.success();

	let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
	let lines: Vec<&str> = stdout.lines().collect();
	assert_eq!(lines.len(), 2);
	for line in &lines {
		let cols: Vec<&str> = line.split('\t').collect();
		assert_eq!(cols.len(), 2, "expected component\\tdocument: {}", line);
		assert_eq!(cols[0], "french-only");
	}
}

#[test]
fn docs_component_filter() {
	let c = corpora();
	let output = montre()
		.args(["docs", c.multi_path.to_str().unwrap(), "--component", "fr"])
		.assert()
		.success();

	let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
	let lines: Vec<&str> = stdout.lines().collect();
	assert_eq!(lines.len(), 2);
	for line in &lines {
		assert!(line.starts_with("fr\t"));
	}
}

#[test]
fn documents_alias_works() {
	let c = corpora();
	let output = montre()
		.args(["documents", c.multi_path.to_str().unwrap()])
		.assert()
		.success();

	let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
	let lines: Vec<&str> = stdout.lines().collect();
	assert_eq!(lines.len(), 4);
}

// ── query --count-only still works ──

#[test]
fn query_count_only_unchanged() {
	let c = corpora();
	montre()
		.args([
			"query",
			c.multi_path.to_str().unwrap(),
			r#"[pos="NOUN"]"#,
			"--count-only",
		])
		.assert()
		.success()
		.stdout(predicate::str::starts_with("12\n"));
}
