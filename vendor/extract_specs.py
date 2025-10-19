#!/usr/bin/env python3
"""
Extract all OpenXML content (XML, media, charts, etc.) from the DOCX specs.

This script expects the DOCX sources to live in ../rdp-specs-source and will
mirror each document into a subdirectory of the current folder.
"""

from __future__ import annotations

import os
import re
import shutil
import sys
import time
import zipfile
import xml.etree.ElementTree as ET
from pathlib import Path, PurePosixPath
from typing import Any, List
from urllib.parse import unquote

try:
    from PIL import Image
except ImportError:  # pragma: no cover - Pillow is optional but recommended
    Image = None
    _RESAMPLE = None
else:  # pragma: no cover - compatibility shim
    try:
        _RESAMPLE = Image.Resampling.LANCZOS  # type: ignore[attr-defined]
    except AttributeError:  # Pillow < 9.1
        _RESAMPLE = Image.LANCZOS  # type: ignore[attr-defined]

try:
    import torch
except ImportError:  # pragma: no cover - torch is optional but recommended
    torch = None  # type: ignore[assignment]

try:
    from transformers import AutoProcessor, Qwen3VLForConditionalGeneration
except ImportError:  # pragma: no cover - transformers is optional but recommended
    AutoProcessor = None  # type: ignore[assignment]
    Qwen3VLForConditionalGeneration = None  # type: ignore[assignment]


REL_NS = {"rel": "http://schemas.openxmlformats.org/package/2006/relationships"}
RESOURCE_SUBDIRS = {"media", "charts", "embeddings"}


DEFAULT_MARKDOWN_PROMPT = (
    "Describe the image in Markdown. Use headings, bullet lists, and tables when helpful. "
    "Focus on capturing structure, relationships, and important labels without guessing." 
)
_MARKDOWN_WARNING_EMITTED = False
_GPU_WARNING_EMITTED = False
_RESIZE_WARNING_EMITTED = False
IMAGE_MAX_BYTES = 200 * 1024
IMAGE_MAX_DIMENSION = 1024
_QWEN_MODEL: Any = None
_QWEN_PROCESSOR: Any = None


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


def extract_docx(docx_path: Path, destination_root: Path) -> tuple[Path, List[Path]]:
    """Extract a single DOCX into its own folder under destination_root."""
    doc_name = sanitize_name(docx_path.stem)
    target_dir = destination_root / doc_name

    if target_dir.exists():
        shutil.rmtree(target_dir)

    with zipfile.ZipFile(docx_path) as archive:
        for member in archive.infolist():
            _safe_extract_member(archive, member, target_dir)

    resources = prefix_resources(target_dir, doc_name)

    return target_dir, resources


def prefix_resources(target_dir: Path, prefix: str) -> List[Path]:
    """Prefix resource filenames and update relationship targets accordingly."""
    if not (target_dir / "word").exists():
        return []

    mapping: dict[str, str] = {}
    collected: List[Path] = []

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
            collected.append(final_path)
            mapping[original_rel] = final_path.relative_to(target_dir).as_posix()

    if not mapping:
        return collected

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

    return collected


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


def _extract_markdown_text(text: str) -> str | None:
    stripped = text.strip()
    if not stripped:
        return None

    fence = re.search(r"```(?:markdown)?\s*([\s\S]*?)```", stripped, re.IGNORECASE)
    if fence:
        candidate = fence.group(1).strip()
        if candidate:
            return candidate

    return stripped or None


def _load_qwen_model() -> bool:
    global _QWEN_MODEL, _QWEN_PROCESSOR, _MARKDOWN_WARNING_EMITTED

    if _QWEN_MODEL is not None and _QWEN_PROCESSOR is not None:
        return True

    if AutoProcessor is None or Qwen3VLForConditionalGeneration is None or torch is None:
        if not _MARKDOWN_WARNING_EMITTED:
            missing = []
            if AutoProcessor is None or Qwen3VLForConditionalGeneration is None:
                missing.append("transformers")
            if torch is None:
                missing.append("torch")
            print(
                "Skipping Markdown generation: missing dependencies ("
                + ", ".join(missing)
                + ").",
                file=sys.stderr,
            )
            _MARKDOWN_WARNING_EMITTED = True
        return False

    model_name = (
        os.environ.get("QWEN_VL_MODEL")
        or os.environ.get("MARKDOWN_VL_MODEL")
        or os.environ.get("MERMAID_VL_MODEL")
        or "Qwen/Qwen3-VL-8B-Instruct"
    )

    try:
        _QWEN_PROCESSOR = AutoProcessor.from_pretrained(model_name)
        _QWEN_MODEL = Qwen3VLForConditionalGeneration.from_pretrained(
            model_name,
            dtype="auto",
            device_map="auto",
        )
    except Exception as exc:  # pragma: no cover - depends on environment
        if not _MARKDOWN_WARNING_EMITTED:
            print(
                f"Failed to load Qwen3-VL model '{model_name}': {exc}",
                file=sys.stderr,
            )
            _MARKDOWN_WARNING_EMITTED = True
        _QWEN_MODEL = None
        _QWEN_PROCESSOR = None
        return False

    _warn_if_cpu_only(_QWEN_MODEL)
    return True


