# Montre development guide

## Design principles

### Corpus as value

A Montre corpus is a self-contained, immutable directory produced by `montre build`. All data required for querying—tokens, annotations, document boundaries, span layers, and alignment relations—lives within this directory.

Montre does not rely on:
- Global registries
- Environment variables
- External configuration files
- Shared indexes or system-wide state

Montre takes the approach: one path → one corpus → one semantic universe.

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
| `montre-py` | Python bindings (future) | all of the above |

## Data model

### Core entities

**Token**: A position in the corpus with annotations across multiple layers.

**Layer**: A named annotation dimension (word, lemma, pos, xpos, feats, deprel, head).

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

**Future: weighted alignment edges** (v0.2+)

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

**Non-exhaustive alignments**

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

### Phase 2a: Query Language MVP

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

### Phase 2b: Labels, Global Constraints, Named Query Results

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

### Phase 3: Parallel Corpus Queries

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

### Phase 1: Basic Queries ✓

- [x] CQL parser (winnow)
- [x] Literal and regex token patterns
- [x] Attribute conjunction (`&`)
- [x] Two-token sequences
- [x] Query planner
- [x] Basic executor
- [x] KWIC display (CLI)

### Phase 1a: Multi-File Support ✓

- [x] Directory traversal (`walkdir`)
- [x] Per-file document boundaries
- [x] Document name tracking
- [x] Lenient CoNLL-U parsing (skip malformed sentences)
- [x] `--strict` mode for fail-fast

### Phase 2a: Query Language MVP (Current)

- [ ] Negation (`!=`)
- [ ] Matchall (`[]`)
- [ ] Quantifiers (`+`, `*`, `?`, `{n,m}`)
- [ ] Alternation (`|`)
- [ ] `within s` / `within doc` constraints
- [ ] N-token sequences (arbitrary length)

### Phase 2b: Labels & Named Query Results

- [ ] Label syntax (`a:[pos="ADJ"]`)
- [ ] Global constraints (`:: a.lemma = b.lemma`)
- [ ] `distance(a, b)` function
- [ ] Named Query Results
- [ ] Set operations (subset, difference, intersection)
- [ ] `expand` to sentence/document

### Phase 2c: Hit Model Enhancement

- [ ] Add `document_index` to `Hit`
- [ ] Add `sentence_index` to `Hit`
- [ ] Compute IDs during execution (not reconstruction)

### Phase 3: Parallel Corpus Support

- [ ] Component model
- [ ] Build manifest (TOML)
- [ ] Alignment ingestion
- [ ] Extensible span layers
- [ ] `within component:X` filter
- [ ] Alignment projection (`=name=>`)
- [ ] Multiple alignments per component pair

### Phase 4: Statistics & Python

- [ ] `count` command
- [ ] `group` command (frequency by attribute)
- [ ] Collocation extraction
- [ ] Python bindings (PyO3)
- [ ] Julia bindings (jlrs)

## Benchmarks

Preliminary numbers on Apple M2 Max, 1.2M token corpus (French prose):

| Query | Matches | Time |
|-------|---------|------|
| `[pos="NOUN"]` | 187,432 | 12ms |
| `[pos="DET"] [pos="NOUN"]` | 89,201 | 28ms |
| `[word="maison"]` | 847 | 0.4ms |
| `[lemma="être"]` | 24,891 | 3ms |

Index size: 42MB (tokens: 1.2M, vocabulary: 89K)

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
cargo test                    # unit tests
cargo test --workspace        # all crates
cargo test -p montre-query    # single crate
```

Integration tests use `assert_cmd` for CLI testing.

## License

Apache-2.0
