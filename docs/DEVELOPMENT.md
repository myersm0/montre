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

### Concurrency without shared mutable state

`Corpus` is immutable after construction, `Send + Sync`, and safe to share across threads. The query engine exploits this by partitioning work across documents — each document is an independent unit of computation with no cross-document dependencies during sequence execution. The build pipeline uses a similar strategy: each input file produces an independent `IndexSink` via parallel parsing, and the results merge sequentially after construction. Multi-component builds process components sequentially (each component fully built and dropped before the next starts) to bound memory, while files within each component are parsed in parallel.

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
| `montre-ffi` | C FFI for Julia/Python/R bindings | `montre-core`, `montre-index`, `montre-query`, `montre-build` |
| `montre-py` | Python bindings (future) | all of the above |

## Data model

### Core entities

**Token**: A position in the corpus with annotations across multiple layers.

**Layer**: A named annotation dimension (word, lemma, upos, xpos, feats, head, deprel, deps). `pos` is a parse-time alias for `upos`. The `head` layer stores sentence-local dependency head indices as integers and is forward-only (not in the inverted index). The `deps` layer stores raw enhanced dependency strings from CoNLL-U column 9 and is also forward-only — its high cardinality makes inverted indexing impractical. Tokens where the deps column is `_` have no entry (returns `None`). With `decompose_feats` enabled, morphological features are additionally indexed as `feats.Number`, `feats.Gender`, etc.

**Span**: A contiguous range of token positions `[start, end)`.

**Span Layer**: A named collection of non-overlapping spans (sentence, document, paragraph, stanza, scene, etc.).

**Multiword Token (MWT)**: A CoNLL-U range-ID row (e.g., `3-4`) representing a surface form that spans multiple syntactic words. MWTs are not assigned positions in the token stream — the constituent words are the indexed tokens. MWTs are stored in a side table (`mwt.bin`) and consulted during surface text reconstruction.

```rust
struct MWTEntry {
    start: u64,           // global token position (inclusive)
    end: u64,             // global token position (exclusive)
    form: String,         // surface form (e.g., "au", "dell'")
    no_space_after: bool, // SpaceAfter=No from the MWT row's misc column
}
```

**SpaceAfter**: A per-token flag indicating that no space separates this token from the next in the original text. Extracted from the CoNLL-U misc column (`SpaceAfter=No`). Stored as a roaring bitmap of positions in `spacing.bin`. For positions covered by an MWT, the MWT's own `no_space_after` flag is authoritative.

**Empty Node**: A CoNLL-U decimal-ID row (e.g., `6.1`) representing a node in the enhanced dependency graph with no surface realization. Empty nodes are not assigned positions and do not participate in querying. Stored in `empty_nodes.json` with all annotation fields preserved.

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
├── inverted.bin         # term → positions (bincode)
├── forward.bin          # position → annotations (flat mmap format)
├── spans.bin            # sentence, document spans (flat mmap format)
├── sentence_ids.bin     # CoNLL-U sent_id values (flat mmap format)
├── mwt.bin              # multiword token side table (flat mmap format, optional)
├── spacing.bin          # SpaceAfter=No flags (roaring bitmap, optional)
├── empty_nodes.json     # empty node records (optional)
└── lexicon.bin          # term dictionary (bincode)
```

### Multi-component corpus with alignments

```
isosceles/
├── corpus.json
├── components/
│   ├── maupassant-fr/
│   │   ├── inverted.bin
│   │   ├── forward.bin
│   │   ├── spans.bin
│   │   ├── sentence_ids.bin
│   │   ├── mwt.bin
│   │   ├── spacing.bin
│   │   └── empty_nodes.json
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

### Phase 2a: Query language MVP ✓

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
[pos="NOUN"] within doc:"la-parure"                 # named document filter
[pos="NOUN"] within doc:"la-parure","boule-de-suif" # plural document filter
[pos="NOUN"] within component:fr,en                 # plural component filter
```

### Phase 2b: Labels, global constraints, named query results

```cql
# Labels mark positions (implemented)
a:[pos="ADJ"] [pos="NOUN"]
a:[pos="ADJ"]+ [pos="NOUN"]        # captures full quantified span
a:[pos="ADJ"] b:[pos="NOUN"]       # multiple labels

# Global constraints express relationships (implemented)
a:[word=".*"] []{0,5} b:[] :: a.word = b.word     # repetition
a:[pos="NOUN"] []* b:[pos="NOUN"] :: a.lemma = b.lemma  # same lemma
a:[pos="ADJ"] b:[pos="NOUN"] :: a.pos != b.pos    # inequality

# Distance constraints (implemented, directional)
a:[lemma="house"] []{0,20} b:[lemma="home"] :: distance(a,b) >= 5