def _collect_model_devices(model: Any) -> set[str]:
    devices: set[str] = set()
    device_map = getattr(model, "hf_device_map", None)
    if isinstance(device_map, dict) and device_map:
        for target in device_map.values():
            if isinstance(target, str):
                devices.add(target)
            elif isinstance(target, (list, tuple, set)):
                for entry in target:
                    devices.add(str(entry))
            else:
                devices.add(str(target))
    if not devices:
        try:
            first_param = next(model.parameters())
            devices.add(str(first_param.device))
        except StopIteration:
            pass
        except AttributeError:
            pass
    attr_device = getattr(model, "device", None)
    if attr_device is not None:
        devices.add(str(attr_device))
    cleaned = {device for device in (d.strip() for d in devices) if device}
    return cleaned or {"cpu"}


def _warn_if_cpu_only(model: Any) -> None:
    global _GPU_WARNING_EMITTED
    if _GPU_WARNING_EMITTED or torch is None:
        return

    try:
        cuda_available = torch.cuda.is_available()
    except Exception:  # pragma: no cover - defensive
        cuda_available = False

    devices = {dev.lower() for dev in _collect_model_devices(model)}
    uses_cuda = any(dev.startswith("cuda") for dev in devices)
    if uses_cuda:
        return

    if not cuda_available:
        reason = "PyTorch reports no CUDA-capable device (torch.cuda.is_available() is False)"
    else:
        reason = f"model placement {sorted(devices)}"

    print(
        f"Warning: Qwen3-VL is running on CPU only ({reason}).",
        file=sys.stderr,
    )
    _GPU_WARNING_EMITTED = True


def _prepare_image_for_model(image_path: Path) -> Path:
    """Return a possibly resized image for Qwen3-VL processing."""
    global _RESIZE_WARNING_EMITTED

    try:
        size_bytes = image_path.stat().st_size
    except FileNotFoundError:
        return image_path

    if size_bytes <= IMAGE_MAX_BYTES:
        return image_path

    if Image is None:
        if not _RESIZE_WARNING_EMITTED:
            print(
                "Cannot resize large images for Markdown generation: Pillow is not installed.",
                file=sys.stderr,
            )
            _RESIZE_WARNING_EMITTED = True
        return image_path

    resized_path = image_path.with_name(f"{image_path.stem}_resized{image_path.suffix}")

    try:
        with Image.open(image_path) as img:
            if img.mode in {"P", "LA"}:
                img = img.convert("RGBA")
            elif img.mode == "CMYK":
                img = img.convert("RGB")

            max_side = IMAGE_MAX_DIMENSION
            resample = _RESAMPLE or getattr(Image, "LANCZOS", getattr(Image, "ANTIALIAS", None))
            if resample is not None:
                img.thumbnail((max_side, max_side), resample)
            else:  # pragma: no cover - legacy Pillow fallback
                img.thumbnail((max_side, max_side))

            save_kwargs: dict[str, object] = {"optimize": True}
            suffix_lower = resized_path.suffix.lower()
            if suffix_lower in {".jpg", ".jpeg"}:
                if img.mode not in {"RGB", "L"}:
                    img = img.convert("RGB")
                save_kwargs["quality"] = 85
            elif suffix_lower == ".png":
                if img.mode not in {"RGB", "RGBA", "L"}:
                    img = img.convert("RGBA")

            resized_path.parent.mkdir(parents=True, exist_ok=True)
            img.save(resized_path, **save_kwargs)
    except Exception as exc:  # pragma: no cover - best effort logging
        if not _RESIZE_WARNING_EMITTED:
            print(
                f"Failed to resize {image_path.name} before Markdown generation: {exc}",
                file=sys.stderr,
            )
            _RESIZE_WARNING_EMITTED = True
        return image_path

    new_size = resized_path.stat().st_size
    if new_size > IMAGE_MAX_BYTES and resized_path.suffix.lower() == ".png":
        try:
            adaptive = getattr(Image, "ADAPTIVE", None)
            with Image.open(resized_path) as png_img:
                if adaptive is not None:
                    quantized = png_img.convert("P", palette=adaptive, colors=128)
                else:
                    quantized = png_img.convert("P", colors=128)
                quantized.save(resized_path, optimize=True)
            new_size = resized_path.stat().st_size
        except Exception as exc:  # pragma: no cover - best effort logging
            if not _RESIZE_WARNING_EMITTED:
                print(
                    f"Unable to further compress {resized_path.name}: {exc}",
                    file=sys.stderr,
                )
                _RESIZE_WARNING_EMITTED = True
    print(
        f"Resized {image_path.name} ({size_bytes} bytes) to {resized_path.name} ({new_size} bytes) for Markdown generation.",
        flush=True,
    )
    return resized_path


