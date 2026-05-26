# Montre daemon clients

Entry point for writing a client that talks to the Montre daemon. Covers the daemon's lifecycle from a client's perspective, the available client paths (Rust via `DaemonClient`, other languages via raw socket), interactive tooling for protocol exploration, and a handful of common patterns.

For the wire surface — error codes, message shapes, the full operation reference — see [`daemon-protocol.md`](daemon-protocol.md). This document points at that one repeatedly rather than restating it.

## Daemon lifecycle

### Auto-spawn

Clients do not start the daemon explicitly under normal use. The pattern is: derive the expected socket path from the corpus path, attempt `connect()`, and on failure spawn `montre serve <corpus>` and poll until the socket is ready.

`DaemonClient::connect_or_spawn(corpus_path)` does all of this. Other-language clients implement the equivalent dance: BLAKE3-16-hex of the canonicalized corpus path → socket path under `~/.local/share/montre/sockets/` → connect → on `ENOENT`, spawn `montre serve` detached and poll with backoff (10-second total timeout) → on `ECONNREFUSED`, probe `<state_dir>/daemon.lock` (open + non-blocking `flock(LOCK_EX | LOCK_NB)`, release immediately) before unlinking the socket file: if the lock is held a daemon is alive and the connection refusal is transient (saturated accept queue, mid-startup), so don't unlink — return an error suggesting retry. Only unlink and proceed to spawn when the lock is free. See the protocol doc's "Discovery" and "Auto-spawn" sections for the full algorithm; `crates/montre-daemon/src/client.rs` is the reference implementation.

Concurrent first-time connects from multiple clients serialize through a `flock`-based lockfile at `<state_dir>/spawn.lock`. The loser of the race connects to the winner's daemon. Established daemons see zero lockfile I/O per connect — the fast path is just `connect()`.

### Explicit start

For protocol exploration, integration tests, or controlled lifetime, start the daemon explicitly with the bundled example:

```bash
cargo run --example serve_local -p montre-daemon -- ./my-corpus
cargo run --example serve_local -p montre-daemon -- ./my-corpus --socket /tmp/m.sock --idle-timeout 0
```

