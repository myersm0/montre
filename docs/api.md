# Montre API reference

## Rust API

### Opening a corpus

```rust
use montre_index::Corpus;

let corpus = montre_index::open("path/to/corpus")?;

corpus.token_count()       // total tokens across all components
corpus.layers()            // &[String]: ["word", "lemma", "pos", ...]
corpus.document_names()    // &[String]
corpus.document_at(pos)    // Option<&str>: document name for a position
corpus.document_index_by_name("la-parure")   // Option<usize>
corpus.document_indices_by_name(&names)      // Vec<usize>
corpus.components()        // &[ComponentMeta]
```

`Corpus` is immutable after construction, `Send + Sync`, and safe to share across threads.

### Query pipeline

```rust
use montre_query::executor::{self, Results, Hit};

// 1. Parse CQL string into AST
let parsed = montre_query::parse(r#"[pos="ADJ"] [pos="NOUN"]"#)?;

// 2. Plan AST into executable plan
let plan = montre_query::planner::plan(&parsed)?;

// 3a. Execute: full results
let results: Results = executor::execute(&plan, &corpus)?;

// 3b. Execute: count only (avoids Hit allocation for simple plans)
let count: usize = executor::execute_count(&plan, &corpus)?;
```

### Working with results

```rust
// Length
results.len()
results.is_empty()

// Access hits
results.hits()        // &[Hit]
results.into_hits()   // Vec<Hit>, consumes Results

// Iteration
for hit in &results { ... }         // borrows
for hit in results { ... }          // consumes

// Populate structural context (lazy, call only when needed)
results.populate_context(&corpus);
```

### Hit

```rust
pub struct Hit {
    pub span: Span,                      // token position range [start, end)
    pub document_index: u32,             // populated by populate_context
    pub sentence_index: u32,             // populated by populate_context
    pub captures: Vec<(String, Span)>,   // labeled submatches (populated by labeled queries, e.g. a:[pos="ADJ"])
}

impl Hit {
    pub const UNPOPULATED: u32 = u32::MAX;
}
```

Both `document_index` and `sentence_index` are initialized to `Hit::UNPOPULATED` (`u32::MAX`). Call `Results::populate_context` to fill them from the span index. Code that reads these fields without calling `populate_context` will see `u32::MAX`, not a plausible-looking zero.

### Span

```rust
#[repr(C)]
pub struct Span {
    pub start: u64,    // inclusive
    pub end: u64,      // exclusive
}

span.len()              // end - start
span.contains(pos)      // true if start <= pos < end
span.contains_span(&s)  // true if s is fully inside
span.overlaps(&s)       // true if any overlap
```

`#[repr(C)]` guarantees a 16-byte `(u64, u64)` layout, enabling zero-copy access from memory-mapped span files.

### Token access

```rust
use montre_index::ForwardIndex;

// String layers (word, lemma, upos, xpos, feats, deps, etc.) — zero-copy with mapped backend
let word: Option<&str> = corpus.forward().get_str(position, "word");
let upos: Option<&str> = corpus.forward().get_str(position, "upos");
let xpos: Option<&str> = corpus.forward().get_str(position, "xpos");
let deps: Option<&str> = corpus.forward().get_str(position, "deps");  // raw enhanced deps string

// Integer layers (head) — sentence-local values, not converted to global positions
let head: Option<i64> = corpus.forward().get_int(position, "head");
```

The `ForwardIndex` trait provides `get_str` and `get_int` as its primary interface. Both enable zero-copy access from the memory-mapped forward index. `Value` (`Value::Str(CompactString)` or `Value::Int(i64)`) is used internally by the build pipeline but does not appear in the query-time API.

### Inverted index

```rust
use montre_index::InvertedIndex;

// Positions where layer==value (as a RoaringBitmap)
let bitmap = corpus.inverted().get("pos", "NOUN");

// All values for a layer
let values: Option<Vec<&str>> = corpus.inverted().values("upos");

// All indexed layer names
let layers: Vec<&str> = corpus.inverted().layers();
```

**Note on `pos` alias**: In CQL queries, `pos` is rewritten to `upos` at parse time. When accessing the inverted index or forward index directly via the API, use `"upos"` (the physical layer name).

