"""Generate a Kaggle morse-v2 submission CSV from the held-out split.

Reads `manifest.jsonl`, runs the decoder on every row whose `split == 'test'`,
and writes a `submission.csv` shaped like the competition's `SampleSubmission.csv`:

    id,Predicted
    cw101,CQ DE W1AW K
    cw102,599 NJ
    ...

You then upload submission.csv at the competition page to score the
leaderboard.
"""

from __future__ import annotations

import argparse
import csv
import json
import re
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[4]
DEFAULT_MANIFEST = Path(__file__).with_name("manifest.jsonl")
DEFAULT_EXE = REPO_ROOT / "experiments" / "cw-decoder" / "target" / "release" / "cw-decoder.exe"
DEFAULT_OUT = Path(__file__).with_name("submission.csv")


def _normalize(s: str) -> str:
    s = (s or "").upper()
    s = re.sub(r"[^A-Z0-9]+", " ", s)
    return re.sub(r"\s+", " ", s).strip()


def _decode(exe: Path, wav: Path, timeout_s: int = 120) -> str:
    p = subprocess.run(
        [str(exe), "stream-region", "--file", str(wav), "--json", "--no-realtime"],
        capture_output=True, text=True, timeout=timeout_s,
    )
    transcript = ""
    for line in p.stdout.splitlines():
        try:
            o = json.loads(line)
        except Exception:
            continue
        if o.get("type") in ("transcript", "end") and o.get("transcript"):
            transcript = o["transcript"]
    return transcript


def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    p.add_argument("--exe", type=Path, default=DEFAULT_EXE)
    p.add_argument("--out", type=Path, default=DEFAULT_OUT)
    p.add_argument("--include-train", action="store_true",
                   help="Also include labeled rows in the submission (some "
                        "competitions require predictions for ALL files).")
    p.add_argument("--timeout-s", type=int, default=120)
    args = p.parse_args()

    if not args.exe.exists():
        print(f"ERROR: decoder not found: {args.exe}", file=sys.stderr)
        sys.exit(2)
    if not args.manifest.exists():
        print(f"ERROR: manifest not found: {args.manifest}", file=sys.stderr)
        sys.exit(2)

    rows = [json.loads(line) for line in args.manifest.read_text(encoding="utf-8").splitlines() if line.strip()]
    target = [r for r in rows if (args.include_train or r.get("split") == "test")]
    if not target:
        print("ERROR: nothing to predict (manifest has no 'test' rows).", file=sys.stderr)
        sys.exit(2)

    print(f"Predicting {len(target)} files ...")
    preds: list[tuple[str, str]] = []
    for r in target:
        wav = Path(r["wav"])
        try:
            hyp_raw = _decode(args.exe, wav, timeout_s=args.timeout_s)
        except subprocess.TimeoutExpired:
            hyp_raw = ""
        hyp = _normalize(hyp_raw)
        preds.append((r["id"], hyp))
        print(f"  {r['id']:6s} -> {hyp[:60]!r}")

    args.out.parent.mkdir(parents=True, exist_ok=True)
    with args.out.open("w", newline="", encoding="utf-8") as f:
        w = csv.writer(f)
        w.writerow(["id", "Predicted"])
        for cw_id, hyp in preds:
            w.writerow([cw_id, hyp])
    print(f"\nsubmission -> {args.out} ({len(preds)} rows)")
    print("Upload via:")
    print(f"  kaggle competitions submit -c morse-learning-machine-challenge-v2 "
          f"-f {args.out} -m \"qsoripper baseline\"")


if __name__ == "__main__":
    main()
