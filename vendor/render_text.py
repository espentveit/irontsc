#!/usr/bin/env python3
"""
Render extracted OpenXML specs into Markdown with embedded image references.

Run after `extract_specs.py`. Optionally copy media into a separate output
directory (for example, `rdp-specs-md`) with a dedicated images subfolder.
"""

from __future__ import annotations

import argparse
import shutil
import sys
from collections import OrderedDict
from pathlib import Path
from typing import Dict, Iterable, List, Optional, Set, Tuple
from xml.etree import ElementTree as ET

NS = {
    "w": "http://schemas.openxmlformats.org/wordprocessingml/2006/main",
    "a": "http://schemas.openxmlformats.org/drawingml/2006/main",
    "r": "http://schemas.openxmlformats.org/officeDocument/2006/relationships",
    "wp": "http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing",
    "c": "http://schemas.openxmlformats.org/drawingml/2006/chart",
    "v": "urn:schemas-microsoft-com:vml",
    "rel": "http://schemas.openxmlformats.org/package/2006/relationships",
}

XML_SPACE = "{http://www.w3.org/XML/1998/namespace}space"

_MARKDOWN_FILES_LOGGED: Set[Path] = set()


def resolve_target(base_dir: Path, target: str, spec_root: Path) -> str:
    if not target or ":" in target.split("/", 1)[0]:
        return target
    combined = (base_dir / target).resolve()
    try:
        return combined.relative_to(spec_root).as_posix()
    except ValueError:
        return combined.as_posix()


def iter_spec_directories(spec_root: Path) -> Iterable[Path]:
    if not spec_root.exists():
        return []
    for entry in sorted(spec_root.iterdir()):
        if entry.is_dir() and (entry / "word" / "document.xml").exists():
            yield entry


def parse_relationships(rels_path: Path, spec_root: Path) -> Dict[str, Tuple[str, str]]:
    if not rels_path.exists():
        return {}

    tree = ET.parse(rels_path)
    rels = {}
    for rel in tree.getroot().findall("rel:Relationship", NS):
        rel_id = rel.attrib.get("Id")
        target = rel.attrib.get("Target", "")
        rel_type = rel.attrib.get("Type", "")
        if rel_id:
            resolved_target = resolve_target(rels_path.parent, target, spec_root)
            rels[rel_id] = (resolved_target, rel_type)
    return rels


def parse_notes(
    notes_path: Path,
    relationships: Dict[str, Tuple[str, str]],
    spec_dir: Path,
    resource_dir: Optional[str],
    resource_collector: Optional[Set[str]],
) -> Dict[str, str]:
    if not notes_path.exists():
        return {}

    tree = ET.parse(notes_path)
    notes = {}
    for footnote in tree.getroot().findall("w:footnote", NS):
        note_id = footnote.attrib.get(f"{{{NS['w']}}}id")
        if note_id is None:
            continue
        note_lines: List[str] = []
        for para in footnote.findall(".//w:p", NS):
            paragraph_text = paragraph_to_text(
                para,
                relationships,
                spec_dir,
                resource_dir,
                None,
                resource_collector,
            )
            if paragraph_text:
                note_lines.append(paragraph_text)
        notes[note_id] = " ".join(note_lines)
    return notes


def local_name(tag: str) -> str:
    return tag.split("}", 1)[-1] if "}" in tag else tag


def paragraph_to_text(
    paragraph: ET.Element,
    relationships: Dict[str, Tuple[str, str]],
    spec_dir: Path,
    resource_dir: Optional[str],
    footnote_refs: Optional[OrderedDict[str, None]],
    resource_collector: Optional[Set[str]],
) -> str:
    parts: List[str] = []
    for run in paragraph.findall(".//w:r", NS):
        for child in list(run):
            lname = local_name(child.tag)
            if lname == "t":
                text = child.text or ""
                if child.attrib.get(XML_SPACE) != "preserve":
                    text = text.replace("\n", " ")
                parts.append(text)
            elif lname in {"tab"}:
                parts.append("\t")
            elif lname in {"br", "cr"}:
                parts.append("\n")
            elif lname == "footnoteReference":
                if footnote_refs is not None:
                    ref_id = child.attrib.get(f"{{{NS['w']}}}id", "?")
                    if ref_id not in footnote_refs:
                        footnote_refs[ref_id] = None
                    parts.append(f"[Footnote {ref_id}]")
            elif lname == "drawing":
                placeholder = render_drawing(
                    child,
                    relationships,
                    spec_dir,
                    resource_dir,
                    resource_collector,
                )
                if placeholder:
                    parts.append(f" {placeholder} ")
            elif lname == "pict":
                placeholder = render_vml_image(
                    child,
                    relationships,
                    spec_dir,
                    resource_dir,
                    resource_collector,
                )
                if placeholder:
                    parts.append(f" {placeholder} ")
    return "".join(parts).strip()


