use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub(crate) fn state_dir_for(corpus_id: &str) -> io::Result<PathBuf> {
	state_dir_under(&state_root_from_env()?, corpus_id)
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

fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
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
}
