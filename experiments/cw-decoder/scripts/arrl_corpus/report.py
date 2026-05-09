"""Stage 5: emit ``quality_report.md`` from the manifest + trim sidecars."""

from __future__ import annotations

import argparse
import json
import random
import statistics
import sys
from collections import defaultdict
from pathlib import Path

from common import (
    CORPUS_ROOT,
    DEFAULT_SPEEDS,
    MANIFEST_PATH,
    QUALITY_REPORT_PATH,
    configure_logging,
    parse_speeds_arg,
    read_jsonl,
    speed_dirname,
)

DURATION_BUCKETS = (5, 10, 15, 30, 60, 90)


def load_trim_metadata(speeds: list[float]) -> list[dict]:
    metas: list[dict] = []
    for wpm in speeds:
        td = CORPUS_ROOT / speed_dirname(wpm) / "trimmed"
        if not td.exists():
            continue
        for js in sorted(td.glob("*.trim.json")):
            try:
                metas.append(json.loads(js.read_text(encoding="utf-8")))
            except Exception:
                continue
    return metas


def bucket_durations(durations: list[float]) -> list[tuple[str, int]]:
    edges = [0.0] + list(DURATION_BUCKETS) + [float("inf")]
    labels = [f"<={DURATION_BUCKETS[0]}s"] + \
             [f"{a}-{b}s" for a, b in zip(DURATION_BUCKETS[:-1], DURATION_BUCKETS[1:])] + \
             [f">{DURATION_BUCKETS[-1]}s"]
    counts = [0] * (len(edges) - 1)
    for d in durations:
        for i in range(len(edges) - 1):
            if edges[i] <= d < edges[i + 1]:
                counts[i] += 1
                break
    return list(zip(labels, counts))


