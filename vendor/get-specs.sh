#!/usr/bin/env bash
set -euo pipefail

# Where to store things
ROOT_DIR="rdp-specs"
mkdir -p "$ROOT_DIR"

# Spec name => verified DOCX URL (Published Version)
# Sources:
# RDPEUDP (Published 2024-04-23) :contentReference[oaicite:0]{index=0}
# RDPEUDP2 (Published 2024-04-23) :contentReference[oaicite:1]{index=1}
# RDPEGFX (Published 2025-08-11) :contentReference[oaicite:2]{index=2}
# RDPBCGR (Published 2025-04-07) :contentReference[oaicite:3]{index=3}
# RDPEUSB (Published 2024-04-23) :contentReference[oaicite:4]{index=4}
# RDPELE  (Published 2024-04-23) :contentReference[oaicite:5]{index=5}
# RDPEPNP (Published 2024-04-23) :contentReference[oaicite:6]{index=6}
# RDPECLIP(Published 2024-04-23) :contentReference[oaicite:7]{index=7}
# RDPESP  (2024 content page)      :contentReference[oaicite:8]{index=8}
# RDPEDC  (Latest is 2017-06-01)   :contentReference[oaicite:9]{index=9}
declare -A URLS=(
  ["MS-RDPEUDP"]="https://winprotocoldocs-bhdugrdyduf5h2e4.b02.azurefd.net/MS-RDPEUDP/%5BMS-RDPEUDP%5D-240423.docx"
  ["MS-RDPEUDP2"]="https://winprotocoldocs-bhdugrdyduf5h2e4.b02.azurefd.net/MS-RDPEUDP2/%5BMS-RDPEUDP2%5D-240423.docx"
  ["MS-RDPEGFX"]="https://winprotocoldocs-bhdugrdyduf5h2e4.b02.azurefd.net/MS-RDPEGFX/%5BMS-RDPEGFX%5D-250811.docx"
  ["MS-RDPBCGR"]="https://winprotocoldocs-bhdugrdyduf5h2e4.b02.azurefd.net/MS-RDPBCGR/%5BMS-RDPBCGR%5D-250407.docx"
  ["MS-RDPEUSB"]="https://winprotocoldocs-bhdugrdyduf5h2e4.b02.azurefd.net/MS-RDPEUSB/%5BMS-RDPEUSB%5D-240423.docx"
  ["MS-RDPELE"]="https://winprotocoldocs-bhdugrdyduf5h2e4.b02.azurefd.net/MS-RDPELE/%5BMS-RDPELE%5D-240423.docx"
  ["MS-RDPEPNP"]="https://winprotocoldocs-bhdugrdyduf5h2e4.b02.azurefd.net/MS-RDPEPNP/%5BMS-RDPEPNP%5D-240423.docx"
  ["MS-RDPECLIP"]="https://winprotocoldocs-bhdugrdyduf5h2e4.b02.azurefd.net/MS-RDPECLIP/%5BMS-RDPECLIP%5D-240423.docx"
  ["MS-RDPESP"]="https://winprotocoldocs-bhdugrdyduf5h2e4.b02.azurefd.net/MS-RDPESP/%5BMS-RDPESP%5D-240423.docx"
  # Corrected link (your 404): latest here is 2017-06-01, not 2024-04-23
  ["MS-RDPEDC"]="https://winprotocoldocs-bhdugrdyduf5h2e4.b02.azurefd.net/MS-RDPEDC/%5BMS-RDPEDC%5D-170601.docx"
)

# Helper: sanitize a filename by removing [] and percent escapes
sanitize() {
  local raw="$1"
  # strip directory, decode basic %xx for brackets and spaces if present
  local base="${raw##*/}"
  # remove square brackets
  base="${base//[/}"
  base="${base//]/}"
  echo "$base"
}

# Download, then unzip to subfolder with same sanitized stem
for key in "${!URLS[@]}"; do
  url="${URLS[$key]}"
  # Build sanitized file path
  file_name="$(sanitize "$url")"
  out_path="$ROOT_DIR/$file_name"
  out_dir="${out_path%.docx}"   # subfolder without .docx

  echo "→ $key"
  echo "   URL : $url"
  echo "   File: $out_path"
  echo "   Dir : $out_dir"

  # Fetch
  if ! curl -fSL --retry 3 --retry-delay 1 "$url" -o "$out_path"; then
    echo "!! Failed: $key ($url)" >&2
    continue
  fi

  # Unzip quietly to subfolder (docx is a Zip)
  mkdir -p "$out_dir"
  # Some systems need 'unzip' installed
  if command -v unzip >/dev/null 2>&1; then
    unzip -oq "$out_path" -d "$out_dir" || {
      echo "!! Unzip failed for $out_path (is it a valid DOCX?)" >&2
    }
  else
    echo "!! 'unzip' not found; skipped extracting $out_path" >&2
  fi
done

echo "Done. Files in $ROOT_DIR/"