def render_drawing(
    drawing: ET.Element,
    relationships: Dict[str, Tuple[str, str]],
    spec_dir: Path,
    resource_dir: Optional[str],
    resource_collector: Optional[Set[str]],
) -> Optional[str]:
    doc_pr = drawing.find(".//wp:docPr", NS)
    described = None
    if doc_pr is not None:
        described = doc_pr.attrib.get("descr") or doc_pr.attrib.get("title")

    blip = drawing.find(".//a:blip", NS)
    if blip is not None:
        embed_id = blip.attrib.get(f"{{{NS['r']}}}embed")
        if embed_id and embed_id in relationships:
            target, rel_type = relationships[embed_id]
            if "chart" in rel_type:
                return format_chart_reference(target, described)
            if register_resource(target, resource_collector):
                return format_image_reference(target, described, resource_dir, spec_dir)
            return None

    chart = drawing.find(".//c:chart", NS)
    if chart is not None:
        chart_id = chart.attrib.get(f"{{{NS['r']}}}id")
        if chart_id and chart_id in relationships:
            target = relationships[chart_id][0]
            return format_chart_reference(target, described)

    return described and format_image_reference("", described, resource_dir, spec_dir)


def render_vml_image(
    pict: ET.Element,
    relationships: Dict[str, Tuple[str, str]],
    spec_dir: Path,
    resource_dir: Optional[str],
    resource_collector: Optional[Set[str]],
) -> Optional[str]:
    imagedata = pict.find(".//v:imagedata", NS)
    if imagedata is None:
        return None
    embed_id = imagedata.attrib.get(f"{{{NS['r']}}}id")
    title = imagedata.attrib.get("title")
    if embed_id and embed_id in relationships:
        target = relationships[embed_id][0]
        if register_resource(target, resource_collector):
            return format_image_reference(target, title, resource_dir, spec_dir)
        return None
    return title and format_image_reference("", title, resource_dir, spec_dir)


def register_resource(target: str, collector: Optional[Set[str]]) -> bool:
    if collector is None:
        return True
    if not target or ":" in target.split("/", 1)[0]:
        return False
    collector.add(target)
    return True


def load_markdown_text(spec_dir: Path, reference: str) -> Optional[str]:
    if not reference or ":" in reference.split("/", 1)[0]:
        return None

    spec_root = spec_dir.resolve()
    candidate = (spec_root / Path(reference)).resolve()
    try:
        candidate.relative_to(spec_root)
    except ValueError:
        return None

    markdown_path = candidate.with_suffix(candidate.suffix + ".markdown.md")
    if not markdown_path.exists():
        return None

    content = markdown_path.read_text(encoding="utf-8").strip()
    if content and markdown_path not in _MARKDOWN_FILES_LOGGED:
        print(f"Loaded Markdown from {markdown_path}:", flush=True)
        print(content, flush=True)
        _MARKDOWN_FILES_LOGGED.add(markdown_path)
    return content or None


def format_image_reference(
    reference: str,
    description: Optional[str],
    resource_dir: Optional[str],
    spec_dir: Path,
) -> str:
    alt_text = description or Path(reference).name or "Image"
    markdown_text = load_markdown_text(spec_dir, reference)

    if not reference:
        if markdown_text:
            return markdown_text
        return f"![{alt_text}]()"

    filename = Path(reference).name
    rel_path = f"{resource_dir}/{filename}" if resource_dir else reference

    if markdown_text:
        return f"{markdown_text}\n\n![{alt_text}]({rel_path})"

    return f"![{alt_text}]({rel_path})"


def format_chart_reference(reference: str, description: Optional[str]) -> str:
    label = description or Path(reference).name or "Chart"
    return f"[Chart: {label} — {reference}]"


def is_heading(paragraph: ET.Element) -> Optional[int]:
    style = paragraph.find("w:pPr/w:pStyle", NS)
    if style is None:
        return None
    style_id = style.attrib.get(f"{{{NS['w']}}}val", "")
    if not style_id:
        return None
    for level in range(1, 7):
        if style_id.lower() in {f"heading{level}", f"h{level}"}:
            return level
    if style_id.upper().startswith("MSDNH"):
        try:
            return int(style_id[-1])
        except ValueError:
            return 2
    if style_id.lower() == "title":
        return 1
    return None


def is_bullet(paragraph: ET.Element) -> bool:
    return paragraph.find("w:pPr/w:numPr", NS) is not None


