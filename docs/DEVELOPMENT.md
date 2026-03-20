# Montre development guide

## Design principles

### Corpus as value

A Montre corpus is a self-contained, immutable directory produced by `montre build`. All data required for querying—tokens, annotations, document boundaries, span layers, and alignment relations—lives within this directory.

Montre does not rely on:
- Global registries
- Environment variables
- External configuration files
- Shared indexes or system-wide state

This design prioritizes embedding, reproducibility, and portability: one path → one corpus → one semantic universe.

### Engine first, presentation last

The query engine returns structured results (spans, IDs). Display concerns (KWIC formatting, highlighting, pagination) belong in the CLI or UI layer, not the core engine. This keeps the engine language-agnostic and embeddable.

### Parallelism as internal relation

Parallel corpora are not "two corpora joined at query time." They are one corpus with multiple components and named alignment relations between them. This ensures:
- Stable identity (the corpus is one artifact)
- Correctness guarantees (alignment validated at build time)
- Simpler query planning (no cross-corpus joins)

**Clarification**: "One corpus" does not mean "one language" or "one text." It means:
- One build artifact
- One lexicon namespace
- One set of stable unit IDs
- One coordinate system for alignment

Components remain independently queryable. You can query `maupassant-fr` as if the English never existed. The "single corpus" constraint is what *enables* flexible alignment, not what restricts it.

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                        montre-cli                           │
│                   (thin shell, display)                     │
└─────────────────────────────────────────────────────────────┘
                              │
        ┌─────────────────────┼─────────────────────┐
        ▼                     ▼                     ▼
┌───────────────┐    ┌───────────────┐    ┌───────────────┐
│  montre-query │    │ montre-build  │    │  montre-index │
│               │    │               │    │               │
│ Parser        │    │ CorpusReader  │    │ Corpus        │
│ AST           │    │ CorpusBuilder │    │ InvertedIndex │
│ Planner       │    │ (conllu, json)│    │ ForwardIndex  │
│ Executor      │    │               │    │ SpanIndex     │
└───────────────┘    └───────────────┘    └───────────────┘
        │                     │                     │
        └─────────────────────┼─────────────────────┘
                              ▼
                    ┌───────────────┐
                    │  montre-core  │
                    │               │
                    │ Span, Token   │
                    │ Position      │
                    │ Value         │
                    └───────────────┘
```

### Crate responsibilities

| Crate | Purpose | Depends On |
|-------|---------|------------|
| `montre-core` | Primitive types: `Span`, `Position`, `Value`, `Token` | (none) |
| `montre-index` | Index structures and corpus loading | `montre-core` |
| `montre-build` | Corpus construction from source formats | `montre-core`, `montre-index` |
| `montre-query` | Query parsing, planning, execution | `montre-core`, `montre-index` |
| `montre-cli` | Command-line interface | all of the above |
| `montre-ffi` | C FFI for Julia/Python/R bindings | `montre-core`, `montre-index`, `montre-query` |
| `montre-py` | Python bindings (future) | all of the above |

## Data model

### Core entities

**Token**: A position in the corpus with annotations across multiple layers.

**Layer**: A named annotation dimension (word, lemma, pos, xpos, feats, deprel, head). With `decompose_feats` enabled, morphological features are additionally indexed as `feats.Number`, `feats.Gender`, etc.

**Span**: A contiguous range of token positions `[start, end)`.

**Span Layer**: A named collection of non-overlapping spans (sentence, document, paragraph, stanza, scene, etc.).

**Component**: A labeled subcorpus within a multi-component corpus. Each document belongs to exactly one component.

```rust
struct Component {
    id: u32,
    name: String,      // "maupassant-fr", "poe-1845"
    language: String,  // "fr", "en"
}
```

**Alignment**: A named relation mapping units in one component/layer to units in another.

```rust
struct Alignment {
    name: String,              // "labse_sentence", "manual_stanza"
    source_component: String,
    target_component: String,
    source_layer: String,      // "sentence", "stanza", "paragraph"
    target_layer: String,      // may differ from source
    directed: bool,
    edges: Vec<(UnitId, UnitId)>,
}

