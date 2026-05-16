use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use montre_daemon::protocol::*;
use montre_daemon::client::{DaemonClientError, NotificationEnvelope};
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
	wait_for_socket_path(&socket, Duration::from_secs(5));
	Fixture { _temp: temp, socket }
}

fn wait_for_socket_path(path: &Path, timeout: Duration) {
	let deadline = Instant::now() + timeout;
	loop {
		if path.exists() {
			return;
		}
		if Instant::now() >= deadline {
			panic!("daemon socket never appeared at {}", path.display());
		}
		thread::sleep(Duration::from_millis(20));
	}
}

fn client() -> (Fixture, DaemonClient) {
	let fx = boot_daemon();
	let client = DaemonClient::connect(&fx.socket).expect("client connect");
	(fx, client)
}

fn register(client: &mut DaemonClient) -> RegisterReply {
	client
		.register(RegisterParams {
			protocol_version: PROTOCOL_VERSION,
			kind: ProcessKind::External,
			label: Some("c4-test".to_string()),
			provides: Vec::new(),
			consumes: Vec::new(),
		})
		.expect("register")
}

#[test]
fn client_register_and_corpus_info() {
	let (_fx, mut client) = client();
	let reply = register(&mut client);
	assert_eq!(reply.process_id, 1);
	assert_eq!(reply.protocol_version, PROTOCOL_VERSION);
	assert_eq!(reply.daemon_epoch, 1);
	assert!(reply.capabilities.coupler_kinds.iter().any(|k| k == "sentence_mirror"));

	let info = client.corpus_info().expect("corpus info");
	assert_eq!(info.name, "test-parallel");
	assert_eq!(info.alignments, vec!["sentence".to_string()]);
}

#[test]
fn client_query_execute_hits_metadata_and_discard() {
	let (_fx, mut client) = client();
	register(&mut client);

	let executed = client
		.query_execute(QueryExecuteParams { cql: "[pos=\"NOUN\"]".to_string() })
		.expect("query execute");
	assert!(executed.handle.starts_with("r-"));
	assert!(executed.hit_count > 0);

	let page = client
		.query_hits(QueryHitsParams {
			handle: executed.handle.clone(),
			offset: 0,
			limit: 100,
		})
		.expect("query hits");
	assert_eq!(page.total_count, executed.hit_count);
	assert_eq!(page.hits.len() as u64, executed.hit_count);
	assert!(page.hits.iter().all(|h| h.document_index != u32::MAX));

	let metadata = client
		.query_metadata(QueryMetadataParams { handle: executed.handle.clone() })
		.expect("query metadata");
	assert_eq!(metadata.handle, executed.handle);
	assert_eq!(metadata.hit_count, executed.hit_count);

	let discarded = client
		.query_discard(QueryDiscardParams { handle: metadata.handle })
		.expect("query discard");
	assert!(discarded.ok);
}

#[test]
fn client_text_and_annotation_methods() {
	let (_fx, mut client) = client();
	register(&mut client);

	let docs = client
		.corpus_documents(CorpusDocumentsParams { component: None })
		.expect("corpus documents");
	let doc = docs
		.documents
		.iter()
		.find(|d| d.name.contains("la_maison"))
		.expect("la_maison doc");

	let sentence = client
		.text_sentence(TextSentenceParams { doc: doc.index, sent: 0 })
		.expect("text sentence");
	assert!(sentence.surface.contains("La"));

	let surface = client
		.text_surface(TextSurfaceParams { start: sentence.span.start, end: sentence.span.end })
		.expect("text surface");
	assert_eq!(surface.surface, sentence.surface);

	let rows = client
		.text_annotations_range(TextAnnotationsRangeParams {
			start: sentence.span.start,
			end: sentence.span.start + 1,
			layers: Some(vec!["word".to_string(), "pos".to_string()]),
		})
		.expect("annotations range");
	assert_eq!(rows.rows.len(), 1);
	assert!(rows.rows[0].values.contains_key("word"));
}

#[test]
fn client_protocol_errors_are_typed() {
	let (_fx, mut client) = client();
	register(&mut client);

	let err = client
		.query_execute(QueryExecuteParams { cql: "[pos=\"NOUN\"".to_string() })
		.unwrap_err();
	match err {
		DaemonClientError::Protocol(protocol) => {
			assert_eq!(protocol.code, error_codes::CQL_PARSE_ERROR);
		}
		other => panic!("expected protocol error, got {other:?}"),
	}
}

#[test]
fn client_receives_roster_notifications() {
	let fx = boot_daemon();
	let mut watcher = DaemonClient::connect(&fx.socket).expect("watcher connect");
	register(&mut watcher);
	watcher
		.subscription_subscribe(SubscriptionParams { topic: "roster_changed".to_string() })
		.expect("subscribe");

	let mut second = DaemonClient::connect(&fx.socket).expect("second connect");
	let second_register = register(&mut second);

	let note = watcher
		.notifications()
		.recv_timeout(Duration::from_secs(2))
		.expect("roster notification");
	match note {
		NotificationEnvelope::RosterChanged { event, process } => {
			assert_eq!(event, "registered");
			assert_eq!(process.id, second_register.process_id);
		}
		other => panic!("unexpected notification {other:?}"),
	}
}

#[test]
fn client_publish_interest_drives_coupler_notification() {
	let fx = boot_daemon();
	let mut master = DaemonClient::connect(&fx.socket).expect("master connect");
	let master_reply = master
		.register(RegisterParams {
			protocol_version: PROTOCOL_VERSION,
			kind: ProcessKind::Reader,
			label: None,
			provides: vec![InterestKind::Sentence],
			consumes: Vec::new(),
		})
		.expect("master register");

	let mut follower = DaemonClient::connect(&fx.socket).expect("follower connect");
	let follower_reply = follower
		.register(RegisterParams {
			protocol_version: PROTOCOL_VERSION,
			kind: ProcessKind::Reader,
			label: None,
			provides: Vec::new(),
			consumes: vec![InterestKind::Sentence],
		})
		.expect("follower register");

	let coupler = follower
		.coupler_create(CouplerCreateParams {
			master_id: master_reply.process_id,
			follower_id: follower_reply.process_id,
			kind: CouplerKind::SentenceMirror,
		})
		.expect("coupler create");

	master
		.publish_interest(PublishInterestParams {
			interest: Interest::Sentence { doc: 0, sent: 0 },
		})
		.expect("publish interest");

	let note = follower
		.notifications()
		.recv_timeout(Duration::from_secs(2))
		.expect("coupler notification");
	match note {
		NotificationEnvelope::CouplerUpdate { coupler_id, interest } => {
			assert_eq!(coupler_id, coupler.coupler_id);
			assert!(matches!(interest, Interest::Sentence { doc: 0, sent: 0 }));
		}
		other => panic!("unexpected notification {other:?}"),
	}
}
