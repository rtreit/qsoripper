r"""Adversarial bench harness for CW decoder.

Iterates the adversarial manifest, runs the decoder against each WAV, and
computes character-level (CER/WER) plus element-level (dit/dah recall &
precision) metrics. Suite-specific metrics include first-N-char survival
(for weak-prefix) and ghost-chars-per-second-of-silence (for
mid-region-collapse and noise-only).

Usage:
    python bench_adversarial.py --exe path\to\cw-decoder.exe \
        [--manifest scripts\adversarial_manifest.jsonl] \
        [--out adversarial_report.json] [--suite NAME]+ [--limit N]

If --exe is omitted, defaults to the local worktree's release binary.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import time
from collections import defaultdict
from pathlib import Path
from statistics import mean

ROOT = Path(__file__).resolve().parents[3]  # ...\adversarial-suite
DEFAULT_EXE = ROOT / "experiments" / "cw-decoder" / "target" / "release" / "cw-decoder.exe"
DEFAULT_MANIFEST = Path(__file__).with_name("adversarial_manifest.jsonl")
DEFAULT_OUT = ROOT / "experiments" / "cw-decoder" / "adversarial_report.json"


MORSE = {
    "A": ".-", "B": "-...", "C": "-.-.", "D": "-..", "E": ".", "F": "..-.",
    "G": "--.", "H": "....", "I": "..", "J": ".---", "K": "-.-", "L": ".-..",
    "M": "--", "N": "-.", "O": "---", "P": ".--.", "Q": "--.-", "R": ".-.",
    "S": "...", "T": "-", "U": "..-", "V": "...-", "W": ".--", "X": "-..-",
    "Y": "-.--", "Z": "--..",
    "0": "-----", "1": ".----", "2": "..---", "3": "...--", "4": "....-",
    "5": ".....", "6": "-....", "7": "--...", "8": "---..", "9": "----.",
    "/": "-..-.", "=": "-...-", ".": ".-.-.-", ",": "--..--", "?": "..--..",
}


def normalize(s: str) -> str:
    s = s.upper()
    s = re.sub(r"[^A-Z0-9]+", " ", s)
    return re.sub(r"\s+", " ", s).strip()


def text_to_elements(text: str) -> str:
    """Convert text to a single dit/dah string (no gaps), used for elemental
    alignment. E.g. 'OK' -> '----.-'."""
    out = []
    for ch in text.upper():
        sym = MORSE.get(ch)
        if sym:
            out.append(sym)
    return "".join(out)


def levenshtein(a, b) -> int:
    if len(a) < len(b):
        a, b = b, a
    prev = list(range(len(b) + 1))
    for i, ca in enumerate(a, 1):
        curr = [i]
        for j, cb in enumerate(b, 1):
            curr.append(min(prev[j] + 1, curr[-1] + 1, prev[j - 1] + (ca != cb)))
        prev = curr
    return prev[-1]


def needleman_wunsch_match(a: str, b: str) -> tuple[int, int, int]:
    """Global alignment match counts: returns (matches, a_len, b_len).

    Match=+1, mismatch=-1, gap=-1.
    """
    if not a or not b:
        return 0, len(a), len(b)
    n, m = len(a), len(b)
    # DP — small alphabets and short strings (CW elements per sample << 1000)
    score = [[0] * (m + 1) for _ in range(n + 1)]
    matches = [[0] * (m + 1) for _ in range(n + 1)]
    for i in range(1, n + 1):
        score[i][0] = -i
    for j in range(1, m + 1):
        score[0][j] = -j
    for i in range(1, n + 1):
        ai = a[i - 1]
        for j in range(1, m + 1):
            bj = b[j - 1]
            diag = score[i - 1][j - 1] + (1 if ai == bj else -1)
            up = score[i - 1][j] - 1
            left = score[i][j - 1] - 1
            best = diag
            src = "d"
            if up > best:
                best, src = up, "u"
            if left > best:
                best, src = left, "l"
            score[i][j] = best
            if src == "d":
                matches[i][j] = matches[i - 1][j - 1] + (1 if ai == bj else 0)
            elif src == "u":
                matches[i][j] = matches[i - 1][j]
            else:
                matches[i][j] = matches[i][j - 1]
    return matches[n][m], n, m


def decode(exe: Path, wav: Path, timeout_s: int = 120) -> tuple[str, float]:
    t0 = time.perf_counter()
    p = subprocess.run(
        [str(exe), "stream-region", "--file", str(wav), "--json", "--no-realtime"],
        capture_output=True, text=True, timeout=timeout_s,
    )
    elapsed = time.perf_counter() - t0
    text = ""
    for line in p.stdout.splitlines():
        try:
            o = json.loads(line)
        except Exception:
            continue
        if o.get("type") in ("transcript", "end") and o.get("transcript"):
            text = o["transcript"]
    return text, elapsed


def first_n_chars_survival(truth: str, hyp: str) -> dict[str, bool]:
    """Did the first 1/2/3/5 characters of the truth survive in the
    hypothesis? Survival = the first-N substring of truth appears as a
    contiguous substring of the hypothesis."""
    out: dict[str, bool] = {}
    truth_compact = re.sub(r"\s+", "", truth)
    hyp_compact = re.sub(r"\s+", "", hyp)
    for n in (1, 2, 3, 5):
        if len(truth_compact) < n:
            out[f"first_{n}"] = False
            continue
        prefix = truth_compact[:n]
        out[f"first_{n}"] = prefix in hyp_compact
    return out


def score(row: dict, hyp_raw: str, decode_s: float) -> dict:
    truth = normalize(row["truth"])
    hyp = normalize(hyp_raw)

    cer = levenshtein(truth, hyp) / max(1, len(truth)) if truth else (1.0 if hyp else 0.0)
    tw, hw = truth.split(), hyp.split()
    wer = levenshtein(tw, hw) / max(1, len(tw)) if tw else (1.0 if hw else 0.0)

    # Element-level alignment.
    truth_elems = text_to_elements(truth)
    hyp_elems = text_to_elements(hyp)
    matches, t_n, h_n = needleman_wunsch_match(truth_elems, hyp_elems)
    elem_recall = matches / t_n if t_n else (0.0 if h_n else 1.0)
    elem_precision = matches / h_n if h_n else (0.0 if t_n else 1.0)

    out = dict(
        suite=row["suite"], id=row["id"], truth=truth, hyp=hyp,
        cer=round(cer, 4), wer=round(wer, 4),
        elem_recall=round(elem_recall, 4), elem_precision=round(elem_precision, 4),
        elem_truth_len=t_n, elem_hyp_len=h_n,
        truth_words=len(tw), hyp_words=len(hw),
        decode_s=round(decode_s, 3),
    )

    # Suite-specific metrics.
    if row["suite"] == "weak-prefix":
        out["prefix_survival"] = first_n_chars_survival(truth, hyp)
    if row["suite"] == "noise-only":
        # Truth is empty; ghost rate per second.
        ghost_chars = len(re.sub(r"\s+", "", hyp))
        dur = float(row.get("duration_s") or 1.0)
        out["ghost_chars"] = ghost_chars
        out["ghost_chars_per_s"] = round(ghost_chars / dur, 4)
        out["empty_correct"] = ghost_chars == 0
    if row["suite"] == "mid-region-collapse":
        # Heuristic: extra chars vs truth attributable to the silence window.
        meta = row.get("meta") or {}
        silence_s = float(meta.get("silence_window_s") or 2.0)
        # Phantom = chars in hyp beyond truth length attributed to silence.
        extra = max(0, len(hyp.replace(" ", "")) - len(truth.replace(" ", "")))
        out["silence_window_s"] = silence_s
        out["extra_chars_vs_truth"] = extra
        out["extra_chars_per_silence_s"] = round(extra / silence_s, 4)

    return out


def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("--exe", type=Path, default=DEFAULT_EXE)
    p.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    p.add_argument("--out", type=Path, default=DEFAULT_OUT)
    p.add_argument("--suite", action="append", default=None)
    p.add_argument("--limit", type=int, default=None,
                   help="Only run first N rows per suite (for smoke testing)")
    p.add_argument("--label", default="baseline",
                   help="Free-form label for this run, recorded in the report")
    args = p.parse_args()

    if not args.exe.exists():
        print(f"ERROR: decoder exe not found: {args.exe}", file=sys.stderr)
        sys.exit(2)
    if not args.manifest.exists():
        print(f"ERROR: manifest not found: {args.manifest}", file=sys.stderr)
        sys.exit(2)

    rows = [json.loads(line) for line in args.manifest.read_text(encoding="utf-8").splitlines() if line.strip()]
    by_suite: dict[str, list[dict]] = defaultdict(list)
    for r in rows:
        by_suite[r["suite"]].append(r)

    results: list[dict] = []
    suite_summaries: dict[str, dict] = {}

    suite_names = list(by_suite.keys()) if not args.suite else args.suite
    for sname in suite_names:
        srows = by_suite.get(sname) or []
        if args.limit:
            srows = srows[: args.limit]
        if not srows:
            continue
        per_suite: list[dict] = []
        for r in srows:
            wav = Path(r["wav"])
            if not wav.is_absolute():
                wav = ROOT / wav
            try:
                hyp, ds = decode(args.exe, wav)
            except subprocess.TimeoutExpired:
                hyp, ds = "", -1.0
            sc = score(r, hyp, ds)
            per_suite.append(sc)
            results.append(sc)
            print(f"  {sc['id']:32s} CER={sc['cer']:.3f} WER={sc['wer']:.3f} "
                  f"el_R={sc['elem_recall']:.2f} el_P={sc['elem_precision']:.2f} "
                  f"hyp={sc['hyp'][:40]!r}")
        # Suite summary
        cers = [s["cer"] for s in per_suite]
        wers = [s["wer"] for s in per_suite]
        elr = [s["elem_recall"] for s in per_suite]
        elp = [s["elem_precision"] for s in per_suite]
        summary = dict(
            n=len(per_suite),
            mean_cer=round(mean(cers), 4),
            mean_wer=round(mean(wers), 4),
            mean_elem_recall=round(mean(elr), 4),
            mean_elem_precision=round(mean(elp), 4),
        )
        if sname == "weak-prefix":
            for k in ("first_1", "first_2", "first_3", "first_5"):
                survived = sum(1 for s in per_suite if s.get("prefix_survival", {}).get(k))
                summary[f"{k}_survival_rate"] = round(survived / len(per_suite), 4)
        if sname == "noise-only":
            ghosts = [s.get("ghost_chars_per_s", 0.0) for s in per_suite]
            empties = sum(1 for s in per_suite if s.get("empty_correct"))
            summary["mean_ghost_chars_per_s"] = round(mean(ghosts), 4)
            summary["max_ghost_chars_per_s"] = round(max(ghosts), 4)
            summary["fully_empty_rate"] = round(empties / len(per_suite), 4)
        if sname == "mid-region-collapse":
            extras = [s.get("extra_chars_per_silence_s", 0.0) for s in per_suite]
            summary["mean_extra_chars_per_silence_s"] = round(mean(extras), 4)
            summary["max_extra_chars_per_silence_s"] = round(max(extras), 4)
        suite_summaries[sname] = summary
        print(f"[{sname}] n={summary['n']} CER={summary['mean_cer']} WER={summary['mean_wer']} "
              f"el_R={summary['mean_elem_recall']} el_P={summary['mean_elem_precision']}")

    overall = {
        "label": args.label,
        "exe": str(args.exe),
        "n_rows": len(results),
        "mean_cer": round(mean([r["cer"] for r in results]), 4) if results else None,
        "mean_wer": round(mean([r["wer"] for r in results]), 4) if results else None,
        "mean_elem_recall": round(mean([r["elem_recall"] for r in results]), 4) if results else None,
        "mean_elem_precision": round(mean([r["elem_precision"] for r in results]), 4) if results else None,
        "suites": suite_summaries,
        "results": results,
    }

    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(overall, indent=2), encoding="utf-8")
    print(f"\nreport -> {args.out}")
    print(f"OVERALL CER={overall['mean_cer']} WER={overall['mean_wer']} "
          f"el_R={overall['mean_elem_recall']} el_P={overall['mean_elem_precision']}")


if __name__ == "__main__":
    main()