// Unit IDs are stable across rebuilds
type UnitId = (u32, u32);  // (document_index, unit_index_within_doc)
```

**Future: Weighted Alignment Edges** (v0.2+)

Neural aligners (LaBSE, vecalign) and even Church-Gale produce confidence scores. A future edge format will support:

```rust
struct AlignmentEdge {
    source: UnitId,
    target: UnitId,
    weight: Option<f32>,  // similarity/confidence score
    flags: u8,            // manual, auto, reviewed, filtered
}
```

This enables threshold filtering (`--min-confidence 0.7`) and empirical comparison of alignment algorithms.

**Non-Exhaustive Alignments**

Not every unit must participate in an alignment. Missing edges are semantically meaningful, not errors. This is essential for:
- Truncated translations (Baudelaire omitting Poe passages)
- Partial editions (1845 vs 1850 textual variants)
- Translator omissions or additions
- Reference corpora with no parallel text

### Query result model

Each hit includes structural context for filtering, grouping, and alignment projection:

```rust
pub struct Hit {
    pub span: Span,
    pub document_index: u32,
    pub sentence_index: u32,
    pub captures: Vec<(String, Span)>,  // labeled submatches
}
```

No strings, no formatting, no KWIC. The engine returns positions and IDs; display is the CLI's job.

## Corpus directory layout

### Simple corpus (single-component)

```
my-corpus/
├── corpus.json          # metadata
├── inverted.bin         # term → positions
├── forward.bin          # position → annotations
├── spans.bin            # sentence, document spans
└── lexicon.bin          # term dictionary
```

### Multi-component corpus with alignments

```
isosceles/
├── corpus.json
├── components/
│   ├── maupassant-fr/
│   │   ├── inverted.bin
│   │   ├── forward.bin
│   │   └── spans.bin
│   ├── maupassant-en/
│   │   └── ...
│   └── poe-1845/
│       └── ...
├── lexicon.bin          # shared across components
└── alignments/
    ├── labse_sentence/
    │   ├── meta.json
    │   └── edges.bin
    ├── church_gale_sentence/
    │   └── ...
    └── manual_stanza/
        └── ...
```

### Alignment metadata

```json
{
  "name": "labse_sentence",
  "source_component": "maupassant-fr",
  "target_component": "maupassant-en",
  "source_layer": "sentence",
  "target_layer": "sentence",
  "directed": true
}
```

## Span layers

Span layers are extensible. Beyond the implicit `sentence` (from CoNLL-U blank lines) and `document` (from file boundaries), corpora can define:

| Layer | Typical Source | Use Case |
|-------|----------------|----------|
| `paragraph` | Blank lines in source text | Prose alignment |
| `stanza` | Blank lines in verse | Poetry |
| `line` | Line breaks | Verse drama, poetry |
| `scene` | Markers (`# SCENE`) | Drama |
| `act` | Markers (`# ACT`) | Drama |
| `chapter` | Markers or metadata | Novels |

Declared in the build manifest:

```toml
[span_layers]
sentence = "auto"
paragraph = "blank_line"
stanza = "blank_line"
scene = "marker:# SCENE"
```

Alignment can cross layer types (paragraph ↔ stanza) for cases like prose poems translated into verse.

## Morphological feature decomposition

CoNLL-U stores morphological features as packed key-value strings: `Gender=Masc|Number=Sing|Person=3`. Querying individual features requires regex (`[feats=/.*Number=Plur.*/]`), which is slow and error-prone.

With `decompose_feats = true` in the build manifest (or `--decompose-feats` on the CLI), the builder splits feats at `|` and `=`, creating separate layers: `feats.Gender` → `Masc`, `feats.Number` → `Sing`. These are indexed in both the inverted and forward indexes, enabling clean queries:

```cql
[pos="NOUN" & feats.Number="Plur"]
[feats.Gender="Masc" & feats.Tense="Past"]
```

Design decisions:

