use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use montre_daemon::DaemonClient;
use tempfile::TempDir;

fn read_frame_from(stream: &mut UnixStream) -> std::io::Result<Vec<u8>> {
	let mut length = [0u8; 4];
	stream.read_exact(&mut length)?;
	let size = u32::from_be_bytes(length) as usize;
	let mut payload = vec![0u8; size];
	stream.read_exact(&mut payload)?;
	Ok(payload)
}

fn write_frame_to(stream: &mut UnixStream, payload: &[u8]) -> std::io::Result<()> {
	let length = u32::try_from(payload.len()).unwrap();
	stream.write_all(&length.to_be_bytes())?;
	stream.write_all(payload)?;
	stream.flush()
}

fn stub_corpus_info_response(id: u64) -> serde_json::Value {
	serde_json::json!({
		"jsonrpc": "2.0",
		"id": id,
		"result": {
			"name": "stub",
			"canonical_path": "/stub",
			"stable_key": "stub",
			"components": [],
			"layers": [],
			"alignments": []
		}
	})
}

fn run_echo_server(
	listener: UnixListener,
	observed_ids: mpsc::Sender<u64>,
) {
	let (mut stream, _) = match listener.accept() {
		Ok(pair) => pair,
		Err(_) => return,
	};
	loop {
		let frame = match read_frame_from(&mut stream) {
			Ok(frame) => frame,
			Err(_) => return,
		};
		let request: serde_json::Value = match serde_json::from_slice(&frame) {
			Ok(value) => value,
			Err(_) => continue,
		};
		let Some(id) = request.get("id").and_then(|v| v.as_u64()) else {
			continue;
		};
		let _ = observed_ids.send(id);
		let response = stub_corpus_info_response(id);
		let bytes = serde_json::to_vec(&response).unwrap();
		if write_frame_to(&mut stream, &bytes).is_err() {
			return;
		}
	}
}

#[test]
fn request_ids_are_monotonic_and_unique() {
	let temp = TempDir::new().expect("tempdir");
	let socket = temp.path().join("echo.sock");
	let listener = UnixListener::bind(&socket).expect("bind");

	let (id_tx, id_rx) = mpsc::channel::<u64>();
	let server = thread::spawn(move || run_echo_server(listener, id_tx));

	let mut client = DaemonClient::connect(&socket).expect("connect");
	let request_count = 8;
	for _ in 0..request_count {
		client.corpus_info().expect("corpus_info should succeed via stub");
	}
	drop(client);
	let _ = server.join();

	let ids: Vec<u64> = id_rx.try_iter().collect();
	assert!(
		ids.len() >= request_count,
		"expected at least {} ids, observed {:?}",
		request_count,
		ids,
	);

	let mut deduplicated = ids.clone();
	deduplicated.sort();
	deduplicated.dedup();
	assert_eq!(deduplicated.len(), ids.len(), "duplicate ids: {:?}", ids);

	let strictly_increasing = ids.windows(2).all(|window| window[1] > window[0]);
	assert!(strictly_increasing, "ids not monotonic: {:?}", ids);
}

#[test]
fn fast_response_does_not_race_pending_insert() {
	let temp = TempDir::new().expect("tempdir");
	let socket = temp.path().join("echo.sock");
	let listener = UnixListener::bind(&socket).expect("bind");

	let (id_tx, _id_rx) = mpsc::channel::<u64>();
	let server = thread::spawn(move || run_echo_server(listener, id_tx));

	let mut client = DaemonClient::connect(&socket).expect("connect");
	for i in 0..5 {
		let result = client.corpus_info();
		assert!(
			result.is_ok(),
			"request {} should succeed; if insert-after-write were the implementation, \
			 a fast reply could outrace the pending insert and recv would block. \
			 got {:?}",
			i,
			result,
		);
	}
	drop(client);
	let _ = server.join();
}

#[test]
fn write_failure_after_server_close_returns_quickly() {
	let temp = TempDir::new().expect("tempdir");
	let socket = temp.path().join("dies.sock");
	let listener = UnixListener::bind(&socket).expect("bind");

	let acceptor = thread::spawn(move || {
		let (stream, _) = listener.accept().expect("accept");
		drop(stream);
	});

	let mut client = DaemonClient::connect(&socket).expect("connect");
	let _ = acceptor.join();

	let start = Instant::now();
	let result = client.corpus_info();
	let elapsed = start.elapsed();

	assert!(result.is_err(), "expected error after server close");
	assert!(
		elapsed < Duration::from_millis(500),
		"request must return quickly when write fails or reader has exited, took {:?}",
		elapsed,
	);
}
