# Montre

[![CI](https://github.com/myersm0/montre/actions/workflows/ci.yml/badge.svg)](https://github.com/myersm0/montre/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/myersm0/montre)](https://github.com/myersm0/montre/releases/latest)

<img src="docs/montre_no-text.svg" align="left" width="180">

A modern, embeddable corpus query engine with first-class support for aligned corpora.

> **montre** *(/mɔ̃tʁ/):* “shows,” “reveals,” “makes visible” — from French _montrer_, “to show.” The Latin root is _monstrare_, “to point out, indicate.”

Montre's internals map cleanly onto the [Universal Dependencies](https://universaldependencies.org/) data model: each CoNLL-U column has a corresponding layer, multiword tokens and empty nodes are preserved with their UD semantics, and the annotation hierarchy (token, sentence, document, component) mirrors the structure of UD treebanks.

No server, external services, or prerequisites. A corpus is a self-contained directory with its own data, indexes, and (optionally) alignments. Build it in one line from your annotation files, or from a TOML manifest describing multiple components.

Designed to be used from the CLI or embedded directly in Julia or Python.

---

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/myersm0/montre/main/install.sh | sh
```


## Quick start
```bash
# Build a corpus from a directory of CoNLL-U files:
montre build -i data/maupassant/ -o my-corpus/

# Query
montre query my-corpus/ '[pos="ADJ"] [pos="NOUN"]'

# Count
montre count my-corpus/ '[pos="ADJ"] [pos="NOUN"]'
montre count my-corpus/ '[pos="NOUN"]' --by-document
montre count my-corpus/ '[pos="NOUN"]' --by-component

# Filter
montre query my-corpus/ '[pos="ADJ"] [pos="NOUN"]' --document la-parure
montre query my-corpus/ '[pos="ADJ"] [pos="NOUN"]' --component maupassant-fr

# Inspect
montre info my-corpus/
montre docs my-corpus/
montre layers my-corpus/
montre vocab my-corpus/ pos
montre vocab my-corpus/ lemma --top 50 --component maupassant-fr
```

## Query language

Montre uses a CQL-based language, extended with labels, constraints, and alignment-aware operations.

### Core patterns
```cql
# Token queries
[pos="NOUN"]
[lemma="maison"]
[word="chat" & pos="NOUN"]
[lemma=/^un.*/]
[pos!="PUNCT"]

# Sequences
[pos="DET"] [pos="ADJ"]* [pos="NOUN"]

# Quantifiers
[pos="ADJ"]+
[pos="ADJ"]*
[pos="ADJ"]?
[pos="ADJ"]{2,4}

# Alternation
([pos="ADJ"] | [pos="ADV"])+ [pos="NOUN"]
```

### Structural constraints
```cql
[pos="DET"] [pos="NOUN"] within s
[lemma="chat"] within doc
```

### UPOS and XPOS

Montre stores the UPOS and XPOS columns from CoNLL-U as separate `upos` and `xpos` layers. The familiar `pos` name is an alias for `upos` and works everywhere:

```cql
[pos="NOUN"]           # equivalent to [upos="NOUN"]
[xpos="NN"]            # Penn Treebank tag
[upos="NOUN" & xpos="NNS"]   # both
```

### Morphological features

Requires using the flag `--decompose-feats` at build time.

```cql
[pos="NOUN" & feats.Number="Plur"]
[feats.Gender="Masc" & feats.Tense="Past"]
```

### Universal Dependencies data model

Montre preserves the full UD annotation structure:

- **Multiword tokens** (range-ID rows like `3-4`): stored in a side table and used for correct surface text reconstruction. Concordance output shows `au` instead of `à le`.
- **SpaceAfter=No**: preserved from the misc column. Surface text joins tokens without spaces where appropriate (e.g., `dort.` not `dort .`).
- **Head column**: stored as a sentence-local integer layer (`head`). Not queryable, but accessible via the API for dependency tree reconstruction.
- **Enhanced deps** (column 9): stored as a forward-only layer (`deps`). Raw strings like `2:obj|4:nsubj` are preserved for round-trip fidelity.
- **Empty nodes** (decimal-ID rows like `6.1`): stored in a JSON sidecar. Not indexed or queryable, but preserved for downstream tools.

### Component and document filtering
```cql
[pos="NOUN"] within component:"maupassant-fr"
[pos="ADJ"] [pos="NOUN"] within doc:"la-parure","boule-de-suif"
```

### Labeled captures and global constraints
```cql
a:[pos="NOUN"] []* b:[pos="NOUN"] :: a.lemma = b.lemma
a:[pos="ADJ"] b:[pos="NOUN"] :: a.lemma != b.lemma
a:[] []{0,20} b:[] :: distance(a,b) >= 5
```

Constraints are evaluated over full matches using labeled spans.

## Parallel corpus support

Montre was designed from the ground up specifically for parallel corpora.

Montre treats a parallel corpus as a single object with multiple ***components*** and explicit alignment relations, rather than as separate corpora joined at query time.

### Key features
- Multiple components (languages, editions, translations)
- Named alignments at any span level (sentence, paragraph, stanza)
- Multiple competing alignment sets (LaBSE, vecalign, manual)
- Alignment projection between components

### Example
```bash
# Query French and project the matches to English via the labse alignment
montre query my-corpus '[lemma="maison"] within component:"maupassant-fr" =labse=>'
```

This enables:
- tracing translations across languages
- detecting omissions or expansions
- comparing editions or variants

**Coming soon**: contrastive alignment queries. Query patterns like "show me sentences where French uses the subjunctive but the English translation doesn't" — expressing constraints that span across aligned components. This is expressible today as composed operations in the Julia bindings, but native CQL syntax will make it concise and efficient.

### Build a multi-component corpus
```toml
[corpus]
name = "isosceles"
decompose_feats = true

[components.maupassant-fr]
path = "data/maupassant/fr/conllu/"
language = "fr"

[components.maupassant-en]
path = "data/maupassant/en/conllu/"
language = "en"

[alignments.labse]
source = "maupassant-fr"
target = "maupassant-en"
edges = "alignments/labse/"
source_layer = "sentence"
target_layer = "sentence"
```

```bash
montre build -m corpus.toml -o my-corpus/
```

## Performance

Montre is competitive with established corpus engines while prioritizing structural flexibility and embeddability.

On a 1.5M token corpus (Maupassant French/English, Apple M4 Max):

| Query | Matches | Time |
|---|---|---|
| `[pos="NOUN"]` | 244,184 | 0.6ms |
| `[pos="ADJ"] [pos="NOUN"]` | 30,672 | 12ms |
| `[pos="ADJ"]? [pos="NOUN"]` | 272,019 | 71ms |
| `([pos="ADJ"] \| [pos="ADV"])+ [pos="NOUN"]` | 33,444 | 27ms |
| `([pos="ADJ"] \| [pos="DET"])+ [pos="NOUN"]` | 198,735 | 71ms |

### Key properties:
- Quantifiers use a run-based execution model (scales with matches, not corpus size)
- `--count-only` avoids hit allocation entirely (nanosecond-scale for simple queries)
- Memory-mapped indexes reduce load time and memory footprint by an order of magnitude

## Bindings
Montre exposes a C FFI for embedding in other languages.

## Session daemon

Montre includes an optional local session daemon (`montre serve <corpus>`) for use cases that go beyond one-shot CLI invocations: long-running interactive sessions, persistent named query results, and coordination between multiple processes viewing the same corpus.

The daemon runs as one process per corpus, spawned automatically on the first client connection, serving clients over a Unix domain socket via a JSON-RPC 2.0 protocol. A Rust client (`DaemonClient`) ships in the same crate and is the shared entry point for all consumers (bindings, terminal tools, integration tests). Idle daemons shut themselves down after a configurable timeout (default 30 minutes).

See [`docs/daemon-protocol.md`](docs/daemon-protocol.md) for more details.

### Terminal interfaces

[montre-tui](https://github.com/myersm0/montre-tui) is a companion project building terminal clients on top of the daemon described above: independent panes for reading, KWIC browsing, CoNLL-U inspection, document picking, vocabulary, named-results browsing, statistics, etc. Each pane is its own process in its own terminal (or in your terminal multiplexer of choice), and the daemon coordinates the shared state (anchored focus across panes, named results, query history) between them. Currently in early development.

### Julia
**[Montre.jl](https://github.com/myersm0/Montre.jl)** — registered Julia package.
```julia
using Montre

corpus = Montre.open("./my-corpus")
hits = query(corpus, "[pos=\"ADJ\"] [pos=\"NOUN\"]")

for line in concordance(corpus, hits)
    println(line)
end
```

### Python (planned)
PyO3 bindings are in early prototyping and not yet usable.

Planned API:
```python
import montre

corpus = montre.open("./my-corpus")
for hit in corpus.query('[pos="DET"] [pos="NOUN"]'):
    print(hit.start, hit.end)
```

## Roadmap
Coming soon:
- Contrastive alignment queries
- Statistics: group, collocation
- Python bindings (feature-complete, pip install)
- REPL (persistent corpus session)
- Terminal UI ([montre-tui](https://github.com/myersm0/montre-tui), separate repo)
- Support for additional input formats (VRT, Stanza JSON, TEI)

## Citing Montre

A paper describing Montre is in preparation. In the meantime, if you use Montre in published research, please cite:
```bibtex
@software{myers-montre,
  author       = {Myers, Michael J.},
  title        = {Montre: A Modern Corpus Query Engine for Aligned Corpora},
  year         = {2026},
  url          = {https://github.com/myersm0/montre},
  version      = {0.6.0}
}
```

## License

Apache-2.0