def table_to_markdown(
    table: ET.Element,
    relationships: Dict[str, Tuple[str, str]],
    spec_dir: Path,
    resource_dir: Optional[str],
    footnote_refs: Optional[OrderedDict[str, None]],
    resource_collector: Optional[Set[str]],
) -> List[str]:
    rows: List[List[str]] = []
    for row in table.findall("w:tr", NS):
        cells: List[str] = []
        for cell in row.findall("w:tc", NS):
            paragraphs = cell.findall(".//w:p", NS)
            texts = []
            for para in paragraphs:
                text = paragraph_to_text(
                    para,
                    relationships,
                    spec_dir,
                    resource_dir,
                    footnote_refs,
                    resource_collector,
                )
                if text:
                    texts.append(text)
            cells.append(" / ".join(texts).strip())
        rows.append(cells)

    if not rows:
        return []

    column_count = max(len(r) for r in rows)
    normalized_rows = [row + [""] * (column_count - len(row)) for row in rows]

    lines: List[str] = []
    header = normalized_rows[0]
    lines.append("| " + " | ".join(header) + " |")
    lines.append("| " + " | ".join("---" for _ in header) + " |")
    for row in normalized_rows[1:]:
        lines.append("| " + " | ".join(row) + " |")
    lines.append("")
    return lines


def render_spec(
    spec_dir: Path,
    output_dir: Path,
    resource_dir: Optional[str],
) -> Tuple[Path, Set[str]]:
    doc_path = spec_dir / "word" / "document.xml"
    doc_relationships = parse_relationships(spec_dir / "word" / "_rels" / "document.xml.rels", spec_dir)
    footnote_relationships = parse_relationships(spec_dir / "word" / "_rels" / "footnotes.xml.rels", spec_dir)
    resources_used: Set[str] = set()
    footnotes = parse_notes(
        spec_dir / "word" / "footnotes.xml",
        footnote_relationships,
        spec_dir,
        resource_dir,
        resources_used,
    )
    footnote_refs: "OrderedDict[str, None]" = OrderedDict()

    tree = ET.parse(doc_path)
    body = tree.getroot().find("w:body", NS)
    if body is None:
        raise RuntimeError(f"No document body found in {doc_path}")

    output_dir.mkdir(parents=True, exist_ok=True)
    lines: List[str] = [f"# {spec_dir.name}"]
    for child in list(body):
        lname = local_name(child.tag)
        if lname == "p":
            text = paragraph_to_text(
                child,
                doc_relationships,
                spec_dir,
                resource_dir,
                footnote_refs,
                resources_used,
            )
            if not text:
                continue
            heading_level = is_heading(child)
            if heading_level is not None:
                prefix = "#" * max(1, min(6, heading_level))
                lines.append(f"{prefix} {text}")
            elif is_bullet(child):
                lines.append(f"- {text}")
            else:
                lines.append(text)
        elif lname == "tbl":
            lines.extend(
                table_to_markdown(
                    child,
                    doc_relationships,
                    spec_dir,
                    resource_dir,
                    footnote_refs,
                    resources_used,
                )
            )

    if footnote_refs:
        lines.append("")
        lines.append("## Footnotes")
        for ref_id in footnote_refs:
            note_text = footnotes.get(ref_id, "").strip()
            lines.append(f"- [{ref_id}] {note_text}")

    output_path = output_dir / "document.md"
    output_path.write_text("\n".join(lines), encoding="utf-8")
    return output_path, resources_used


def copy_resources(
    spec_dir: Path,
    output_dir: Path,
    resource_dir: str,
    resources: Set[str],
) -> None:
    if not resources:
        return
    destination = output_dir / resource_dir
    destination.mkdir(parents=True, exist_ok=True)
    for resource in resources:
        src = (spec_dir / resource).resolve()
        if not src.exists():
            continue
        filename = Path(resource).name
        dst = destination / filename
        shutil.copy2(src, dst)


def build_cli() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Render extracted RDP spec docs to Markdown.")
    parser.add_argument(
        "--output-root",
        type=Path,
        help="Directory where rendered specs should be written. Defaults to in-place.",
    )
    parser.add_argument(
        "--resource-dir",
        default="media",
        help="Subdirectory name for media references (default: media).",
    )
    parser.add_argument(
        "--copy-resources",
        action="store_true",
        help="Copy referenced media into the output directory under the resource-dir name.",
    )
    return parser


def main() -> int:
    parser = build_cli()
    args = parser.parse_args()

    script_dir = Path(__file__).resolve().parent
    spec_root = script_dir / "rdp-specs"
    spec_dirs = list(iter_spec_directories(spec_root))
    if not spec_dirs:
        print("No extracted spec directories found. Run extract_specs.py first.", file=sys.stderr)
        return 1

    output_root = args.output_root
    resource_dir = args.resource_dir

    for spec_dir in spec_dirs:
        destination_dir = (output_root / spec_dir.name) if output_root else spec_dir
        relative_print_base = output_root or spec_root
        print(f"Rendering {spec_dir.name}...")
        output_path, resources = render_spec(spec_dir, destination_dir, resource_dir)
        try:
            display_path = output_path.relative_to(relative_print_base)
        except ValueError:
            display_path = output_path
        print(f"  -> {display_path}")
        if args.copy_resources and resource_dir:
            copy_resources(spec_dir, destination_dir, resource_dir, resources)

    print("Rendering complete.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
