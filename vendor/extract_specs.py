#!/usr/bin/env python3
"""
Extract all OpenXML content (XML, media, charts, etc.) from the DOCX specs.

This script expects the DOCX sources to live in ../rdp-specs-source and will
mirror each document into a subdirectory of the current folder.
"""

from __future__ import annotations

import os
import shutil
import sys
import zipfile
import xml.etree.ElementTree as ET
from pathlib import Path, PurePosixPath
from urllib.parse import unquote

try:
    from PIL import Image
except ImportError:  # pragma: no cover - Pillow is optional but recommended
    Image = None

REL_NS = {"rel": "http://schemas.openxmlformats.org/package/2006/relationships"}
RESOURCE_SUBDIRS = {"media", "charts", "embeddings"}


def _safe_extract_member(zip_file: zipfile.ZipFile, member: zipfile.ZipInfo, target_root: Path) -> None:
    """Extract a single zip member while protecting against path traversal."""
    raw_path = PurePosixPath(member.filename)
    if raw_path.is_absolute() or ".." in raw_path.parts:
        raise RuntimeError(f"Unsafe path detected in archive: {member.filename}")

    if raw_path.name == "":
        # Directory entries have empty names; ensure the directory exists.
        target_path = target_root / Path(*raw_path.parts)
        target_path.mkdir(parents=True, exist_ok=True)
        return

    relative_parts = Path(*raw_path.parts)
    target_path = target_root / relative_parts

    target_path.parent.mkdir(parents=True, exist_ok=True)

    with zip_file.open(member) as src, target_path.open("wb") as dst:
        shutil.copyfileobj(src, dst)


def sanitize_name(name: str) -> str:
    decoded = unquote(name)
    return decoded.replace("[", "").replace("]", "")


def extract_docx(docx_path: Path, destination_root: Path) -> Path:
    """Extract a single DOCX into its own folder under destination_root."""
    doc_name = sanitize_name(docx_path.stem)
    target_dir = destination_root / doc_name

    if target_dir.exists():
        shutil.rmtree(target_dir)

    with zipfile.ZipFile(docx_path) as archive:
        for member in archive.infolist():
            _safe_extract_member(archive, member, target_dir)

    prefix_resources(target_dir, doc_name)

    return target_dir


def prefix_resources(target_dir: Path, prefix: str) -> None:
    """Prefix resource filenames and update relationship targets accordingly."""
    if not (target_dir / "word").exists():
        return

    mapping: dict[str, str] = {}

    for subdir in RESOURCE_SUBDIRS:
        base = target_dir / "word" / subdir
        if not base.exists():
            continue
        for file_path in base.iterdir():
            if not file_path.is_file():
                continue
            original_rel = file_path.relative_to(target_dir).as_posix()
            prefixed_name = f"{prefix}_{file_path.name}"
            prefixed_path = file_path.with_name(prefixed_name)
            file_path.rename(prefixed_path)
            final_path = ensure_png(prefixed_path)
            mapping[original_rel] = final_path.relative_to(target_dir).as_posix()

    if not mapping:
        return

    for rels_path in target_dir.rglob("*.rels"):
        tree = ET.parse(rels_path)
        root = tree.getroot()
        updated = False

        for rel in root.findall("rel:Relationship", REL_NS):
            target = rel.attrib.get("Target")
            if not target:
                continue
            resolved = resolve_relationship_target(rels_path.parent, target)
            if resolved is None:
                continue
            try:
                rel_key = resolved.relative_to(target_dir).as_posix()
            except ValueError:
                continue
            if rel_key in mapping:
                new_target_path = Path(mapping[rel_key])
                new_target = os.path.relpath(target_dir / new_target_path, rels_path.parent)
                rel.attrib["Target"] = Path(new_target).as_posix()
                updated = True

        if updated:
            tree.write(rels_path, encoding="utf-8", xml_declaration=True)


def resolve_relationship_target(base_dir: Path, target: str) -> Path | None:
    """Resolve a relationship target to an absolute filesystem path."""
    if not target:
        return None
    if ":" in target.split("/", 1)[0]:
        return None
    source_base = base_dir
    if source_base.name == "_rels":
        source_base = source_base.parent
    return (source_base / target).resolve()


def ensure_png(path: Path) -> Path:
    """Ensure the given resource is stored as a PNG file and return its path."""
    detected = detect_image_type(path)
    if detected == "png":
        if path.suffix.lower() != ".png":
            new_path = path.with_suffix(".png")
            path.rename(new_path)
            return new_path
        return path

    if detected in {"jpeg", "bmp", "gif", "tiff"}:
        return convert_to_png(path)

    # Leave non-image or already-correct resources untouched.
    return path


def convert_to_png(path: Path) -> Path:
    """Convert an image file to PNG format."""
    if Image is None:
        raise RuntimeError(
            f"Conversion to PNG requires Pillow. Unable to convert {path.name}."
        )
    with Image.open(path) as img:
        png_path = path.with_suffix(".png")
        img.save(png_path, "PNG")
    path.unlink()
    return png_path


def detect_image_type(path: Path) -> str | None:
    """Detect a simple set of raster formats based on file signatures."""
    with path.open("rb") as fh:
        signature = fh.read(12)

    if signature.startswith(b"\x89PNG\r\n\x1a\n"):
        return "png"
    if signature.startswith(b"\xff\xd8"):
        return "jpeg"
    if signature.startswith(b"GIF87a") or signature.startswith(b"GIF89a"):
        return "gif"
    if signature.startswith(b"BM"):
        return "bmp"
    if signature.startswith(b"II*\x00") or signature.startswith(b"MM\x00*"):
        return "tiff"
    return None


def main() -> int:
    script_dir = Path(__file__).resolve().parent
    destination_root = script_dir / "rdp-specs"
    source_root = script_dir / "rdp-specs-source"
    destination_root.mkdir(parents=True, exist_ok=True)

    if not source_root.exists():
        print(f"Source directory not found: {source_root}", file=sys.stderr)
        return 1

    docx_files = sorted(source_root.glob("*.docx"))
    if not docx_files:
        print(f"No DOCX files found in {source_root}", file=sys.stderr)
        return 1

    for docx in docx_files:
        print(f"Extracting {docx.name}...")
        extracted_dir = extract_docx(docx, destination_root)
        print(f"  -> {extracted_dir}")

    print("Extraction complete.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
