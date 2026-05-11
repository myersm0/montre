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

fn request(
	stream: &mut UnixStream,
	id: u64,
	method: &str,
	params: Option<serde_json::Value>,
) -> serde_json::Value {
	let mut req = serde_json::json!({ "jsonrpc": "2.0", "id": id, "method": method });
	if let Some(p) = params {
		req["params"] = p;
	}
	write_frame(stream, &serde_json::to_vec(&req).unwrap());
	let bytes = read_frame(stream);
	serde_json::from_slice(&bytes).unwrap()
}

fn register(stream: &mut UnixStream) -> serde_json::Value {
	request(
		stream,
		0,
		"session.register",
		Some(serde_json::json!({ "protocol_version": 1, "kind": "external" })),
	)
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

fn connect_and_register() -> (Fixture, UnixStream) {
	let fx = boot_daemon();
	let mut stream = wait_for_socket(&fx.socket, Duration::from_secs(5));
	let reply = register(&mut stream);
	assert!(reply.get("error").is_none(), "register failed: {}", reply);
	(fx, stream)
}

#[test]
fn register_round_trip_over_socket() {
	let fx = boot_daemon();
	let mut stream = wait_for_socket(&fx.socket, Duration::from_secs(5));
	let response = register(&mut stream);

	assert_eq!(response["jsonrpc"], "2.0");
	assert!(response.get("error").is_none(), "got error: {}", response);
	let result = &response["result"];
	assert_eq!(result["process_id"], 1);
	assert_eq!(result["protocol_version"], 1);
	assert_eq!(result["daemon_epoch"], 1);
	assert!(result["capabilities"]["anchor_kinds"].is_array());
}

#[test]
fn unknown_method_returns_method_not_found_over_socket() {
	let (_fx, mut stream) = connect_and_register();
	let response = request(&mut stream, 1, "does.not.exist", None);
	assert_eq!(response["error"]["code"], -32601);
}

#[test]
fn corpus_info_round_trip_over_socket() {
	let (_fx, mut stream) = connect_and_register();
	let response = request(&mut stream, 1, "corpus.info", None);

	let result = &response["result"];
	assert_eq!(result["name"], "test-parallel");
	assert!(result["stable_key"].is_string());
	assert!(result["components"].is_array());
	assert_eq!(result["alignments"], serde_json::json!(["sentence"]));
}

#[test]
fn text_round_trip_via_corpus_documents_over_socket() {
	let (_fx, mut stream) = connect_and_register();

	let docs_response = request(&mut stream, 1, "corpus.documents", None);
	let documents = docs_response["result"]["documents"]
		.as_array()
		.expect("documents array");
	let la_maison = documents
		.iter()
		.find(|d| d["name"].as_str().unwrap().contains("la_maison"))
		.expect("la_maison present");
	let doc_idx = la_maison["index"].as_u64().unwrap();

	let sent_response = request(
		&mut stream,
		2,
		"text.sentence",
		Some(serde_json::json!({ "doc": doc_idx, "sent": 0 })),
	);
	let sent = &sent_response["result"];
	assert_eq!(sent["sentence_id"], "1");
	assert!(sent["surface"].as_str().unwrap().contains("La"));
	assert!(sent["span"]["end"].as_u64().unwrap() > sent["span"]["start"].as_u64().unwrap());
}

#[test]
fn alignment_list_round_trip_over_socket() {
	let (_fx, mut stream) = connect_and_register();
	let response = request(&mut stream, 1, "alignment.list", None);

	let alignments = response["result"]["alignments"]
		.as_array()
		.expect("alignments array");
	assert_eq!(alignments.len(), 1);
	let alignment = &alignments[0];
	assert_eq!(alignment["name"], "sentence");
	assert_eq!(alignment["source_component"], "fr");
	assert_eq!(alignment["target_component"], "en");
	assert_eq!(alignment["edge_count"], 4);
}

#[test]
fn query_execute_then_hits_round_trip_over_socket() {
	let (_fx, mut stream) = connect_and_register();

	let exec_response = request(
		&mut stream,
		1,
		"query.execute",
		Some(serde_json::json!({ "cql": "[pos=\"NOUN\"]" })),
	);
	let exec_result = &exec_response["result"];
	let handle = exec_result["handle"].as_str().expect("handle").to_string();
	let hit_count = exec_result["hit_count"].as_u64().expect("hit_count");
	assert!(hit_count > 0);
	assert!(handle.starts_with("r-"));

	let hits_response = request(
		&mut stream,
		2,
		"query.hits",
		Some(serde_json::json!({
			"handle": handle,
			"offset": 0,
			"limit": 100,
		})),
	);
	let hits_result = &hits_response["result"];
	assert_eq!(hits_result["total_count"], hit_count);
	let hits = hits_result["hits"].as_array().expect("hits array");
	assert_eq!(hits.len() as u64, hit_count);
	for hit in hits {
		assert_ne!(hit["document_index"].as_u64().unwrap(), u32::MAX as u64);
		assert_ne!(hit["sentence_index"].as_u64().unwrap(), u32::MAX as u64);
	}
}
