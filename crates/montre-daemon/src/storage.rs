use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use fs4::fs_std::FileExt;

use crate::protocol::{ResultForm, ResultHandle, ResultMetadata};

pub(crate) struct NamedResultRecord {
	pub handle: ResultHandle,
	pub cql: String,
	pub hit_count: u64,
	pub created_at: String,
}

#[derive(Debug)]
pub(crate) struct DaemonLock {
	_file: File,
}

pub(crate) fn state_dir_for(corpus_id: &str) -> io::Result<PathBuf> {
	state_dir_under(&state_root_from_env()?, corpus_id)
}

pub(crate) fn acquire_daemon_lock(state_dir: &Path) -> io::Result<DaemonLock> {
	let path = state_dir.join("daemon.lock");
	let file = OpenOptions::new()
		.create(true)
		.write(true)
		.read(true)
		.truncate(false)
		.open(&path)?;
	if file.try_lock_exclusive()? {
		Ok(DaemonLock { _file: file })
	} else {
		Err(io::Error::new(
			io::ErrorKind::WouldBlock,
			"daemon lock already held for this corpus",
		))
	}
}

pub(crate) fn daemon_lock_held(state_dir: &Path) -> io::Result<bool> {
	let path = state_dir.join("daemon.lock");
	let file = OpenOptions::new()
		.create(true)
		.write(true)
		.read(true)
		.truncate(false)
		.open(&path)?;
	match file.try_lock_exclusive()? {
		true => Ok(false),
		false => Ok(true),
	}
}

fn state_dir_under(root: &Path, corpus_id: &str) -> io::Result<PathBuf> {
	let dir = root.join(corpus_id);
	std::fs::create_dir_all(&dir)?;
	Ok(dir)
}

fn state_root_from_env() -> io::Result<PathBuf> {
	if let Ok(xdg) = std::env::var("XDG_STATE_HOME") {
		if !xdg.is_empty() {
			return Ok(PathBuf::from(xdg).join("montre"));
		}
	}
	let home = std::env::var("HOME")
		.ok()
		.filter(|h| !h.is_empty())
		.map(PathBuf::from)
		.ok_or_else(|| {
			io::Error::new(
				io::ErrorKind::NotFound,
				"unable to determine state directory: HOME and XDG_STATE_HOME both unset",
			)
		})?;
	Ok(home.join(".local/share/montre/state"))
}

pub(crate) fn load_and_bump_epoch(state_dir: &Path) -> io::Result<u64> {
	let path = state_dir.join("epoch");
	let current = match std::fs::read_to_string(&path) {
		Ok(text) => text.trim().parse::<u64>().map_err(|error| {
			io::Error::new(
				io::ErrorKind::InvalidData,
				format!("epoch file contains invalid integer: {}", error),
			)
		})?,
		Err(error) if error.kind() == io::ErrorKind::NotFound => 0,
		Err(error) => return Err(error),
	};
	let next = current.checked_add(1).ok_or_else(|| {
		io::Error::new(io::ErrorKind::InvalidData, "epoch counter overflow")
	})?;
	write_atomic(&path, next.to_string().as_bytes())?;
	Ok(next)
}

pub(crate) fn named_results_path(state_dir: &Path) -> PathBuf {
	state_dir.join("named_results.jsonl")
}

pub(crate) fn persist_named_results<'a, I>(
	state_dir: &Path,
	corpus_id: &str,
	records: I,
) -> io::Result<()>
where
	I: IntoIterator<Item = (&'a str, &'a NamedResultRecord)>,
{
	let mut content = String::new();
	for (name, record) in records {
		let metadata = ResultMetadata {
			handle: record.handle.clone(),
			query: record.cql.clone(),
			created_at: record.created_at.clone(),
			materialized_at: None,
			hit_count: record.hit_count,
			corpus_id: corpus_id.to_string(),
			name: Some(name.to_string()),
			form: ResultForm::QueryBacked,
		};
		let line = serde_json::to_string(&metadata).map_err(|error| {
			io::Error::new(io::ErrorKind::InvalidData, error)
		})?;
		content.push_str(&line);
		content.push('\n');
	}
	write_atomic(&named_results_path(state_dir), content.as_bytes())
}

