"""Shared benchmark harness for CW decoder experiments.

Decodes every *.mp3 in data/cw-samples/training-set-a/ that has a paired
*.truth.txt, runs the experiment's cw-decoder.exe (built in --release of
the worktree), and emits a JSON report with CER / WER / per-sample text.

Usage:
    python bench.py <worktree_root> [<label>] [<output_filename>]

Writes results to: <worktree_root>/<output_filename> (default
experiment_report.json). Set DITDAH_DISABLE_WPM_SEED=1 in the
environment to disable the warm-start WPM seed for diagnostic A/B runs;
the harness will record the resulting label/env state in the JSON.
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
from pathlib import Path


SAMPLES_DIR = Path(r"C:\Users\randy\Git\qsoripper\data\cw-samples\training-set-a")


def normalize(s: str) -> str:
    s = s.upper()
    s = re.sub(r"[^A-Z0-9]+", " ", s)
    return re.sub(r"\s+", " ", s).strip()


def levenshtein(a, b):
    if len(a) < len(b):
        a, b = b, a
    prev = list(range(len(b) + 1))
    for i, ca in enumerate(a, 1):
        curr = [i]
        for j, cb in enumerate(b, 1):
            curr.append(min(prev[j] + 1, curr[-1] + 1, prev[j - 1] + (ca != cb)))
        prev = curr
    return prev[-1]


def decode(exe: Path, mp3: Path):
    p = subprocess.run(
        [str(exe), "stream-region", "--file", str(mp3), "--json", "--no-realtime"],
        capture_output=True, text=True, timeout=120,
    )
    text = ""
    seed_wpm = None
    seed_pitch_hz = None
    wpm_seed_disabled = None
    for line in p.stdout.splitlines():
        try:
            o = json.loads(line)
        except Exception:
            continue
        if o.get("type") == "ready":
            seed_wpm = o.get("seed_wpm")
            seed_pitch_hz = o.get("seed_pitch_hz")
            wpm_seed_disabled = o.get("wpm_seed_disabled")
        if o.get("type") in ("transcript", "end") and o.get("transcript"):
            text = o["transcript"]
    return text, seed_wpm, seed_pitch_hz, wpm_seed_disabled


def score_sample(truth: str, hyp: str) -> dict:
    t = normalize(truth)
    h = normalize(hyp)
    cer = levenshtein(t, h) / max(1, len(t))
    tw = t.split()
    hw = h.split()
    wer = levenshtein(tw, hw) / max(1, len(tw))
    truth_set = set(tw)
    hyp_set = set(hw)
    matched = truth_set & hyp_set
    recall = len(matched) / max(1, len(truth_set))
    precision = len(matched) / max(1, len(hyp_set))
    return dict(
        truth=t, hyp=h, cer=round(cer, 4), wer=round(wer, 4),
        recall=round(recall, 4), precision=round(precision, 4),
        truth_words=len(tw), hyp_words=len(hw),
    )


def main(worktree: Path, label: str, out_name: str) -> None:
    exe = worktree / "experiments" / "cw-decoder" / "target" / "release" / "cw-decoder.exe"
    if not exe.exists():
        print(f"ERROR: build first: {exe} not found", file=sys.stderr)
        sys.exit(2)
    out: dict = {
        "worktree": str(worktree),
        "label": label,
        "env_DITDAH_DISABLE_WPM_SEED": os.environ.get("DITDAH_DISABLE_WPM_SEED"),
        "samples": {},
    }
    samples = sorted(SAMPLES_DIR.glob("*.mp3"))
    for mp3 in samples:
        truth_path = mp3.with_suffix(".truth.txt")
        if not truth_path.exists():
            continue
        truth = truth_path.read_text(encoding="utf-8").strip()
        hyp, seed_wpm, seed_pitch, seed_disabled = decode(exe, mp3)
        rec = score_sample(truth, hyp)
        rec["seed_wpm"] = seed_wpm
        rec["seed_pitch_hz"] = seed_pitch
        rec["wpm_seed_disabled_in_run"] = seed_disabled
        out["samples"][mp3.stem] = rec
        seed_str = "off" if seed_disabled else (f"{seed_wpm:.1f}" if seed_wpm else "none")
        print(
            f"{mp3.stem:32s} CER={rec['cer']:.3f} WER={rec['wer']:.3f} "
            f"recall={rec['recall']:.2f}  seed={seed_str}"
        )
    cers = [s["cer"] for s in out["samples"].values()]
    wers = [s["wer"] for s in out["samples"].values()]
    out["mean_cer"] = round(sum(cers) / len(cers), 4) if cers else None
    out["mean_wer"] = round(sum(wers) / len(wers), 4) if wers else None
    print(f"\n[{label}] MEAN CER={out['mean_cer']}  MEAN WER={out['mean_wer']}")
    (worktree / out_name).write_text(json.dumps(out, indent=2))


if __name__ == "__main__":
    if len(sys.argv) < 2:
        print(__doc__)
        sys.exit(1)
    wt = Path(sys.argv[1])
    lbl = sys.argv[2] if len(sys.argv) >= 3 else "default"
    name = sys.argv[3] if len(sys.argv) >= 4 else "experiment_report.json"
    main(wt, lbl, name)
