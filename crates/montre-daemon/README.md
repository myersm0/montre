# montre-daemon

Session daemon for the Montre corpus query engine. One process per corpus, serving multiple clients (TUI panes, FFI consumers, scripts) over a Unix domain socket using a length-prefixed JSON-RPC 2.0 protocol.

The daemon is the substrate for any tool that needs more than a one-shot CLI invocation: anchored reading (multiple panes coordinating on a shared focus), persistent named results, the future REPL and TUI. Clients connect via `DaemonClient` (in `client.rs`); the daemon itself runs as `montre serve <corpus-path>`, typically spawned implicitly by the first client.

See [`daemon-protocol.md`](../../daemon-protocol.md) (top-level) for the wire surface — error codes, message shapes, anchor kinds, the full operation reference. This README covers the implementation side: architecture, invariants, file layout, conventions.

## Architecture

```
                  ┌─────────────────────────┐
                  │      state thread       │
                  │  roster, anchors,       │
                  │  subscriptions,         │
                  │  named_results,         │
                  │  counters,              │
                  │  daemon_epoch           │
                  └────────────▲────────────┘
                               │ Command (mpsc)
            ┌──────────────────┼──────────────────┐
            │                  │                  │
   ┌────────┴────────┐ ┌───────┴───────┐ ┌────────┴────────┐
   │  connection 1   │ │ connection 2  │ │ connection N    │
   │  reader thread  │ │ reader thread │ │ reader thread   │
   │  writer thread  │ │ writer thread │ │ writer thread   │
   └─────────────────┘ └───────────────┘ └─────────────────┘
        ▲    │              ▲    │            ▲    │
        │    ▼              │    ▼            │    ▼
      socket             socket             socket
```

One state thread owns all mutable shared state and is reached exclusively through `mpsc::Sender<Command>`. Each accepted connection spawns two threads: a reader (parses inbound frames, dispatches RPCs) and a writer (the sole socket-write site, draining a bounded `SyncSender<Outbound>` with capacity 256). On outbound-queue overflow the producer drops the newest message and logs a warning, per the protocol's subscription backpressure decision.

The corpus itself lives behind `Arc<CorpusHandle>`, shared by the state thread and every `RpcContext`. `Corpus` is immutable after construction and `Send + Sync`, so read-only access from connection threads needs no coordination.

## Invariants

These hold across all phases. Code review and new handlers should preserve them.

**Corpus identity is the BLAKE3-16-hex of the canonical corpus path.** The same value appears as the socket filename stem (`<hash>.sock`), as `CorpusHandle::corpus_id`, and on the wire as `stable_key` in `CorpusInfo` and `corpus_id` in `ResultMetadata`. The two wire names diverged historically; `daemon-protocol.md` documents the equivalence, and the rename will happen alongside the v2 UUID transition. Clients treat the value as opaque. When the engine adopts UUID-stamped corpora (v2 of the spec), this value evolves to the UUID without changing any client code that doesn't parse it.

**Hits move through the state thread exactly once.** `query.execute` parses, plans, and executes against the immutable corpus directly on its connection thread, then sends `Command::InsertResult { cql, hits }` through the channel. The `Vec<Hit>` traverses the channel by move, never by clone. This is what keeps a 70ms quantifier query from serializing all other clients. Any future code that touches stored hits must preserve the move-once invariant — for example, metadata mutations on `ResultEntry` should not re-clone the hits vector.

**Query execution never blocks the state thread.** The state thread only owns coordination state (roster, anchors, results table, counters). Anything that scales with corpus size (parser, planner, executor, surface text reconstruction, alignment projection inside handlers) runs on the connection thread. Notification transforms in `transform_interest` are the one current exception, called from within `Command::PublishInterest` and `Command::AnchorCreate` handlers; see "Known trade-offs" below.

