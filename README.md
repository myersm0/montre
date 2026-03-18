# Montre

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
| `[pos="NOUN"]` | ~180K | ~12ms |
| `[pos="ADJ"] [pos="NOUN"]` | 30,672 | 13ms |
| `[pos="ADJ"]? [pos="NOUN"]` | 272,019 | 72ms |
| `([pos="ADJ"] \| [pos="ADV"])+ [pos="NOUN"]` | 33,444 | 27ms |
| `([pos="ADJ"] \| [pos="DET"])+ [pos="NOUN"]` | 198,735 | 71ms |
| Alignment projection | — | ~250µs overhead |

Quantifiers use a run-based execution model that scales with matching tokens, not corpus size.

## Library usage

Montre ships a C FFI (`libmontre_ffi`) for embedding in other languages.

### Julia

```julia
using Montre

corpus = open_corpus("./my-corpus")
hits = query(corpus, """[pos="ADJ"] [pos="NOUN"]""")
for line in concordance(corpus, hits)
    println(line)
end
```

### Python

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
montre-build    Corpus construction from CoNLL-U, multi-component builder
montre-cli      Command-line interface
montre-ffi      C FFI for Julia/Python/R bindings
montre-py       Python bindings (PyO3, stub)
```

## Status

**v0.1.0** — core engine is functional and tested.

Working: token queries, sequences, quantifiers, alternation, regex, negation, conjunction, `within` constraints, multi-component corpora, sentence-level alignment, alignment projection, C FFI.

Next: labeled captures and global constraints (Phase 2b), statistics commands, Julia and Python bindings, TUI.

## License

Apache-2.0
