use std::io::Read;
use std::os::unix::net::UnixListener;
use std::thread;
use std::time::{Duration, Instant};

use montre_daemon::DaemonClient;
use tempfile::TempDir;

#[test]
fn close_returns_promptly_when_daemon_does_not_reply() {
	let temp = TempDir::new().expect("tempdir");
	let socket = temp.path().join("nonresponsive.sock");
	let listener = UnixListener::bind(&socket).expect("bind");

	let acceptor = thread::spawn(move || {
		let (mut stream, _) = listener.accept().expect("accept");
		let mut buffer = [0u8; 4096];
		loop {
			match stream.read(&mut buffer) {
				Ok(0) | Err(_) => return,
				Ok(_) => continue,
			}
		}
	});

	let client = DaemonClient::connect(&socket).expect("connect");

	let start = Instant::now();
	client.close().expect("close should not propagate an error");
	let elapsed = start.elapsed();

	assert!(
		elapsed < Duration::from_secs(1),
		"close should return within the bounded deadline, took {:?}",
		elapsed,
	);

	let _ = acceptor.join();
}

#[test]
fn drop_returns_promptly_when_daemon_does_not_reply() {
	let temp = TempDir::new().expect("tempdir");
	let socket = temp.path().join("nonresponsive.sock");
	let listener = UnixListener::bind(&socket).expect("bind");

	let acceptor = thread::spawn(move || {
		let (mut stream, _) = listener.accept().expect("accept");
		let mut buffer = [0u8; 4096];
		loop {
			match stream.read(&mut buffer) {
				Ok(0) | Err(_) => return,
				Ok(_) => continue,
			}
		}
	});

	let client = DaemonClient::connect(&socket).expect("connect");

	let start = Instant::now();
	drop(client);
	let elapsed = start.elapsed();

	assert!(
		elapsed < Duration::from_secs(1),
		"drop should return within the bounded deadline, took {:?}",
		elapsed,
	);

	let _ = acceptor.join();
}