**`daemon_epoch` is the cache-invalidation contract.** Returned in `session.register`. Increments on daemon restart and on any persistent-state reset. Clients caching handle IDs, anchor IDs, or process IDs across reconnects must check epoch on each registration; if it changed, all prior cached IDs are invalid. The counter is persisted to `<state_dir>/epoch` (default `~/.local/share/montre/state/<corpus_id>/epoch`, override via `XDG_STATE_HOME`) and bumped on every daemon startup.

**Notifications carry no ID.** Strict JSON-RPC 2.0. `session.publish_interest` is a client-to-daemon notification (fire-and-forget). `notification.anchor_update`, `notification.roster_changed`, `notification.named_results_changed`, and `notification.shutdown` are daemon-to-client notifications.

### DaemonClient reader-thread invariant

`DaemonClient` owns one background reader thread per connection. The reader is the only code path that receives frames, routes responses to pending request waiters, and forwards notifications.

If the reader exits for any reason — EOF, framing error, protocol error, or panic during unwinding — it must mark the client as closed and release all pending response waiters. Request methods must never be able to enqueue a waiter and then block forever because the reader died without setting the closed flag.

This is enforced with a drop guard in the reader thread rather than relying on normal loop exit.

## File layout

```
src/
├── lib.rs            public API: serve(), DaemonError, ServeOptions, CorpusHandle, socket path derivation
├── protocol.rs       wire types: params, replies, enums, PROTOCOL_VERSION, error_codes module
├── dispatch.rs       framing, RPC dispatch, listener, reader/writer threads, RpcContext, handle_register
├── state.rs          State, Command, run loop, anchor compat matrix, transform_interest
├── client.rs         DaemonClient (c4)
├── storage.rs        state-dir resolution, epoch persistence (c5)
└── handlers/
    ├── mod.rs
    ├── corpus.rs        corpus.info, corpus.documents, corpus.layer_info
    ├── text.rs          text.surface, text.sentence, text.sentences, text.document, text.annotations[_range]
    ├── alignment.rs     alignment.list, alignment.project; shared project_alignment helper
    ├── query.rs         query.execute, query.execute_count, query.hits, query.metadata,
    │                    query.save, query.materialize, query.load, query.list_named,
    │                    query.delete_named, query.discard
    ├── anchor.rs        anchor.create, anchor.remove, anchor.list
    ├── session.rs       session.unregister, session.update_label, session.roster
    └── subscription.rs  subscription.subscribe, subscription.unsubscribe
```

`session.register` lives in `dispatch.rs` rather than `handlers/session.rs` because it sets `RpcContext::process_id` and is gated separately from the registration check applied to every other method.

Cross-handler helpers (e.g. `document_component`, `document_sentence_count` in `corpus.rs`, `project_alignment` in `alignment.rs`) are `pub(crate)`.

## Adding a handler

Handlers live in `src/handlers/<namespace>.rs`. The standard shape uses helpers from `dispatch`:

```rust
use crate::dispatch::{parse_params, serialize_reply, state_roundtrip, RpcContext};
use crate::state::Command;

pub(crate) fn handle_namespace_method(
    params: Option<serde_json::Value>,
    ctx: &RpcContext,
) -> Result<serde_json::Value, ProtocolError> {
    let parsed: NamespaceMethodParams = parse_params("namespace.method", params)?;

    // Read-only against the corpus:
    let result = ctx.handle.corpus.something(&parsed);

    // ...or mutate shared state via the state thread:
    let result = state_roundtrip(ctx, |reply| Command::SomeAction {
        field: parsed.field,
        reply,
    })?;

    serialize_reply(SomeReply { result })
}
```

Three parse/serialize helpers, all in `dispatch.rs`:

- `parse_params::<T>(method, params)` for required-params handlers.
- `parse_params_or_default::<T>(params)` for handlers that tolerate `None` and want `T::default()`.
- `serialize_reply(value)` for the response.

For state-thread roundtrips, `state_roundtrip(ctx, |reply| Command::Foo { ..., reply })` takes care of allocating the oneshot channel, sending the command, and waiting for the reply. It returns the bare reply value; for `Command` variants whose reply is itself `Result<R, ProtocolError>`, the call site uses `??` to flatten.

