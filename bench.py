"""Shared benchmark harness for CW decoder experiments.

Decodes every *.mp3 in data/cw-samples/training-set-a/ that has a paired
*.truth.txt, runs the experiment's cw-decoder.exe (built in --release of
the worktree), and emits a JSON report with CER / WER / per-sample text.

Usage:
    python bench.py <worktree_root>

Writes results to: <worktree_root>/experiment_report.json
"""

from __future__ import annotations

import json
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


def decode(exe: Path, mp3: Path) -> str:
    p = subprocess.run(
        [str(exe), "stream-region", "--file", str(mp3), "--json", "--no-realtime"],
        capture_output=True, text=True, timeout=120,
    )
    text = ""
    for line in p.stdout.splitlines():
        try:
            o = json.loads(line)
        except Exception:
            continue
        if o.get("type") in ("transcript", "end") and o.get("transcript"):
            text = o["transcript"]
    return text


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


def main(worktree: Path) -> None:
    exe = worktree / "experiments" / "cw-decoder" / "target" / "release" / "cw-decoder.exe"
    if not exe.exists():
        print(f"ERROR: build first: {exe} not found", file=sys.stderr)
        sys.exit(2)
    out: dict = {"worktree": str(worktree), "samples": {}}
    samples = sorted(SAMPLES_DIR.glob("*.mp3"))
    for mp3 in samples:
        truth_path = mp3.with_suffix(".truth.txt")
        if not truth_path.exists():
            continue
        truth = truth_path.read_text(encoding="utf-8").strip()
        hyp = decode(exe, mp3)
        rec = score_sample(truth, hyp)
        out["samples"][mp3.stem] = rec
        print(f"{mp3.stem:32s} CER={rec['cer']:.3f} WER={rec['wer']:.3f} recall={rec['recall']:.2f}")
    cers = [s["cer"] for s in out["samples"].values()]
    wers = [s["wer"] for s in out["samples"].values()]
    out["mean_cer"] = round(sum(cers) / len(cers), 4) if cers else None
    out["mean_wer"] = round(sum(wers) / len(wers), 4) if wers else None
    print(f"\nMEAN CER={out['mean_cer']}  MEAN WER={out['mean_wer']}")
    (worktree / "experiment_report.json").write_text(json.dumps(out, indent=2))


if __name__ == "__main__":
    if len(sys.argv) != 2:
        print(__doc__)
        sys.exit(1)
    main(Path(sys.argv[1]))
