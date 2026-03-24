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
	assert!(lines.contains(&"pos"));
}

// ── vocab ──

#[test]
fn vocab_pos_layer() {
	let c = corpora();
	let output = montre()
		.args(["vocab", c.multi_path.to_str().unwrap(), "pos"])
		.assert()
		.success();

	let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
	assert!(stdout.contains("NOUN\t"));
	assert!(stdout.contains("DET\t"));
	assert!(stdout.contains("VERB\t"));
	assert!(stdout.contains("ADJ\t"));

	let lines: Vec<&str> = stdout.lines().collect();
	for line in &lines {
		let cols: Vec<&str> = line.split('\t').collect();
		assert_eq!(cols.len(), 2, "expected value\\tcount: {}", line);
	}
}

#[test]
fn vocab_sorted_descending() {
	let c = corpora();
	let output = montre()
		.args(["vocab", c.multi_path.to_str().unwrap(), "pos"])
		.assert()
		.success();

	let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
	let counts: Vec<u64> = stdout
		.lines()
		.filter_map(|line| line.split('\t').nth(1))
		.filter_map(|c| c.parse().ok())
		.collect();
	for window in counts.windows(2) {
		assert!(window[0] >= window[1], "not sorted descending: {:?}", counts);
	}
}

#[test]
fn vocab_with_top() {
	let c = corpora();
	let output = montre()
		.args(["vocab", c.multi_path.to_str().unwrap(), "pos", "--top", "3"])
		.assert()
		.success();

	let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
	let lines: Vec<&str> = stdout.lines().collect();
	assert_eq!(lines.len(), 3);
}

#[test]
fn vocab_all_flag() {
	let c = corpora();
	let all_output = montre()
		.args(["vocab", c.multi_path.to_str().unwrap(), "pos", "--all"])
		.assert()
		.success();

	let all_stdout = String::from_utf8(all_output.get_output().stdout.clone()).unwrap();
	let all_count = all_stdout.lines().count();

	let top_output = montre()
		.args(["vocab", c.multi_path.to_str().unwrap(), "pos", "--top", "2"])
		.assert()
		.success();

	let top_stdout = String::from_utf8(top_output.get_output().stdout.clone()).unwrap();
	let top_count = top_stdout.lines().count();

	assert!(all_count > top_count);
}

#[test]
fn vocab_component_filter() {
	let c = corpora();
	let fr_output = montre()
		.args([
			"vocab",
			c.multi_path.to_str().unwrap(),
			"pos",
			"--component",
			"fr",
			"--all",
		])
		.assert()
		.success();

	let en_output = montre()
		.args([
			"vocab",
			c.multi_path.to_str().unwrap(),
			"pos",
			"--component",
			"en",
			"--all",
		])
		.assert()
		.success();

	let fr_stdout = String::from_utf8(fr_output.get_output().stdout.clone()).unwrap();
	let en_stdout = String::from_utf8(en_output.get_output().stdout.clone()).unwrap();

	let fr_noun: u64 = fr_stdout
		.lines()
		.find(|l| l.starts_with("NOUN\t"))
		.and_then(|l| l.split('\t').nth(1))
		.and_then(|c| c.parse().ok())
		.unwrap();

	let en_noun: u64 = en_stdout
		.lines()
		.find(|l| l.starts_with("NOUN\t"))
		.and_then(|l| l.split('\t').nth(1))
		.and_then(|c| c.parse().ok())
		.unwrap();

	assert_eq!(fr_noun, 6);
	assert_eq!(en_noun, 6);
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
