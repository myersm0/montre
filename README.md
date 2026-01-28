# Montre
A modern, embeddable query engine for corpus linguistics. Lightweight, easy to install and use, but powerful.

> **montre** *(/mɔ̃tʁ/):* to show; to reveal; to make visible (French)  
> *From Latin* **monstrare** “to point out, indicate.”

Montre is particularly suited for aligned literary corpora and multi-edition corpora.

## Status
**Early development.** Not yet usable for real research work.
The architecture and data model are stabilizing; APIs, formats, and CLI are still in flux.

## What is Montre?
Montre is a **local-first corpus query engine**:

* No server
* No daemon
* No global registry
* No service dependencies

A Montre corpus is a **portable artifact**: a single directory containing indexed text, annotations, and (optionally) alignments. You can open it from the CLI, Python, Julia, or R -- as a library, not as a service.

## Goals

* **Fast queries** on large annotated corpora (100M+ tokens)
* **Embeddable**: use as a library, not a server
* **Native NLP integration**:

  * CoNLL-U
  * Stanza JSON
  * UDPipe
  * spaCy exports
* **Clean, expressive query language** based on CQL
* **First-class parallel corpus support**:

  * multiple languages
  * multiple editions
  * multiple competing alignments
  * alignment-aware querying

## Design principles

### 1. Corpus as artifact

A corpus is a **build product**, not a runtime configuration:

* immutable
* reproducible
* portable
* versionable

This enables:

* stable research artifacts
* reproducible experiments
* reliable citation

### 2. Components of a corpus

A corpus may contain multiple **components**:

* monolingual subcorpora
* reference corpora
* editions
* translations

Each component is independently queryable, but can participate in structured relations (alignments).

### 3. Alignments as data

Parallelism of corpora is flexible.

Alignments are:

* named
* typed
* layered
* replaceable

You can have several alignments (over sentences, paragraphs, etc) potentially from different models (e.g. LaBSE, vecalign) and choose which alignment(s) to use at query time.

### 4. Alignment-native querying

Queries can project across alignments:

```cql
<lemma="bibelot"] =labse_sentence=> component:"maupassant-en"
```

This is projection, not a join:

* hit sets move between components
* cardinality may change
* relations are explicit and named

---

## Capabilities (planned)

### Querying

* token queries
* span queries
* structural queries
* metadata filters
* distributional queries
* alignment projection

### Parallel corpus support

* sentence-level alignments
* paragraph-level alignments
* many-to-many mappings
* multiple competing alignment models
* edition-aware alignment

### Interfaces

* CLI
* embeddable Rust API
* Python bindings
* Julia bindings
* R bindings
* TUI (ratatui-based, separate repo)

---

## Building

```bash
cargo build --release
```

---

## Usage (planned)

```bash
# Build a corpus from CoNLL-U
montre build --input corpus.conllu --output ./my-corpus

# Query
montre query ./my-corpus '[pos="NOUN"] [pos="NOUN"]'

# Info
montre info ./my-corpus
```

---

## Library usage (planned)

### Python

```python
import montre

corpus = montre.open("./my-corpus")
for hit in corpus.query('[pos="DET"] [pos="NOUN"]'):
    print(hit.start, hit.end)
```

### Julia

```julia
using Montre

corpus = open_corpus("./my-corpus")
for hit in query(corpus, "[pos=\"DET\"] [pos=\"NOUN\"]")
    println(hit)
end
```

---

## Architecture

```
montre-core     Core data model (Position, Span, Token, Unit, Component)
montre-index    Index structures (inverted, forward, span indexes)
montre-query    Query parser, planner, optimizer, executor
montre-build    Corpus construction (CoNLL-U, JSON, text + metadata)
montre-align    Alignment ingestion and projection engine
montre-cli      Command-line interface
montre-py       Python bindings
montre-jl       Julia bindings
```

---

## License

Apache-2.0

