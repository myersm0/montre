use std::path::PathBuf;
use std::process;
use std::time::Duration;

use montre_daemon::{serve, ServeOptions};

fn print_usage() {
	eprintln!("usage: serve_local <corpus-path> [--socket PATH] [--idle-timeout SECS]");
	eprintln!();
	eprintln!("    --socket PATH         default: /tmp/montre-daemon.sock");
	eprintln!("    --idle-timeout SECS   default: daemon default (600s); 0 disables");
	eprintln!();
	eprintln!("example:");
	eprintln!("    cargo run --example serve_local -p montre-daemon -- ./my-corpus");
	eprintln!("    cargo run --example serve_local -p montre-daemon -- ./my-corpus --idle-timeout 0");
}

fn parse_args() -> (PathBuf, PathBuf, Option<Duration>) {
	let mut args = std::env::args().skip(1);
	let corpus_path = match args.next() {
		Some(a) if !a.starts_with("--") => PathBuf::from(a),
		_ => { print_usage(); process::exit(1); }
	};

	let mut socket_path = PathBuf::from("/tmp/montre-daemon.sock");
	let mut idle_timeout: Option<Duration> = None;

	while let Some(flag) = args.next() {
		match flag.as_str() {
			"--socket" => {
				let path = args.next().unwrap_or_else(|| {
					eprintln!("--socket requires a path");
					process::exit(1);
				});
				socket_path = PathBuf::from(path);
			}
			"--idle-timeout" => {
				let secs: u64 = args.next()
					.and_then(|s| s.parse().ok())
					.unwrap_or_else(|| {
						eprintln!("--idle-timeout requires an integer (seconds)");
						process::exit(1);
					});
				idle_timeout = Some(Duration::from_secs(secs));
			}
			_ => {
				eprintln!("unknown argument: {}", flag);
				print_usage();
				process::exit(1);
			}
		}
	}

	(corpus_path, socket_path, idle_timeout)
}

fn main() {
	let (corpus_path, socket_path, idle_timeout) = parse_args();

	eprintln!("daemon: corpus = {}", corpus_path.display());
	eprintln!("daemon: socket = {}", socket_path.display());
	match idle_timeout {
		Some(d) if d.is_zero() => eprintln!("daemon: idle timeout = disabled"),
		Some(d) => eprintln!("daemon: idle timeout = {}s", d.as_secs()),
		None => eprintln!("daemon: idle timeout = default"),
	}
	eprintln!("daemon: ready (Ctrl-C, SIGTERM, or daemon.shutdown to stop)");

	let options = ServeOptions {
		corpus_path,
		socket_path: Some(socket_path),
		idle_timeout,
	};

	if let Err(e) = serve(options) {
		eprintln!("daemon: serve failed: {}", e);
		process::exit(1);
	}
}
