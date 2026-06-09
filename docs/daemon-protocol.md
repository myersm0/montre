# Montre daemon protocol

Wire protocol specification for the Montre session daemon. This doc lives in the `montre` repo (alongside `api.md` and `DEVELOPMENT.md`) and is the authoritative reference for both daemon and client implementations.

Read alongside `montre-tui-design.md` for the why; this doc covers the what and how.

---

## Status and scope

This is the v1 protocol specification. It is implementation-blocking: no daemon code, no client code touches the wire until this doc is settled.

Covers: wire format, framing, connection lifecycle, version negotiation, type definitions, complete operation reference, notification reference, subscription topics, error model, implementation notes.

Does not cover: daemon implementation details (storage backend, internal data structures), client-side caching strategies, observation / workspace operations (specified in v2 when those features land — slots reserved here).

---

## Wire format

### Transport

Unix domain stream sockets. No TCP, no other transport in v1.

### Encoding

JSON, UTF-8.

### Framing

Length-prefixed: 4-byte big-endian unsigned integer giving payload length in bytes, followed by exactly that many bytes of JSON. No trailing newline, no other padding.

```
+--------+------------------------+--------+------------------------+
| u32 BE |     JSON payload       | u32 BE |     JSON payload       |  ...
+--------+------------------------+--------+------------------------+
```

Maximum payload size: 16 MiB. Larger requires a different operation (paginated query results, ranged text retrieval). Daemon and client both reject larger frames with a connection-level error and close the connection.

Newline-delimited JSON was considered and rejected: a malformed message desyncs the stream irrecoverably. Length-prefixing lets either side skip a malformed frame and continue.

### Message structure

JSON-RPC 2.0 strict. Three message types:

**Request** (client → daemon, expecting a response):
```json
{
  "jsonrpc": "2.0",
  "id": 17,
  "method": "query.execute",
  "params": { "cql": "[pos=\"NOUN\"]" }
}
```

**Response** (daemon → client):
```json
{
  "jsonrpc": "2.0",
  "id": 17,
  "result": { "handle": "r-9c2f...", "hit_count": 244184 }
}
```

or:
```json
{
  "jsonrpc": "2.0",
  "id": 17,
  "error": { "code": 1100, "message": "CQL parse error", "data": { ... } }
}
```

**Notification** (either direction, no response):
```json
{
  "jsonrpc": "2.0",
  "method": "notification.coupler_update",
  "params": { ... }
}
```

Notifications have no `id` field. Server uses notifications for push events; client uses them for fire-and-forget operations (`session.publish_interest`).

Batched requests (JSON-RPC 2.0 array form) are not supported in v1.

---

## Protocol version negotiation

The version is negotiated once, at registration. No per-message version field.

Client sends `protocol_version` in `session.register`. Daemon responds with the version it accepts. Currently `1`.