def _move_inputs_to_device(inputs: Any, device: Any) -> Any:
    if hasattr(inputs, "to"):
        return inputs.to(device)
    if isinstance(inputs, dict):
        return {
            key: _move_inputs_to_device(value, device)
            for key, value in inputs.items()
        }
    if isinstance(inputs, (list, tuple)):
        return type(inputs)(_move_inputs_to_device(value, device) for value in inputs)
    return inputs


def _format_duration(seconds: float) -> str:
    total_seconds = max(0, int(seconds))
    hours, remainder = divmod(total_seconds, 3600)
    minutes, secs = divmod(remainder, 60)
    return f"{hours:02d}:{minutes:02d}:{secs:02d}"


class ProgressTracker:
    def __init__(self, total_images: int) -> None:
        self.total_images = total_images
        self.total_processed = 0
        self.total_start = time.monotonic()
        self.doc_name = ""
        self.doc_total = 0
        self.doc_processed = 0
        self.doc_start = self.total_start

    def start_document(self, doc_name: str, doc_total: int) -> None:
        self.doc_name = doc_name
        self.doc_total = doc_total
        self.doc_processed = 0
        self.doc_start = time.monotonic()

    def finish_item(self, image_name: str, reused: bool) -> str:
        self.doc_processed += 1
        self.total_processed += 1

        doc_elapsed = time.monotonic() - self.doc_start
        total_elapsed = time.monotonic() - self.total_start

        if self.total_processed > 0:
            avg_per_item = total_elapsed / self.total_processed
        else:
            avg_per_item = 0.0

        remaining = max(0.0, (self.total_images - self.total_processed) * avg_per_item)

        doc_total_display = self.doc_total if self.doc_total else self.doc_processed
        total_display = self.total_images if self.total_images else self.total_processed

        message = (
            f"Generating Markdown for {image_name} "
            f"({self.doc_processed}/{doc_total_display} for this document, "
            f"{self.total_processed}/{total_display} total. "
            f"Time taken: {_format_duration(doc_elapsed)}, "
            f"Estimated time until complete: {_format_duration(remaining)}"
        )
        if reused:
            message += ", reused existing output"
        message += ")..."
        return message


