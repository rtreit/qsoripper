"""Batch driver for the ARRL augmentation pipeline.

Walks the source manifest (1,576 chunks) and, for each, generates K=30
deterministic variants. Uses a ProcessPoolExecutor to fan out across all
cores. Writes a JSONL manifest with one entry per variant.

Usage::

    py -m augment_arrl.run \\
        --source-manifest <path> \\
        --out-root data/cw-samples/arrl-augmented \\
        --variants 30 \\
        --workers 0 \\
        --limit 0 \\
        --sample-out arrl_augmented_sample_manifest.jsonl

Set ``--limit N`` to dry-run on the first N chunks. Set ``--sample-only N``
to render only N variants total (used for the validation suite).
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
from concurrent.futures import ProcessPoolExecutor, as_completed
from pathlib import Path

import numpy as np

# Allow `python run.py` and `python -m augment_arrl.run` both to work.
if __package__ in (None, ""):
    sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
    from augment_arrl import config as cfg_mod  # type: ignore
    from augment_arrl.render import render_variant, write_wav  # type: ignore
else:
    from . import config as cfg_mod
    from .render import render_variant, write_wav


def _read_jsonl(path: Path) -> list[dict]:
    rows: list[dict] = []
    with path.open("r", encoding="utf-8") as fh:
        for line in fh:
            line = line.strip()
            if line:
                rows.append(json.loads(line))
    return rows


def _chunk_id_for(row: dict) -> str:
    """Globally unique id: '{wpm_dir}_{wav_stem}'.

    The bare WAV stem is reused across WPM dirs (e.g. ``230905_0000`` exists
    in 15/20/25/30 wpm), so we prefix with the WPM bucket to keep both the
    output filename and the deterministic RNG seed distinct.
    """

    p = Path(row["wav_path"])
    wpm_dir = p.parent.parent.name  # e.g. "20wpm"
    return f"{wpm_dir}_{p.stem}"


def _resolve_src_path(row: dict, source_root: Path) -> Path:
    """Map a manifest entry's wav_path back to an absolute file."""

    p = Path(row["wav_path"])
    if p.is_absolute() and p.exists():
        return p
    # Direct match using the manifest path layout (``<wpm>/chunks/<file>``).
    candidate = source_root / p.parent.parent.name / "chunks" / p.name
    if candidate.exists():
        return candidate
    # Last-ditch: walk into source_root looking for the file basename.
    matches = list(source_root.rglob(p.name))
    if matches:
        return matches[0]
    raise FileNotFoundError(f"could not resolve source wav for manifest row: {row}")