**Forward-only layers**: The `head` and `deps` layers are stored in the forward index only — they are not present in the inverted index. `head` contains sentence-local integers; `deps` contains raw enhanced dependency strings (e.g., `"2:obj|5.1:nsubj:pass"`). Both have high cardinality or non-string types that make inverted indexing impractical. Use `get_int` to access head values and `get_str` for deps strings. Tokens without enhanced deps return `None` (underscore values are not stored).

### Span index

```rust
use montre_index::SpanIndex;

// All spans for a layer
let sentences: Option<&[Span]> = corpus.spans().spans("sentence");
let documents: Option<&[Span]> = corpus.spans().spans("document");

// Find the span containing a position
let span: Option<&Span> = corpus.spans().containing("sentence", position);

// Available span layers
let layers: Vec<&str> = corpus.spans().layers();
```

### Sentence IDs

```rust
// Sentence ID from CoNLL-U # sent_id comment, or fallback "{doc_name}:{N}"
corpus.sentence_id(sentence_index)    // Option<&str>
corpus.sentence_id_count()            // usize
```

Sentence IDs are stored in a memory-mapped flat file (`sentence_ids.bin`) parallel to the sentence span array. The fallback format for sentences without a `# sent_id` comment is `{document_name}:{0-based_sentence_index_within_document}`.

### Multiword tokens

```rust
use montre_index::MWTEntry;

// Lookup by position — returns the MWT covering this position, if any
corpus.mwt_covering(position)              // Option<MWTEntry>

// All MWTs overlapping a span
corpus.mwts_in_range(start, end)           // Vec<MWTEntry>

// MWTEntry fields
pub struct MWTEntry {
    pub start: u64,           // global token position (inclusive)
    pub end: u64,             // global token position (exclusive)
    pub form: String,         // surface form (e.g., "au", "dell'")
    pub no_space_after: bool, // SpaceAfter=No on the MWT row
}
```

MWTs are stored in `mwt.bin`, a flat memory-mapped binary format sorted by start position. Lookups use binary search. MWT entries do not occupy positions in the token stream — the constituent words are the indexed tokens.

### Spacing

```rust
// Check whether a position has SpaceAfter=No
corpus.has_no_space_after(position)    // bool
```

Spacing flags are stored as a roaring bitmap in `spacing.bin`. The bitmap contains positions where the CoNLL-U misc column includes `SpaceAfter=No`. For positions covered by an MWT, the MWT's own `no_space_after` flag is authoritative — the bitmap is not consulted.

### Empty nodes

```rust
use montre_index::EmptyNodeStore;

corpus.empty_nodes()                                    // Option<&EmptyNodeStore>
corpus.empty_nodes().unwrap().in_sentence(sentence_idx) // &[EmptyNode]
corpus.empty_nodes().unwrap().len()                     // usize
```

Empty nodes (decimal-ID rows like `6.1` in CoNLL-U) are stored in `empty_nodes.json`. They are not assigned positions in the token stream and do not appear in the forward index, inverted index, or query results. Fields: `sentence_index`, `node_id`, `form`, `lemma`, `upos`, `xpos`, `feats`, `deps`, `misc` (all optional strings except `sentence_index`, `node_id`, and `form`).

### Surface text

```rust
// MWT-aware, SpaceAfter-aware text reconstruction
corpus.surface_text(start, end)    // String
```

Reconstructs visible text for a token range by consulting MWTs and spacing flags. When a position is covered by an MWT, the MWT surface form is emitted and constituent word positions are skipped. Spacing between tokens is suppressed where `SpaceAfter=No` applies — either from the MWT's flag or the token-level spacing bitmap.

### Components and alignments

