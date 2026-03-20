#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
DATA_DIR="$SCRIPT_DIR/data"
ISOSCELES_DIR="$DATA_DIR/isosceles"
CORPUS_DIR="$DATA_DIR/corpus"

mkdir -p "$DATA_DIR"

if [ ! -d "$ISOSCELES_DIR" ]; then
	echo "Cloning isosceles..."
	git clone --depth 1 https://github.com/myersm0/isosceles.git "$ISOSCELES_DIR"
else
	echo "Isosceles already cloned at $ISOSCELES_DIR"
fi

if [ ! -f "$CORPUS_DIR/corpus.json" ]; then
	echo "Building benchmark corpus..."
	cargo build --release -p montre-cli
	target/release/montre build \
		-m "$SCRIPT_DIR/bench_corpus.toml" \
		-o "$CORPUS_DIR" \
		--force
else
	echo "Benchmark corpus already built at $CORPUS_DIR"
fi

echo "Done. Run benchmarks with:"
echo "  MONTRE_BENCH_CORPUS=$CORPUS_DIR cargo bench"
