use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::sync::mpsc::RecvTimeoutError;
use std::thread;
use std::time::{Duration, Instant};

use montre_daemon::client::DaemonClientError;
use montre_daemon::DaemonClient;
use tempfile::TempDir;

fn wait_for_reader_exit(client: &DaemonClient, timeout: Duration) {
	let deadline = Instant::now() + timeout;
	loop {
		match client.notifications().recv_timeout(Duration::from_millis(20)) {
			Err(RecvTimeoutError::Disconnected) => return,
			Err(RecvTimeoutError::Timeout) => {
				if Instant::now() >= deadline {
					panic!("reader thread did not exit within {:?}", timeout);
				}
			}
			Ok(_) => {}
		}
	}
}

#[test]
fn in_flight_request_fails_fast_when_server_closes_connection() {
	let temp = TempDir::new().expect("tempdir");
	let socket = temp.path().join("client.sock");
	let listener = UnixListener::bind(&socket).expect("bind");

	let acceptor = thread::spawn(move || {
		let (mut stream, _) = listener.accept().expect("accept");
		let mut buffer = [0u8; 4096];
		let _ = stream.read(&mut buffer);
		drop(stream);
	});

	let mut client = DaemonClient::connect(&socket).expect("connect");
	let start = Instant::now();
	let result = client.corpus_info();
	let elapsed = start.elapsed();

	let _ = acceptor.join();

	match result {
		Err(DaemonClientError::ReaderClosed) => {}
		other => panic!("expected ReaderClosed, got {:?}", other),
	}
	assert!(
		elapsed < Duration::from_secs(1),
		"in-flight request should fail fast after server close, took {:?}",
		elapsed,
	);
}

#[test]
fn in_flight_request_fails_fast_when_reader_sees_framing_error() {
	let temp = TempDir::new().expect("tempdir");
	let socket = temp.path().join("client.sock");
	let listener = UnixListener::bind(&socket).expect("bind");

	let acceptor = thread::spawn(move || {
		let (mut stream, _) = listener.accept().expect("accept");
		let mut buffer = [0u8; 4096];
		let _ = stream.read(&mut buffer);
		let _ = stream.write_all(&u32::MAX.to_be_bytes());
		thread::sleep(Duration::from_millis(50));
		drop(stream);
	});

	let mut client = DaemonClient::connect(&socket).expect("connect");
	let start = Instant::now();
	let result = client.corpus_info();
	let elapsed = start.elapsed();

	let _ = acceptor.join();

	match result {
		Err(DaemonClientError::ReaderClosed) => {}
		other => panic!("expected ReaderClosed, got {:?}", other),
	}
	assert!(
		elapsed < Duration::from_secs(1),
		"in-flight request should fail fast after framing error, took {:?}",
		elapsed,
	);
}

#[test]
fn request_issued_after_reader_exit_fails_fast() {
	let temp = TempDir::new().expect("tempdir");
	let socket = temp.path().join("client.sock");
	let listener = UnixListener::bind(&socket).expect("bind");

	let acceptor = thread::spawn(move || {
		let (stream, _) = listener.accept().expect("accept");
		drop(stream);
	});

	let mut client = DaemonClient::connect(&socket).expect("connect");
	let _ = acceptor.join();

	wait_for_reader_exit(&client, Duration::from_secs(2));

	let start = Instant::now();
	let result = client.corpus_info();
	let elapsed = start.elapsed();

	match result {
		Err(DaemonClientError::ReaderClosed) => {}
		other => panic!("expected ReaderClosed, got {:?}", other),
	}
	assert!(
		elapsed < Duration::from_millis(100),
		"post-exit request should fail fast, took {:?}",
		elapsed,
	);
}