```rust
// Components
corpus.components()                    // &[ComponentMeta]
corpus.component("maupassant-fr")      // Option<&ComponentMeta>
corpus.component_for_document(idx)     // Option<&ComponentMeta>
corpus.is_multi_component()            // bool

// ComponentMeta
pub struct ComponentMeta {
    pub id: u32,
    pub name: String,
    pub language: String,
    pub document_range: (usize, usize),   // half-open range into document list
}

// Alignments
corpus.alignment_meta("labse")         // Option<&AlignmentMeta>
corpus.alignment_edges("labse")        // Option<&[(UnitId, UnitId)]>

// AlignmentMeta
pub struct AlignmentMeta {
    pub name: String,
    pub source_component: String,
    pub target_component: String,
    pub source_layer: String,
    pub target_layer: String,
    pub directed: bool,
    pub edge_count: usize,
}
```

### Projection helpers

These are public in `montre_query::executor` for use by FFI and external tools:

```rust
use montre_core::UnitId;  // (u32, u32) = (doc_within_component, sentence_within_doc)

// Build a HashMap for O(1) edge lookup
let edge_map = executor::build_edge_map(edges);

// Locate a hit's document and sentence within a component
let (doc, sent) = executor::find_doc_and_sent(&hit, doc_spans, sent_spans, &comp)?;

// Resolve a target unit ID to a span
let span = executor::resolve_target_span(tgt_doc, tgt_sent, doc_spans, tgt_sent_spans, &tgt_comp)?;
```

### Building a corpus

```rust
use montre_build::builder::CorpusBuilder;
use montre_build::format::conllu::ConllUReader;
use montre_build::format::CorpusReader;

// Single-component build from a directory (preferred for large corpora —
// uses streaming forward writer to avoid accumulating forward index in memory)
let builder = CorpusBuilder::from_directory(
    "my-corpus",
    Path::new("data/"),
    true,       // decompose_feats
    false,      // strict
)?;
builder.build("output/path")?;

// Incremental build (forward index accumulates in memory)
let mut builder = CorpusBuilder::new("my-corpus")
    .decompose_feats(true);

let file = std::fs::File::open("data.conllu")?;
let mut reader = ConllUReader::new(file);
let sentences = reader.read_sentences()?;
builder.add_document("data.conllu", sentences);
builder.build("output/path")?;

// Multi-component build from manifest (streaming, sequential components)
use montre_build::MultiCorpusBuilder;

MultiCorpusBuilder::from_manifest("corpus.toml")?
    .strict(true)                // fail on first parse error
    .decompose_feats(true)       // override manifest setting
    .build("output/path")?;
```

### Error types

```rust
// montre_index::IndexError
IndexError::Io(io::Error)
IndexError::Format(String)
IndexError::NotFound(String)
IndexError::LayerNotFound(String)
IndexError::VersionMismatch { expected, found }

// montre_query::QueryError
QueryError::Parse { position, message }
QueryError::Regex(regex::Error)
QueryError::UnknownLayer(String)
QueryError::UnknownLabel(String)
QueryError::Execution(String)

// montre_build::BuildError
BuildError::Io(io::Error)
BuildError::Parse { line, message }
BuildError::Json(serde_json::Error)
BuildError::Manifest(String)
BuildError::UnknownComponent(String)
BuildError::Alignment(String)
```

## C FFI

The `montre-ffi` crate exports 78 `extern "C"` functions across eight modules. All string arguments are `*const c_char` (null-terminated). All returned strings are owned by Rust; copy and free with `montre_string_free`.

### Error handling

Thread-local last-error pattern. Functions that can fail return null pointers (for pointer-returning functions) or -1 (for count functions).

```c
const char *montre_last_error(void);      // NULL if no error
void montre_string_free(char *s);
void montre_string_array_free(char **arr, uint64_t len);
void montre_i32_array_free(int32_t *arr, uint64_t len);
void montre_u32_array_free(uint32_t *arr, uint64_t len);
void montre_u64_array_free(uint64_t *arr, uint64_t len);
```

### Corpus

