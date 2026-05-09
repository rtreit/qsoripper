r"""Kaggle morse-v2 bench harness.

Iterates the manifest, runs the CW decoder against each WAV, scores against
truth (when available), and writes a JSON report with aggregate statistics
plus per-bucket breakdowns.

Buckets:
  - Estimated WPM: <20, 20-30, 30-45, 45-60, >60
  - Estimated peak pitch (Hz): <700, 700-900, 900-1100, >1100

The decoder emits region trace events via `--emit-events stdout` (when
present); we sniff the trace's `pitch_hz` and `wpm` fields. If the trace is
unavailable we leave bucket fields null and aggregate at file level only.

Usage:
    python bench.py [--manifest <path>] [--exe <decoder>] [--out <json>]
                    [--label LABEL] [--limit N] [--split {train,test,both}]

Default manifest: ./manifest.jsonl
Default exe: experiments\cw-decoder\target\release\cw-decoder.exe
Default out: experiments\cw-decoder\kaggle_report.json
"""

from __future__ import annotations

import argparse
import json
import re
import statistics
import subprocess
import sys
import time
from collections import defaultdict
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parents[4]
DEFAULT_MANIFEST = Path(__file__).with_name("manifest.jsonl")
DEFAULT_EXE = REPO_ROOT / "experiments" / "cw-decoder" / "target" / "release" / "cw-decoder.exe"
DEFAULT_OUT = REPO_ROOT / "experiments" / "cw-decoder" / "kaggle_report.json"


def _normalize(s: str) -> str:
    s = (s or "").upper()
    s = re.sub(r"[^A-Z0-9]+", " ", s)
    return re.sub(r"\s+", " ", s).strip()


def _levenshtein(a: str, b: str) -> int:
    if len(a) < len(b):
        a, b = b, a
    prev = list(range(len(b) + 1))
    for i, ca in enumerate(a, 1):
        curr = [i]
        for j, cb in enumerate(b, 1):
            curr.append(min(prev[j] + 1, curr[-1] + 1, prev[j - 1] + (ca != cb)))
        prev = curr
    return prev[-1]


def _decode(exe: Path, wav: Path, timeout_s: int = 120) -> tuple[str, dict[str, Any], float]:
    """Returns (transcript, trace_summary, decode_seconds).

    `trace_summary` aggregates pitch_hz / wpm across emitted region trace events
    by selecting the median (robust to a stray warm-up sample).
    """
    t0 = time.perf_counter()
    p = subprocess.run(
        [str(exe), "stream-region", "--file", str(wav), "--json", "--no-realtime"],
        capture_output=True, text=True, timeout=timeout_s,
    )
    elapsed = time.perf_counter() - t0
    transcript = ""
    pitches: list[float] = []
    wpms: list[float] = []
    region_count = 0
    for line in p.stdout.splitlines():
        try:
            o = json.loads(line)
        except Exception:
            continue
        kind = o.get("type")
        if kind in ("transcript", "end") and o.get("transcript"):
            transcript = o["transcript"]
        # Region trace shape: {"type": "region", "pitch_hz": 712.5, "wpm": 22.4, ...}
        # If layer-trace (PR #414) emits enriched region records they will be
        # caught here as well.
        if kind in ("region", "region_trace") and isinstance(o, dict):
            region_count += 1
            ph = o.get("pitch_hz") or (o.get("pitch") or {}).get("hz")
            wpm = o.get("wpm") or (o.get("decoded") or {}).get("wpm")
            if isinstance(ph, (int, float)) and ph > 0:
                pitches.append(float(ph))
            if isinstance(wpm, (int, float)) and wpm > 0:
                wpms.append(float(wpm))
    summary: dict[str, Any] = {"region_count": region_count}
    if pitches:
        summary["pitch_hz_median"] = round(statistics.median(pitches), 2)
    if wpms:
        summary["wpm_median"] = round(statistics.median(wpms), 2)
    return transcript, summary, elapsed


def _wpm_bucket(wpm: float | None) -> str:
    if wpm is None:
        return "unknown"
    if wpm < 20: return "<20"
    if wpm < 30: return "20-30"
    if wpm < 45: return "30-45"
    if wpm < 60: return "45-60"
    return ">60"


def _pitch_bucket(hz: float | None) -> str:
    if hz is None:
        return "unknown"
    if hz < 700: return "<700"
    if hz < 900: return "700-900"
    if hz < 1100: return "900-1100"
    return ">1100"


def _summarize(rows: list[dict]) -> dict[str, Any]:
    if not rows:
        return {"n": 0}
    cers = [r["cer"] for r in rows if r.get("cer") is not None]
    wers = [r["wer"] for r in rows if r.get("wer") is not None]
    out: dict[str, Any] = {
        "n": len(rows),
        "n_scored": len(cers),
    }
    if cers:
        out.update({
            "mean_cer": round(statistics.mean(cers), 4),
            "median_cer": round(statistics.median(cers), 4),
            "p95_cer": round(sorted(cers)[max(0, int(len(cers) * 0.95) - 1)], 4),
            "exact_match_rate": round(sum(1 for c in cers if c == 0.0) / len(cers), 4),
        })
    if wers:
        out["mean_wer"] = round(statistics.mean(wers), 4)
    return out