pub(crate) fn load_named_results(
	state_dir: &Path,
) -> io::Result<HashMap<String, NamedResultRecord>> {
	let path = named_results_path(state_dir);
	let content = match std::fs::read_to_string(&path) {
		Ok(text) => text,
		Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(HashMap::new()),
		Err(error) => return Err(error),
	};
	let mut out = HashMap::new();
	for (index, line) in content.lines().enumerate() {
		let trimmed = line.trim();
		if trimmed.is_empty() {
			continue;
		}
		let metadata: ResultMetadata = match serde_json::from_str(trimmed) {
			Ok(value) => value,
			Err(error) => {
				tracing::warn!(
					line = index + 1,
					error = %error,
					"skipping malformed named-results line",
				);
				continue;
			}
		};
		let Some(name) = metadata.name else {
			tracing::warn!(
				line = index + 1,
				"skipping named-results line with no name",
			);
			continue;
		};
		out.insert(
			name,
			NamedResultRecord {
				handle: metadata.handle,
				cql: metadata.query,
				hit_count: metadata.hit_count,
				created_at: metadata.created_at,
			},
		);
	}
	Ok(out)
}

pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
	let temporary_path = path.with_extension("tmp");
	{
		let mut file = std::fs::File::create(&temporary_path)?;
		file.write_all(bytes)?;
		file.sync_all()?;
	}
	std::fs::rename(&temporary_path, path)?;
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;
	use tempfile::TempDir;

	#[test]
	fn state_dir_under_creates_per_corpus_directory() {
		let temp = TempDir::new().expect("tempdir");
		let dir = state_dir_under(temp.path(), "abc123").expect("state_dir_under");
		assert!(dir.is_dir());
		assert_eq!(dir, temp.path().join("abc123"));
	}

	#[test]
	fn state_dir_under_is_idempotent() {
		let temp = TempDir::new().expect("tempdir");
		let first = state_dir_under(temp.path(), "abc123").expect("first");
		let second = state_dir_under(temp.path(), "abc123").expect("second");
		assert_eq!(first, second);
		assert!(first.is_dir());
	}

	#[test]
	fn acquire_daemon_lock_creates_lockfile() {
		let temp = TempDir::new().expect("tempdir");
		let lock = acquire_daemon_lock(temp.path()).expect("acquire");
		assert!(temp.path().join("daemon.lock").exists());
		drop(lock);
	}

	#[test]
	fn acquire_daemon_lock_blocks_while_held() {
		let temp = TempDir::new().expect("tempdir");
		let first = acquire_daemon_lock(temp.path()).expect("first");
		let error = acquire_daemon_lock(temp.path())
			.expect_err("second should fail while first held");
		assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
		drop(first);
	}

	#[test]
	fn acquire_daemon_lock_available_after_release() {
		let temp = TempDir::new().expect("tempdir");
		let first = acquire_daemon_lock(temp.path()).expect("first");
		drop(first);
		let second = acquire_daemon_lock(temp.path()).expect("second after release");
		drop(second);
	}

	#[test]
	fn daemon_lock_held_reflects_acquisition_state() {
		let temp = TempDir::new().expect("tempdir");
		assert!(!daemon_lock_held(temp.path()).expect("probe before acquire"));
		let held = acquire_daemon_lock(temp.path()).expect("acquire");
		assert!(daemon_lock_held(temp.path()).expect("probe while held"));
		drop(held);
		assert!(!daemon_lock_held(temp.path()).expect("probe after release"));
	}

	#[test]
	fn load_and_bump_epoch_starts_at_one() {
		let temp = TempDir::new().expect("tempdir");
		let value = load_and_bump_epoch(temp.path()).expect("bump");
		assert_eq!(value, 1);
	}

	#[test]
	fn load_and_bump_epoch_increments_across_calls() {
		let temp = TempDir::new().expect("tempdir");
		assert_eq!(load_and_bump_epoch(temp.path()).unwrap(), 1);
		assert_eq!(load_and_bump_epoch(temp.path()).unwrap(), 2);
		assert_eq!(load_and_bump_epoch(temp.path()).unwrap(), 3);
	}

	#[test]
	fn load_and_bump_epoch_persists_to_file() {
		let temp = TempDir::new().expect("tempdir");
		load_and_bump_epoch(temp.path()).unwrap();
		let contents = std::fs::read_to_string(temp.path().join("epoch")).unwrap();
		assert_eq!(contents.trim(), "1");
	}

	#[test]
	fn load_and_bump_epoch_resumes_from_existing_value() {
		let temp = TempDir::new().expect("tempdir");
		std::fs::write(temp.path().join("epoch"), "16").unwrap();
		assert_eq!(load_and_bump_epoch(temp.path()).unwrap(), 17);
	}

	#[test]
	fn load_and_bump_epoch_rejects_corrupted_file() {
		let temp = TempDir::new().expect("tempdir");
		std::fs::write(temp.path().join("epoch"), "not-a-number").unwrap();
		let result = load_and_bump_epoch(temp.path());
		assert!(result.is_err());
		assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
	}

	#[test]
	fn write_atomic_overwrites_stale_temporary_file() {
		let temp = TempDir::new().expect("tempdir");
		let path = temp.path().join("epoch");
		let temporary_path = temp.path().join("epoch.tmp");
		std::fs::write(&temporary_path, b"garbage").unwrap();
		write_atomic(&path, b"42").unwrap();
		assert_eq!(std::fs::read_to_string(&path).unwrap(), "42");
		assert!(!temporary_path.exists());
	}

	fn record(handle: &str, cql: &str, hit_count: u64) -> NamedResultRecord {
		NamedResultRecord {
			handle: handle.to_string(),
			cql: cql.to_string(),
			hit_count,
			created_at: "2026-05-12T00:00:00Z".to_string(),
		}
	}

	#[test]
	fn load_named_results_missing_file_returns_empty_map() {
		let temp = TempDir::new().expect("tempdir");
		let loaded = load_named_results(temp.path()).expect("load");
		assert!(loaded.is_empty());
	}

	#[test]
	fn persist_and_load_named_results_roundtrip() {
		let temp = TempDir::new().expect("tempdir");
		let alpha = record("r-alpha", "[pos=\"NOUN\"]", 100);
		let beta = record("r-beta", "[pos=\"ADJ\"]", 50);
		let snapshot = vec![("alpha", &alpha), ("beta", &beta)];
		persist_named_results(temp.path(), "corpus-id", snapshot).expect("persist");

		let loaded = load_named_results(temp.path()).expect("load");
		assert_eq!(loaded.len(), 2);
		assert_eq!(loaded.get("alpha").unwrap().handle, "r-alpha");
		assert_eq!(loaded.get("alpha").unwrap().cql, "[pos=\"NOUN\"]");
		assert_eq!(loaded.get("alpha").unwrap().hit_count, 100);
		assert_eq!(loaded.get("beta").unwrap().handle, "r-beta");
	}

	#[test]
	fn persist_named_results_empty_iter_truncates_file() {
		let temp = TempDir::new().expect("tempdir");
		let alpha = record("r-alpha", "[pos=\"NOUN\"]", 100);
		persist_named_results(temp.path(), "corpus-id", vec![("alpha", &alpha)]).unwrap();
		persist_named_results::<std::iter::Empty<_>>(
			temp.path(),
			"corpus-id",
			std::iter::empty(),
		)
		.unwrap();
		let loaded = load_named_results(temp.path()).unwrap();
		assert!(loaded.is_empty());
	}

	#[test]
	fn load_named_results_skips_malformed_lines() {
		let temp = TempDir::new().expect("tempdir");
		let path = named_results_path(temp.path());
		let good = serde_json::to_string(&ResultMetadata {
			handle: "r-good".to_string(),
			query: "[]".to_string(),
			created_at: "2026-05-12T00:00:00Z".to_string(),
			materialized_at: None,
			hit_count: 1,
			corpus_id: "c".to_string(),
			name: Some("good".to_string()),
			form: ResultForm::QueryBacked,
		})
		.unwrap();
		let content = format!("{}\n{{not valid json\n\n{}\n", good, good.replace("good", "good2"));
		std::fs::write(&path, content).unwrap();
		let loaded = load_named_results(temp.path()).unwrap();
		assert_eq!(loaded.len(), 2);
		assert!(loaded.contains_key("good"));
		assert!(loaded.contains_key("good2"));
	}

	#[test]
	fn load_named_results_skips_lines_without_name() {
		let temp = TempDir::new().expect("tempdir");
		let path = named_results_path(temp.path());
		let anonymous = serde_json::to_string(&ResultMetadata {
			handle: "r-anon".to_string(),
			query: "[]".to_string(),
			created_at: "2026-05-12T00:00:00Z".to_string(),
			materialized_at: None,
			hit_count: 1,
			corpus_id: "c".to_string(),
			name: None,
			form: ResultForm::QueryBacked,
		})
		.unwrap();
		std::fs::write(&path, format!("{}\n", anonymous)).unwrap();
		let loaded = load_named_results(temp.path()).unwrap();
		assert!(loaded.is_empty());
	}

	#[test]
	fn persist_named_results_writes_atomically_via_temporary() {
		let temp = TempDir::new().expect("tempdir");
		let alpha = record("r-alpha", "[]", 1);
		let path = named_results_path(temp.path());
		std::fs::write(path.with_extension("tmp"), b"garbage").unwrap();
		persist_named_results(temp.path(), "c", vec![("alpha", &alpha)]).unwrap();
		assert!(!path.with_extension("tmp").exists());
		assert!(path.exists());
	}

	#[test]
	fn load_skips_truncated_last_line_from_crash_mid_write() {
		let temp = TempDir::new().expect("tempdir");
		let path = named_results_path(temp.path());
		let good = serde_json::to_string(&ResultMetadata {
			handle: "r-alpha".to_string(),
			query: "[]".to_string(),
			created_at: "2026-05-12T00:00:00Z".to_string(),
			materialized_at: None,
			hit_count: 1,
			corpus_id: "c".to_string(),
			name: Some("alpha".to_string()),
			form: ResultForm::QueryBacked,
		})
		.unwrap();
		let truncated = r#"{"handle":"r-trunc","query":"[]","created_at":"2026"#;
		let content = format!("{}\n{}", good, truncated);
		std::fs::write(&path, content).unwrap();

		let loaded = load_named_results(temp.path()).expect("load");
		assert_eq!(loaded.len(), 1, "good prefix record should load, truncated last line skipped");
		assert!(loaded.contains_key("alpha"));
	}

	#[test]
	fn load_is_unaffected_by_orphan_temporary_file() {
		let temp = TempDir::new().expect("tempdir");
		let alpha = record("r-alpha", "[]", 1);
		persist_named_results(temp.path(), "c", vec![("alpha", &alpha)]).unwrap();

		let temporary_path = named_results_path(temp.path()).with_extension("tmp");
		std::fs::write(&temporary_path, b"completely different garbage").unwrap();

		let loaded = load_named_results(temp.path()).expect("load");
		assert_eq!(loaded.len(), 1, "canonical content should load; orphan .tmp ignored");
		assert!(loaded.contains_key("alpha"));
	}
}