```c
void      *montre_corpus_open(const char *path);
void       montre_corpus_close(void *corpus);
uint64_t   montre_corpus_token_count(const void *corpus);
uint32_t   montre_corpus_layer_count(const void *corpus);
char      *montre_corpus_layer_name(const void *corpus, uint32_t index);
uint32_t   montre_corpus_document_count(const void *corpus);
char      *montre_corpus_document_name(const void *corpus, uint32_t index);
uint32_t   montre_corpus_component_count(const void *corpus);
char      *montre_corpus_component_name(const void *corpus, uint32_t index);
char      *montre_corpus_component_language(const void *corpus, uint32_t index);
int32_t    montre_corpus_component_document_range(const void *corpus, uint32_t index, uint32_t *out_start, uint32_t *out_end);
int32_t    montre_corpus_component_for_document(const void *corpus, uint32_t doc_index);
int64_t    montre_corpus_component_token_count(const void *corpus, uint32_t index);
int64_t    montre_corpus_document_index_by_name(const void *corpus, const char *name);
int32_t    montre_corpus_component_index_by_name(const void *corpus, const char *name);
```

`montre_corpus_document_index_by_name` returns the global document index, or -1 if not found. `montre_corpus_component_index_by_name` returns the component index, or -1 if not found.

### Inverted index introspection

```c
// All distinct values for a layer (e.g., all POS tags). Free with montre_string_array_free.
char **montre_corpus_inverted_values(const void *corpus, const char *layer, uint64_t *out_len);

// Bitmap cardinality for a single layer/value pair. Returns -1 if not found.
int64_t montre_corpus_inverted_count(const void *corpus, const char *layer, const char *value);

// All values and their bitmap cardinalities for a layer.
// Returns parallel arrays: out_values (string array) and out_counts (u64 array).
// Free values with montre_string_array_free, counts with montre_u64_array_free.
// Returns 1 on success, 0 if the layer does not exist.
int32_t montre_corpus_inverted_counts(
    const void *corpus, const char *layer,
    char ***out_values, uint64_t **out_counts, uint64_t *out_len
);
```

### Token access

```c
char *montre_corpus_token_annotation(const void *corpus, uint64_t position, const char *layer);
char *montre_corpus_span_text(const void *corpus, uint64_t start, uint64_t end, const char *layer);

// Bulk annotation extraction for a position range. Free with montre_string_array_free.
char **montre_corpus_token_annotations(const void *corpus, uint64_t start, uint64_t end, const char *layer, uint64_t *out_len);
```

### Query

```c
void    *montre_query(const void *corpus, const char *cql);
void    *montre_query_in_component(const void *corpus, const char *cql, const char *component);
int64_t  montre_query_count(const void *corpus, const char *cql);
int64_t  montre_query_count_in_component(const void *corpus, const char *cql, const char *component);
void     montre_hitlist_free(void *hits);
uint64_t montre_hitlist_len(const void *hits);
uint64_t montre_hit_start(const void *hits, uint64_t index);
uint64_t montre_hit_end(const void *hits, uint64_t index);
uint32_t montre_hit_document_index(const void *hits, uint64_t index);
uint32_t montre_hit_sentence_index(const void *hits, uint64_t index);
void     montre_hitlist_populate_context(void *hits, const void *corpus);
```

`montre_hit_document_index` and `montre_hit_sentence_index` return `UINT32_MAX` (`Hit::UNPOPULATED`) before `montre_hitlist_populate_context` is called. This includes hits returned by `montre_project`. Check for this sentinel before using the index as a lookup key.

```c
// Bulk hit field extraction as flat u64 arrays. Free with montre_u64_array_free.
uint64_t *montre_hitlist_starts(const void *hits, uint64_t *out_len);
uint64_t *montre_hitlist_ends(const void *hits, uint64_t *out_len);
uint64_t *montre_hitlist_document_indices(const void *hits, uint64_t *out_len);
uint64_t *montre_hitlist_sentence_indices(const void *hits, uint64_t *out_len);

// Labeled capture accessors for a single hit.
uint32_t montre_hit_capture_count(const void *hits, uint64_t index);
char    *montre_hit_capture_name(const void *hits, uint64_t hit_index, uint32_t capture_index);
uint64_t montre_hit_capture_start(const void *hits, uint64_t hit_index, uint32_t capture_index);
uint64_t montre_hit_capture_end(const void *hits, uint64_t hit_index, uint32_t capture_index);
```

Capture accessors return data from labeled queries (e.g., `a:[pos="ADJ"] b:[pos="NOUN"]`). `montre_hit_capture_count` returns 0 for unlabeled queries. Free capture name strings with `montre_string_free`.