def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    p.add_argument("--exe", type=Path, default=DEFAULT_EXE)
    p.add_argument("--out", type=Path, default=DEFAULT_OUT)
    p.add_argument("--label", default="baseline")
    p.add_argument("--limit", type=int, default=None,
                   help="Only run the first N rows (for smoke testing)")
    p.add_argument("--split", choices=("train", "test", "both"), default="train",
                   help="Which split to bench (default: train, the labeled rows).")
    p.add_argument("--timeout-s", type=int, default=120)
    args = p.parse_args()

    if not args.exe.exists():
        print(f"ERROR: decoder not found: {args.exe}", file=sys.stderr)
        sys.exit(2)
    if not args.manifest.exists():
        print(f"ERROR: manifest not found: {args.manifest}", file=sys.stderr)
        sys.exit(2)

    rows = [json.loads(line) for line in args.manifest.read_text(encoding="utf-8").splitlines() if line.strip()]
    if args.split != "both":
        rows = [r for r in rows if r.get("split") == args.split]
    if args.limit:
        rows = rows[: args.limit]
    if not rows:
        print(f"ERROR: no rows after filtering split={args.split}", file=sys.stderr)
        sys.exit(2)

    results: list[dict] = []
    by_wpm: dict[str, list[dict]] = defaultdict(list)
    by_pitch: dict[str, list[dict]] = defaultdict(list)

    print(f"Running {len(rows)} files (split={args.split}) through {args.exe.name} ...")
    for r in rows:
        wav = Path(r["wav"])
        try:
            hyp_raw, trace, decode_s = _decode(args.exe, wav, timeout_s=args.timeout_s)
        except subprocess.TimeoutExpired:
            hyp_raw, trace, decode_s = "", {"region_count": 0}, -1.0
        hyp = _normalize(hyp_raw)
        truth = r.get("truth")
        if truth is not None:
            cer = _levenshtein(truth, hyp) / max(1, len(truth)) if truth else (1.0 if hyp else 0.0)
            tw, hw = truth.split(), hyp.split()
            wer = _levenshtein(tw, hw) / max(1, len(tw)) if tw else (1.0 if hw else 0.0)
        else:
            cer = None
            wer = None
        wpm_med = trace.get("wpm_median")
        pitch_med = trace.get("pitch_hz_median")
        out_row = {
            "id": r["id"],
            "split": r.get("split"),
            "wav": r["wav"],
            "truth": truth,
            "hyp": hyp,
            "cer": round(cer, 4) if cer is not None else None,
            "wer": round(wer, 4) if wer is not None else None,
            "decode_s": round(decode_s, 3),
            "region_count": trace.get("region_count", 0),
            "wpm_median": wpm_med,
            "pitch_hz_median": pitch_med,
            "wpm_bucket": _wpm_bucket(wpm_med),
            "pitch_bucket": _pitch_bucket(pitch_med),
        }
        results.append(out_row)
        by_wpm[out_row["wpm_bucket"]].append(out_row)
        by_pitch[out_row["pitch_bucket"]].append(out_row)
        cer_str = f"{cer:.3f}" if cer is not None else "  -  "
        print(f"  {r['id']:6s} CER={cer_str} hyp={hyp[:48]!r}  "
              f"wpm~{wpm_med if wpm_med is not None else '?'} "
              f"pitch~{pitch_med if pitch_med is not None else '?'}")

    overall = _summarize(results)

    # Worst-10 (only meaningful if we have CER values).
    scored = [r for r in results if r.get("cer") is not None]
    worst10 = sorted(scored, key=lambda r: -r["cer"])[:10]

    report = {
        "label": args.label,
        "exe": str(args.exe),
        "manifest": str(args.manifest),
        "split": args.split,
        "n": len(results),
        "overall": overall,
        "by_wpm_bucket": {k: _summarize(v) for k, v in by_wpm.items()},
        "by_pitch_bucket": {k: _summarize(v) for k, v in by_pitch.items()},
        "worst10": [
            {"id": r["id"], "cer": r["cer"], "truth": r["truth"], "hyp": r["hyp"]}
            for r in worst10
        ],
        "results": results,
    }

    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(report, indent=2), encoding="utf-8")
    print(f"\nreport -> {args.out}")
    print(f"OVERALL n={overall.get('n_scored', 0)}/{overall['n']} "
          f"mean_CER={overall.get('mean_cer')} "
          f"median_CER={overall.get('median_cer')} "
          f"p95_CER={overall.get('p95_cer')} "
          f"exact={overall.get('exact_match_rate')}")
    if any(k != "unknown" for k in by_wpm):
        print("by WPM bucket:")
        for k, s in sorted(by_wpm.items()):
            sm = _summarize(s)
            print(f"  {k:>7s} n={sm['n']} mean_CER={sm.get('mean_cer')}")
    else:
        print("(WPM/pitch bucketing unavailable: decoder did not emit region "
              "trace fields. File-level CER is reported above.)")


if __name__ == "__main__":
    main()