Read-only handlers work directly off `ctx.handle.corpus` without touching the state thread. Wire the new method into `dispatch_request` in `dispatch.rs`.

## Test infrastructure

Two seams. Unit tests in each module use `#[cfg(test)] mod tests` against the same-file private items. Cross-module dispatch tests use `dispatch::test_support`, exposed `#[cfg(test)] pub(crate)`:

- `corpus_fixture()` — builds the `testdata/parallel/` corpus once via `OnceLock`.
- `make_handle()` — returns `Arc<CorpusHandle>` against the fixture.
- `with_state_thread(body)` — spawns a state thread, hands `(Sender<Command>, Arc<CorpusHandle>)` to `body`, joins on drop.
- `register_context(state_tx, handle, kind, provides, consumes)` — registers a process with given provides/consumes; returns `(RpcContext, Receiver<Outbound>)`. The receiver must be bound to keep the outbound channel alive.
- `with_registered_context(body)` — convenience wrapper for the common single-context case.
- `find_doc_index(corpus, needle)` — substring lookup for a document name in the fixture.

The pattern for testing notification fan-out is `with_state_thread` → two `register_context` calls (one master, one follower) → bind both `Receiver<Outbound>`s → drive the master through `dispatch_request` / `dispatch_notification` → drain the follower's receiver with `recv_timeout` and assert on the payloads. See `publish_interest_alignment_fans_out_to_follower` in `dispatch.rs::tests` for the canonical example.

Integration tests in `tests/` use a real socket (`tests/c2_plumbing.rs` shows the form: bind, spawn `serve` in a thread, connect with raw `UnixStream`, write framed JSON-RPC). Once `DaemonClient` lands in c4, integration tests will exercise it instead of raw framing.

## Known trade-offs

**Notification transforms run on the state thread.** `transform_interest` — including `project_alignment` walks for `Alignment` anchors — executes inside `Command::PublishInterest` handling. On the current test corpus this is microseconds; on a huge corpus with a wide `Interest::Span` master interest it scales as O(sentences_in_doc × edges) on the serialized state loop and can hold up other state operations behind it. Acceptable today. If anchor traffic ever becomes hot, the fix is to compute transforms off-thread and only route the resulting `(follower, payload)` tuples through state. Same parallelization argument as `query.execute`.

**1→many alignment fan-out emits N notifications.** One `notification.anchor_update` per projected target. Settled in c3-phase-2.

**Inert anchor kinds.** `DocPickerSelection`, `NamedResultsSelection`, and `ConlluView` compat-accept their self→self interest pair so `anchor.create` succeeds, but `transform_interest` returns empty for them pending UX design. Anchors of these kinds can be created and listed; they just don't push notifications. Documented in the spec; the daemon's behavior matches.

**Single connection per client.** Each `DaemonClient` opens one socket. Clients that need to coordinate on more than one corpus open one client per corpus. v1 does not support cross-corpus operations.

## Conventions

- `pub(crate)` is the default visibility for cross-module helpers.
- `pub` items in `lib.rs` and `protocol.rs` are the daemon's public API; treat them as semver-relevant.
- Tabs for indentation, snake_case constants, no docstrings unless they pull weight, minimal comments (self-documenting code preferred).
- When adding code that touches multiple files (e.g. a new RPC), add tests alongside, not deferred. Each new handler gets at least happy-path + error-path coverage.

## Related docs

- [`daemon-protocol.md`](../../daemon-protocol.md) — wire protocol, error codes, anchor kinds, full operation reference.
- [`api.md`](../../api.md) — public Rust API of `montre-index::Corpus` and friends. Read this before touching anything that calls into the corpus.
- [`DEVELOPMENT.md`](../../DEVELOPMENT.md) — workspace conventions, test corpus location, build/test commands.

Phase-specific handoff documents live at the workspace root (`montre-daemon-handoff.md`, `montre-daemon-followups.md`). Those churn; this README does not.