# Multiple constraints (implemented, flat conjunction with &)
a:[pos="ADJ"] b:[pos="NOUN"] :: a.lemma != b.lemma & distance(a,b) >= 3

# Named Query Results (deferred to REPL/bindings layer)
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
- [x] `within doc:"name"` / `within doc:"a","b"` (named document filter, plural)
- [x] `within component:a,b` (plural component filter)
- [x] N-token sequences (arbitrary length)
- [x] Run-based quantifier execution (see below)

**Run-based quantifier model**

Quantifiers are implemented as *run-based span generators*, not per-position expansion:

1. **Run detection**: Convert positions to maximal contiguous runs in O(k) where k = matching tokens
2. **Span generation**: First step generates spans from runs; subsequent steps probe runs at boundary positions
3. **Boundary tracking**: Active set stored as `HashMap<end, Vec<start>>` (unlabeled fast path) or `HashMap<end, Vec<ActiveMatch>>` (labeled path with capture tracking) to avoid O(N²) blowup
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

- [x] Label syntax (`a:[pos="ADJ"]`)
- [x] Capture tracking in executor (Option C: full quantified span)
- [x] Reserved-word check for label names
- [x] Duplicate label name rejection (post-parse AST walk)
- [x] Global constraints (`:: a.lemma = b.lemma`)
- [x] `distance(a, b)` function (directional: `b.start - a.end`)
- [x] Equality and inequality (`a.lemma = b.lemma`, `a.pos != b.pos`)
- [x] Multiple constraints with `&` conjunction
- [x] `GlobalConstraintFilter` plan node (distinct executor stage)
- [x] Label validation from AST at plan time (`QueryError::UnknownLabel`)
- [x] Per-hit attribute resolution (precomputed `(label, attr)` pairs, resolved once)
- [ ] Named Query Results (deferred to REPL/bindings layer)
- [ ] Set operations (subset, difference, intersection) (deferred to REPL/bindings layer)
- [ ] `expand` to sentence/document (deferred to REPL/bindings layer)

**Implementation plan**

Phase 2b introduces *labeled captures* and *global constraints*. This is a significant extension because constraints operate over the full match, not just local token properties.

**Step 1: Labels in AST and parser** ✓

The existing `Query::Capture { name, inner }` AST variant is used. Parser recognizes `a:[...]` syntax in `parse_labeled_atom`. Labels can appear on any query element, including groups and quantified expressions. Reserved words (`doc`, `document`, `s`, `sent`, `sentence`, `p`, `para`, `paragraph`, `component`) cannot be used as label names.

**Step 2: Captures in Hit** ✓

`Hit.captures: Vec<(String, Span)>` is populated by the executor during sequence execution.

**Step 3: Planner changes** ✓

`SequenceStep` carries `label: Option<String>`. The `extract_label` helper peels `Capture` wrappers from AST nodes and places the label on the corresponding step. Labels inside quantified expressions (`a:[pos="ADJ"]+`) are correctly extracted from `Repetition { inner: Capture { ... } }`.

**Step 4: Executor capture tracking** ✓

Capture semantics: **Option C** — labels on quantified expressions capture the full quantified span. `a:[pos="ADJ"]+` produces one capture `("a", span)` covering the entire adjective run. Attribute access on multi-token captures (for future global constraints) refers to the first token.

Two execution paths to avoid performance regression:
- `run_sequence_steps_unlabeled`: original `HashMap<u64, Vec<u64>>` fast path, used when no labels are present
- `run_sequence_steps_labeled`: `HashMap<u64, Vec<ActiveMatch>>` path with capture tracking, used only when `has_labels(steps)` is true

The bifurcation avoids ~60% regression on ScanAll-first queries caused by `ActiveMatch`/`Vec` clone overhead. Shared `build_run_indices` helper eliminates code duplication for run index construction.

**Step 5: Global constraints** ✓

Global constraints appear after `::` and express relationships between labeled positions:

```cql
a:[pos="NOUN"] []* b:[pos="NOUN"] :: a.lemma = b.lemma
```

AST types:
```rust
Query::Constrained {
    inner: Box<Query>,
    constraints: Vec<GlobalConstraint>,
}

enum GlobalConstraint {
    Eq { left: LabelAttr, right: LabelAttr },
    Ne { left: LabelAttr, right: LabelAttr },
    Distance { left: String, right: String, op: CmpOp, value: u32 },
}

struct LabelAttr {
    label: String,
    attr: String,  // "lemma", "word", "pos", "feats.Number", etc.
}
```