This binds to a known socket path (default `/tmp/montre-daemon.sock`), bypassing the per-corpus path-hash. Clients connect to that socket directly rather than using `connect_or_spawn`. See [Interactive tooling](#interactive-tooling) below.

The production form is `montre serve <corpus-path>` via the CLI, which uses the corpus-hash-derived socket path and the standard state directory. `serve_local` is a thin wrapper around the same `montre_daemon::serve` entry point with command-line ergonomics suited to dev workflows.

### Shutdown

A daemon exits when any one of these happens:

- a registered client calls `client.daemon_shutdown(DaemonShutdownParams { reason: ... })` (wire method `daemon.shutdown`, `reason: "requested"` if not specified)
- the daemon receives SIGHUP, SIGINT, or SIGTERM (`reason: "signal"`)
- the idle timeout elapses with no clients registered (`reason: "idle_timeout"`, default 30 minutes (1800s), configurable, `0` disables)

All three paths go through the same sequence: broadcast `notification.shutdown` to every registered client, wait 500ms, close active streams, exit. Clients see one notification carrying the `reason`, then EOF.

The `daemon_epoch` value returned in `session.register` increments on every daemon startup. Clients caching anything keyed on daemon identity (handle IDs, coupler IDs, process IDs) check it on each registration and invalidate prior caches if it changed.

## Rust: DaemonClient

The `client` module of `montre-daemon` exposes `DaemonClient`, a synchronous client that wraps the wire protocol. Every Rust consumer — future FFI entry points, terminal interfaces, integration tests — goes through it.

### Hello world

```rust
use montre_daemon::client::{DaemonClient, NotificationEnvelope};
use montre_daemon::protocol::{
	ProcessKind, QueryExecuteParams, QueryHitsParams, RegisterParams,
};
use std::path::Path;

let mut client = DaemonClient::connect_or_spawn(Path::new("./my-corpus"))?;
let register_reply = client.register(RegisterParams {
	protocol_version: 1,
	kind: ProcessKind::External,
	label: Some("my-script".into()),
	provides: vec![],
	consumes: vec![],
})?;

let result = client.query_execute(QueryExecuteParams {
	cql: r#"[pos="NOUN"]"#.into(),
})?;
println!("{} hits", result.hit_count);

let page = client.query_hits(QueryHitsParams {
	handle: result.handle.clone(),
	offset: 0,
	limit: 100,
})?;
for hit in &page.hits {
	println!("{}..{}", hit.span.start, hit.span.end);
}
```

Every protocol RPC has a typed method on `DaemonClient`. The convention is one-Params-struct in, one-Reply-struct out:

- Each method (with four exceptions noted below) takes exactly one positional argument: the operation's `*Params` struct from `montre_daemon::protocol`. For example, `client.query_execute(QueryExecuteParams { cql: "...".into() })`.
- Each method returns `Result<*Reply, DaemonClientError>` where `*Reply` is the operation's reply struct, also in `montre_daemon::protocol`. Field names match the JSON shape in `daemon-protocol.md`.
- Operations with no params (`corpus.info`, `session.unregister`, `alignment.list`, `query.list_named`) expose a zero-argument method. Their reply types are the same `*Reply` convention.
- `publish_interest` is the lone fire-and-forget notification. It returns `Result<(), std::io::Error>`: there is no protocol response to wait for, but the underlying socket write can still fail. Don't swallow the result.

For the full operation → params/reply map, see "Rust API reference" at the end of `daemon-protocol.md`.

### Notifications

`DaemonClient::notifications()` returns a `&std::sync::mpsc::Receiver<NotificationEnvelope>` fed by the client's background reader thread. The receiver is held by reference (not owned) and lives as long as the `DaemonClient`. The application polls or selects on it:

```rust
let notifications = client.notifications();

while let Ok(notif) = notifications.recv() {
	match notif {
		NotificationEnvelope::CouplerUpdate { coupler_id, interest } => {
			handle_focus_change(coupler_id, interest);
		}
		NotificationEnvelope::RosterChanged { event, process } => { /* ... */ }
		NotificationEnvelope::Shutdown { reason, .. } => break,
		_ => {}
	}
}
```

The reader is the only code path that receives frames. If it exits — EOF, framing error, protocol error, panic — it marks the client closed and releases all pending response waiters before terminating. Request methods on a closed client return an error rather than blocking forever.

### Sending notifications

`session.publish_interest` is the lone client-to-daemon notification (fire-and-forget, no reply). `DaemonClient::publish_interest(params)` sends it. Use this whenever the calling process is the master in one or more coupler relationships and its focus has moved.

The method returns `Result<(), std::io::Error>`. There is no protocol response, but the socket write can still fail (broken pipe, kernel buffer issues, etc.) — handle the error rather than dropping it.

The daemon silently drops a publish whose `interest` kind is not in the publishing process's declared `provides` (with a debug-level trace log on the daemon side). Because the operation is fire-and-forget, there is no error surfaced to the client — clients that intend to publish a given `InterestKind` must declare it at `session.register`.

### Thread safety

`DaemonClient` is `Send`: you can transfer ownership to another thread. It is **not** `Sync`: the notifications receiver is `!Sync`, and request methods take `&mut self`, so two threads cannot use the same `DaemonClient` simultaneously even with shared references.

If you want a layout where one thread pumps notifications and another issues requests against the same connection, wrap the client in `Arc<Mutex<DaemonClient>>`. This serializes all access (notifications and requests both go through the mutex); for most current consumer shapes that's fine, since per-request hold times are short. If finer-grained concurrency becomes a problem, future API work could split the request side from the notification side.

### Cleanup

`DaemonClient` implements `Drop`: going out of scope closes the socket, and the daemon detects EOF and cleans up the registered process (dropping couplers, subscriptions, and outstanding handles). For callers that want to surface unregister errors, `client.close()` is the explicit form.

## Other languages

FFI entry points wrapping `DaemonClient` for Julia, Python, and other callers are planned but not yet implemented. Until they land, non-Rust clients implement the JSON-RPC protocol directly against the Unix socket. See [`daemon-protocol.md`](daemon-protocol.md) for framing (4-byte big-endian length prefix + JSON), the message envelope (JSON-RPC 2.0 strict), the type definitions, and the full operation reference.

`tools/dclient.py` (described below) is a working pure-stdlib Python implementation suitable as a reference for what a minimum-viable client in another language looks like — handshake, framing, request/response correlation, and notification handling fit in ~200 lines.

## Interactive tooling

Two utilities live in the repo for hands-on protocol work.

### `serve_local`

Minimal daemon launcher. Starts a daemon on a specified corpus and socket path, prints status to stderr, runs until killed.

```bash
cargo run --example serve_local -p montre-daemon -- ./my-corpus
cargo run --example serve_local -p montre-daemon -- ./my-corpus --idle-timeout 0
cargo run --example serve_local -p montre-daemon -- ./my-corpus --socket /tmp/m.sock
```

Flags:

- `--socket PATH` — bind to a specific socket path (default `/tmp/montre-daemon.sock`)
- `--idle-timeout SECS` — override the idle shutdown timer; `0` disables

Use cases: a known socket path for `dclient.py`, no auto-spawn race during debugging, explicit daemon lifetime under your control, an `--idle-timeout 0` daemon that won't disappear mid-investigation.

### `dclient.py`

Pure-stdlib Python REPL. Connects to a running daemon, auto-registers, and accepts method invocations. Registration is shaped by CLI flags:

- `--kind` — one of `external` (default), `reader`, `kwic`, `conllu`, `docs`, `vocab`, `results`
- `--label` — optional roster label
- `--provides` — comma-separated `InterestKind`s the process publishes
- `--consumes` — comma-separated `InterestKind`s the process consumes

```
$ python3 tools/dclient.py --socket /tmp/montre-daemon.sock
registered as process_id=1, server_version=0.6.0, daemon_epoch=17
daemon> corpus.info
{
  "result": { "name": "isosceles", ... }
}
daemon> query.execute {"cql": "[pos=\"NOUN\"]"}
daemon> query.hits {"handle": "r-3a7f...", "offset": 0, "limit": 5}
daemon> subscription.subscribe {"topic": "roster_changed"}
daemon> .notify session.publish_interest {"interest": {"type": "sentence", "doc": 0, "sent": 0}}
```

Notifications from the daemon (coupler updates, roster changes, named-results changes, shutdown) print asynchronously as they arrive; the REPL stays usable.

REPL commands:

- `method [params]` — send a request, print the reply
- `.notify method [params]` — send as a notification (no `id`, no reply expected)
- `.help` — list known methods
- `.quit` / `exit` / EOF — disconnect

The `.notify` form is the primary way to exercise master-side coupler behavior from the REPL. Launch two `dclient.py` instances with appropriate registration shape:

```
# terminal 1 — follower
python3 dclient.py --kind reader --consumes sentence

# terminal 2 — master
python3 dclient.py --kind kwic --provides hit
daemon> coupler.create {"master_id": 2, "follower_id": 1, "kind": {"type": "sentence_mirror"}}
daemon> .notify session.publish_interest {"interest": {"type": "hit", "result": "r-...", "hit_idx": 0}}
```

The follower receives transformed `notification.coupler_update` messages.

## Common patterns

### Run a query and page through hits

```rust
let result = client.query_execute(QueryExecuteParams {
	cql: r#"[pos="ADJ"] [pos="NOUN"]"#.into(),
})?;
let total = result.hit_count;

let mut offset = 0;
while offset < total {
	let page = client.query_hits(QueryHitsParams {
		handle: result.handle.clone(),
		offset,
		limit: 1000,
	})?;
	for hit in &page.hits {
		process(hit);
	}
	offset += page.hits.len() as u64;
}
```

`query.hits` enforces a maximum `limit` of 1000 per call. For count-only queries, prefer `query.execute_count`, which avoids hit allocation. Discard the handle with `query.discard` if you won't need it again and don't want to wait for daemon idle shutdown to release the memory.

### Save and reuse a named result

```rust
let result = client.query_execute(QueryExecuteParams {
	cql: r#"[pos="ADJ"] [pos="NOUN"]"#.into(),
})?;
client.query_save(QuerySaveParams {
	handle: result.handle.clone(),
	name: "adj-noun-pairs".into(),
})?;

// later, in any session:
let loaded = client.query_load(QueryLoadParams {
	name: "adj-noun-pairs".into(),
})?;
let page = client.query_hits(QueryHitsParams {
	handle: loaded.handle.clone(),
	offset: 0,
	limit: 100,
})?;
```

Named results are query-backed by default: the daemon persists the CQL plus metadata, and re-executes on `query.load`. This survives daemon restarts and corpus rebuilds gracefully (the query may produce different hits after a rebuild, but doesn't break). For snapshot semantics — preserving exactly the hit list as it stood at a point in time — call `query.materialize` to freeze the current hits. Materialization is session-scoped and does not persist across daemon restarts.

If a stored CQL no longer parses or executes against a rebuilt corpus, `query.load` (and subsequent `query.hits`) returns error code `1204`. The named result is not auto-deleted; the client decides whether to delete, re-save with new CQL, or alert the user.

### Coupled coordination

The protocol's coupler mechanism lets one process (the master) drive another (the follower) without either party knowing the other's address. The daemon owns the relationship: when the master publishes an interest, the daemon transforms it according to the coupler's `kind` and pushes the result to the follower.

```rust
// Follower: register with consumes=[Sentence], wait for coupler_update notifications
let follower_reply = follower.register(RegisterParams {
	protocol_version: 1,
	kind: ProcessKind::External,
	label: Some("reader".into()),
	provides: vec![],
	consumes: vec![InterestKind::Sentence],
})?;

// Master: register with provides=[Hit], create a coupler between us
let master_reply = master.register(RegisterParams {
	protocol_version: 1,
	kind: ProcessKind::External,
	label: Some("kwic".into()),
	provides: vec![InterestKind::Hit],
	consumes: vec![],
})?;

let coupler = master.coupler_create(CouplerCreateParams {
	master_id: master_reply.process_id,
	follower_id: follower_reply.process_id,
	kind: CouplerKind::KwicSelection,
})?;

// Master: when the user selects a hit in the KWIC, publish it
master.publish_interest(PublishInterestParams {
	interest: Interest::Hit {
		result: result_handle.clone(),
		hit_idx: 23,
	},
})?;
// → daemon transforms (KwicSelection: Hit → containing Sentence)
// → follower receives notification.coupler_update with Interest::Sentence
```

Each `CouplerKind` defines what the master can publish (`provides`) and what the follower receives after the daemon's transformation (`consumes`). The full matrix is in `daemon-protocol.md` under "Transformation matrix". The most common kinds:

- `SentenceMirror` — widens a `Position` or `Span` to its containing sentence; useful for "keep this pane on the same sentence as that one"
- `Alignment { name }` — projects via a named alignment; the daemon emits one notification per target sentence (1→N fan-out)
- `KwicSelection` — resolves a `Hit` to its containing sentence; drives a reader from a KWIC pane

`coupler.create` validates that the master's `provides` and the follower's `consumes` are compatible with the kind's transformation row and returns error `1400` otherwise. Process IDs that disappear (registration ends) silently drop their couplers.

### Subscribe to roster changes

```rust
client.subscription_subscribe(SubscriptionParams {
	topic: "roster_changed".into(),
})?;
// notifications appear on the channel as processes come and go:
// NotificationEnvelope::RosterChanged { event: "registered", process: ProcessInfo { ... } }
```

Topics in v1: `roster_changed`, `named_results_changed`. Coupler updates do not require a subscription — they flow automatically to the follower of each coupler.

## Reference

- [`daemon-protocol.md`](daemon-protocol.md) — wire protocol, error codes, coupler kinds, full operation reference
- [`api.md`](api.md) — Rust API of `montre-index::Corpus` and friends; needed when reading hits, projecting alignments, or reconstructing surface text outside the daemon
- `crates/montre-daemon/src/client.rs` — `DaemonClient` implementation; the canonical reference for method names, parameter types, and error mapping
- `crates/montre-daemon/README.md` — daemon internals (architecture, invariants, handler conventions); read when modifying the daemon itself, not when writing clients