### Bulk extraction

```c
// Matched text for every hit
char **montre_hitlist_texts(const void *hits, const void *corpus, const char *layer, uint64_t *out_len);

// Context tokens with relative positions
void montre_context_tokens(
    const void *hits, const void *corpus,
    uint32_t window, const char *layer,
    int32_t **out_positions, char ***out_tokens,
    uint64_t **out_offsets, uint64_t *out_len
);
```

### Alignments and projection

```c
uint32_t montre_corpus_alignment_count(const void *corpus);
char    *montre_corpus_alignment_name(const void *corpus, uint32_t index);
char    *montre_corpus_alignment_source(const void *corpus, uint32_t index);
char    *montre_corpus_alignment_target(const void *corpus, uint32_t index);
uint64_t montre_corpus_alignment_edge_count(const void *corpus, uint32_t index);
char    *montre_corpus_alignment_source_layer(const void *corpus, uint32_t index);
char    *montre_corpus_alignment_target_layer(const void *corpus, uint32_t index);
int32_t  montre_corpus_alignment_directed(const void *corpus, uint32_t index);

// Raw edge access: flat array of [src_doc, src_sent, tgt_doc, tgt_sent, ...] quads.
// out_len receives edge count (array length is out_len * 4). Free with montre_u32_array_free.
uint32_t *montre_corpus_alignment_edges(const void *corpus, const char *name, uint64_t *out_len);

void *montre_project(
    const void *corpus, const void *source_hits, const char *alignment_name,
    uint64_t *out_unmapped, uint64_t *out_no_alignment, uint64_t *out_projected
);

// Per-document alignment coverage for the source side of a named alignment.
// Returns parallel u32 arrays: global document indices, aligned unit counts, total unit counts.
// Free each array with montre_u32_array_free(array, len).
// Returns 1 on success, 0 on failure.
int32_t montre_corpus_alignment_coverage(
    const void *corpus, const char *alignment_name,
    uint32_t **out_doc_indices, uint32_t **out_aligned, uint32_t **out_total,
    uint64_t *out_len
);
```

The three out-parameters on `montre_project` are nullable. Pass NULL for any diagnostics you don't need.

`montre_corpus_alignment_coverage` returns one entry per document in the source component of the named alignment. For each document, `aligned` is the number of source-layer units (typically sentences) that have at least one alignment edge, and `total` is the total number of source-layer units in that document.

### Span index access

```c
uint32_t montre_corpus_span_layer_count(const void *corpus);
char    *montre_corpus_span_layer_name(const void *corpus, uint32_t index);
int64_t  montre_corpus_span_count(const void *corpus, const char *layer);
int32_t  montre_corpus_span_at(const void *corpus, const char *layer, uint64_t index, uint64_t *out_start, uint64_t *out_end);
int64_t  montre_corpus_span_containing(const void *corpus, const char *layer, uint64_t position, uint64_t *out_start, uint64_t *out_end);
int64_t  montre_corpus_span_count_in_range(const void *corpus, const char *layer, uint64_t token_start, uint64_t token_end);

// Resolve a sentence span from component-relative coordinates.
// Returns the global sentence index (for use with montre_corpus_sentence_id), or -1 on failure.
int64_t montre_corpus_sentence_span(
    const void *corpus, uint32_t component_index,
    uint32_t doc_within_component, uint32_t sentence_within_doc,
    uint64_t *out_start, uint64_t *out_end
);
```

`montre_corpus_span_containing` returns the span index, or -1 if not found. The out-parameters are nullable — pass NULL if you only need the index.

`montre_corpus_sentence_span` collapses the multi-step coordinate translation (component → document range → document span → sentence span) into a single call. The returned global sentence index can be passed directly to `montre_corpus_sentence_id`.

### Sentence IDs

```c
uint64_t montre_corpus_sentence_id_count(const void *corpus);
char    *montre_corpus_sentence_id(const void *corpus, uint64_t sentence_index);
char   **montre_corpus_sentence_ids(const void *corpus, uint64_t *out_len);
```