Parser: `::` has lowest precedence, parsed after `maybe_wrap_within` (structural filters and projection). Constraint list is a flat conjunction separated by `&`. Duplicate label names in a query are rejected by a post-parse AST walk (`check_duplicate_labels`). `parse_label_attr` uses `parse_bare_identifier` (no dots) for the label part and `parse_identifier` (with dots) for the attr part, so `a.feats.Number` correctly parses as label `a`, attr `feats.Number`.

Planner: `GlobalConstraintFilter` plan node wraps the inner plan. Label validation operates on the AST (not the plan) via `collect_declared_labels` and `referenced_labels`, producing `QueryError::UnknownLabel` for typos.

**Step 6: Constraint evaluation** ✓

Implemented as a distinct executor stage (`GlobalConstraintFilter` branch in `execute_node`), not logic bolted onto `SequenceScan`. The plan tree shape:

```
GlobalConstraintFilter
  └── FilterBySpan / FilterByComponent / FilterByDocument
        └── SequenceScan (with capture tracking)
```

Evaluation is capture-centric with resolve-once-per-hit semantics:
1. `collect_attr_keys` precomputes all unique `(label, attr)` pairs referenced by the constraint set (once per query).
2. `resolve_attrs` resolves all pairs for a given hit into a `Vec<Option<&str>>` (once per hit).
3. Each constraint indexes into the resolved array rather than doing its own forward lookup.
4. Hits are retained only if all constraints pass (flat conjunction).

If a referenced layer does not exist in the corpus, the forward lookup returns `None` and the constraint evaluates to `false`. This is consistent with how `ScanLiteral` for a nonexistent layer returns no results.

The `count_node` fast path falls through to `execute_node` + `.len()` for `GlobalConstraintFilter` — constrained queries cannot avoid hit materialization.

**Step 7: Distance function** ✓

`distance(a, b)` is **directional**. It measures the token gap from `a` to `b`:
- Returns `b.start.saturating_sub(a.end)` — the number of tokens between the end of `a` and the start of `b`.
- If `a` and `b` are adjacent, distance is 0.
- If `b` begins before `a` ends, result is 0 (saturating subtraction).
- `distance(a, b)` is **not** the same as `distance(b, a)`.

This is the natural interpretation for left-to-right text order in sequence patterns.

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

**Testing priorities** (all implemented)

1. `a:[pos="ADJ"] [pos="NOUN"]` — basic label, capture in hit ✓
2. `a:[pos="ADJ"]+ [pos="NOUN"]` — label on quantified expression ✓
3. `a:[pos="ADJ"] b:[pos="NOUN"] :: a.pos = b.pos` — equality constraint (filters all: ADJ ≠ NOUN) ✓
4. `a:[pos="ADJ"] b:[pos="NOUN"] :: a.pos != b.pos` — inequality constraint ✓
5. `a:[pos="ADJ"] []{0,5} b:[pos="NOUN"] :: distance(a,b) >= 1` — distance constraint ✓
6. `a:[pos="ADJ"]+ b:[pos="NOUN"] :: a.pos != b.pos` — quantified label with constraint ✓
7. `a:[pos="ADJ"] [] b:[pos="NOUN"] :: distance(a,b) >= 1` / `:: distance(b,a) >= 1` — distance directionality ✓
8. `a:[] :: x.lemma = b.lemma` — unknown label produces planner error ✓
9. `a:[pos="ADJ"] [] a:[pos="NOUN"]` — duplicate label rejected at parse time ✓

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

- [x] C FFI crate (`montre-ffi`, 55 exported functions across 8 modules)
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
- [x] 150+ tests (was ~50), including executor integration, alignment projection, alternation edge cases, global constraint evaluation
- [x] CI and release workflows
- [x] Shared test corpus (`testdata/parallel/`)

### Phase 3b: Computational parallelism (rayon) ✓

- [x] Index merge operations (`merge_from` on InMemoryInverted, InMemorySpans, InMemoryLexicon)
- [x] Parallel build: per-file IndexSink construction via `build_from_directory_streaming`
- [x] `CorpusBuilder::from_directory` parallel constructor (streaming)
- [x] Multi-component build (components built sequentially, files within each in parallel)
- [x] Document-parallel sequence execution with range-restricted position lookups
- [x] Adaptive dispatch heuristic (`benefits_from_parallel`)
- [x] Parallel corpus deserialization (`rayon::join` for inverted index and lexicon; forward and spans are memory-mapped)
- [x] Criterion benchmark suite (`montre-bench` crate)

**Build pipeline**

The build pipeline uses a two-level strategy. At the outer level, `MultiCorpusBuilder` builds components sequentially — each component is fully built, merged, and dropped before the next starts. This ensures that only one component's per-file sinks are in memory at a time. At the inner level, `build_from_directory_streaming` processes each `.conllu` file independently:

1. Collect and sort file paths (deterministic ordering).
2. `par_iter` over files: each thread opens, parses, and builds a complete per-file `IndexSink` (inverted index, forward index, spans, lexicon).
3. Collect results in input order (rayon's `collect` preserves order).
4. Sequential merge: iterate over per-file sinks. For each sink, extract forward data via `take_forward()` and append to the `StreamingForwardWriter` (written to per-layer temp files), then merge the remaining indexes (inverted, spans, lexicon) into the combined sink via `merge_from`.

The combined sink's `forward` field is `None` (created via `IndexSink::new_without_forward()`), so no forward data accumulates in memory during the merge. At write time, the `StreamingForwardWriter` reads each temp file one layer at a time, builds the MFWD flat format, and writes `forward.bin`.

The merge step applies a position offset so that each file's token positions land at the correct global offset. This is inherently sequential — the offset depends on the accumulated token count — but it's fast relative to parsing. The index structures support efficient merging: roaring bitmaps shift and OR, spans shift boundaries, and the lexicon takes the union of term sets.

Document ordering is deterministic: file paths are sorted before dispatch, and `par_iter().collect()` preserves input order regardless of thread scheduling. This guarantees identical corpus output on every build.

**Document-parallel query execution**

Sequence execution (the `SequenceScan` plan node) partitions work by document spans. Each document is processed independently with range-restricted position lookups:

1. Retrieve document spans from the span index.
2. `par_iter` over document spans: for each document `[start, end)`, call `run_sequence_steps_in_range` with the document bounds.
3. Position lookups (`get_matching_positions_in_range`) restrict bitmap iteration to the document's range using `skip_while`/`take_while` on roaring iterators. `ScanAll` generates only the document's position range.
4. Concatenate per-document results.

This is correct because sequences cannot meaningfully cross document boundaries in linguistic corpora. The range restriction also provides a secondary benefit beyond parallelism: for `ScanAll` in first position, the working set per document is O(document_size) instead of O(corpus_size), which reduces the quadratic cost of span generation.

Not all sequences benefit from parallel dispatch. The `benefits_from_parallel` heuristic gates parallelism on the presence of quantifiers (`min != 1` or `max != Some(1)`) or `ScanAll`/`Difference`-over-`ScanAll` steps. Fixed-length sequences like `[pos="ADJ"] [pos="NOUN"]` (two literal steps, both `min=1, max=Some(1)`) run single-threaded because the per-document dispatch overhead exceeds the parallelism benefit for these cheap queries.

The `count_sequence` path uses the same document-parallel strategy, summing per-document counts instead of collecting hit vectors.

### Phase 3c: Memory-mapped indexes ✓

- [x] `#[repr(C)]` on `Span` (guarantees 16-byte `(u64, u64)` layout for zero-copy mmap cast)
- [x] Flat binary span format (`MSPN` magic, version, BOM, layer directory, string pool, aligned span arrays)
- [x] `MappedSpans`: memory-mapped span index via `memmap2`, implements `SpanIndex`
- [x] `SpanStore` enum (`InMemory | Mapped`) implementing `SpanIndex`
- [x] `get_str` and `get_int` methods on `ForwardIndex` trait (zero-copy string access, typed numeric access)
- [x] Flat binary forward format (`MFWD` magic, per-layer encoding: `DictEncoded` with variable-width term IDs, `DenseNumeric` for integer layers)
- [x] `MappedForward`: memory-mapped forward index with bitmap-sparse dictionary-coded layers, implements `ForwardIndex`
- [x] Reader-side dense fast path: layers at 100% presence bypass roaring bitmap rank entirely
- [x] `ForwardStore` enum (`InMemory | Mapped`) implementing `ForwardIndex`
- [x] `Corpus` uses `SpanStore` and `ForwardStore`; forward and spans mmapped on open
- [x] Builder writes flat formats for both spans and forward
- [x] CLI and FFI migrated from `forward.get()` to `get_str`/`get_int`
- [x] Index version bumped to 3 (bumped again to 4 in Phase 4a)

**Design decisions**

ELTeC profiling (10M+ tokens, 25-30 layers with decomposed morphological features) drove the forward index design. Key findings:

- Layer sparsity is a gradient, not a binary split. `feats.Number` is present on 54% of tokens in French, 37% in English. `feats.Gender` is 35% / 5.6%. No natural cutoff between "dense" and "sparse."
- Feature vocabularies are tiny: Number is {Sing, Plur}, Gender is {Masc, Fem, Neut}. Core layers vary: POS ~17 values, word ~80K, lemma ~60K.
- A naive dense layout (8 bytes × 10M tokens × 25 layers) would be 2.4GB for French alone.

The solution: uniform on-disk format (roaring presence bitmap + packed dictionary-coded term IDs per layer), with variable-width encoding (u8/u16/u32) chosen per layer based on vocabulary size. The `head` layer, which stores integer dependency head indices, uses a separate `DenseNumeric` encoding — a flat u32 array with no bitmap or term table.

At open time, the reader detects fully-present layers (bitmap cardinality equals token count) and marks them for direct indexing, bypassing bitmap rank entirely. This covers word, lemma, upos, deprel — the layers that dominate collocation, KWIC, and bulk extraction. Sparse feats layers go through contains+rank at ~50ns per lookup, which is acceptable given their lower access frequency.

Result: ~200MB forward index for 10M tokens across 25 layers, compared to 2.4GB dense. Corpus open time reduced by 93-96%.

**Forward index on-disk format**

Each layer is stored as one of two encoding kinds:

*DictEncoded*: a roaring bitmap marking present positions, a sorted string term table, and a packed array of term IDs (u8, u16, or u32 depending on vocabulary size). Lookup: check bitmap → rank query → read term ID → index into term table.

*DenseNumeric*: a flat u32 array indexed directly by position. Used for integer layers like `head`.

The file has a header (magic, version, BOM, token count, layer count), a layer directory with per-layer encoding tag and section offsets, a string pool for layer names, and per-layer data sections. All sections are 8-byte aligned. Native endianness with a BOM field for detection.

### Phase 3d: Streaming forward builder ✓

- [x] `IndexSink::forward` changed to `Option<InMemoryForward>` (absent during merge phase)
- [x] `IndexSink::new_without_forward()` constructor for merge-target sinks
- [x] `IndexSink::take_forward()` extracts forward data for streaming
- [x] Unified `merge_from` that skips forward when `None`
- [x] Unified `write` that skips `forward.bin` when forward is `None`
- [x] `StreamingForwardWriter`: per-layer temp files, tagged binary format, `finalize` reads one layer at a time
- [x] `Drop` on `StreamingForwardWriter` for automatic temp dir cleanup
- [x] `build_from_directory_streaming`: parallel file parsing, streaming forward extraction during merge
- [x] `MultiCorpusBuilder::build` uses sequential component builds with streaming forward
- [x] `CorpusBuilder::from_directory` uses streaming path
- [x] Non-streaming `build_from_directory` removed (no remaining callers)
- [x] `write_mfwd` extracted from `write_flat_forward` for reuse by streaming writer
- [x] `LayerBuild`, `build_dict_encoded_layer`, `build_dense_numeric_layer` made pub in `forward_flat`

**Memory reduction**

The forward index is the dominant memory consumer at build time: 10M tokens × 25 layers × 32 bytes per `Value` = ~8GB per component. Previously, both per-file sinks and the combined sink accumulated forward data in memory. With two components built in parallel, three copies of the forward index could coexist, reaching 25-35GB peak RSS.

The streaming forward writer eliminates forward accumulation in the combined sink. During the per-file merge loop, each sink's `InMemoryForward` is extracted and appended to per-layer temp files on disk. The combined sink holds only inverted, spans, and lexicon data. At write time, temp files are read back one layer at a time (peak: ~320MB for a 10M-token dense layer), converted to MFWD `LayerBuild` structures, and serialized.

Combined with sequential component builds (each component fully built and dropped before the next starts), peak RSS dropped from 35GB to 8.3GB for a two-component ELTeC corpus (~20M tokens, 25 layers). The remaining ~8GB is dominated by the per-file sinks coexisting after `par_iter().collect()` and before the sequential merge consumes them.

### Phase 3e: FFI overhaul ✓

- [x] Module restructuring: single `lib.rs` (836 lines) split into 8 modules (`error`, `strings`, `corpus`, `tokens`, `query`, `spans`, `alignment`, `build`)
- [x] Span index access via FFI (`montre_corpus_span_layer_count`, `span_layer_name`, `span_count`, `span_at`, `span_containing`)
- [x] Component metadata: document range, component-for-document lookup, per-component token count
- [x] Alignment metadata: source/target layer, directed flag
- [x] Inverted index introspection (`montre_corpus_inverted_values`)
- [x] Bulk forward range access (`montre_corpus_token_annotations`)
- [x] Component-scoped count (`montre_query_count_in_component`)
- [x] Bulk hit field extraction as flat arrays (`montre_hitlist_starts`, `_ends`, `_document_indices`, `_sentence_indices`)
- [x] Build from FFI: single-component (`montre_build_directory`) and multi-component (`montre_build_manifest`)
- [x] Raw alignment edge access (`montre_corpus_alignment_edges`: flat u32 quad array)
- [x] `montre-build` added as FFI crate dependency
- [x] Integration test suite (`tests/ffi_integration.rs`): 3 tests covering all 57 functions
- [x] Zero-length allocation guards in bulk extraction functions

Expands the FFI surface from 35 to 57 exported functions. All existing function signatures and semantics are unchanged. The module structure maps to the API's logical groupings: corpus lifecycle, token access, query execution, span access, alignment, and build.

### Phase 3f: CLI improvements ✓

- [x] `montre count` subcommand (bare count, `--by-document`, `--by-component`)
- [x] `montre vocab` subcommand (frequency-sorted vocabulary per layer, `--top`, `--all`, `--component`)
- [x] `montre layers` subcommand (list annotation layers)
- [x] `montre docs` updated to `component\tdocument` TSV format
- [x] Consistent component column in all output (single-component corpora use corpus name)
- [x] Document collision warning on stderr when `--document` matches across components
- [x] `Difference` count fast path in `count_node` (`ScanAll - ScanLiteral`: 89ms → 23ns)
- [x] CLI integration test suite (18 tests via `assert_cmd`)
- [x] Criterion benchmarks for `vocab` and `count_by_document`

**`count --by-document` implementation**

The naive approach (one query per document) was noticeably slow on the 307-document Maupassant corpus. The implemented approach runs a single query, calls `populate_context`, and groups hits by `document_index` in a count array. This completes in ~7ms for `[pos="NOUN"]` on the full Maupassant corpus.

**`Difference` count fast path**

`count_node` previously had no dedicated path for `PlanNode::Difference`, causing `[pos!="PUNCT"]` count to fall through to full hit materialization (89ms). The fix handles the common `ScanAll - ScanLiteral` case with `corpus.token_count() - bitmap.len()`, reducing to 23ns.

**Single-component CLI workaround**

Single-component builds leave `CorpusMeta.components` empty, so `within component:"name"` in CQL returns zero results. The CLI avoids emitting component filters for single-component corpora. A proper engine fix (always populate `ComponentMeta`) is tracked as future work.

### Phase 4a: UD correctness — items 1–3 (v0.6.0) ✓

- [x] **Head layer**: `head` column stored as `DenseNumeric` layer in forward index. Values are raw sentence-local integers (0 = root). Not converted to global positions at build time.
- [x] **Forward-only layer mechanism**: builder-level `HashSet<String>` of layer names excluded from inverted index and lexicon. `head` is forward-only by default. `add_token_int_annotation` method for integer values.
- [x] **UPOS/XPOS split**: CoNLL-U column 4 → `upos` layer, column 5 → `xpos` layer. Both indexed in inverted and forward indexes. `pos` is a parse-time alias for `upos` (resolved in `parse_constraint` and `parse_label_attr`). CLI `vocab` command also resolves the alias.
- [x] **Sentence ID preservation**: `# sent_id = ...` extracted from CoNLL-U comments. Stored in `sentence_ids.bin` (flat mmap format: header + u32 offset table + string pool). Fallback `{document_name}:{sentence_index_within_document}` when absent. Accessible via `corpus.sentence_id(index)` and FFI (`montre_corpus_sentence_id`, `montre_corpus_sentence_ids`).
- [x] Index version bumped to 4. Version mismatch error directs users to rebuild.
- [x] Default layer set expanded: word, lemma, upos, xpos, feats, head, deprel (was: word, lemma, pos, xpos, feats, deprel).
- [x] 170+ tests (was 150+), including head round-trip, UPOS/XPOS layer verification, sentence ID pipeline tests with fallback and mixed presence.

**Sentence ID storage format (`MSID`)**

Flat binary, memory-mapped at open time with zero deserialization:

```
Header (16 bytes):
    magic: "MSID"
    version: u32 = 1
    bom: u32 = 0x01020304
    count: u32

Offset table: (count + 1) × u32 byte offsets into string pool
    padded to 8-byte alignment

String pool: concatenated UTF-8 sentence ID strings
```

Lookup is O(1): read two adjacent u32 offsets, slice the pool.

### Phase 4b: UD compliance — items 4–6 (v0.6.0) ✓

- [x] **MWT preservation**: CoNLL-U range-ID rows (e.g., `3-4`) parsed into `MWTEntry` with global positions. Stored in `mwt.bin` (flat mmap format: header + fixed-size entries sorted by start position + string pool). Binary search for `covering(position)` and `in_range(start, end)`. `no_space_after` flag stored per entry.
- [x] **SpaceAfter preservation**: `SpaceAfter=No` extracted from CoNLL-U misc column for both ordinary tokens and MWT rows. Token-level flags stored as a roaring bitmap in `spacing.bin`. MWT-level flags stored in the MWT entry itself (Option A: spacing belongs to the emitted surface unit).
- [x] **Empty node preservation**: CoNLL-U decimal-ID rows (e.g., `6.1`) parsed into `EmptyNode` with all annotation fields. Stored in `empty_nodes.json` (sorted by sentence index). Not assigned positions, not indexed, not queryable.
- [x] **Enhanced deps preservation**: CoNLL-U column 9 stored as a `DictEncoded` forward-only layer (`deps`). Underscore values filtered at build time (return `None`, not `"_"`). Forward-only mechanism reused from `head`.
- [x] **Surface text reconstruction**: `Corpus::surface_text(start, end)` consults MWT side table and spacing bitmap. MWT forms replace constituent words; `no_space_after` suppresses inter-token spaces. CLI KWIC display and FFI `span_text`/`hitlist_texts` updated to use surface text for the `"word"` layer.
- [x] Index version bumped to 5.
- [x] Default layer set expanded: word, lemma, upos, xpos, feats, head, deprel, deps (was: word, lemma, upos, xpos, feats, head, deprel).
- [x] 7 new FFI functions: `montre_corpus_mwt_form`, `montre_corpus_mwt_at`, `montre_corpus_surface_text`, `montre_corpus_has_no_space_after`, `montre_corpus_empty_node_count`, `montre_corpus_empty_node_count_in_sentence`, `montre_corpus_empty_node_field` (64 total, was 57).
- [x] 180+ tests (was 170+), including MWT surface text reconstruction, MWT+SpaceAfter combined, deps forward-only verification, empty node preservation and non-indexing, end-to-end build-query-display pipeline.

**MWT storage format (`MMWT`)**

Flat binary, memory-mapped at open time:

```
Header (16 bytes):
    magic: "MMWT"
    version: u32 = 1
    bom: u32 = 0x01020304
    count: u32

Entries (count × 24 bytes each, sorted by start):
    start: u64          global token position (inclusive)
    end: u64            global token position (exclusive)
    form_offset: u32    byte offset into string pool
    form_len: u16       byte length of form
    flags: u8           bit 0 = no_space_after
    padding: u8

String pool: concatenated UTF-8 form strings (8-byte aligned start)
```

Lookup is O(log n) via binary search on `start`. `covering(position)` finds the entry whose range contains the position. `in_range(start, end)` finds all entries overlapping the query span.

**Spacing storage format (`MSPC`)**

Header (8 bytes: magic `"MSPC"` + version u32) followed by a serialized roaring bitmap. The bitmap contains positions where `SpaceAfter=No` is set. Typical size is a few KB even for large corpora.

**Empty node storage format**

JSON array of objects, sorted by `(sentence_index, node_id)`. Fields with `_` values are omitted from the JSON. Chosen over a binary format because empty nodes are rare, structurally complex, and not on any hot access path.

**Design decisions**

*Spacing belongs to the emitted surface unit.* MWT spacing (`no_space_after` on `MWTEntry`) and token spacing (roaring bitmap) are separate sources. Surface text reconstruction uses the MWT flag when rendering an MWT, the bitmap when rendering an ordinary token. The bitmap is not consulted for positions covered by an MWT.

*Deps underscore filtering.* Tokens where CoNLL-U column 9 is `_` do not get a `deps` entry in the forward index. `get_str(position, "deps")` returns `None`, consistent with how other optional layers behave.

*Empty nodes as JSON.* Binary format considered and deferred — empty nodes are infrequent (many treebanks have zero), structurally complex (many string fields), and have no hot-path access pattern. JSON is debuggable and trivially extensible.

### Phase 4: Statistics & bindings

- [x] `count` command (CLI)
- [ ] `group` command (frequency by attribute)
- [ ] Collocation extraction
- [ ] Python bindings (PyO3)
- [ ] REPL mode (readline loop, corpus held in memory)
- [ ] TUI

## Benchmarks

Current numbers on Apple M4 Max, 1.5M token corpus (Maupassant French/English). Benchmarks driven by Criterion via the `montre-bench` crate.

### Query execution

| Query | Matches | Time |
|-------|---------|------|
| `[pos="NOUN"]` | 244,184 | 0.7ms |
| `[pos="ADJ"] [pos="NOUN"]` | 30,672 | 12ms |
| `[pos="ADJ"]? [pos="NOUN"]` | 272,019 | 22ms |
| `([pos="ADJ"] \| [pos="ADV"])+ [pos="NOUN"]` | 33,444 | 20ms |
| `([pos="ADJ"] \| [pos="DET"])+ [pos="NOUN"]` | 198,735 | 29ms |
| `[pos="ADJ"]{2,4}` | 1,628 | 0.5ms |
| `[lemma="maison"]` | ~800 | <1ms |
| `[] [lemma="chat"]` | 120 | 9.5ms |

`execute_count` fast path: `[pos="NOUN"]` count in 22ns (bitmap `.len()`). `[pos!="PUNCT"]` count in 23ns (`ScanAll - ScanLiteral` fast path via `token_count - bitmap.len()`).

Alignment projection adds negligible overhead with indexed edge lookup.

Quantifier queries and `ScanAll`-in-first-position use document-parallel execution (see Phase 3b). The `[] [lemma="chat"]` improvement from 132ms to 9.5ms (14×) comes from both parallelism and range restriction — each document processes O(document_size) positions instead of O(corpus_size).

### CLI operations

| Operation | Time | Notes |
|---|---|---|
| `vocab pos` | 627ns | Bitmap cardinality + sort for ~17 POS tags |
| `vocab lemma` | 1.3ms | ~60K entries |
| `vocab word` | 2.2ms | ~80K entries |
| `count --by-document` (`[pos="NOUN"]`) | 7.0ms | Single query + populate_context + grouping |
| `count --by-document` (`[pos="ADJ"] [pos="NOUN"]`) | 14ms | Same approach |

### Build pipeline

| Benchmark | Time |
|---|---|
| `build_single_component` (300 .conllu files, parallel) | 452ms |
| `build_multi_component` (2 components + alignments, parallel) | 1.32s |

### Corpus loading

| Benchmark | Time | Notes |
|---|---|---|
| `corpus_open` (1.5M token Maupassant) | 20ms | forward + spans memory-mapped; inverted + lexicon bincode-deserialized |
| `corpus_open` (1.5M + 10M token Maupassant + ELTeC-fra) | 116ms | same; remaining time is inverted index deserialization |
| `corpus_open` (pre-mmap, v0.3, 1.5M token Maupassant) | 285ms | all four indexes bincode-deserialized with rayon |

### Peak memory (query-time RSS)

| Corpus | Current (mmap) | Pre-mmap (v0.3) | Reduction |
|---|---|---|---|
| Maupassant (1.5M tokens, 6 layers) | 94MB | 1.75GB | 18.7× |
| Maupassant + ELTeC-fra (11.5M tokens, 25 layers) | 1.2GB | n/a | — |

The mmap-backed forward and span indexes contribute only the pages the OS faults in during the query. The remaining RSS is dominated by the inverted index and lexicon, which are still heap-deserialized.

### Peak memory (build-time RSS)

| Corpus | Current (streaming) | Pre-streaming |
|---|---|---|
| ELTeC fr+en (20M tokens, 25 layers, 2 components) | 8.3GB | 35GB |

The streaming forward writer avoids accumulating forward index data in the combined sink during the merge phase. Sequential component builds ensure only one component's per-file sinks are in memory at a time. The remaining build-time RSS is dominated by the per-file sinks coexisting after parallel parsing (before merge) and the growing inverted index.

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

The test suite has 180+ tests across all crates. The integration test suite (`crates/montre-query/tests/executor_integration.rs`) covers end-to-end query execution including sequences, quantifiers, alternation edge cases, within constraints, multi-document queries, alignment projection, feats decomposition, labeled captures, global constraint evaluation, the Results API, head layer round-trip, UPOS/XPOS layer verification, `pos` alias resolution, sentence ID preservation with fallback generation, MWT surface text reconstruction, SpaceAfter handling, deps forward-only verification, and empty node preservation.

A shared test corpus at `testdata/parallel/` provides a small multi-component French/English corpus with sentence alignments, suitable for reuse in Julia and Python binding test suites. See `testdata/parallel/POSITIONS.md` for the position reference.

## Benchmarking

Criterion benchmarks live in the `montre-bench` crate (`crates/montre-bench/`). Benchmarks are driven by environment variables pointing at local data:

- `MONTRE_BENCH_CORPUS` — path to a pre-built montre corpus (query benchmarks)
- `MONTRE_BENCH_CONLLU` — path to a directory of .conllu files (single-component build + parse benchmarks)
- `MONTRE_BENCH_MANIFEST` — path to a corpus build manifest TOML (multi-component build benchmark)

```bash
cargo bench -p montre-bench --bench query    # query and load benchmarks
cargo bench -p montre-bench --bench build    # build pipeline benchmarks
cargo bench -p montre-bench --bench lookup   # inverted index lookup microbenchmarks
cargo bench -p montre-bench --bench query -- corpus_open   # filter to a single benchmark
```

Results are stored in `target/criterion/` with HTML reports.

## License

Apache-2.0