def generate_markdown_sidecar(image_path: Path, prompt: str | None = None) -> tuple[str | None, bool]:
    """Generate (or reuse) a Markdown sidecar for the given image."""
    global _MARKDOWN_WARNING_EMITTED

    sidecar_path = image_path.with_suffix(image_path.suffix + ".markdown.md")
    if sidecar_path.exists():
        try:
            existing = sidecar_path.read_text(encoding="utf-8").strip()
        except Exception as exc:  # pragma: no cover - best effort logging
            print(
                f"Warning: unable to read existing Markdown for {image_path.name}: {exc}",
                file=sys.stderr,
            )
            existing = ""
        if existing:
            print(
                f"Markdown already exists for {image_path.name}; reusing {sidecar_path}.",
                flush=True,
            )
            return existing, True

    if Image is None:
        if not _MARKDOWN_WARNING_EMITTED:
            print(
                "Skipping Markdown generation: Pillow is not installed to open images.",
                file=sys.stderr,
            )
            _MARKDOWN_WARNING_EMITTED = True
        return None, False

    if not _load_qwen_model():
        return None, False

    llm_input = _prepare_image_for_model(image_path)

    try:
        with Image.open(llm_input) as pil_img:
            image_payload = pil_img.convert("RGB")
    except Exception as exc:  # pragma: no cover - best effort logging
        if not _MARKDOWN_WARNING_EMITTED:
            print(
                f"Unable to open {llm_input} for Markdown generation: {exc}",
                file=sys.stderr,
            )
            _MARKDOWN_WARNING_EMITTED = True
        return None, False

    effective_prompt = (
        prompt
        or os.environ.get("QWEN_VL_PROMPT")
        or os.environ.get("MARKDOWN_PROMPT")
        or os.environ.get("MERMAID_PROMPT")
        or DEFAULT_MARKDOWN_PROMPT
    )
    messages = [
        {
            "role": "user",
            "content": [
                {"type": "image", "image": image_payload},
                {"type": "text", "text": effective_prompt},
            ],
        }
    ]

    try:
        inputs = _QWEN_PROCESSOR.apply_chat_template(
            messages,
            tokenize=True,
            add_generation_prompt=True,
            return_dict=True,
            return_tensors="pt",
        )

        device = getattr(_QWEN_MODEL, "device", None)
        if device is None and hasattr(_QWEN_MODEL, "hf_device_map"):
            device = next(iter(_QWEN_MODEL.hf_device_map.values()))  # type: ignore[attr-defined]
        if device is None:
            device = "cpu"
        if str(device).lower().startswith("cpu"):
            _warn_if_cpu_only(_QWEN_MODEL)
        inputs = _move_inputs_to_device(inputs, device)

        max_tokens_env = (
            os.environ.get("QWEN_VL_MAX_NEW_TOKENS")
            or os.environ.get("MARKDOWN_VL_MAX_NEW_TOKENS")
            or os.environ.get("MERMAID_VL_MAX_NEW_TOKENS")
            or "512"
        )
        try:
            max_new_tokens = max(1, int(max_tokens_env))
        except ValueError:
            max_new_tokens = 512

        generated_ids = _QWEN_MODEL.generate(
            **inputs,
            max_new_tokens=max_new_tokens,
        )

        input_ids = inputs["input_ids"]
        generated_ids_trimmed = [
            out[len(in_ids) :]
            for in_ids, out in zip(input_ids, generated_ids)
        ]

        raw_outputs = _QWEN_PROCESSOR.batch_decode(
            generated_ids_trimmed,
            skip_special_tokens=True,
            clean_up_tokenization_spaces=False,
        )
        raw_output = raw_outputs[0] if raw_outputs else ""
    except Exception as exc:  # pragma: no cover - depends on environment
        if not _MARKDOWN_WARNING_EMITTED:
            print(
                f"Failed to generate Markdown output for {image_path.name}: {exc}",
                file=sys.stderr,
            )
            _MARKDOWN_WARNING_EMITTED = True
        return None, False

    markdown_text = _extract_markdown_text(raw_output)
    if not markdown_text:
        if not _MARKDOWN_WARNING_EMITTED:
            print(
                f"Model response for {image_path.name} did not contain usable Markdown.",
                file=sys.stderr,
            )
            _MARKDOWN_WARNING_EMITTED = True
        return None, False

    sidecar_path.write_text(markdown_text + "\n", encoding="utf-8")

    print(
        f"Markdown output for {image_path.name} saved to {sidecar_path}:",
        flush=True,
    )
    print(markdown_text, flush=True)
    return markdown_text, False


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

    doc_resources: List[tuple[Path, List[Path]]] = []

    for docx in docx_files:
        print(f"Extracting {docx.name}...")
        extracted_dir, resources = extract_docx(docx, destination_root)
        doc_resources.append((extracted_dir, resources))
        print(f"  -> {extracted_dir}")

    print("Extraction complete.")

    total_images = sum(len(resources) for _, resources in doc_resources)
    tracker = ProgressTracker(total_images) if total_images else None

    for extracted_dir, resources in doc_resources:
        if not resources:
            continue
        if tracker is not None:
            tracker.start_document(extracted_dir.name, len(resources))
        for image_path in sorted(resources, key=lambda path: path.name):
            _, reused = generate_markdown_sidecar(image_path)
            if tracker is not None:
                message = tracker.finish_item(image_path.name, reused)
                print(message)

    if tracker is not None and tracker.total_images:
        print("Markdown generation complete.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
