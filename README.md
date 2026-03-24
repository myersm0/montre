# Montre
[![CI](https://github.com/myersm0/montre/actions/workflows/ci.yml/badge.svg)](https://github.com/myersm0/montre/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/myersm0/montre)](https://github.com/myersm0/montre/releases/latest)

A modern, embeddable corpus query engine with first-class support for aligned corpora.

> **montre** *(/mɔ̃tʁ/):* “shows,” “reveals,” “makes visible” — from French _montrer_, “to show.” The Latin root is _monstrare_, “to point out, indicate.”

No server, external services, or prerequisites.

A corpus is a self-contained directory with its own data, indexes, and (optionally) alignments. Build it in one line from your annotation files, or from a TOML manifest describing multiple components.

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

# Count only (fast path)
montre query my-corpus/ '[pos="ADJ"]+ [pos="NOUN"]' --count-only

# Filter
montre query my-corpus/ '[pos="ADJ"] [pos="NOUN"]' --document la-parure
montre query my-corpus/ '[pos="ADJ"] [pos="NOUN"]' --component fr

# Inspect
montre info my-corpus/
montre docs my-corpus/
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

### Morphological features

Requires using the flag `--decompose-feats` at build time.

```cql
[pos="NOUN" & feats.Number="Plur"]
[feats.Gender="Masc" & feats.Tense="Past"]
```

### Component and document filtering
```cql
[pos="NOUN"] within component:fr
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
```cql
# Query French, project to English
[lemma="maison"] within component:fr =labse=>
```

This enables:
- tracing translations across languages
- detecting omissions or expansions
- comparing editions or variants

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

### Julia (almost complete)
**[Montre.jl](https://github.com/myersm0/Montre.jl)**
```julia
using Montre

corpus = open_corpus("./my-corpus")
hits = query(corpus, "[pos=\"ADJ\"] [pos=\"NOUN\"]")

for line in concordance(corpus, hits)
    println(line)
end
```

### Python (early)
Bindings via PyO3 are in progress.
```python
import montre

corpus = montre.open("./my-corpus")
for hit in corpus.query('[pos="DET"] [pos="NOUN"]'):
    print(hit.start, hit.end)
```

## Roadmap
Coming soon:
- Statistics: count, group, collocation
- Python bindings (feature-complete, pip install)
- REPL (persistent corpus session)
- TUI for interactive exploration
- Support for additional input formats (VRT, Stanza JSON, TEI)

## Citing Montre

A paper describing Montre is in preparation. In the meantime, if you use Montre in published research, please cite:
```bibtex
@software{myers-montre,
  author       = {Myers, Michael J.},
  title        = {Montre: A Modern Corpus Query Engine for Aligned Corpora},
  year         = {2026},
  url          = {https://github.com/myersm0/montre},
  version      = {0.4.0}
}
```

## License

Apache-2.0
