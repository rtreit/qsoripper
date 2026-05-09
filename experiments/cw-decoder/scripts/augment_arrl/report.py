"""Render a quality + coverage report from the augmentation manifest.

Aggregates:
  - total variants
  - total hours of audio
  - per-impairment coverage (fraction of variants that include each)
  - SNR / Watterson profile distribution
  - decoder eval (CER vs SNR) if --eval is supplied

Output is plain markdown to stdout.
"""

from __future__ import annotations

import argparse
import json
import sys
from collections import Counter
from pathlib import Path

import numpy as np

if __package__ in (None, ""):
    sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
    from augment_arrl import config as cfg_mod  # type: ignore
else:
    from . import config as cfg_mod


def _read_jsonl(path: Path) -> list[dict]:
    rows: list[dict] = []
    if not path.exists():
        return rows
    with path.open("r", encoding="utf-8") as fh:
        for line in fh:
            line = line.strip()
            if line:
                rows.append(json.loads(line))
    return rows


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--manifest", default=str(cfg_mod.AUGMENTED_MANIFEST_PATH))
    p.add_argument("--eval", default=str(Path(cfg_mod.AUGMENTED_MANIFEST_PATH).parent / "augment_eval.jsonl"))
    args = p.parse_args(argv)

    rows = _read_jsonl(Path(args.manifest))
    if not rows:
        print(f"ERROR: no rows in {args.manifest}", file=sys.stderr)
        return 2

    total = len(rows)
    total_hours = sum(r["duration_s"] for r in rows) / 3600.0
    chunks = sorted({r["chunk_id"] for r in rows})

    print("# Augmentation manifest report\n")
    print(f"- variants: **{total}**")
    print(f"- unique source chunks: **{len(chunks)}**")
    print(f"- total audio hours: **{total_hours:.2f}**")
    print(f"- median variant duration: **{np.median([r['duration_s'] for r in rows]):.1f} s**")
    print()

    # Impairment coverage.
    counts: Counter[str] = Counter()
    for r in rows:
        for imp in r["applied"]:
            counts[imp] += 1
    print("## Impairment coverage\n")
    print("| impairment | variants | fraction |")
    print("|---|---:|---:|")
    for name, n in counts.most_common():
        print(f"| {name} | {n} | {n/total:.2%} |")
    print()

    # SNR distribution.
    snr_counts = Counter(round(float(r["snr_db"]), 1) for r in rows)
    print("## SNR distribution\n")
    print("| SNR (dB) | variants |")
    print("|---:|---:|")
    for snr, n in sorted(snr_counts.items()):
        print(f"| {snr} | {n} |")
    print()

    # Watterson profile distribution.
    wcounts = Counter(r["watterson_profile"] for r in rows)
    print("## Watterson profile distribution\n")
    print("| profile | variants |")
    print("|---|---:|")
    for k, n in sorted(wcounts.items(), key=lambda x: -x[1]):
        print(f"| {k} | {n} |")
    print()

    # Eval (CER vs SNR) if available.
    eval_rows = _read_jsonl(Path(args.eval))
    if eval_rows:
        print(f"## Decoder eval (n={len(eval_rows)})\n")
        print("| SNR (dB) | n | median CER | mean CER | p90 CER |")
        print("|---:|---:|---:|---:|---:|")
        by_snr: dict[float, list[float]] = {}
        for r in eval_rows:
            by_snr.setdefault(float(r["snr_db"]), []).append(r["cer"])
        for snr in sorted(by_snr):
            cers = np.array(by_snr[snr])
            print(
                f"| {snr:.1f} | {cers.size} | {np.median(cers):.3f} | "
                f"{np.mean(cers):.3f} | {np.percentile(cers, 90):.3f} |"
            )
        print()

        # CER by Watterson profile.
        by_w: dict[str, list[float]] = {}
        for r in eval_rows:
            by_w.setdefault(r["watterson_profile"], []).append(r["cer"])
        print("## CER by Watterson profile\n")
        print("| profile | n | median CER | mean CER |")
        print("|---|---:|---:|---:|")
        for k, cers in sorted(by_w.items(), key=lambda x: -len(x[1])):
            arr = np.array(cers)
            print(f"| {k} | {arr.size} | {np.median(arr):.3f} | {np.mean(arr):.3f} |")
        print()

    print("## Manifest schema (per row)\n")
    print("```")
    if rows:
        sample = rows[0]
        for k, v in sample.items():
            sv = type(v).__name__ if not isinstance(v, (int, float, str, bool, type(None))) else repr(v)[:60]
            print(f"{k!r}: {sv}")
    print("```")

    return 0


if __name__ == "__main__":
    sys.exit(main())