Version mismatch (client's requested version not supported by daemon) results in an error response and connection close. Future versions of the daemon may support multiple protocol versions simultaneously.

A long-lived connection that survives a daemon restart (e.g., daemon dies idle, comes back at new version, client reconnects) re-handshakes from scratch — the connection is closed on daemon shutdown, no version state persists.

---

## Connection lifecycle

### Discovery

Socket path is derived from the canonical corpus path:

```
~/.local/share/montre/sockets/<hash>.sock
```

Where `<hash>` is the first 16 hex characters of BLAKE3 of `std::fs::canonicalize(corpus_path)`. Symlink-resolved, byte-stable, collision-resistant for any plausible number of corpora.

**Stable corpus identity is committed v2 direction.** Once persistence (named results, observations, workspaces) accumulates user value, path-hash identity becomes insufficient: moving the corpus directory invalidates everything keyed against it. The engine will graduate to a UUID written into `corpus.json` at build time, and `stable_key` will become that UUID. Path-hash will remain as a fallback for older corpora and for ad-hoc cases. v1 ships with path-hash; v2 ships with UUID.

The `corpus_id` field appearing throughout the protocol (in `ResultMetadata`, in storage paths, in socket paths) is opaque to clients and evolves alongside `stable_key`: it carries the path hash in v1 and the UUID in v2. Clients should never parse it or assume a format — they treat it as an identity token and compare for equality.

### Auto-spawn

When a client tries to connect to a corpus and finds no daemon:

1. Client computes the expected socket path.
2. Attempts `connect()`. If it succeeds: skip to handshake.
3. On `ENOENT` (no socket file): no daemon exists. Spawn `montre serve <corpus_path>` as a detached process (double-fork or platform equivalent), then poll `connect()` with exponential backoff, 10-second total timeout.
4. On `ECONNREFUSED` (socket file exists but refuses connection): the file could be a stale leftover from a crashed daemon, or a live daemon with a saturated accept queue, or a daemon mid-startup that has acquired `daemon.lock` but not yet bound the socket. The client distinguishes by probing `<state_dir>/daemon.lock`:
   - If the lock is free: no daemon owns the socket, the file is stale. Unlink it and proceed as in step 3.
   - If the lock is held: a daemon is alive. Do **not** unlink the socket. Return an error suggesting retry (the saturation or startup transient should clear in milliseconds).
5. On successful connect, proceed to handshake.
6. On spawn-poll timeout, fail with a daemon-unreachable error including any captured stderr from the spawned process.

Race condition (two clients spawn simultaneously): file-locking on a sentinel file in the sockets directory. Loser of the race connects to the winner's daemon.

Daemon-side exclusion is independent of the client-side spawn lock: the daemon itself acquires an exclusive `fs4` flock on `<state_dir>/daemon.lock` immediately after resolving the state directory and holds it for the process's lifetime. Any second `montre serve` invocation against the same corpus — whether via auto-spawn racing past the sentinel, a direct manual invocation, or a separate tool — fails fast rather than clobbering the running listener. The two layers protect against different failure modes: the client-side lock keeps auto-spawn races to a single winner; the daemon-side lock prevents split-brain regardless of how the second daemon was launched. The `daemon.lock`-probe in step 4 above is the third related mechanism: it lets the client side use the same lock as a non-destructive liveness signal so a saturated daemon's socket isn't accidentally unlinked.

### Handshake

First message from client must be `session.register`. Daemon responds with the assigned `process_id`, server version, and capabilities object. Until registration succeeds, no other operations are accepted.

Daemon rejects registration with a protocol error if:
- Protocol version not supported.
- Corpus failed to load (first client only — daemon shuts down rather than continuing in a broken state).

### Normal operation

After registration, the client sends requests and notifications; the daemon sends responses and notifications. Multiple requests may be in flight simultaneously; responses are matched to requests by `id`. The client must use unique `id` values per request within a connection (monotonic counter is fine).

### Disconnection

**Graceful**: client sends `session.unregister`, daemon responds `ok`, client closes the socket. Daemon cleans up:
- Drops couplers involving this process.
- Drops subscriptions held by this process.
- Fires `notification.roster_changed` to subscribers.

**Abrupt**: client closes socket without `unregister`, or process dies. Daemon detects via socket EOF and performs the same cleanup.

**Daemon-initiated**: daemon shutting down (idle timeout, signal received) sends `notification.shutdown` to all clients with a brief reason, then closes connections. Clients should exit cleanly or attempt reconnect (which will likely auto-spawn a fresh daemon).

### Idle shutdown

When the last registered process disconnects, the daemon starts an idle timer (default: 1800 seconds, i.e. 30 minutes). If no client registers before the timer expires, the daemon exits.

Reconnect cancels the timer. While any client is registered, the timer does not run; the daemon has no operation-level activity check, because the registration gate means no operation can reach the daemon without a registered client to issue it. "Any operation resets the timer" is therefore operationally equivalent to "registration cancels the timer" — there is no scenario where the timer is running and an operation could reach the state thread to reset it.

The idle timeout is configurable via `montre serve --idle-timeout <seconds>` and defaults to 1800 (30 minutes). Setting `0` disables idle shutdown.

---

## Type definitions

Wire types referenced throughout the operation reference. JSON forms shown.

### Rust mapping

The Rust types backing the wire format live in `montre_daemon::protocol` (params, replies, shared types) and `montre_daemon::client` (notifications, errors). Mapping rules:

- **Params and replies.** Each operation `namespace.method` has matching Rust structs `NamespaceMethodParams` and `NamespaceMethodReply` in `montre_daemon::protocol`. JSON field names match the Rust field names directly; integer types use the widths listed under "Primitives" above. A complete operation → Params/Reply table is in "Rust API reference" at the end of this document.
- **Tagged enums.** JSON shapes with a `"type"` discriminator (`Interest`, `CouplerKind`) map to Rust enums with `#[serde(tag = "type")]`. Variant names and field names in the Rust type match the JSON exactly: e.g. `{ "type": "sentence", "doc": 3, "sent": 142 }` corresponds to `Interest::Sentence { doc: 3, sent: 142 }`.
- **Enum string values.** Enums serialized as plain strings (`ProcessKind`, `ShutdownReason`, `ResultForm`, `LayerKind`, `Topic`, `InterestKind`, `CouplerKind` tag values) all use `snake_case` on the wire. The Rust identifiers are `UpperCamelCase`: `ProcessKind::External` ↔ `"external"`, `CouplerKind::KwicSelection` ↔ `"kwic_selection"`, etc.
- **Notifications.** Server-pushed messages with method names `notification.snake_case_name` map to variants `NotificationEnvelope::UpperCamelName` in `montre_daemon::client` — for example `notification.coupler_update` → `NotificationEnvelope::CouplerUpdate`.
- **Optional fields.** JSON `null` values map to Rust `Option<T>` with `None`. Some optional input fields can also be omitted entirely (serde `#[serde(default)]`) — both forms produce the same `None`.

### Primitives

```
ProcessId       integer, u32 (assigned by daemon, unique within daemon's lifetime)
CouplerId        integer, u32 (assigned by daemon, unique within daemon's lifetime)
DaemonEpoch     integer, u64 (monotonically incremented on each daemon startup; see session.register)
ResultHandle    string  (UUID v4 prefixed with `r-`, e.g. "r-3a7f8e2c-...")
DocumentIndex   integer, u32 (corpus-wide)
SentenceIndex   integer, u32 (corpus-wide; also used for within-document sentence indexing — context disambiguates)
TokenPosition   integer, u64 (corpus-wide global position)
Timestamp       string  (RFC 3339, e.g. "2026-05-10T14:30:00Z")
```

Wire-side these are all JSON Numbers (subject to JSON's 2^53 integer precision limit, which the daemon's actual ranges fit comfortably). Rust clients should use the listed widths for direct deserialization via the `*Params` / `*Reply` types in `montre_daemon::protocol`.

### Identifier field naming

Identifier values (`ProcessId`, `CouplerId`, `ResultHandle`, ...) appear as JSON fields under two conventions:

- **Bare `id`** inside a typed struct, for that struct's own identifier — `ProcessInfo.id`, `Coupler.id`.
- **`<concept>_id`** everywhere else — in flat replies returning an identifier (`session.register` → `process_id`, `coupler.create` → `coupler_id`), in notification payloads (`notification.coupler_update.coupler_id`), and as a parameter referring to an existing entity (`coupler.remove` params → `coupler_id`).

The Rust types in `montre_daemon::protocol` reflect this via per-field serde naming where needed; clients deserializing into the typed structs see the same shape as the wire.

Exception: `Coupler.master` and `Coupler.follower` are bare `ProcessId` references with no `_id` suffix, while the `coupler.create` params side uses `master_id` / `follower_id`. This historical divergence is noted at the `Coupler` definition; new types should follow the rule above.

### `Span`

```json
{ "start": 1247, "end": 1289 }
```

Half-open `[start, end)` over global token positions.

### `Interest`

Tagged union by `type` field:

```json
{ "type": "position",  "doc": 3,  "position": 1247 }
{ "type": "span",      "doc": 3,  "start": 1247, "end": 1289 }
{ "type": "sentence",  "doc": 3,  "sent": 142 }
{ "type": "hit",       "result": "r-3a7f...", "hit_idx": 17 }
{ "type": "results",   "handle": "r-3a7f..." }
{ "type": "document",  "doc": 3 }
```

### `InterestKind`

String enum: `"position" | "span" | "sentence" | "hit" | "results" | "document"`.

### `ProcessKind`

String enum: `"reader" | "kwic" | "conllu" | "docs" | "vocab" | "results" | "external"`.

`"external"` is the kind used by non-TUI clients (Julia, Python, scripts). The kind is a tag for filtering (`session.roster --filter.kinds`); it does not gate participation. External processes register, publish interest, become masters or followers in couplers, and subscribe to topics on equal terms with TUI processes.

### `ProcessInfo`

```json
{
  "id": 4,
  "kind": "reader",
  "label": "fr/la-parure",
  "provides": ["position", "span", "sentence", "document"],
  "consumes": ["position", "span", "sentence", "document"],
  "current_interest": { "type": "sentence", "doc": 3, "sent": 142 }
}
```

`current_interest` is `null` if the process hasn't published one yet.

`label` is `null` when the client passed no label at registration (and has not subsequently called `session.update_label`). The daemon does not substitute a default — it returns `null` to clients verbatim. Roster-rendering consumers may choose to display something like `unlabeled-{id}` for display, but the wire value remains `null`.

### `CouplerKind`

Tagged union by `type` field:

```json
{ "type": "sentence_mirror" }
{ "type": "alignment", "name": "labse" }
{ "type": "kwic_selection" }
{ "type": "doc_picker_selection" }
{ "type": "named_results_selection" }
{ "type": "conllu_view" }
```

#### Transformation matrix

Each coupler kind defines (a) which master `provides` `InterestKind`s it accepts, and (b) which follower `consumes` `InterestKind`s its transformation produces. `coupler.create` rejects with error `1400` when neither side overlaps the kind's row.

| CouplerKind | Accepts (master provides) | Produces (follower consumes) | Notes |
|---|---|---|---|
| `SentenceMirror` | `Position` \| `Span` \| `Sentence` | `Sentence` | Position/Span are widened to their containing sentence. |
| `Alignment { name }` | `Sentence` \| `Span` | `Span` | 1→many: one notification per target. |
| `KwicSelection` | `Hit` | `Sentence` | Containing sentence of the hit. |
| `DocPickerSelection` | `Document` | `Document` | Inert in v1 (see below). |
| `NamedResultsSelection` | `Results` | `Results` | Inert in v1 (see below). |
| `ConlluView` | `Sentence` | `Sentence` | Inert in v1 (see below). |

**Inert kinds.** `DocPickerSelection`, `NamedResultsSelection`, and `ConlluView` compat-accept their self→self interest pair so `coupler.create` succeeds, but the daemon's transformation returns no notifications for them in v1. Their UX semantics will be defined alongside the corresponding TUI panes; clients can create these couplers today but must not depend on receiving updates.

### `Coupler`

```json
{
  "id": 7,
  "master": 4,
  "follower": 9,
  "kind": { "type": "alignment", "name": "labse" }
}
```

Note: `Coupler` uses `master` / `follower`; `coupler.create` params use `master_id` / `follower_id`. The names diverged historically. Clients should write one form and read the other.

### `Hit`

```json
{
  "span": { "start": 1247, "end": 1252 },
  "document_index": 3,
  "sentence_index": 142,
  "captures": null
}
```

`captures` is `null` for queries without labeled captures, or an object mapping label names to spans:
```json
"captures": { "a": { "start": 1247, "end": 1248 }, "b": { "start": 1251, "end": 1252 } }
```

### `ResultMetadata`

```json
{
  "handle": "r-3a7f...",
  "query": "[pos=\"ADJ\"] [pos=\"NOUN\"]",
  "created_at": "2026-05-10T14:30:00Z",
  "materialized_at": null,
  "hit_count": 30672,
  "corpus_id": "9c2f8e3a4b1d7f06",
  "name": null,
  "form": "session"
}
```

`name` is `null` for unnamed results, or the chosen name string after `query.save`.

`materialized_at` is `null` until `query.materialize` is invoked; thereafter an RFC 3339 timestamp.

`corpus_id` is the same opaque value as `CorpusInfo.stable_key` — the names diverged historically and clients should treat both as the same identity token.

`form` is one of:
- `"session"` — unnamed, daemon-lifetime only
- `"query_backed"` — named, stored as CQL, re-executed on access
- `"materialized"` — named, hits snapshotted

### `AlignmentInfo`

```json
{
  "name": "labse",
  "source_component": "maupassant-fr",
  "target_component": "maupassant-en",
  "source_layer": "sentence",
  "target_layer": "sentence",
  "edge_count": 47230
}
```

### `CorpusInfo`

```json
{
  "name": "isosceles",
  "canonical_path": "/Users/.../isosceles",
  "stable_key": "9c2f8e3a4b1d7f06",
  "components": ["maupassant-fr", "maupassant-en"],
  "layers": ["upos", "xpos", "lemma", "feats", ...],
  "alignments": ["labse"]
}
```

---

## Operation reference

Organized by namespace. Each operation lists method name, params shape, result shape, and possible error codes.

**Conventions applied across every operation:**

- Every operation other than `session.register` may return error `1002` (not registered) if called before registration completes. Per-operation error lists below do not repeat this.
- Every operation may return `-32602` (invalid params) when the params object fails to deserialize or fails operation-specific validation, and `-32603` (internal error) when daemon-internal invariants are violated (missing index layer, lookup inconsistency, etc.). Per-operation error lists below cover application-code paths only.
- `session.publish_interest` is the only client→daemon method that may be sent as a JSON-RPC notification (no `id`). All other methods sent without an `id` are silently dropped. Notifications received before `session.register` completes are also silently dropped.
- Triggers for `notification.roster_changed` and `notification.named_results_changed` are listed in the "Subscription topics" table; per-operation pages below do not repeat them.

### `session`

#### `session.register`

Initial handshake. Must be the first message on a new connection.

**Params**:
```json
{
  "protocol_version": 1,
  "kind": "reader",
  "label": "fr/la-parure",
  "provides": ["position", "span", "sentence", "document"],
  "consumes": ["position", "span", "sentence", "document"]
}
```

**Result**:
```json
{
  "process_id": 4,
  "server_version": "0.6.0",
  "protocol_version": 1,
  "daemon_epoch": 17,
  "capabilities": {
    "observations": false,
    "workspaces": false,
    "coupler_kinds": ["sentence_mirror", "alignment", "kwic_selection",
                     "doc_picker_selection", "named_results_selection", "conllu_view"]
  }
}
```

See "Capability advertisement" below for the schema. `daemon_epoch` increments on daemon restart and on any persistent state reset. Clients that cache handle IDs, coupler IDs, or process IDs across reconnects must check this on each registration: if the epoch has changed, all prior cached IDs are invalid and must be re-acquired.

**Errors**: `1000` (protocol mismatch), `-32600` (called twice on the same connection — a connection accepts exactly one `session.register`). Corpus load failure is *not* a wire error: if the corpus fails to load, the daemon never starts, and the client sees a connect failure (ENOENT or ECONNREFUSED) instead.

#### `session.unregister`

Graceful disconnect. Daemon cleans up couplers and subscriptions before responding.

**Params**: `{}`

**Result**: `{ "ok": true }`

#### `session.publish_interest`

Notification (no response). Sent by master processes whenever their focus changes.

**Params**:
```json
{ "interest": { "type": "sentence", "doc": 3, "sent": 142 } }
```

Daemon walks coupler table, applies transformations, pushes `notification.coupler_update` to followers.

**Provides validation.** Daemon silently drops the publish (with a debug-level trace log) when the `interest`'s kind is not present in the publishing process's declared `provides`. Declared `provides` is a contract, not decoration: clients that intend to publish a given `InterestKind` must declare it at `session.register`. There is no error response — `publish_interest` is fire-and-forget and has no response channel.

#### `session.roster`

List all currently-connected processes.

**Params**:
```json
{ "filter": { "provides_any_of": ["sentence", "span"] } }
```

`filter` is optional. Subfields:
- `provides_any_of`: array of `InterestKind`
- `consumes_any_of`: array of `InterestKind`
- `kinds`: array of `ProcessKind`

**Filter semantics.** An empty filter object (or omitting `filter` entirely) returns all processes. Within a single subfield, multiple values are an OR (a process matches if any of its provides/consumes/kind values is in the array). Across subfields, the conditions AND together (a process must match every populated subfield). An empty or missing subfield is not a filter on that axis. Example: `{ "kinds": ["reader"], "provides_any_of": ["sentence", "hit"] }` returns readers that provide either sentence or hit (or both).

**Result**:
```json
{ "processes": [ProcessInfo, ...] }
```

#### `session.update_label`

Update the current process's label (e.g., when a reader navigates to a new document).

**Params**: `{ "label": "fr/boule-de-suif" }`

**Result**: `{ "ok": true }`

Triggers `notification.roster_changed` to subscribers.

### `corpus`

#### `corpus.info`

Returns all corpus metadata in one call. Cheap; clients typically call once at startup.

**Params**: `{}`

**Result**: `CorpusInfo`

#### `corpus.documents`

List documents, optionally filtered by component.

**Params**: `{ "component": "maupassant-fr" }` (component optional)

**Result**:
```json
{
  "documents": [
    { "index": 0, "name": "la-parure", "component": "maupassant-fr", "sentence_count": 142 },
    ...
  ]
}
```

#### `corpus.layer_info`

Detailed info on one annotation layer.

**Params**: `{ "layer": "upos" }`

**Result**:
```json
{ "name": "upos", "kind": "string", "value_count": 17 }
```

`kind` is `"string"` or `"int"` for forward layers.

Nonexistent layer returns `-32602` (invalid params). Layer existence is a named lookup, not a range; clients that don't know a layer name should call `corpus.info` first to discover the available layers.

### `query`

#### `query.execute`

Run a query, return a handle plus hit count. Hits retrievable via `query.hits`.

**Params**: `{ "cql": "[pos=\"ADJ\"] [pos=\"NOUN\"]" }`

**Result**:
```json
{ "handle": "r-3a7f...", "hit_count": 30672 }
```

**Errors**: `1100` (parse error), `1101` (plan error), `1102` (execution error).

#### `query.execute_count`

Run a query, return only the count. Avoids hit allocation; faster for sizing checks.

**Params**: `{ "cql": "..." }`

**Result**: `{ "count": 30672 }`

#### `query.hits`

Paginated retrieval. `offset` and `limit` are required. Maximum `limit` per call is **1000**; larger values fail with error `1203`.

**Params**: `{ "handle": "r-3a7f...", "offset": 0, "limit": 100 }`

**Result**:
```json
{
  "hits": [Hit, ...],
  "offset": 0,
  "limit": 100,
  "total_count": 30672
}
```

**Errors**: `1200` (handle invalid), `1203` (limit exceeds maximum), `1204` (stored query no longer valid — query-backed named results only).

#### `query.metadata`

Fetch metadata for an existing result handle.

**Params**: `{ "handle": "r-3a7f..." }`

**Result**: `ResultMetadata`

**Errors**: `1200`.

#### `query.save`

Promote an unnamed result to a named, persistent one. By default the named result is **query-backed** — the daemon stores the CQL and re-executes on access. The handle remains valid; subsequent `query.metadata` returns the same handle with `name` populated.

**Params**: `{ "handle": "r-3a7f...", "name": "adj-noun-pairs" }`

**Result**: `{ "ok": true, "form": "query_backed" }`

**Errors**: `1200` (handle invalid), `1201` (name already exists).

#### `query.materialize`

Promote a named result from query-backed to materialized form, snapshotting the current hits. Use when snapshot semantics matter (preserving exactly today's hits regardless of future corpus changes).

**Params**: `{ "name": "adj-noun-pairs" }`

**Result**: `{ "ok": true, "hit_count": 30672, "materialized_at": "..." }`

**Errors**: `1202` (name not found), `1205` (already materialized).

#### `query.load`

Look up a named result. Returns the handle (which may be the same or freshly issued, depending on daemon storage strategy).

**Params**: `{ "name": "adj-noun-pairs" }`

**Result**: `{ "handle": "r-3a7f...", "hit_count": 30672, "form": "query_backed" }`

**Errors**: `1202` (name not found), `1204` (stored query no longer valid against current corpus — only possible for query-backed results after a rebuild).

#### `query.list_named`

**Params**: `{}`

**Result**:
```json
{
  "names": [
    { "name": "adj-noun-pairs", "hit_count": 30672, "created_at": "..." },
    ...
  ]
}
```

#### `query.delete_named`

**Params**: `{ "name": "adj-noun-pairs" }`

**Result**: `{ "ok": true }`

**Errors**: `1202`.

Triggers `notification.named_results_changed` to subscribers.

**Outstanding handles**: deletion always succeeds. Any handle previously issued for this named result silently invalidates — subsequent `query.hits` / `query.metadata` calls against it return `1200` (handle invalid). Callers holding handles must be prepared for this. Avoids the alternative of refusing deletion when handles are outstanding, which would let any client block deletion indefinitely.

#### `query.discard`

Release an unnamed handle. Optional — handles also expire on daemon idle shutdown — but useful for long-running clients to avoid memory accumulation.

**Params**: `{ "handle": "r-3a7f..." }`

**Result**: `{ "ok": true }`

Discarding a named result's handle is a no-op (named results are persisted).

### `text`

**Boundary contract (applies across this namespace).** Three categories of operation, distinguished by how they treat out-of-range inputs:

- **Range-based ops** (`text.surface`, `text.surface_with_token_spans`, `text.annotations_range`, and `text.sentences`' `sent_start`/`sent_end` axis): out-of-range endpoints clamp to the valid range. `start > end` (or `sent_start > sent_end`) returns `-32602` regardless — that is a client bug, not a clamp case. An empty post-clamp range returns an empty result (empty `surface`, empty `tokens`, empty `sentences`).
- **Named-coordinate lookups** (`text.sentence`, `text.document`, and the `doc` field within `text.sentences`): nonexistent target returns `-32602`. A nonexistent document or sentence is not a clamp case; it indicates a bad reference.
- **Sparse ops** (`text.annotations`): empty `positions` returns an empty `values` array. Out-of-range or layer-missing positions are silently omitted from the result.

The clamping rule for range ops is motivated by KWIC and similar window-around-hit consumers, which compute `hit.span.end + window_tokens` without knowing corpus or document boundaries. Pushing the bound check to the daemon avoids a round-trip per render. Named-coordinate ops do not get the same treatment because no analogous "compute coordinate without knowing boundaries" pattern exists for them.

#### `text.surface`

Return the surface text for a token range. Honors multiword tokens and `SpaceAfter=No`.

**Params**: `{ "start": 1247, "end": 1289 }`

**Result**: `{ "surface": "Mathilde Loisel était jolie..." }`

`start` and `end` clamp to `token_count` (range op). For per-token byte offsets within the produced surface, see `text.surface_with_token_spans`.

#### `text.surface_with_token_spans`

Batched MWT-aware surface reconstruction that also returns per-token byte offsets within each produced surface string, plus a flag identifying which position in each MWT carries the surface bytes. The hot path for KWIC rendering and any other consumer that needs to highlight specific tokens inside reconstructed text.

**Params**:
```json
{ "ranges": [ { "start": 1247, "end": 1252 }, { "start": 2018, "end": 2031 } ] }
```

`ranges` is an array of `Span`. Each is processed independently with the standard range-op clamp rules. A malformed range (`start > end`) anywhere in the array fails the entire call with `-32602` — clients batch what they trust.

**Result**:
```json
{
  "results": [
    {
      "surface": "à le chien",
      "tokens": [
        { "position": 1247, "surface_start": 0, "surface_end": 2, "emitted": true },
        { "position": 1248, "surface_start": 3, "surface_end": 6, "emitted": true },
        { "position": 1249, "surface_start": 7, "surface_end": 10, "emitted": true }
      ]
    },
    {
      "surface": "au chien",
      "tokens": [
        { "position": 2018, "surface_start": 0, "surface_end": 2, "emitted": true },
        { "position": 2019, "surface_start": 2, "surface_end": 2, "emitted": false },
        { "position": 2020, "surface_start": 3, "surface_end": 8, "emitted": true }
      ]
    }
  ]
}
```

`results` is in input order, one entry per requested range.

**Per-token entries.** Each `SurfaceToken` has:
- `position` — global token position
- `surface_start`, `surface_end` — half-open byte offsets into the same entry's `surface`
- `emitted` — `true` for the position that carries the surface bytes, `false` for MWT constituent positions that share the bytes with their MWT's emitter

For a non-MWT token, the entry is its own emitter and the byte range is non-empty (or empty only for tokens whose `word` value is the empty string).

**MWT semantics.** When an MWT covers `[mwt.start, mwt.end)` and the requested range fully contains the MWT (`mwt.start >= range.start && mwt.end <= range.end`), the daemon emits the MWT surface form once. The emitter is the position at `mwt.start`; constituent positions `mwt.start + 1 .. mwt.end` each get an entry with `surface_start == surface_end` at the post-emit byte offset and `emitted: false`. The MWT's `no_space_after` flag applies for spacing after the MWT.

**MWTs that cross the range boundary.** If `mwt.start < range.start` or `mwt.end > range.end`, the daemon falls back to per-position emission: each in-range position emits its own `word` value as a singleton emitter. This avoids leaking surface bytes that belong to positions outside the requested range. Clients that want MWT-level rendering should align their window boundaries to MWT or sentence boundaries; a request that cuts a French contraction like `au` mid-MWT will see `à le` reconstructed instead.

**Spacing.** Between emitters, the daemon appends a single space byte unless the preceding emitter's `no_space_after` is set (from MWT's flag or the token-level spacing bitmap). No trailing space is appended at the range end.

#### `text.sentence`

Return surface text plus span and sentence ID for a single sentence.

**Params**: `{ "doc": 3, "sent": 142 }`

**Result**:
```json
{
  "span": { "start": 1247, "end": 1289 },
  "surface": "...",
  "sentence_id": "la-parure-142"
}
```

`sent` is sentence index within the document. Named-coordinate lookup: nonexistent `doc` or `sent` returns `-32602`.

#### `text.sentences`

Bulk variant for rendering a document range.

**Params**: `{ "doc": 3, "sent_start": 140, "sent_end": 150 }`

`[sent_start, sent_end)` half-open. `sent_start` and `sent_end` clamp to the document's sentence count (range axis); `sent_start > sent_end` returns `-32602`; nonexistent `doc` returns `-32602` (named axis).

**Result**:
```json
{
  "sentences": [
    { "sent": 140, "span": ..., "surface": "...", "sentence_id": "..." },
    ...
  ]
}
```

#### `text.document`

Document metadata.

**Params**: `{ "doc": 3 }`

**Result**:
```json
{
  "index": 3,
  "name": "la-parure",
  "component": "maupassant-fr",
  "span": { "start": 1100, "end": 5400 },
  "sentence_count": 187
}
```

Named-coordinate lookup: nonexistent `doc` returns `-32602`.

#### `text.annotations`

Fetch token-level annotation values at specific positions. Use for sparse lookups (e.g., annotations at hit positions). For contiguous ranges, prefer `text.annotations_range`.

**Params**:
```json
{ "positions": [1247, 1248, 1249], "layers": ["upos", "lemma"] }
```

**Result**:
```json
{
  "values": [
    { "position": 1247, "layer": "upos",  "value": "DET" },
    { "position": 1247, "layer": "lemma", "value": "le" },
    { "position": 1247, "layer": "head",  "value": 2 },
    ...
  ]
}
```

For positions/layers with no value, the entry is omitted. Value types reflect the underlying layer kind: string layers emit JSON strings, int layers (e.g. `head`) emit JSON numbers. Same shape as `text.annotations_range` rows below.

Sparse semantics: empty `positions` returns an empty `values` array. Out-of-range positions and unknown layer names are silently omitted from the result. There is no boundary error for this operation; the only `-32602` here is for malformed params (e.g. wrong types).

#### `text.annotations_range`

Fetch annotation values for every token in a contiguous range, organized by token. The hot path for CoNLL-U inspectors and inline annotation rendering.

**Params**:
```json
{ "start": 1247, "end": 1289, "layers": ["upos", "lemma", "head", "feats"] }
```

`layers` is optional; omitting it returns all layers. `start` and `end` clamp to `token_count` (range op); `start > end` returns `-32602`.

**Result**:
```json
{
  "rows": [
    { "position": 1247, "values": { "upos": "DET", "lemma": "le", "head": 2 } },
    { "position": 1248, "values": { "upos": "ADJ", "lemma": "joli", "head": 4, "feats": "Gender=Fem" } },
    ...
  ]
}
```

Tokens with no values for any requested layer still appear in `rows` with an empty `values` object — the row index reflects the contiguous range, so consumers can iterate without gaps. Value types follow the same string/int convention as `text.annotations`.

### `alignment`

#### `alignment.list`

**Params**: `{}`

**Result**: `{ "alignments": [AlignmentInfo, ...] }`

#### `alignment.project`

Project a span across an alignment.

**Params**:
```json
{
  "source": { "doc": 3, "start": 1247, "end": 1289 },
  "alignment_name": "labse"
}
```

**Result**:
```json
{
  "targets": [
    { "doc": 7, "start": 980, "end": 1010 },
    { "doc": 7, "start": 1015, "end": 1042 }
  ]
}
```

Empty array if no edge exists from the source span (gap).

**Errors**: `1300` (alignment not found), `1301` (source span outside alignment's source component).

### `coupler`

#### `coupler.create`

**Params**:
```json
{
  "master_id": 4,
  "follower_id": 9,
  "kind": { "type": "alignment", "name": "labse" }
}
```

**Result**: `{ "coupler_id": 7 }`

**Errors**: `1400` (incompatible interest types — master's `provides` and follower's `consumes` don't overlap the kind's row in the transformation matrix above), `1403` (coupler kind not supported by daemon — kind not present in `capabilities.coupler_kinds`), `1500` (process not found), `1402` (coupler would create a cycle).

Daemon side-effects:
- Adds entry to coupler table.
- Subscribes follower to derivative interest updates from this master.
- Pushes initial `notification.coupler_update` to follower with current transformed interest (if master has published one).

#### `coupler.remove`

**Params**: `{ "coupler_id": 7 }`

**Result**: `{ "ok": true }`

**Errors**: `1401`.

#### `coupler.list`

**Params**:
```json
{ "process_id": 4 }
```

`process_id` optional; without it, returns all couplers. With it, returns couplers involving that process (as master or follower).

**Result**: `{ "couplers": [Coupler, ...] }`

### `subscription`

Couplers automatically generate `notification.coupler_update` for followers; no subscription needed for that. Explicit subscriptions are for non-coupler topics.

#### `subscription.subscribe`

**Params**: `{ "topic": "roster_changed" }`

**Result**: `{ "ok": true }`

**Errors**: `1500` (caller's process not found in roster — typically can't happen unless the registration was torn down concurrently), `1600` (unknown topic).

#### `subscription.unsubscribe`

**Params**: `{ "topic": "roster_changed" }`

**Result**: `{ "ok": true }`

#### Subscription topics

| Topic | Notification | Trigger |
|---|---|---|
| `roster_changed` | `notification.roster_changed` | Process registers, unregisters, or updates label |
| `named_results_changed` | `notification.named_results_changed` | `query.save`, `query.materialize`, `query.delete_named` |

Future topics (v2): `observations_changed`, `workspaces_changed`.

### `daemon`

#### `daemon.shutdown`

Request that the daemon shut down. Any registered client may call this; access is not gated. Daemon side-effects:
1. Broadcasts `notification.shutdown` to every registered client with the supplied `reason`.
2. Waits 500ms for clients to drain.
3. Closes all active connections.
4. Exits.

**Params**:
```json
{ "reason": "requested" }
```

`reason` is optional and defaults to `"requested"`. Accepted values: `"requested" | "idle_timeout" | "signal" | "fatal_error"`. The value flows through to `notification.shutdown.reason` so clients can distinguish a user-initiated shutdown from one triggered by other paths. In practice clients call this with the default and let the other reasons originate inside the daemon (signal handler, idle timer, fatal-path).

**Result**: `{ "ok": true }`

The response is sent before the daemon begins the broadcast-and-close sequence. Clients should expect EOF on their connection shortly after receiving the response.

---

## Rust API reference

Each operation has a typed method on `montre_daemon::DaemonClient`. The table maps the wire method to its Rust method, the params struct it takes, and the reply struct it returns. All struct types live in `montre_daemon::protocol` unless otherwise noted.

For operations with **no params**, the client method takes no arguments — there is no zero-field `*Params` struct.

| Method | Rust call | Params type | Reply type |
|---|---|---|---|
| `session.register` | `client.register(params)` | `RegisterParams` | `RegisterReply` |
| `session.unregister` | `client.unregister()` | (none) | `OkReply` |
| `session.update_label` | `client.update_label(params)` | `SessionUpdateLabelParams` | `OkReply` |
| `session.roster` | `client.roster(params)` | `SessionRosterParams` | `SessionRosterReply` |
| `session.publish_interest` | `client.publish_interest(params)` | `PublishInterestParams` | `Result<(), io::Error>` (notification) |
| `corpus.info` | `client.corpus_info()` | (none) | `CorpusInfo` |
| `corpus.documents` | `client.corpus_documents(params)` | `CorpusDocumentsParams` | `CorpusDocumentsReply` |
| `corpus.layer_info` | `client.corpus_layer_info(params)` | `CorpusLayerInfoParams` | `LayerInfo` |
| `text.surface` | `client.text_surface(params)` | `TextSurfaceParams` | `TextSurfaceReply` |
| `text.surface_with_token_spans` | `client.text_surface_with_token_spans(params)` | `TextSurfaceWithTokenSpansParams` | `TextSurfaceWithTokenSpansReply` |
| `text.sentence` | `client.text_sentence(params)` | `TextSentenceParams` | `TextSentenceReply` |
| `text.sentences` | `client.text_sentences(params)` | `TextSentencesParams` | `TextSentencesReply` |
| `text.document` | `client.text_document(params)` | `TextDocumentParams` | `TextDocumentReply` |
| `text.annotations` | `client.text_annotations(params)` | `TextAnnotationsParams` | `TextAnnotationsReply` |
| `text.annotations_range` | `client.text_annotations_range(params)` | `TextAnnotationsRangeParams` | `TextAnnotationsRangeReply` |
| `alignment.list` | `client.alignment_list()` | (none) | `AlignmentListReply` |
| `alignment.project` | `client.alignment_project(params)` | `AlignmentProjectParams` | `AlignmentProjectReply` |
| `query.execute` | `client.query_execute(params)` | `QueryExecuteParams` | `QueryExecuteReply` |
| `query.execute_count` | `client.query_execute_count(params)` | `QueryExecuteParams` (reused) | `QueryExecuteCountReply` |
| `query.hits` | `client.query_hits(params)` | `QueryHitsParams` | `QueryHitsReply` |
| `query.metadata` | `client.query_metadata(params)` | `QueryMetadataParams` | `ResultMetadata` |
| `query.save` | `client.query_save(params)` | `QuerySaveParams` | `QuerySaveReply` |
| `query.materialize` | `client.query_materialize(params)` | `QueryMaterializeParams` | `QueryMaterializeReply` |
| `query.load` | `client.query_load(params)` | `QueryLoadParams` | `QueryLoadReply` |
| `query.list_named` | `client.query_list_named()` | (none) | `QueryListNamedReply` |
| `query.delete_named` | `client.query_delete_named(params)` | `QueryDeleteNamedParams` | `OkReply` |
| `query.discard` | `client.query_discard(params)` | `QueryDiscardParams` | `OkReply` |
| `coupler.create` | `client.coupler_create(params)` | `CouplerCreateParams` | `CouplerCreateReply` |
| `coupler.remove` | `client.coupler_remove(params)` | `CouplerRemoveParams` | `OkReply` |
| `coupler.list` | `client.coupler_list(params)` | `CouplerListParams` | `CouplerListReply` |
| `subscription.subscribe` | `client.subscription_subscribe(params)` | `SubscriptionParams` | `OkReply` |
| `subscription.unsubscribe` | `client.subscription_unsubscribe(params)` | `SubscriptionParams` | `OkReply` |
| `daemon.shutdown` | `client.daemon_shutdown(params)` | `DaemonShutdownParams` | `OkReply` |

### Notifications

Server-pushed notifications surface as variants of `NotificationEnvelope` (in `montre_daemon::client`). The enum is `#[non_exhaustive]` — future protocol revisions add notification methods as new variants, so client `match` statements must include a catch-all arm:

| Wire method | Rust variant | Payload fields |
|---|---|---|
| `notification.coupler_update` | `NotificationEnvelope::CouplerUpdate` | `coupler_id: CouplerId`, `interest: Interest` |
| `notification.roster_changed` | `NotificationEnvelope::RosterChanged` | `event: String`, `process: ProcessInfo` |
| `notification.named_results_changed` | `NotificationEnvelope::NamedResultsChanged` | `event: String`, `name: String`, `metadata: Option<ResultMetadata>` |
| `notification.shutdown` | `NotificationEnvelope::Shutdown` | `reason: String`, `in_seconds: u32` |

The `event` fields on `RosterChanged` and `NamedResultsChanged`, and the `reason` field on `Shutdown`, are typed as `String` on the client side even though the daemon emits values from a closed set (the wire-format values listed in the corresponding `*Reason`/`Topic`/etc. enums elsewhere in this document). Clients that want to pattern-match should compare against the documented string values directly.

### Shared types

Types referenced from multiple params/replies. Rust types live in `montre_daemon::protocol` unless noted. For string-typed enums, the Rust variant identifiers follow the snake_case ↔ UpperCamelCase rule above; they are repeated parenthetically here for convenience.

| Type | JSON shape | Notes |
|---|---|---|
| `Interest` | `{ "type": <kind>, ... }` | tagged enum, see "Interest" above |
| `InterestKind` | string | `"position" / "span" / "sentence" / "hit" / "results" / "document"` (Rust: `InterestKind::Position / Span / Sentence / Hit / Results / Document`) |
| `ProcessKind` | string | `"reader" / "kwic" / "conllu" / "docs" / "vocab" / "results" / "external"` (Rust: `ProcessKind::Reader / Kwic / Conllu / Docs / Vocab / Results / External`) |
| `ProcessInfo` | object | full process descriptor |
| `CouplerKind` | `{ "type": <kind>, ... }` | tagged enum, see "CouplerKind" above |
| `Coupler` | object | `{ id, master, follower, kind }` |
| `Hit` | object | `{ span, document_index, sentence_index, captures }` |
| `Span` | object | `{ start, end }` |
| `ResultMetadata` | object | full named-result descriptor |
| `ResultForm` | string | `"session" / "query_backed" / "materialized"` (Rust: `ResultForm::Session / QueryBacked / Materialized`) |
| `LayerInfo` | object | `{ name, kind, value_count }` |
| `LayerKind` | string | `"string" / "int"` (also `"unknown"` for forward-compatibility) (Rust: `LayerKind::String / Int / Unknown`) |
| `AlignmentInfo` | object | full alignment descriptor |
| `CorpusInfo` | object | full corpus descriptor |
| `ShutdownReason` | string | `"requested" / "idle_timeout" / "signal" / "fatal_error"` (Rust: `ShutdownReason::Requested / IdleTimeout / Signal / FatalError`). Daemon-side emit type; client-side `NotificationEnvelope::Shutdown.reason` is typed as `String`. |
| `Topic` | string | `"roster_changed" / "named_results_changed"` (Rust: `Topic::RosterChanged / NamedResultsChanged`) |
| `OkReply` | object | `{ "ok": true }` — used for operations that confirm success with no other payload. Always emits `ok: true`; if you receive a successful response at all, the field will be `true`. |

---

## Notification reference

Server-pushed messages. Always JSON-RPC notifications (no `id`, no response expected).

### `notification.coupler_update`

Sent to followers when their master's interest changes. Daemon has already applied the coupler's transformation.

**Params**:
```json
{
  "coupler_id": 7,
  "interest": { "type": "span", "doc": 7, "start": 980, "end": 1010 }
}
```

For 1→many transformations (e.g. `Alignment` projecting to multiple target sentences), the daemon emits **one `notification.coupler_update` per target**. Followers see them in source-order; each carries the same `coupler_id` and a single transformed `interest`.

### `notification.roster_changed`

Sent to subscribers when the process roster changes.

**Params**:
```json
{
  "event": "registered",
  "process": ProcessInfo
}
```

`event` is `"registered" | "unregistered" | "label_updated"`.

### `notification.named_results_changed`

Sent to subscribers on save/delete of named results.

**Params**:
```json
{
  "event": "saved",
  "name": "adj-noun-pairs",
  "metadata": ResultMetadata
}
```

`event` is `"saved" | "deleted"`.

### `notification.shutdown`

Sent to all clients when daemon is shutting down. No subscription required.

**Params**:
```json
{ "reason": "idle_timeout", "in_seconds": 0 }
```

`reason` values: `"requested" | "idle_timeout" | "signal" | "fatal_error"`. `requested` corresponds to a `daemon.shutdown` call from a registered client; `idle_timeout` and `signal` originate inside the daemon. `fatal_error` is reserved for an unexpected-error path; v1 daemons never currently emit it. `in_seconds` is a hint about how long the daemon will wait before closing connections: `0` means immediate close (current v1 behavior for all reasons), positive values reserved for a future graceful-shutdown window. Clients should not rely on `in_seconds > 0` for anything load-bearing.

---

## Error model

JSON-RPC standard error structure:

```json
{ "code": <integer>, "message": <string>, "data": <object?> }
```

### Standard JSON-RPC codes

| Code | Meaning |
|---|---|
| -32700 | Parse error (malformed JSON) |
| -32600 | Invalid request |
| -32601 | Method not found |
| -32602 | Invalid params |
| -32603 | Internal error |

### Application codes

| Code | Meaning |
|---|---|
| 1000 | Protocol version mismatch |
| 1002 | Not registered (operation called before `session.register`) |
| 1100 | CQL parse error |
| 1101 | Plan error |
| 1102 | Execution error |
| 1200 | Result handle invalid |
| 1201 | Named result already exists |
| 1202 | Named result not found |
| 1203 | Page limit exceeds maximum |
| 1204 | Stored query no longer valid (query-backed result, corpus changed) |
| 1205 | Result already materialized |
| 1300 | Alignment not found |
| 1301 | Span outside alignment source |
| 1400 | Coupler: incompatible interest types |
| 1401 | Coupler not found |
| 1402 | Coupler would create cycle |
| 1403 | Coupler kind not supported by daemon |
| 1500 | Process not found |
| 1600 | Unknown subscription topic |

### Error data fields

For `1100` (CQL parse error):
```json
{
  "code": 1100,
  "message": "CQL parse error",
  "data": { "position": 12, "expected": "]", "found": "&" }
}
```

For `1400` (coupler incompatibility):
```json
{
  "code": 1400,
  "message": "Coupler incompatibility",
  "data": {
    "master_provides": ["position", "span"],
    "follower_consumes": ["hit", "results"]
  }
}
```

Other application codes use `data` for context as appropriate.

---

## Implementation notes

### Daemon layout

The daemon is invoked as `montre serve`, a subcommand of the main `montre` binary. The implementation lives in a library crate `montre-daemon` (workspace member of the `montre` repo), which exposes a `serve(options)` entry point. `montre-cli` adds a `serve` subcommand that calls into it. One binary, no separate `montre-daemon` executable — keeps auto-spawn straightforward (`Command::new(current_exe).arg("serve")...`), keeps distribution to a single artifact, and keeps daemon and CLI versions in lockstep.

As-implemented layout:

```
crates/montre-daemon/
├── src/
│   ├── lib.rs        — entry point (`serve`), corpus open, state directory, listener wire-up
│   ├── dispatch.rs   — wire framing, JSON-RPC parsing, request/notification dispatch, per-connection reader/writer threads
│   ├── state.rs      — state-owning thread: roster, couplers, subscriptions, named results, idle-shutdown timer
│   ├── client.rs     — `DaemonClient`: shared client module (see below)
│   ├── protocol.rs   — wire types (params/replies, error codes, enums)
│   ├── storage.rs    — atomic file writes, epoch persistence, named-results persistence
│   ├── shutdown.rs   — coordinator that closes active streams during shutdown
│   ├── signals.rs    — SIGHUP/SIGINT/SIGTERM handling
│   └── handlers/     — per-namespace request handlers (alignment, coupler, corpus, daemon, query, session, subscription, text)
├── examples/
│   └── serve_local.rs — bare daemon launcher for protocol-exploration tooling
└── tests/            — integration tests (over real Unix sockets)
```

See `crates/montre-daemon/README.md` for the architectural rationale (state-thread invariants, the move-once Hit channel, query execution staying off the state thread, etc.).

### Shared client module

The client side of the protocol — auto-spawn dance, length-prefix framing, JSON-RPC dispatch, response correlation, notification routing — lives as a `client` module within `montre-daemon`, exposed publicly as `montre_daemon::DaemonClient`. Every consumer uses it: TUI binaries, the FFI surface, future Rust scripts, integration tests. Writing it once keeps protocol adherence centralized; a wire-format change happens in one place.

`DaemonClient`'s public surface exposes:

- `connect_or_spawn(corpus_path)` / `connect(socket_path)` for setup.
- One typed method per protocol operation. Each method takes the operation's `*Params` struct (from `montre_daemon::protocol`) and returns the operation's `*Reply` struct. For example: `client.query_execute(QueryExecuteParams { cql: "...".into() })` returns `Result<QueryExecuteReply>`.
- `notifications()` returns a `&std::sync::mpsc::Receiver<NotificationEnvelope>` for server-pushed notifications (coupler updates, roster changes, named-results changes, shutdown).
- `publish_interest(params)` is fire-and-forget; no reply.
- `close(self)` for an explicit unregister-and-shutdown sequence.

Method signatures, parameter types, and return types are all in `crates/montre-daemon/src/client.rs`. Companion guide for client authors: `daemon-clients.md`. FFI entry points wrapping `DaemonClient` for Julia/Python are planned but not yet shipped.

Co-locating the client with the daemon (rather than as a separate `montre-daemon-client` crate) keeps the wire types in one place — same `serde`-derived structs serialize on both sides. If a downstream consumer ever wants the client without pulling in the daemon's storage/coordination code, the module can be split out then; not premature now.

### Concurrency

Synchronous, single-threaded main loop. No `tokio`, no `async`.

Connection handling: one OS thread per client connection. Threads communicate with the main state-owning thread via channels (mpsc). State mutations happen on the main thread; client threads do reads against immutable corpus data directly when safe, route mutations through the channel.

This is sufficient for v1. Local Unix-socket workloads at expected client counts (handful) don't need anything fancier.

**Query execution stays on the connection thread.** `query.execute` parses, plans, and runs against the immutable corpus directly in the connection thread that received the request. Only the handle-table insert (registering the resulting `Results` so it can be retrieved by handle) routes through the main thread via channel — the `Results` moves through that channel exactly once.

This matters: a complex quantifier query taking 70ms must not serialize every other client. Forcing all execution through the main thread would do exactly that. Since `Corpus` is `Send + Sync` and immutable post-open, executing on connection threads is safe by construction.

`query.hits` retrieval consults the handle table (which is on the main thread) but the actual hit array is held by reference once the connection thread has located it. Pagination work happens on the connection thread.

### Storage

Named results, query history, and (later) observations / workspaces are persisted in `~/.local/share/montre/state/<corpus_stable_key>/`:

```
state/
└── <hash>/
    ├── named_results.jsonl
    ├── query_history.jsonl
    └── observations.jsonl    # v2
```

JSONL (newline-delimited JSON, one record per line). Append-only. Crash-tolerant (partial last line is discarded on read). Compaction is a future concern.

**`daemon_epoch` persistence**: the epoch counter advertised in `session.register` lives at `~/.local/share/montre/state/<hash>/epoch` as a single integer in a one-line file. Daemon reads on startup, increments, writes back. Without persistence, every cold start would invalidate every cached client ID, defeating the cache-invalidation contract. The file is created on first daemon launch with epoch `1`.

**Named results store queries by default, not hit lists.** A saved result records the CQL plus metadata; hits are re-derived on access via `query.hits`. Re-execution costs are negligible at current corpus sizes (milliseconds), the on-disk record is tiny (a CQL string), and a query-backed result survives corpus rebuilds gracefully — it may produce different hits after rebuild, but it doesn't break.

```rust
// On-disk record (one JSON object per line in named_results.jsonl):
struct StoredNamedResult {
    cql: String,
    hit_count: u64,
    created_at: Timestamp,
    // ... and the standard ResultMetadata fields
}
```

Only the query-backed form is persisted. `query.materialize` produces an in-memory snapshot that is *not* written to disk: across a daemon restart, a previously-materialized result reloads as query-backed and re-executes against the current corpus.

Materialization is an explicit opt-in via `query.materialize(name)`, for cases where snapshot semantics matter within a daemon's lifetime: "preserve exactly the hits I had since this morning's daemon start, regardless of corpus changes." Once the daemon exits, the snapshot is gone — clients that need durable point-in-time hits should export them externally (e.g., write the CQL plus the current hit list to their own file). Persistent materialization is a v2 consideration; it would require either storing the full hit list on disk (large) or storing the corpus revision the snapshot was taken against (requires versioned corpus metadata).

**Failure mode for query-backed results**: if a saved query's CQL no longer parses or executes against a rebuilt corpus (e.g., a layer was renamed), `query.hits` for that handle returns error `1204` (stored query invalid). This is a feature, not a bug — the alternative would be silently incorrect results. The user can re-save the result with updated CQL or migrate to materialized form before the rebuild.

### Capability advertisement

The `capabilities` object in `session.register` response advertises optional features:

```json
{
  "observations": false,
  "workspaces": false,
  "coupler_kinds": ["sentence_mirror", "alignment", "kwic_selection",
                   "doc_picker_selection", "named_results_selection", "conllu_view"]
}
```

Clients gate features on capabilities. New optional features land by extending `capabilities`, not by bumping the protocol version.

Bump the protocol version only for incompatible changes (renamed methods, changed param shapes, removed operations).

---

## Examples

### Minimal session: open, query, retrieve hits

Client `→` daemon:
```json
{ "jsonrpc": "2.0", "id": 1, "method": "session.register",
  "params": { "protocol_version": 1, "kind": "external", "label": "julia-script",
              "provides": [], "consumes": [] } }
```

Daemon `→` client:
```json
{ "jsonrpc": "2.0", "id": 1,
  "result": { "process_id": 1, "server_version": "0.6.0", "protocol_version": 1,
              "capabilities": { ... } } }
```

Client `→` daemon:
```json
{ "jsonrpc": "2.0", "id": 2, "method": "query.execute",
  "params": { "cql": "[pos=\"NOUN\"]" } }
```

Daemon `→` client:
```json
{ "jsonrpc": "2.0", "id": 2, "result": { "handle": "r-3a7f...", "hit_count": 244184 } }
```

Client `→` daemon:
```json
{ "jsonrpc": "2.0", "id": 3, "method": "query.hits",
  "params": { "handle": "r-3a7f...", "offset": 0, "limit": 100 } }
```

Daemon `→` client: 100-element hit array.

Client closes connection (or sends `session.unregister` first).

### Coupled reading: KWIC drives reader

Two clients connected. KWIC at `process_id: 4`, reader at `process_id: 9`.

Reader establishes the coupler:
```json
{ "jsonrpc": "2.0", "id": 17, "method": "coupler.create",
  "params": { "master_id": 4, "follower_id": 9, "kind": { "type": "kwic_selection" } } }
```

Daemon responds with `coupler_id`. KWIC user navigates to a different hit; KWIC publishes:
```json
{ "jsonrpc": "2.0", "method": "session.publish_interest",
  "params": { "interest": { "type": "hit", "result": "r-3a7f...", "hit_idx": 23 } } }
```

Daemon transforms `Hit -> Sentence` (resolves the hit's containing sentence) and pushes to the reader:
```json
{ "jsonrpc": "2.0", "method": "notification.coupler_update",
  "params": { "coupler_id": 7, "interest": { "type": "sentence", "doc": 3, "sent": 142 } } }
```

Reader scrolls to the new sentence and highlights it.

### Auto-spawn cold start

User runs `montre reader corpus/`. Reader binary:

1. Computes socket path `~/.local/share/montre/sockets/9c2f8e3a4b1d7f06.sock`.
2. Attempts `connect()` → `ENOENT`.
3. Spawns `montre serve corpus/` detached.
4. Polls `connect()` starting at 50ms, doubling with exponential backoff up to a 250ms ceiling per attempt; 10-second total deadline.
5. After ~2 seconds, daemon is ready; connect succeeds.
6. Reader sends `session.register`.

User sees the reader UI come up. From their perspective there was no "daemon" — the corpus just opened.

---

## Open items for implementation

1. **Cross-corpus operations** — explicitly out of scope for v1. Each daemon serves one corpus; clients connecting to multiple corpora open multiple connections.
2. **Request cancellation** — no in-flight cancellation in v1. Long-running queries run to completion. Downstream callers will eventually want this — likely shape: `request.cancel(id)` operation, with cancelled requests returning a dedicated error code. Defer until a real query lags noticeably.