def percentile(xs: list[float], p: float) -> float:
    if not xs:
        return float("nan")
    xs_sorted = sorted(xs)
    k = (len(xs_sorted) - 1) * (p / 100.0)
    f = int(k)
    c = min(f + 1, len(xs_sorted) - 1)
    if f == c:
        return xs_sorted[f]
    return xs_sorted[f] + (xs_sorted[c] - xs_sorted[f]) * (k - f)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--speeds", default=None)
    parser.add_argument("--spot-checks", type=int, default=8)
    parser.add_argument("--seed", type=int, default=42)
    parser.add_argument("--perf-json", default=None,
                        help="Path to JSON file with pipeline timing stats produced by run.py")
    args = parser.parse_args(argv)

    logger = configure_logging()
    speeds = parse_speeds_arg(args.speeds)
    rows = read_jsonl(MANIFEST_PATH)
    trim_meta = load_trim_metadata(speeds)

    perf: dict | None = None
    if args.perf_json:
        try:
            perf = json.loads(Path(args.perf_json).read_text(encoding="utf-8"))
        except Exception as exc:
            logger.warning(f"report: could not load perf json: {exc}")

    by_speed = defaultdict(list)
    for r in rows:
        by_speed[r["wpm"]].append(r)
    trim_by_speed = defaultdict(list)
    for m in trim_meta:
        trim_by_speed[m["wpm"]].append(m)

    lines: list[str] = []
    lines.append("# ARRL CW Corpus — Quality Report (Index-Driven Pipeline)")
    lines.append("")
    lines.append("Auto-generated from `data/cw-samples/arrl-archive/manifest.jsonl` and the "
                 "per-session `*.trim.json` sidecars by the index-driven parallel harvester.")
    lines.append("")

    # Pipeline performance section first, per spec.
    lines.append("## Pipeline Performance")
    lines.append("")
    if perf:
        wall = perf.get("wall_total_s", 0.0)
        lines.append(f"- **Total wall time:** {wall:.1f} s ({wall/60:.1f} min)")
        lines.append("")
        lines.append("| Stage | Duration (s) | Notes |")
        lines.append("|:------|------------:|:------|")
        for stg in perf.get("stages", []):
            lines.append(f"| {stg['name']} | {stg['seconds']:.1f} | {stg.get('detail', '')} |")
        lines.append("")
        if perf.get("session_count"):
            sc = perf["session_count"]
            chunk_count = perf.get("chunk_count", len(rows))
            lines.append(f"- Sessions in pilot: {sc}")
            lines.append(f"- Chunks produced: {chunk_count}")
            if perf.get("download_seconds"):
                lines.append(f"- Download throughput: {sc * 60.0 / max(perf['download_seconds'], 1e-3):.1f} files/min")
            if perf.get("align_seconds"):
                lines.append(f"- Alignment throughput: {sc * 60.0 / max(perf['align_seconds'], 1e-3):.1f} sessions/min")
        lines.append("")
    else:
        lines.append("_No performance JSON supplied (run via `run.py` to capture stage timings)._")
        lines.append("")

    total_chunks = len(rows)
    total_audio_s = sum(r["duration_s"] for r in rows)
    total_chars = sum(r["char_count"] for r in rows)
    lines.append("## Summary")
    lines.append("")
    lines.append(f"- Speeds covered: {sorted(by_speed.keys())}")
    lines.append(f"- Sessions trimmed: {len(trim_meta)}")
    lines.append(f"- Chunks retained: **{total_chunks}**")
    lines.append(f"- Total labeled audio: **{total_audio_s/3600:.2f} h** ({total_audio_s:.0f} s)")
    lines.append(f"- Total labeled characters: {total_chars:,}")
    lines.append("")

    lines.append("## Per-Speed Breakdown")
    lines.append("")
    lines.append("| WPM | Sessions trimmed | Trimmed audio (h) | Chunks | Audio kept (h) | Median align CER | p95 align CER |")
    lines.append("|----:|-----------------:|------------------:|-------:|---------------:|-----------------:|--------------:|")
    for wpm in sorted(set(list(by_speed.keys()) + list(trim_by_speed.keys()))):
        sessions = trim_by_speed.get(wpm, [])
        chunks = by_speed.get(wpm, [])
        trimmed_h = sum(s.get("trimmed_s", 0.0) for s in sessions) / 3600.0
        kept_h = sum(c["duration_s"] for c in chunks) / 3600.0
        scores = [c["alignment_score"] for c in chunks]
        med = statistics.median(scores) if scores else float("nan")
        p95 = percentile(scores, 95) if scores else float("nan")
        lines.append(
            f"| {wpm} | {len(sessions)} | {trimmed_h:.2f} | {len(chunks)} | {kept_h:.2f} | "
            f"{med:.4f} | {p95:.4f} |"
        )
    lines.append("")

    lines.append("## Chunk Duration Distribution")
    lines.append("")
    durations = [r["duration_s"] for r in rows]
    if durations:
        lines.append(f"- min={min(durations):.2f}s, median={statistics.median(durations):.2f}s, max={max(durations):.2f}s")
        lines.append("")
        lines.append("| Bucket | Count |")
        lines.append("|:-------|------:|")
        for label, count in bucket_durations(durations):
            lines.append(f"| {label} | {count} |")
    else:
        lines.append("_No chunks._")
    lines.append("")

    lines.append("## Per-Chunk Alignment Score Distribution")
    lines.append("")
    scores = [r["alignment_score"] for r in rows]
    if scores:
        lines.append(f"- median={statistics.median(scores):.4f}, p95={percentile(scores, 95):.4f}, max retained={max(scores):.4f}")
        lines.append(f"- (drop threshold: 0.05; chunks above were filtered out by `align_parallel.py`)")
    else:
        lines.append("_No chunks._")
    lines.append("")

    lines.append(f"## Spot Checks ({args.spot_checks} random chunks)")
    lines.append("")
    if rows:
        rng = random.Random(args.seed)
        sample = rng.sample(rows, k=min(args.spot_checks, len(rows)))
        lines.append("| WPM | Date | Duration | Align CER | First 80 chars of truth |")
        lines.append("|----:|:-----|---------:|----------:|:------------------------|")
        for r in sample:
            preview = r["text"][:80].replace("|", "/")
            lines.append(
                f"| {r['wpm']} | {r['date']} | {r['duration_s']:.1f}s | {r['alignment_score']:.4f} | `{preview}` |"
            )
    else:
        lines.append("_No chunks available for spot-check._")
    lines.append("")

    lines.append("## Trim / Intro Detection")
    lines.append("")
    if trim_meta:
        intro_yes = sum(1 for m in trim_meta if m.get("intro_stripped"))
        outro_yes = sum(1 for m in trim_meta if m.get("outro_stripped"))
        avg_intro = statistics.mean(
            [m["trim_start_s"] for m in trim_meta if m.get("intro_stripped")] or [0]
        )
        avg_outro = statistics.mean(
            [m["original_s"] - m["trim_end_s"] for m in trim_meta if m.get("outro_stripped")] or [0]
        )
        lines.append(f"- Intro stripped on {intro_yes}/{len(trim_meta)} files (avg {avg_intro:.1f} s removed)")
        lines.append(f"- Outro stripped on {outro_yes}/{len(trim_meta)} files (avg {avg_outro:.1f} s removed)")
    else:
        lines.append("_No trim metadata._")
    lines.append("")

    lines.append("## Known Limitations")
    lines.append("")
    lines.append("- **Studio-clean source.** ARRL bulletins are recorded directly from a code generator; "
                 "no QSB, no QRM, no fading, single oscillator pitch (~700 Hz). A model trained only on "
                 "this corpus will be brittle on real on-air audio. Use it as a pretraining seed and add "
                 "on-the-fly augmentation (additive noise, SNR randomization, pitch shifts ±300 Hz, "
                 "time-warp ±10 %, fading, QSB, key-clicks).")
    lines.append("- **Bulletin vocabulary.** Texts are pulled from QST articles, ARRL announcements and "
                 "callsign drills. Ham vocabulary is well represented; conversational English / contest "
                 "exchanges are not.")
    lines.append("- **One WPM per file.** Mixed-speed bursts (typical of real QSOs) are not represented.")
    lines.append("- **Uniform char-rate alignment.** We assume constant CW rate within a session; small "
                 "tempo wobble or operator pauses can shift chunk boundaries by 0.3–0.8 s. The drop "
                 "threshold (alignment CER > 0.05) catches the worst cases.")
    lines.append("- **Bi-weekly cadence.** The ARRL archive has been bi-weekly (every 2 weeks) since at "
                 "least 2014. The index parser pulls every available date in one HTTP request per speed, "
                 "eliminating the prior pipeline's blind date probing.")
    lines.append("")

    lines.append("## Recommended Use")
    lines.append("")
    lines.append("- **Pretraining only.** Use this corpus to bootstrap a CTC / transducer encoder.")
    lines.append("- **Always augment.** Suggested chain: random gain → additive band-limited noise to "
                 "reach SNR ∈ [-3, +20] dB → ±300 Hz pitch shift → random 5–10 % time stretch → "
                 "occasional QSB envelope (0.3–1.5 Hz). Keep the truth label unchanged.")
    lines.append("- **Validation set.** Reserve ≥ 1 full session per speed (hold out by date) so you "
                 "never evaluate on a chunk whose neighbours were trained on.")
    lines.append("- **Mix with augmented synthetic.** Combine 70 % ARRL chunks with 30 % synthetic CW "
                 "from `gen-rough-fist` / `gen-qso-suite` to add fist variability and prosigns.")
    lines.append("")

    lines.append("## Scaling Estimate")
    lines.append("")
    lines.append("ARRL archive coverage observed during pilot index build:")
    lines.append("")
    lines.append("- ~285 sessions per speed × 11 speeds available = ~3 100 candidate sessions.")
    lines.append("- Median trimmed length ~9 min ≈ ~470 hours of clean labeled CW at full coverage.")
    lines.append("- At pilot's chunks-per-session density, that's ~30 000–40 000 labeled chunks.")
    lines.append("- Storage at 8 kHz int16: ~17 GB raw WAV. MP3 source ~35 GB.")
    lines.append("")

    QUALITY_REPORT_PATH.write_text("\n".join(lines), encoding="utf-8")
    logger.info(f"report: wrote {QUALITY_REPORT_PATH}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
