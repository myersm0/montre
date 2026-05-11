use std::path::PathBuf;
use std::process;

use montre_daemon::{serve, ServeOptions};

fn main() {
	let args: Vec<String> = std::env::args().collect();
	if args.len() < 2 {
		eprintln!("usage: serve_local <corpus-path> [socket-path]");
		eprintln!();
		eprintln!("starts the daemon on the given corpus directory.");
		eprintln!("default socket path: /tmp/montre-daemon.sock");
		eprintln!();
		eprintln!("example:");
		eprintln!("    cargo run --example serve_local -p montre-daemon -- ./my-corpus");
		process::exit(1);
	}

	let corpus_path = PathBuf::from(&args[1]);
	let socket_path = args
		.get(2)
		.map(PathBuf::from)
		.unwrap_or_else(|| PathBuf::from("/tmp/montre-daemon.sock"));

	eprintln!("daemon: corpus = {}", corpus_path.display());
	eprintln!("daemon: socket = {}", socket_path.display());
	eprintln!("daemon: ready. Ctrl-C to stop.");

	let options = ServeOptions {
		corpus_path,
		socket_path: Some(socket_path),
		idle_timeout: None,
	};

	if let Err(e) = serve(options) {
		eprintln!("daemon error: {}", e);
		process::exit(1);
	}
}
