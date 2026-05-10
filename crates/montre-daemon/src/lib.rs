pub mod client;
pub mod protocol;
mod rpc;
mod state;

use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::channel;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

pub use client::DaemonClient;

#[derive(Debug, Clone)]
pub struct ServeOptions {
	pub corpus_path: PathBuf,
	pub socket_path: Option<PathBuf>,
	pub idle_timeout: Option<Duration>,
}

#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
	#[error("not yet implemented")]
	NotImplemented,
	#[error("failed to open corpus: {0}")]
	CorpusLoad(#[from] montre_index::IndexError),
	#[error("io error: {0}")]
	Io(#[from] std::io::Error),
}

pub fn serve(options: ServeOptions) -> Result<(), DaemonError> {
	tracing::info!(corpus = %options.corpus_path.display(), "opening corpus");
	let corpus = Arc::new(montre_index::open(&options.corpus_path)?);

	let canonical = std::fs::canonicalize(&options.corpus_path)?;
	let corpus_id = derive_corpus_id(&canonical);

	let socket_path = match &options.socket_path {
		Some(p) => p.clone(),
		None => default_socket_path(&canonical)?,
	};

	let daemon_epoch = 1;
	let state = state::State::new(corpus_id, daemon_epoch);
	let results = state.results();

	let (state_tx, state_rx) = channel();

	let state_thread = thread::spawn(move || state::run(state, state_rx));

	let listener_result = rpc::run_listener(&socket_path, state_tx, corpus, results);

	let _ = state_thread.join();

	let _ = std::fs::remove_file(&socket_path);

	listener_result?;
	Ok(())
}

fn derive_corpus_id(canonical: &Path) -> String {
	hex_prefix(canonical, 16)
}

fn derive_socket_filename(canonical: &Path) -> String {
	format!("{}.sock", hex_prefix(canonical, 16))
}

fn hex_prefix(canonical: &Path, hex_chars: usize) -> String {
	let bytes = canonical.as_os_str().as_encoded_bytes();
	let hash = blake3::hash(bytes);
	let hash_bytes = hash.as_bytes();
	let take = hex_chars / 2;
	let mut out = String::with_capacity(hex_chars);
	for byte in &hash_bytes[..take] {
		write!(out, "{:02x}", byte).expect("write to String never fails");
	}
	out
}

fn default_socket_path(canonical: &Path) -> io::Result<PathBuf> {
	let dir = data_dir().ok_or_else(|| {
		io::Error::new(
			io::ErrorKind::NotFound,
			"unable to determine XDG data directory: HOME and XDG_DATA_HOME both unset",
		)
	})?;
	let sockets_dir = dir.join("montre/sockets");
	std::fs::create_dir_all(&sockets_dir)?;
	Ok(sockets_dir.join(derive_socket_filename(canonical)))
}

fn data_dir() -> Option<PathBuf> {
	if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
		if !xdg.is_empty() {
			return Some(PathBuf::from(xdg));
		}
	}
	std::env::var("HOME")
		.ok()
		.filter(|h| !h.is_empty())
		.map(|h| PathBuf::from(h).join(".local/share"))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn socket_filename_format() {
		let name = derive_socket_filename(Path::new("/some/canonical/path"));
		assert!(name.ends_with(".sock"));
		let stem = &name[..name.len() - ".sock".len()];
		assert_eq!(stem.len(), 16);
		assert!(stem.chars().all(|c| c.is_ascii_hexdigit()));
	}

	#[test]
	fn socket_filename_deterministic() {
		let path = Path::new("/some/canonical/path");
		assert_eq!(derive_socket_filename(path), derive_socket_filename(path));
	}

	#[test]
	fn socket_filename_distinct_for_distinct_paths() {
		let a = derive_socket_filename(Path::new("/path/one"));
		let b = derive_socket_filename(Path::new("/path/two"));
		assert_ne!(a, b);
	}

	#[test]
	fn corpus_id_format() {
		let id = derive_corpus_id(Path::new("/some/path"));
		assert_eq!(id.len(), 16);
		assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
	}

	#[test]
	fn corpus_id_distinct_for_distinct_paths() {
		let a = derive_corpus_id(Path::new("/path/one"));
		let b = derive_corpus_id(Path::new("/path/two"));
		assert_ne!(a, b);
	}
}
