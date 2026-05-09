"""Render a single variant on the fly.

Useful when a downstream training job wants to re-derive a variant deterministically
without storing the full ~535 h corpus on disk.

Usage::

    py -m augment_arrl.render_on_demand \\
        --chunk-id 230905_0000 --augment-seed 7 \\
        --out variant.wav

If ``--out -`` (stdout), the WAV bytes are written to stdout (binary).
"""

from __future__ import annotations

import argparse
import io
import json
import sys
from pathlib import Path

import soundfile as sf

if __package__ in (None, ""):
    sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
    from augment_arrl import config as cfg_mod  # type: ignore
    from augment_arrl.render import render_variant  # type: ignore
else:
    from . import config as cfg_mod
    from .render import render_variant


def _row_chunk_id(row: dict) -> str:
    p = Path(row["wav_path"])
    return f"{p.parent.parent.name}_{p.stem}"


def _resolve_chunk(chunk_id: str, source_root: Path, manifest_path: Path) -> tuple[Path, dict]:
    """Look up a chunk by ``{wpm_dir}_{wav_stem}`` id in the source manifest.

    For backwards convenience we also match the bare wav stem (returns the
    first hit, which is order-dependent — prefer fully qualified ids).
    """

    fallback: tuple[Path, dict] | None = None
    with manifest_path.open("r", encoding="utf-8") as fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            row = json.loads(line)
            cid = _row_chunk_id(row)
            stem = Path(row["wav_path"]).stem
            if cid == chunk_id or stem == chunk_id:
                p = Path(row["wav_path"])
                if not p.is_absolute():
                    # Prefer the wpm-qualified path under source_root.
                    candidate = source_root / p.parent.parent.name / "chunks" / p.name
                    if candidate.exists():
                        p = candidate
                    else:
                        matches = list(source_root.rglob(p.name))
                        if matches:
                            p = matches[0]
                if cid == chunk_id:
                    return p, row
                if fallback is None:
                    fallback = (p, row)
    if fallback is not None:
        return fallback
    raise SystemExit(f"chunk_id not found in source manifest: {chunk_id}")


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--chunk-id", required=True)
    p.add_argument("--augment-seed", type=int, required=True)
    p.add_argument("--source-root", default=str(cfg_mod.DEFAULT_SOURCE_CORPUS_ROOT))
    p.add_argument("--source-manifest", default=str(cfg_mod.DEFAULT_SOURCE_MANIFEST))
    p.add_argument("--out", default="-",
                   help="output WAV path; '-' for stdout (binary)")
    p.add_argument("--qrm-partner", default=None,
                   help="optional partner chunk_id for QRM mixing")
    args = p.parse_args(argv)

    src_path, row = _resolve_chunk(args.chunk_id, Path(args.source_root), Path(args.source_manifest))
    qrm_partner = None
    if args.qrm_partner:
        qrm_path, _ = _resolve_chunk(args.qrm_partner, Path(args.source_root), Path(args.source_manifest))
        qrm_partner = (args.qrm_partner, qrm_path)

    result = render_variant(
        src_path, chunk_id=args.chunk_id,
        augment_seed=args.augment_seed,
        qrm_partner=qrm_partner,
    )

    if args.out == "-":
        buf = io.BytesIO()
        sf.write(buf, result.audio, result.sr, format="WAV", subtype="PCM_16")
        sys.stdout.buffer.write(buf.getvalue())
    else:
        out = Path(args.out)
        out.parent.mkdir(parents=True, exist_ok=True)
        sf.write(str(out), result.audio, result.sr, subtype="PCM_16")
        # Side-channel: dump the chosen impairments to stderr for inspection.
        meta = result.manifest_dict(
            wav_path=str(out).replace("\\", "/"),
            src_wav=row["wav_path"], truth=row["text"], wpm=float(row["wpm"]),
        )
        print(json.dumps(meta, indent=2), file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
