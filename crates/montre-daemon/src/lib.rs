pub mod client;
mod rpc;
mod state;

use std::path::PathBuf;
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
	#[error("daemon not yet implemented")]
	NotImplemented,
}

pub fn serve(options: ServeOptions) -> Result<(), DaemonError> {
	tracing::warn!(
		corpus = %options.corpus_path.display(),
		"montre serve: scaffold only — protocol implementation pending",
	);
	Err(DaemonError::NotImplemented)
}
