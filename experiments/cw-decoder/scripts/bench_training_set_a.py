"""Run cw-decoder.exe stream-region against training-set-a and report per-file CER.

Usage:
    python bench_training_set_a.py [--exe <decoder>] [--label <name>]
"""
from __future__ import annotations

import argparse
import json
import re
import statistics
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[3]
DEFAULT_EXE = REPO / "experiments" / "cw-decoder" / "target" / "release" / "cw-decoder.exe"
DEFAULT_DIR = REPO / "data" / "cw-samples" / "training-set-a"


def _norm(s: str) -> str:
    return re.sub(r"\s+", " ", re.sub(r"[^A-Z0-9]+", " ", (s or "").upper())).strip()


def _lev(a: str, b: str) -> int:
    if len(a) < len(b):
        a, b = b, a
    prev = list(range(len(b) + 1))
    for i, ca in enumerate(a, 1):
        cur = [i]
        for j, cb in enumerate(b, 1):
            cur.append(min(prev[j] + 1, cur[-1] + 1, prev[j - 1] + (ca != cb)))
        prev = cur
    return prev[-1]


def _decode(exe: Path, mp3: Path, timeout: int = 240) -> str:
    p = subprocess.run(
        [str(exe), "stream-region", "--file", str(mp3), "--json", "--no-realtime"],
        capture_output=True, text=True, timeout=timeout,
    )
    out = ""
    for line in p.stdout.splitlines():
        try:
            o = json.loads(line)
        except Exception:
            continue
        if o.get("type") in ("transcript", "end") and o.get("transcript"):
            out = o["transcript"]
    return out


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--exe", type=Path, default=DEFAULT_EXE)
    ap.add_argument("--dir", type=Path, default=DEFAULT_DIR)
    ap.add_argument("--label", default="current-main")
    ap.add_argument("--out", type=Path, default=DEFAULT_DIR / "_benchmark_current.json")
    args = ap.parse_args()

    if not args.exe.exists():
        print(f"ERROR: decoder not found: {args.exe}", file=sys.stderr)
        return 2

    truths = sorted(args.dir.glob("*.truth.txt"))
    rows = []
    print(f"Benching {len(truths)} samples through {args.exe.name}\n")
    for tp in truths:
        stem = tp.name.removesuffix(".truth.txt")
        mp3 = args.dir / f"{stem}.mp3"
        if not mp3.exists():
            continue
        truth = _norm(tp.read_text(encoding="utf-8"))
        try:
            hyp_raw = _decode(args.exe, mp3)
        except subprocess.TimeoutExpired:
            hyp_raw = ""
        hyp = _norm(hyp_raw)
        cer = _lev(truth, hyp) / max(1, len(truth))
        tw, hw = truth.split(), hyp.split()
        wer = _lev(tw, hw) / max(1, len(tw))
        rows.append({"id": stem, "truth_len": len(truth), "hyp_len": len(hyp), "cer": round(cer, 4), "wer": round(wer, 4), "hyp": hyp[:80]})
        print(f"  {stem:30s} CER={cer:.3f} WER={wer:.3f} hyp_len={len(hyp):4d} '{hyp[:60]}'")

    cers = [r["cer"] for r in rows]
    wers = [r["wer"] for r in rows]
    summary = {
        "label": args.label,
        "n": len(rows),
        "mean_cer": round(statistics.mean(cers), 4),
        "median_cer": round(statistics.median(cers), 4),
        "mean_wer": round(statistics.mean(wers), 4),
        "rows": rows,
    }
    args.out.write_text(json.dumps(summary, indent=2), encoding="utf-8")
    print(f"\nLABEL={args.label}  n={len(rows)}  mean_CER={summary['mean_cer']}  median_CER={summary['median_cer']}  mean_WER={summary['mean_wer']}")
    print(f"report -> {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