`montre_corpus_sentence_id` returns an owned string (free with `montre_string_free`), or NULL if the index is out of bounds. `montre_corpus_sentence_ids` returns a string array (free with `montre_string_array_free`).

### Multiword tokens and surface text

```c
// MWT form at a position, or NULL if no MWT covers it
char *montre_corpus_mwt_form(const void *corpus, uint64_t position);

// Full MWT info at a position. Returns 1 if found, 0 otherwise. Out-params are nullable.
int32_t montre_corpus_mwt_at(
    const void *corpus, uint64_t position,
    uint64_t *out_start, uint64_t *out_end, char **out_form
);

// MWT-aware, SpaceAfter-aware text reconstruction for a token range.
char *montre_corpus_surface_text(const void *corpus, uint64_t start, uint64_t end);

// Check SpaceAfter=No flag. Returns 1 if set, 0 otherwise.
int32_t montre_corpus_has_no_space_after(const void *corpus, uint64_t position);
```

`montre_corpus_span_text` and `montre_hitlist_texts` automatically use surface text reconstruction when the layer is `"word"`.

### Empty nodes

```c
uint64_t montre_corpus_empty_node_count(const void *corpus);

uint64_t montre_corpus_empty_node_count_in_sentence(const void *corpus, uint32_t sentence_index);

// Access a field of the Nth empty node in a sentence. Field names: "node_id", "form",
// "lemma", "upos", "xpos", "feats", "deps", "misc". Returns NULL for unknown fields
// or absent optional fields. Free with montre_string_free.
char *montre_corpus_empty_node_field(
    const void *corpus, uint32_t sentence_index, uint32_t node_index, const char *field
);
```

### Build

```c
int32_t montre_build_directory(const char *name, const char *input_dir, const char *output_dir, int32_t decompose_feats, int32_t strict);
int32_t montre_build_manifest(const char *manifest_path, const char *output_dir, int32_t decompose_feats, int32_t strict);
```

Both return 1 on success, 0 on failure. Check `montre_last_error()` on failure. For `montre_build_manifest`, a nonzero `decompose_feats` overrides the manifest setting; zero leaves the manifest default.

## Corpus format

A built corpus is a directory:

```
corpus/
├── corpus.json          # metadata (name, version, layers, components, alignments)
├── inverted.bin         # term → positions (bincode-serialized HashMap<String, HashMap<String, RoaringBitmap>>)
├── forward.bin          # position → annotations per layer (flat mmap format: bitmap-sparse, dictionary-coded)
├── spans.bin            # sentence, document, and custom span layers (flat mmap format)
├── sentence_ids.bin     # CoNLL-U sent_id values parallel to sentence spans (flat mmap format)
├── mwt.bin              # multiword token side table (flat mmap format, optional)
├── spacing.bin          # SpaceAfter=No flags as roaring bitmap (optional)
├── empty_nodes.json     # empty node records per sentence (optional)
├── lexicon.bin          # term dictionary per layer (bincode)
└── alignments.bin       # alignment edges (optional, only for multi-component corpora)
```

Index version is stored in `corpus.json` and checked on load. Current version: 5.

The `forward.bin`, `spans.bin`, `sentence_ids.bin`, and `mwt.bin` files use custom flat binary formats designed for memory-mapped access. They are opened via `mmap` on `Corpus::open` with no deserialization — the OS pages data into RAM on demand. The `inverted.bin` and `lexicon.bin` files are still bincode-serialized and deserialized into heap structures on open. The `spacing.bin` file contains a serialized roaring bitmap with a small header. The `empty_nodes.json` file is deserialized into memory on open.

## Build manifest

```toml
[corpus]
name = "isosceles"
decompose_feats = true     # optional, default false

[components.fr]
path = "data/fr/conllu/"
language = "fr"

[components.en]
path = "data/en/conllu/"
language = "en"

[alignments.labse]
source = "fr"
target = "en"
edges = "alignments/labse/"     # single TSV file or directory of TSVs
source_layer = "sentence"       # default: "sentence"
target_layer = "sentence"       # default: "sentence"
```

Alignment TSV format: `src_doc\tsrc_sent\ttgt_doc\ttgt_sent` (tab-separated, 0-based sentence indices within document).
