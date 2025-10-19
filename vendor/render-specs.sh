#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REQUIRE_MERMAID_DEPS="${REQUIRE_MERMAID_DEPS:-1}"

if ! command -v python3 >/dev/null 2>&1; then
    echo "Error: python3 interpreter not found in PATH." >&2
    exit 1
fi

check_mermaid_python_deps() {
    python3 - <<'PY'
import importlib
import sys

missing = []
for module in ("torch", "transformers", "PIL", "torchvision", "accelerate"):
    try:
        importlib.import_module(module)
    except Exception:
        missing.append(module)

if missing:
    print(
        "Missing Python dependencies for Mermaid generation: " + ", ".join(missing) + ".",
        file=sys.stderr,
    )
    print(
        "Install them with 'pip install torch transformers pillow torchvision accelerate' or set REQUIRE_MERMAID_DEPS=0 to bypass.",
        file=sys.stderr,
    )
    sys.exit(1)
PY
}

if ! check_mermaid_python_deps; then
    if [[ "$REQUIRE_MERMAID_DEPS" == "1" ]]; then
        exit 1
    fi
    echo "Warning: Mermaid generation dependencies missing; continuing without Mermaid output." >&2
fi

(
    cd "$SCRIPT_DIR"
    OUTPUT_DIR="$SCRIPT_DIR/rdp-specs-md"
    mkdir -p "$OUTPUT_DIR"
    python3 extract_specs.py
    python3 render_text.py --output-root "$OUTPUT_DIR" --resource-dir images --copy-resources
#    rm -rf rdp-specs
)
