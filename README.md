# Montre
[![CI](https://github.com/myersm0/montre/actions/workflows/ci.yml/badge.svg)](https://github.com/myersm0/montre/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/myersm0/montre)](https://github.com/myersm0/montre/releases/latest)

A modern, embeddable query engine for corpus linguistics.

> **montre** *(/mɔ̃tʁ/):* to show; to reveal; to make visible (French)
> *From Latin* **monstrare** "to point out, indicate."

Montre is particularly suited for aligned literary corpora and multi-edition corpora.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/myersm0/montre/main/install.sh | sh
```

Or build from source:

```bash
cargo build --release
```

## Quick start

```bash
# Build a corpus from CoNLL-U files
montre build -i data/maupassant/ -o my-corpus/

# Build a multi-component corpus with alignments
montre build -m corpus.toml -o my-corpus/

# Build with morphological feature decomposition
montre build -i data/ -o my-corpus/ --decompose-feats

# Query
montre query my-corpus/ '[pos="ADJ"] [pos="NOUN"]'

# Count matches
montre query my-corpus/ '[pos="ADJ"]+ [pos="NOUN"]' --count-only

# Corpus info
montre info my-corpus/
```

## What is Montre?

Montre is a **local-first corpus query engine**. No server, no daemon, no service dependencies.

A Montre corpus is a portable artifact: a single directory containing indexed text, annotations, and (optionally) alignments. You can open it from the CLI or from Julia and Python as a library.

## Query language

Montre uses a CQL-based query language:

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
[pos="ADJ"]+                    # one or more
[pos="ADJ"]*                    # zero or more
[pos="ADJ"]?                    # optional
[pos="ADJ"]{2,4}                # 2 to 4

# Alternation
([pos="ADJ"] | [pos="ADV"])+ [pos="NOUN"]

# Structural constraints
[pos="DET"] [pos="NOUN"] within s
[lemma="chat"] within doc

# Morphological features (requires --decompose-feats at build time)
[pos="NOUN" & feats.Number="Plur"]
[feats.Gender="Masc" & feats.Tense="Past"]

# Component filtering (multi-component corpora)
[pos="NOUN"] within component:"maupassant-fr"

# Alignment projection
[lemma="maison"] within component:fr =labse=>
```

## Parallel corpus support

Montre has first-class support for multi-component corpora with alignments:

- Multiple components (languages, editions, translations) in one corpus
- Named alignments at any span level (sentence, paragraph, stanza)
- Multiple competing alignments from different models (LaBSE, vecalign, manual)
- Alignment projection: query one language, project hits to another

Define a multi-component corpus with a TOML manifest:

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

## Performance

On a 1.5M token corpus (Maupassant French/English), Apple M-series:

| Query | Matches | Time |
|---|---|---|
| `[pos="NOUN"]` | 244,184 | 0.6ms |
| `[pos="ADJ"] [pos="NOUN"]` | 30,672 | 12ms |
| `[pos="ADJ"]? [pos="NOUN"]` | 272,019 | 71ms |
| `([pos="ADJ"] \| [pos="ADV"])+ [pos="NOUN"]` | 33,444 | 27ms |
| `([pos="ADJ"] \| [pos="DET"])+ [pos="NOUN"]` | 198,735 | 71ms |

Quantifiers use a run-based execution model that scales with matching tokens, not corpus size. The `--count-only` fast path avoids Hit allocation entirely for simple queries (22ns for `[pos="NOUN"]`).

Corpus loading uses memory-mapped indexes for the forward and span stores. On the 1.5M token Maupassant corpus, `Corpus::open` completes in ~20ms with a peak RSS of 94MB (compared to 285ms and 1.75GB before mmap). On a combined 11.5M token corpus (Maupassant + ELTeC-fra, 25 annotation layers), open time is ~116ms.

For a two-component ELTeC corpus (~20M tokens, 25 layers), build-time peak RSS is ~8GB.

## Bindings

Montre ships a C FFI (`libmontre_ffi`) for embedding in other languages.

### Julia

**[Montre.jl](https://github.com/myersm0/Montre.jl)** provides a native Julia interface:

```julia
using Montre

corpus = open_corpus("./my-corpus")
hits = query(corpus, "[pos=\"ADJ\"] [pos=\"NOUN\"]")
for line in concordance(corpus, hits)
    println(line)
end
```

Features include concordancing, alignment projection, component-scoped queries, bulk token extraction, and a Tables.jl interface for interoperability with DataFrames and other tabular data packages.

### Python

Python bindings via PyO3 are stubbed but not yet feature-complete.

```python
import montre

corpus = montre.open("./my-corpus")
for hit in corpus.query('[pos="DET"] [pos="NOUN"]'):
    print(hit.start, hit.end)
```

## Architecture

```
montre-core     Primitives: Span, Token, Position, Value
montre-index    Inverted index, forward index, span index, corpus loading
montre-query    CQL parser, query planner, executor
montre-build    Corpus construction from CoNLL-U, multi-component builder, streaming forward writer
montre-cli      Command-line interface
montre-ffi      C FFI for language bindings (35 exported functions)
montre-py       Python bindings (PyO3, stub)
```

See [API.md](API.md) for the full Rust and C FFI reference.

## Status

**v0.4.0**

Working: token queries, sequences, quantifiers, alternation, regex, negation, conjunction, `within` constraints, multi-component corpora, sentence-level alignment, alignment projection, morphological feature decomposition, C FFI, Julia bindings, memory-mapped forward and span indexes.

New in v0.4: memory-mapped corpus indexes for the forward and span stores (93-96% faster corpus opening, 18× lower query-time memory). Forward index uses bitmap-sparse dictionary-coded layers with variable-width term IDs and a reader-side fast path for fully-present layers. Streaming forward builder reduces build-time peak memory by 4× for large multi-component corpora. Index format version 3 (requires corpus rebuild from v0.3).

New in v0.3: multithreaded build pipeline and document-parallel query execution via rayon. Parallel corpus deserialization.

Next: labeled captures and global constraints (Phase 2b), statistics commands (`count`, `group`, collocation), additional input formats, Python bindings, TUI.

## License

Apache-2.0
