pub mod client;
pub mod protocol;
mod rpc;
mod state;

use std::path::PathBuf;
use std::sync::Arc;
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
	tracing::info!(
		corpus = %options.corpus_path.display(),
		"opening corpus",
	);
	let corpus = Arc::new(montre_index::open(&options.corpus_path)?);
	let daemon_epoch = 1;
	let _ = state::State::new(corpus, daemon_epoch);
	tracing::warn!(
		"phase (b) scaffold: corpus loaded, state core present; RPC not yet wired",
	);
	Err(DaemonError::NotImplemented)
}