- **Dotted names** (`feats.Number`, not bare `Number`): avoids namespace collision with core layers, enables introspection (`corpus.layers().filter(|l| l.starts_with("feats."))`) without new API.
- **Layers created lazily**: the set of feature keys is discovered during the build pass. Not all tokens have all features.
- **Raw feats always indexed**: the concatenated string stays available for exact-match and regex queries regardless of the decomposition flag.
- **Off by default**: no surprise index bloat. Opt in via manifest or CLI.

## Build configuration

### Single-file build

```bash
montre build -i corpus.conllu -o my-corpus/
```

### Multi-component build (manifest)

```toml
# isosceles.toml
[corpus]
name = "isosceles"

[components.maupassant-fr]
path = "data/maupassant/fr/conllu/"
language = "fr"

[components.maupassant-en]
path = "data/maupassant/en/conllu/"
language = "en"

[components.poe-1845]
path = "data/poe/1845/conllu/"
language = "en"

[components.poe-1850]
path = "data/poe/1850/conllu/"
language = "en"

[components.baudelaire]
path = "data/poe/baudelaire/conllu/"
language = "fr"

[span_layers]
sentence = "auto"
paragraph = "blank_line"

[alignments.labse_sentence]
source = "maupassant-fr"
target = "maupassant-en"
source_layer = "sentence"
target_layer = "sentence"
edges = "alignments/maupassant_labse.tsv"

[alignments.poe_1845_baudelaire]
source = "poe-1845"
target = "baudelaire"
source_layer = "sentence"
target_layer = "sentence"
edges = "alignments/poe_1845_baud.tsv"

[alignments.poe_1850_baudelaire]
source = "poe-1850"
target = "baudelaire"
source_layer = "sentence"
target_layer = "sentence"
edges = "alignments/poe_1850_baud.tsv"
```

```bash
montre build -m isosceles.toml -o isosceles/
```

## Query pipeline

```
CQL string
    │
    ▼
┌─────────┐
│ Parser  │  winnow-based, produces AST
└─────────┘
    │
    ▼
┌─────────┐
│   AST   │  TokenPattern, Sequence, Constraint, Label
└─────────┘
    │
    ▼
┌─────────┐
│ Planner │  AST → PlanNode tree
└─────────┘
    │
    ▼
┌──────────┐
│ PlanNode │  ScanLiteral, ScanRegex, Sequence, Filter, AlignProject
└──────────┘
    │
    ▼
┌──────────┐
│ Executor │  Evaluates plan against index
└──────────┘
    │
    ▼
  Results (Vec<Hit>)
```

## Query syntax reference

### Implemented (Phase 0-1)

```cql
[word="house"]              # literal match
[lemma="be"]                # any layer
[pos="NOUN"]
[word="house" & pos="NOUN"] # conjunction
[word="hou.*"]              # regex
"house"                     # shorthand for [word="house"]
[pos="DET"] [pos="NOUN"]    # sequence
```

### Phase 2a: Query language MVP

```cql
[pos!="PUNCT"]              # negation
[]                          # matchall (any token)
[pos="NOUN"]+               # one or more
[pos="ADJ"]*                # zero or more
[pos="DET"]?                # optional
[]{2,5}                     # repetition range
[pos="NOUN"] | [pos="VERB"] # alternation
[pos="DET"] [pos="ADJ"]* [pos="NOUN"]  within s    # sentence constraint
[lemma="house"] within doc                          # document constraint
```

### Phase 2b: Labels, global constraints, named query results

```cql
# Labels mark positions
a:[pos="ADJ"] [pos="NOUN"]

# Global constraints express relationships
a:[word=".*"] []{0,5} b:[] :: a.word = b.word     # repetition
a:[pos="NOUN"] []* b:[pos="NOUN"] :: a.lemma = b.lemma  # same lemma

# Distance constraints
a:[lemma="house"] []{0,20} b:[lemma="home"] :: distance(a,b) >= 5

# Named Query Results
A = [lemma="maison"];
B = subset A where match.document_author = "Baudelaire";
C = difference A B;
expand C to s;
```

### Phase 3: Parallel corpus queries

