# Montre API reference

## Rust API

### Opening a corpus

```rust
use montre_index::Corpus;

let corpus = montre_index::open("path/to/corpus")?;

corpus.token_count()       // total tokens across all components
corpus.layers()            // &[String]: ["word", "lemma", "pos", ...]
corpus.document_names()    // &[String]
corpus.components()        // &[ComponentMeta]
corpus.document_at(pos)    // Option<&str>: document name for a position
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
    pub captures: Vec<(String, Span)>,   // labeled submatches (Phase 2b)
}
```

### Span

```rust
pub struct Span {
    pub start: u64,    // inclusive
    pub end: u64,      // exclusive
}

span.len()              // end - start
span.contains(pos)      // true if start <= pos < end
span.contains_span(&s)  // true if s is fully inside
span.overlaps(&s)       // true if any overlap
```

### Token access

```rust
use montre_core::Value;
use montre_index::ForwardIndex;

// Single position, single layer
let val: Option<&Value> = corpus.forward.get(position, "word");

// Range of positions
let vals: Vec<Option<&Value>> = corpus.forward.get_range(start, end, "lemma");
```

`Value` is either `Value::Str(CompactString)` or `Value::Int(i64)`.

### Inverted index

```rust
use montre_index::InvertedIndex;

// Positions where layer==value (as a RoaringBitmap)
let bitmap = corpus.inverted.get("pos", "NOUN");

// All values for a layer
let values: Option<Vec<&str>> = corpus.inverted.values("pos");

// All indexed layer names
let layers: Vec<&str> = corpus.inverted.layers();
```

### Span index

```rust
use montre_index::SpanIndex;

// All spans for a layer
let sentences: Option<&[Span]> = corpus.spans.spans("sentence");
let documents: Option<&[Span]> = corpus.spans.spans("document");

// Find the span containing a position
let span: Option<&Span> = corpus.spans.containing("sentence", position);

// Available span layers
let layers: Vec<&str> = corpus.spans.layers();
```

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

// Single-component build
let mut builder = CorpusBuilder::new("my-corpus")
    .decompose_feats(true);      // optional: index feats.Number, feats.Gender, etc.

let file = std::fs::File::open("data.conllu")?;
let mut reader = ConllUReader::new(file);
let sentences = reader.read_sentences()?;
builder.add_document("data.conllu", sentences);
builder.build("output/path")?;

// Multi-component build from manifest
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

The `montre-ffi` crate exports 35 `extern "C"` functions. All string arguments are `*const c_char` (null-terminated). All returned strings are owned by Rust; copy and free with `montre_string_free`.

### Error handling

Thread-local last-error pattern. Functions that can fail return null pointers (for pointer-returning functions) or -1 (for count functions).

```c
const char *montre_last_error(void);      // NULL if no error
void montre_string_free(char *s);
void montre_string_array_free(char **arr, uint64_t len);
void montre_i32_array_free(int32_t *arr, uint64_t len);
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
```

### Token access

```c
char *montre_corpus_token_annotation(const void *corpus, uint64_t position, const char *layer);
char *montre_corpus_span_text(const void *corpus, uint64_t start, uint64_t end, const char *layer);
```

### Query

```c
void    *montre_query(const void *corpus, const char *cql);
void    *montre_query_in_component(const void *corpus, const char *cql, const char *component);
int64_t  montre_query_count(const void *corpus, const char *cql);
void     montre_hitlist_free(void *hits);
uint64_t montre_hitlist_len(const void *hits);
uint64_t montre_hit_start(const void *hits, uint64_t index);
uint64_t montre_hit_end(const void *hits, uint64_t index);
uint32_t montre_hit_document_index(const void *hits, uint64_t index);
uint32_t montre_hit_sentence_index(const void *hits, uint64_t index);
void     montre_hitlist_populate_context(void *hits, const void *corpus);
```

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

void *montre_project(
    const void *corpus, const void *source_hits, const char *alignment_name,
    uint64_t *out_unmapped, uint64_t *out_no_alignment, uint64_t *out_projected
);
```

The three out-parameters on `montre_project` are nullable. Pass NULL for any diagnostics you don't need.

## Corpus format

A built corpus is a directory:

```
corpus/
├── corpus.json     # metadata (name, version, layers, components, alignments)
├── inverted.bin    # term → positions (bincode-serialized HashMap<String, HashMap<String, RoaringBitmap>>)
├── forward.bin     # position → annotations per layer
├── spans.bin       # sentence, document, and custom span layers
├── lexicon.bin     # term dictionary per layer
└── alignments.bin  # alignment edges (optional, only for multi-component corpora)
```

Index version is stored in `corpus.json` and checked on load. Current version: 2.

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
