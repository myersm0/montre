# Montre
A modern, embeddable query engine for corpus linguistics.

## Status
**Early development.** Not yet usable for real work.

## Goals
- Fast queries on large annotated corpora (100M+ tokens)
- Embeddable: use from Python/Julia/R without running a server
- Native CoNLL-U input (works directly with Stanza, UDPipe, spaCy output)
- Clean CQL-like query language
- First-class parallel corpus support

## Building
```bash
cargo build --release
```

## Usage (planned)
```bash
# Build a corpus from CoNLL-U
montre build --input corpus.conllu --output ./my-corpus

# Query
montre query ./my-corpus '[pos="NOUN"] [pos="NOUN"]'

# Info
montre info ./my-corpus
```

From Python (planned):

```python
import montre

corpus = montre.open("./my-corpus")
for hit in corpus.query('[pos="DET"] [pos="NOUN"]'):
    print(hit.start, hit.end)
```

## Architecture
```
montre-core     Core data model (Position, Span, Token, etc.)
montre-index    Index structures (inverted, forward, spans)
montre-query    Query parser, planner, executor
montre-build    Corpus construction from CoNLL-U, JSON, etc.
montre-cli      Command-line interface
montre-py       Python bindings
```

## License
Apache-2.0