```cql
# Query with component filter
[pos="ADJ"] [pos="NOUN"] within component:"maupassant-fr"

# Alignment projection
[lemma="bibelot"] =labse_sentence=> component:"maupassant-en"

# Different alignment
[lemma="bibelot"] =manual_stanza=> component:"maupassant-en"
```

**Projection Semantics**: The `=name=>` operator performs *projection*, not a join:
- Input: hits in source component
- Output: corresponding spans in target component
- Cardinality may change (1→many, many→1, or 0 if unaligned)
- Result is a new hit set in the target, not paired tuples

This is distinct from "show aligned pairs" (a separate display/export operation).

Note: Not all constructs will be implemented simultaneously. Early versions prioritize semantic correctness over syntactic completeness.

## Implementation status

### Phase 0: Foundation ✓

- [x] Core data model (`Span`, `Token`, `Value`)
- [x] In-memory inverted index (roaring bitmaps)
- [x] In-memory forward index
- [x] Sentence/document span tracking
- [x] Lexicon with term IDs
- [x] Bincode serialization

### Phase 1: Basic queries ✓

- [x] CQL parser (winnow)
- [x] Literal and regex token patterns
- [x] Attribute conjunction (`&`)
- [x] Two-token sequences
- [x] Query planner
- [x] Basic executor
- [x] KWIC display (CLI)

### Phase 1a: Multi-file support ✓

- [x] Directory traversal (`walkdir`)
- [x] Per-file document boundaries
- [x] Document name tracking
- [x] Lenient CoNLL-U parsing (skip malformed sentences)
- [x] `--strict` mode for fail-fast

### Phase 2a: Query language MVP ✓

- [x] Negation (`!=`)
- [x] Matchall (`[]`)
- [x] Quantifiers (`+`, `*`, `?`, `{n,m}`)
- [x] Alternation (`|`)
- [x] `within s` / `within doc` constraints
- [x] N-token sequences (arbitrary length)
- [x] Run-based quantifier execution (see below)

**Run-based quantifier model**

Quantifiers are implemented as *run-based span generators*, not per-position expansion:

1. **Run detection**: Convert positions to maximal contiguous runs in O(k) where k = matching tokens
2. **Span generation**: First step generates spans from runs; subsequent steps probe runs at boundary positions
3. **Boundary tracking**: Active set stored as `HashMap<end, Vec<start>>` to avoid O(N²) blowup
4. **Epsilon handling**: `min=0` propagates spans unchanged (no zero-length span materialization)

This model:
- Scales linearly with matching tokens, not corpus size
- Handles `[]` (ScanAll) efficiently in non-first positions
- Makes optional patterns (`?`, `{0,n}`) correct by construction
- Caps quantifier width at 100 to bound worst-case expansion

Performance on test corpus (Maupassant sub-corpus of stories from Isosceles corpus: 1.6m French tokens, 1m English tokens):

| Query | Matches | Time |
|-------|---------|------|
| `[pos="ADJ"] [pos="NOUN"]` | 30,672 | 13ms |
| `[pos="ADJ"]? [pos="NOUN"]` | 272,019 | 72ms |
| `[pos="ADJ"]{2,4}` | 1,628 | 0.5ms |
| `[pos="DET"]{2} [pos="NOUN"]{2}` | 14 | 2.6ms |
| `[] [lemma="chat"]` | 120 | 132ms |

### Phase 2b: Labels & global constraints

- [ ] Label syntax (`a:[pos="ADJ"]`)
- [ ] Global constraints (`:: a.lemma = b.lemma`)
- [ ] `distance(a, b)` function
- [ ] Named Query Results
- [ ] Set operations (subset, difference, intersection)
- [ ] `expand` to sentence/document

**Implementation plan**

Phase 2b introduces *labeled captures* and *global constraints*. This is a significant extension because constraints operate over the full match, not just local token properties.

**Step 1: Labels in AST and parser**

Extend `Query` AST:
```rust
Query::Labeled {
    name: String,
    inner: Box<Query>,
}
```

Parser recognizes `a:[...]` syntax. Labels can appear on any query element, including groups and quantified expressions.

