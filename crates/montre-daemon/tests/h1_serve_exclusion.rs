use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use montre_daemon::{serve, DaemonClient, DaemonError, ServeOptions};
use tempfile::TempDir;

fn build_test_corpus(out: &Path) {
	let manifest = Path::new(env!("CARGO_MANIFEST_DIR"))
		.join("../../testdata/parallel/corpus.toml");
	montre_build::MultiCorpusBuilder::from_manifest(&manifest)
		.expect("manifest load")
		.build(out)
		.expect("corpus build");
}

fn wait_for_serving(path: &Path, timeout: Duration) {
	let deadline = Instant::now() + timeout;
	loop {
		if UnixStream::connect(path).is_ok() {
			return;
		}
		if Instant::now() >= deadline {
			panic!("daemon never became connectable at {}", path.display());
		}
		thread::sleep(Duration::from_millis(20));
	}
}

#[test]
fn second_serve_with_same_socket_fails_with_already_running() {
	let temp = TempDir::new().expect("tempdir");
	let corpus = temp.path().join("corpus");
	build_test_corpus(&corpus);
	let socket = temp.path().join("daemon.sock");

	let first = ServeOptions {
		corpus_path: corpus.clone(),
		socket_path: Some(socket.clone()),
		idle_timeout: None,
	};
	thread::spawn(move || {
		let _ = serve(first);
	});
	wait_for_serving(&socket, Duration::from_secs(5));

	let probe = DaemonClient::connect(&socket).expect("first daemon connect");

	let second = ServeOptions {
		corpus_path: corpus.clone(),
		socket_path: Some(socket.clone()),
		idle_timeout: None,
	};
	let result = serve(second);
	assert!(
		matches!(result, Err(DaemonError::AlreadyRunning)),
		"expected AlreadyRunning, got {:?}",
		result,
	);
	assert!(socket.exists(), "socket file must not be clobbered by failed serve");

	let _post_probe = DaemonClient::connect(&socket)
		.expect("first daemon still reachable after failed second serve");

	drop(probe);
}

#[test]
fn second_serve_with_alt_socket_fails_on_state_lock() {
	let temp = TempDir::new().expect("tempdir");
	let corpus = temp.path().join("corpus");
	build_test_corpus(&corpus);
	let primary_socket = temp.path().join("primary.sock");

	let first = ServeOptions {
		corpus_path: corpus.clone(),
		socket_path: Some(primary_socket.clone()),
		idle_timeout: None,
	};
	thread::spawn(move || {
		let _ = serve(first);
	});
	wait_for_serving(&primary_socket, Duration::from_secs(5));

	let alt_socket = temp.path().join("alt.sock");
	let second = ServeOptions {
		corpus_path: corpus.clone(),
		socket_path: Some(alt_socket.clone()),
		idle_timeout: None,
	};
	let result = serve(second);
	assert!(
		matches!(result, Err(DaemonError::AlreadyRunning)),
		"expected AlreadyRunning from state lock, got {:?}",
		result,
	);
	assert!(!alt_socket.exists(), "alt socket must not be created by failed serve");
}

#[test]
fn serve_replaces_stale_socket_file() {
	let temp = TempDir::new().expect("tempdir");
	let corpus = temp.path().join("corpus");
	build_test_corpus(&corpus);
	let socket = temp.path().join("daemon.sock");

	{
		let _stale = UnixListener::bind(&socket).expect("bind stale");
	}
	assert!(socket.exists(), "stale socket file must remain after listener drop");

	let options = ServeOptions {
		corpus_path: corpus,
		socket_path: Some(socket.clone()),
		idle_timeout: None,
	};
	thread::spawn(move || {
		let _ = serve(options);
	});

	wait_for_serving(&socket, Duration::from_secs(5));

	let _probe = DaemonClient::connect(&socket)
		.expect("new daemon connect over replaced stale socket");
}

#[test]
fn serve_refuses_to_clobber_external_listener() {
	let temp = TempDir::new().expect("tempdir");
	let corpus = temp.path().join("corpus");
	build_test_corpus(&corpus);
	let socket = temp.path().join("daemon.sock");

	let occupier = UnixListener::bind(&socket).expect("bind occupier");

	let options = ServeOptions {
		corpus_path: corpus,
		socket_path: Some(socket.clone()),
		idle_timeout: None,
	};
	let result = serve(options);
	assert!(
		matches!(result, Err(DaemonError::AlreadyRunning)),
		"expected AlreadyRunning when external listener occupies path, got {:?}",
		result,
	);
	assert!(socket.exists(), "external listener's socket must not be clobbered");

	drop(occupier);
}
