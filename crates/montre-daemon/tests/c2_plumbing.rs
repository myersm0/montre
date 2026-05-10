use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use montre_daemon::{serve, ServeOptions};
use tempfile::TempDir;

fn build_test_corpus(out: &Path) {
	let manifest = Path::new(env!("CARGO_MANIFEST_DIR"))
		.join("../../testdata/parallel/corpus.toml");
	montre_build::MultiCorpusBuilder::from_manifest(&manifest)
		.expect("manifest load")
		.build(out)
		.expect("corpus build");
}

fn wait_for_socket(path: &Path, timeout: Duration) -> UnixStream {
	let deadline = Instant::now() + timeout;
	loop {
		if path.exists() {
			if let Ok(stream) = UnixStream::connect(path) {
				return stream;
			}
		}
		if Instant::now() >= deadline {
			panic!("daemon socket never became connectable at {}", path.display());
		}
		thread::sleep(Duration::from_millis(20));
	}
}

fn write_frame(stream: &mut UnixStream, payload: &[u8]) {
	let len = u32::try_from(payload.len()).expect("payload fits u32");
	stream.write_all(&len.to_be_bytes()).expect("write len");
	stream.write_all(payload).expect("write payload");
	stream.flush().expect("flush");
}

fn read_frame(stream: &mut UnixStream) -> Vec<u8> {
	let mut len_buf = [0u8; 4];
	stream.read_exact(&mut len_buf).expect("read len");
	let len = u32::from_be_bytes(len_buf) as usize;
	let mut payload = vec![0u8; len];
	stream.read_exact(&mut payload).expect("read payload");
	payload
}

struct Fixture {
	_temp: TempDir,
	socket: PathBuf,
}

fn boot_daemon() -> Fixture {
	let temp = TempDir::new().expect("tempdir");
	let corpus = temp.path().join("corpus");
	build_test_corpus(&corpus);
	let socket = temp.path().join("daemon.sock");
	let options = ServeOptions {
		corpus_path: corpus,
		socket_path: Some(socket.clone()),
		idle_timeout: None,
	};
	thread::spawn(move || {
		let _ = serve(options);
	});
	Fixture { _temp: temp, socket }
}

#[test]
fn register_round_trip_over_socket() {
	let fx = boot_daemon();
	let mut stream = wait_for_socket(&fx.socket, Duration::from_secs(5));

	let request = serde_json::json!({
		"jsonrpc": "2.0",
		"id": 1,
		"method": "session.register",
		"params": {
			"protocol_version": 1,
			"kind": "external",
		},
	});
	write_frame(&mut stream, &serde_json::to_vec(&request).unwrap());

	let response_bytes = read_frame(&mut stream);
	let response: serde_json::Value = serde_json::from_slice(&response_bytes).unwrap();

	assert_eq!(response["jsonrpc"], "2.0");
	assert_eq!(response["id"], 1);
	assert!(response.get("error").is_none(), "got error: {}", response);

	let result = &response["result"];
	assert_eq!(result["process_id"], 1);
	assert_eq!(result["protocol_version"], 1);
	assert_eq!(result["daemon_epoch"], 1);
	assert!(result["capabilities"]["anchor_kinds"].is_array());
}

#[test]
fn unknown_method_returns_method_not_found_over_socket() {
	let fx = boot_daemon();
	let mut stream = wait_for_socket(&fx.socket, Duration::from_secs(5));

	let register = serde_json::json!({
		"jsonrpc": "2.0",
		"id": 1,
		"method": "session.register",
		"params": { "protocol_version": 1, "kind": "external" },
	});
	write_frame(&mut stream, &serde_json::to_vec(&register).unwrap());
	let _ = read_frame(&mut stream);

	let bogus = serde_json::json!({
		"jsonrpc": "2.0",
		"id": 2,
		"method": "does.not.exist",
	});
	write_frame(&mut stream, &serde_json::to_vec(&bogus).unwrap());

	let response_bytes = read_frame(&mut stream);
	let response: serde_json::Value = serde_json::from_slice(&response_bytes).unwrap();
	assert_eq!(response["id"], 2);
	assert_eq!(response["error"]["code"], -32601);
}
