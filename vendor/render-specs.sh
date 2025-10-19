#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

(
    cd "$SCRIPT_DIR"
    OUTPUT_DIR="$SCRIPT_DIR/rdp-specs-md"
    mkdir -p "$OUTPUT_DIR"
    python3 extract_specs.py
    python3 render_text.py --output-root "$OUTPUT_DIR" --resource-dir images --copy-resources
    rm -rf rdp-specs
)