**Step 2: Captures in Hit**

The `Hit` struct already has `captures: Vec<(String, Span)>`. The executor must populate this when labeled nodes match.

**Step 3: Planner changes**

Labels are transparent to the planner — they wrap nodes without changing execution strategy. The planner passes label information through to the executor.

**Step 4: Executor capture tracking**

During sequence execution, track which labeled subexpressions matched at which spans. After a complete match, record `(label_name, span)` pairs in the Hit.

Key complexity: a label inside a quantified expression (e.g., `a:[pos="ADJ"]+`) may capture multiple spans. Decide semantics:
- Option A: Capture first occurrence only
- Option B: Capture all occurrences (changes capture type to `Vec<Span>`)
- Option C: Capture the full quantified span

Recommend Option C for simplicity — the label captures the entire quantified match, not individual repetitions.

**Step 5: Global constraints**

Global constraints appear after `::` and express relationships between labeled positions:

```cql
a:[pos="NOUN"] []* b:[pos="NOUN"] :: a.lemma = b.lemma
```

Parser extension:
```rust
Query::Constrained {
    pattern: Box<Query>,
    constraints: Vec<GlobalConstraint>,
}

enum GlobalConstraint {
    Eq { left: LabelAttr, right: LabelAttr },
    Ne { left: LabelAttr, right: LabelAttr },
    Distance { left: String, right: String, op: CmpOp, value: u32 },
}

struct LabelAttr {
    label: String,
    attr: String,  // "lemma", "word", "pos", etc.
}
```

**Step 6: Constraint evaluation**

After finding candidate matches, filter by global constraints. This requires:
1. For each candidate hit, look up attribute values at captured positions
2. Evaluate constraint predicates
3. Keep only hits where all constraints are satisfied

This is a post-filter operation — execute the pattern first, then filter. For very selective constraints on large result sets, this could be slow. Future optimization: push constraints into execution when possible.

**Step 7: Distance function**

`distance(a, b)` returns token distance between labeled spans:
```
distance = b.start - a.end  // gap between spans
// or: b.start - a.start   // start-to-start distance
```

Document the chosen semantics clearly.

**Step 8: Named Query Results (deferred)**

This is essentially query variables and set operations:
```cql
A = [lemma="maison"];
B = subset A where match.document_author = "Baudelaire";
C = difference A B;
```

This requires:
- REPL or script mode (not single-query CLI)
- Result storage
- Metadata filtering

Recommend deferring this to Phase 2c or later. It's useful but not core to the query language.

**Testing priorities**

1. `a:[pos="ADJ"] [pos="NOUN"]` — basic label, capture in hit
2. `a:[pos="ADJ"]+ [pos="NOUN"]` — label on quantified expression
3. `a:[word=".*"] b:[word=".*"] :: a.word = b.word` — simple equality constraint
4. `a:[pos="NOUN"] []{0,5} b:[pos="NOUN"] :: a.lemma = b.lemma` — same-lemma repetition
5. `a:[] b:[] :: distance(a,b) >= 3` — distance constraint

### Phase 2c: Hit model enhancement ✓

- [x] Add `document_index` to `Hit`
- [x] Add `sentence_index` to `Hit`
- [x] Lazy context population (`Results::populate_context`)

### Phase 3: Parallel corpus support ✓

- [x] TOML build manifest
- [x] Component model (`ComponentMeta`)
- [x] Multi-component builder (`MultiCorpusBuilder`)
- [x] Alignment storage (`AlignmentIndex`)
- [x] Alignment edge ingestion (TSV format)
- [x] `within component:X` filter
- [x] `=alignment=>` projection operator
- [x] CLI `--manifest` build option
- [x] `montre info` shows components and alignments

### Phase 3a: FFI, performance, code quality ✓