def _job(args: tuple) -> list[dict]:
    (chunk_id, src_path_s, augment_seeds, out_dir_s, qrm_pool, truth, wpm, src_wav_rel) = args
    src_path = Path(src_path_s)
    out_dir = Path(out_dir_s)
    out_dir.mkdir(parents=True, exist_ok=True)
    rows: list[dict] = []
    for seed in augment_seeds:
        # Pick a deterministic QRM partner (different chunk) using the seed.
        rng = np.random.default_rng(int(seed) ^ 0xA5A5A5A5)
        partner = qrm_pool[int(rng.integers(0, len(qrm_pool)))] if qrm_pool else None
        partner_arg = None
        if partner is not None:
            partner_arg = (partner[0], Path(partner[1]))
        result = render_variant(
            src_path,
            chunk_id=chunk_id,
            augment_seed=int(seed),
            qrm_partner=partner_arg,
        )
        out_path = out_dir / f"{cfg_mod.variant_basename(chunk_id, int(seed))}.wav"
        write_wav(out_path, result.audio, result.sr)
        rows.append(
            result.manifest_dict(
                wav_path=str(out_path).replace("\\", "/"),
                src_wav=src_wav_rel,
                truth=truth,
                wpm=float(wpm),
            )
        )
    return rows


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--source-manifest", default=str(cfg_mod.DEFAULT_SOURCE_MANIFEST))
    p.add_argument("--source-root", default=str(cfg_mod.DEFAULT_SOURCE_CORPUS_ROOT))
    p.add_argument("--out-root", default=str(cfg_mod.AUGMENTED_ROOT))
    p.add_argument("--manifest", default=str(cfg_mod.AUGMENTED_MANIFEST_PATH))
    p.add_argument("--sample-manifest", default=str(cfg_mod.SAMPLE_MANIFEST_PATH))
    p.add_argument("--variants", type=int, default=cfg_mod.AUG_VARIANTS_PER_CHUNK)
    p.add_argument("--workers", type=int, default=0, help="0 => os.cpu_count()")
    p.add_argument("--limit", type=int, default=0, help="if >0, only first N chunks")
    p.add_argument("--sample-only", type=int, default=0,
                   help="render only N variants total (validation mode)")
    p.add_argument("--sample-lines", type=int, default=100,
                   help="how many lines from manifest to copy into sample manifest")
    p.add_argument("--seed-stride", type=int, default=1,
                   help="if >1, only generate variants whose seed is a multiple of stride")
    args = p.parse_args(argv)

    source_root = Path(args.source_root)
    manifest = _read_jsonl(Path(args.source_manifest))
    if not manifest:
        print(f"ERROR: source manifest empty: {args.source_manifest}", file=sys.stderr)
        return 2
    if args.limit > 0:
        manifest = manifest[: args.limit]

    # Build the QRM partner pool (chunk_id, src_path) pairs once.
    qrm_pool: list[tuple[str, str]] = []
    for row in manifest:
        try:
            qrm_pool.append((_chunk_id_for(row), str(_resolve_src_path(row, source_root))))
        except FileNotFoundError:
            continue

    out_root = Path(args.out_root)
    out_root.mkdir(parents=True, exist_ok=True)

    seeds = list(range(0, args.variants, max(1, args.seed_stride)))

    # Build per-chunk job tuples. Each job renders all K variants for one
    # chunk (good batching: source WAV is loaded once per chunk).
    jobs: list[tuple] = []
    total_variants = 0
    for row in manifest:
        chunk_id = _chunk_id_for(row)
        try:
            src_path = _resolve_src_path(row, source_root)
        except FileNotFoundError as exc:
            print(f"warn: {exc}", file=sys.stderr)
            continue
        wpm_dir = src_path.parent.parent.name  # e.g. 20wpm
        out_dir = out_root / wpm_dir
        # If --sample-only, cap the variant count for this chunk.
        chunk_seeds = seeds
        if args.sample_only > 0 and total_variants + len(seeds) > args.sample_only:
            chunk_seeds = seeds[: args.sample_only - total_variants]
            if not chunk_seeds:
                break
        jobs.append(
            (
                chunk_id, str(src_path), tuple(chunk_seeds), str(out_dir),
                tuple(qrm_pool[: 64]),  # bound the per-job pool we ship to workers
                row["text"], float(row["wpm"]),
                row["wav_path"],
            )
        )
        total_variants += len(chunk_seeds)
        if args.sample_only > 0 and total_variants >= args.sample_only:
            break

    workers = args.workers if args.workers > 0 else os.cpu_count() or 4
    print(
        f"Augmenting: {len(jobs)} chunks × ~{len(seeds)} variants "
        f"= {total_variants} total variants on {workers} workers"
    )
    print(f"Output: {out_root}")
    print(f"Manifest: {args.manifest}")

    Path(args.manifest).parent.mkdir(parents=True, exist_ok=True)
    t0 = time.time()
    n_done = 0
    n_variants_done = 0
    out_hours = 0.0
    with open(args.manifest, "w", encoding="utf-8") as out_fh:
        if workers == 1:
            iterator = (_job(j) for j in jobs)
        else:
            ex = ProcessPoolExecutor(max_workers=workers)
            futures = [ex.submit(_job, j) for j in jobs]
            iterator = (f.result() for f in as_completed(futures))
        for rows in iterator:
            for r in rows:
                out_fh.write(json.dumps(r, ensure_ascii=False) + "\n")
                out_hours += r["duration_s"] / 3600.0
            n_done += 1
            n_variants_done += len(rows)
            if n_done % max(1, len(jobs) // 50) == 0:
                rate = n_variants_done / max(1e-3, time.time() - t0)
                print(
                    f"  {n_done}/{len(jobs)} chunks ({n_variants_done} variants, "
                    f"{rate:.1f} v/s, {out_hours:.2f} h written)",
                    flush=True,
                )
        if workers != 1:
            ex.shutdown(wait=True)

    elapsed = time.time() - t0
    print(
        f"Done: {n_variants_done} variants in {elapsed:.1f}s "
        f"({n_variants_done / max(1e-3, elapsed):.1f} v/s, {out_hours:.2f} h audio)"
    )

    # Sample manifest = first N lines of the full manifest, deterministic.
    sample_n = max(0, int(args.sample_lines))
    if sample_n > 0:
        with open(args.manifest, "r", encoding="utf-8") as fh:
            sample = [next(fh) for _ in range(min(sample_n, n_variants_done))]
        Path(args.sample_manifest).parent.mkdir(parents=True, exist_ok=True)
        with open(args.sample_manifest, "w", encoding="utf-8") as fh:
            fh.writelines(sample)
        print(f"Sample manifest: {len(sample)} lines -> {args.sample_manifest}")

    return 0


if __name__ == "__main__":
    sys.exit(main())
