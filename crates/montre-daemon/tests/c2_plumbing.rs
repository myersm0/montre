use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use montre_daemon::client::DaemonClientError;
use montre_daemon::protocol::*;
use montre_daemon::{serve, DaemonClient, ServeOptions};
use tempfile::TempDir;

fn build_test_corpus(out: &Path) {
	let manifest = Path::new(env!("CARGO_MANIFEST_DIR"))
		.join("../../testdata/parallel/corpus.toml");
	montre_build::MultiCorpusBuilder::from_manifest(&manifest)
		.expect("manifest load")
		.build(out)
		.expect("corpus build");
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

fn connect(fx: &Fixture) -> DaemonClient {
	let deadline = Instant::now() + Duration::from_secs(5);
	loop {
		if let Ok(client) = DaemonClient::connect(&fx.socket) {
			return client;
		}
		if Instant::now() >= deadline {
			panic!("daemon socket never became connectable at {}", fx.socket.display());
		}
		thread::sleep(Duration::from_millis(20));
	}
}

fn connect_and_register() -> (Fixture, DaemonClient) {
	let fx = boot_daemon();
	let mut client = connect(&fx);
	let reply = register(&mut client);
	assert_eq!(reply.process_id, 1);
	(fx, client)
}

fn register(client: &mut DaemonClient) -> RegisterReply {
	client
		.register(RegisterParams {
			protocol_version: PROTOCOL_VERSION,
			kind: ProcessKind::External,
			label: Some("c2-plumbing".to_string()),
			provides: Vec::new(),
			consumes: Vec::new(),
		})
		.expect("register")
}

#[test]
fn register_round_trip_over_socket() {
	let fx = boot_daemon();
	let mut client = connect(&fx);
	let response = register(&mut client);

	assert_eq!(response.process_id, 1);
	assert_eq!(response.protocol_version, PROTOCOL_VERSION);
	assert_eq!(response.daemon_epoch, 1);
	assert!(response.capabilities.coupler_kinds.iter().any(|k| k == "sentence_mirror"));
}

#[test]
fn protocol_errors_are_returned_over_socket() {
	let (_fx, mut client) = connect_and_register();
	let error = client
		.query_execute(QueryExecuteParams { cql: "[pos=\"NOUN\"".to_string() })
		.unwrap_err();
	match error {
		DaemonClientError::Protocol(protocol) => {
			assert_eq!(protocol.code, error_codes::CQL_PARSE_ERROR);
		}
		other => panic!("expected protocol error, got {other:?}"),
	}
}

#[test]
fn corpus_info_round_trip_over_socket() {
	let (_fx, mut client) = connect_and_register();
	let result = client.corpus_info().expect("corpus info");

	assert_eq!(result.name, "test-parallel");
	assert!(!result.corpus_id.is_empty());
	assert!(!result.components.is_empty());
	assert_eq!(result.alignments, vec!["sentence".to_string()]);
}

#[test]
fn text_round_trip_via_corpus_documents_over_socket() {
	let (_fx, mut client) = connect_and_register();

	let docs = client
		.corpus_documents(CorpusDocumentsParams { component: None })
		.expect("corpus documents");
	let la_maison = docs
		.documents
		.iter()
		.find(|d| d.name.contains("la_maison"))
		.expect("la_maison present");

	let sentence = client
		.text_sentence(TextSentenceParams { doc: la_maison.index, sent: 0 })
		.expect("text sentence");
	assert_eq!(sentence.sentence_id, "1");
	assert!(sentence.surface.contains("La"));
	assert!(sentence.span.end > sentence.span.start);
}

#[test]
fn alignment_list_round_trip_over_socket() {
	let (_fx, mut client) = connect_and_register();
	let response = client.alignment_list().expect("alignment list");

	assert_eq!(response.alignments.len(), 1);
	let alignment = &response.alignments[0];
	assert_eq!(alignment.name, "sentence");
	assert_eq!(alignment.source_component, "fr");
	assert_eq!(alignment.target_component, "en");
	assert_eq!(alignment.edge_count, 4);
}

#[test]
fn query_execute_then_hits_round_trip_over_socket() {
	let (_fx, mut client) = connect_and_register();

	let exec = client
		.query_execute(QueryExecuteParams { cql: "[pos=\"NOUN\"]".to_string() })
		.expect("query execute");
	assert!(exec.hit_count > 0);
	assert!(exec.handle.starts_with("r-"));

	let hits = client
		.query_hits(QueryHitsParams {
			handle: exec.handle,
			offset: 0,
			limit: 100,
		})
		.expect("query hits");
	assert_eq!(hits.total_count, exec.hit_count);
	assert_eq!(hits.hits.len() as u64, exec.hit_count);
	for hit in hits.hits {
		assert_ne!(hit.document_index, u32::MAX);
		assert_ne!(hit.sentence_index, u32::MAX);
	}
}