- [x] C FFI crate (`montre-ffi`, 35 exported functions)
- [x] Inverted index: two-level HashMap (zero-alloc lookups)
- [x] Binary search in FilterBySpan and FilterByComponent
- [x] ScanAll: `RunIndex::full` avoids materializing all positions
- [x] Builder deduplication: shared `IndexSink`
- [x] `within s`/`within doc` alias expansion (was silently a no-op)
- [x] Indexed alignment projection (HashMap edge lookup + binary search)
- [x] `execute_count` fast path (bitmap `.len()` for ScanLiteral: 22ns)
- [x] Feats layer indexed
- [x] Feats decomposition (`feats.Number`, `feats.Gender`, etc.) with build-time toggle
- [x] Dot-notation in parser for decomposed layer names
- [x] Component-scoped FFI query (`montre_query_in_component`)
- [x] Bulk text extraction (`montre_hitlist_texts`)
- [x] Bulk context-token extraction (`montre_context_tokens`)
- [x] Projection diagnostics (unmapped/no-alignment counters)
- [x] Shared projection helpers (`build_edge_map`, `find_doc_and_sent`, `resolve_target_span`)
- [x] Julia bindings ([Montre.jl](https://github.com/myersm0/Montre.jl))
- [x] 140+ tests (was ~50), including executor integration, alignment projection, alternation edge cases
- [x] CI and release workflows
- [x] Shared test corpus (`testdata/parallel/`)

### Phase 4: Statistics & bindings

- [ ] `count` command (CLI)
- [ ] `group` command (frequency by attribute)
- [ ] Collocation extraction
- [ ] Python bindings (PyO3)
- [ ] TUI

## Benchmarks

Current numbers on Apple M-series, 1.5M token corpus (Maupassant French/English):

| Query | Matches | Time |
|-------|---------|------|
| `[pos="NOUN"]` | 244,184 | 0.6ms |
| `[pos="ADJ"] [pos="NOUN"]` | 30,672 | 12ms |
| `[pos="ADJ"]? [pos="NOUN"]` | 272,019 | 71ms |
| `([pos="ADJ"] \| [pos="ADV"])+ [pos="NOUN"]` | 33,444 | 27ms |
| `([pos="ADJ"] \| [pos="DET"])+ [pos="NOUN"]` | 198,735 | 71ms |
| `[pos="ADJ"]{2,4}` | 1,628 | 0.5ms |
| `[lemma="maison"]` | ~800 | <1ms |
| `[] [lemma="chat"]` | 120 | 132ms |

`execute_count` fast path: `[pos="NOUN"]` count in 22ns (bitmap `.len()`).

Alignment projection adds negligible overhead with indexed edge lookup.

## Error handling

### CoNLL-U parsing

Default: warn and continue, skipping malformed sentences.

```
2026-01-27T02:31:45  WARN  Skipping malformed sentence at 25francs.conllu:1833 (expected 10 fields, found 1)
2026-01-27T02:31:50  INFO  Parsed 307 documents, 15231 sentences (47 skipped), 298054 tokens
```

Strict mode (`--strict`): fail on first error.

```bash
montre build -i data/ -o idx/ --strict
```

### Query errors

- Parse errors: report position and expected tokens
- Plan errors: unsupported constructs flagged at planning time
- Execution errors: layer not found, index corruption

## Adding new input formats

Implement the `CorpusReader` trait:

```rust
pub trait CorpusReader {
    fn read_sentences(&mut self) -> Result<Vec<ParsedSentence>>;
}
```

Planned formats:
- Stanza JSON
- TEI XML (basic)
- VRT (CWB format)

JSON and XML are source formats only. Montre's internal representation remains token/sentence/annotation-centric. Query semantics never depend on source format structure.

## Testing

```bash
cargo test --workspace        # all crates
cargo test -p montre-query    # single crate
cargo test -p montre-query --test executor_integration   # integration tests
```

The test suite has 140+ tests across all crates. The integration test suite (`crates/montre-query/tests/executor_integration.rs`) covers end-to-end query execution including sequences, quantifiers, alternation edge cases, within constraints, multi-document queries, alignment projection, feats decomposition, and the Results API.

A shared test corpus at `testdata/parallel/` provides a small multi-component French/English corpus with sentence alignments, suitable for reuse in Julia and Python binding test suites. See `testdata/parallel/POSITIONS.md` for the position reference.

## License

Apache-2.0
