"""Build the Kaggle morse-v2 manifest.

Reads `SampleSubmission.csv` (and optional `train.csv` if present) from the
extracted dataset and emits a `manifest.jsonl` with one row per file:

    {"id": "cw001", "wav": "<abspath>", "truth": "...", "split": "train"}
    {"id": "cw101", "wav": "<abspath>", "truth": null, "split": "test"}

Convention:
- `train` split: row has a known truth label (graded locally).
- `test`  split: row's truth is unknown locally; produce a prediction
  for the leaderboard via `submit.py`.

The Kaggle competition publishes labels for ~half the files in
`SampleSubmission.csv` (the rows whose `Predicted` column is non-empty in
sample form, OR a separate `train.csv`). This script handles both layouts.
"""

from __future__ import annotations

import argparse
import csv
import json
import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[4]
DEFAULT_DATA = REPO_ROOT / "data" / "cw-samples" / "kaggle-morse-v2"
DEFAULT_OUT = Path(__file__).with_name("manifest.jsonl")


def _normalize_truth(s: str) -> str:
    s = (s or "").upper()
    s = re.sub(r"[^A-Z0-9]+", " ", s)
    return re.sub(r"\s+", " ", s).strip()


def _read_csv_labels(csv_path: Path) -> dict[str, str]:
    """Return id -> truth (uppercase, normalized). Empty/blank truths are skipped."""
    if not csv_path.exists():
        return {}
    out: dict[str, str] = {}
    with csv_path.open(newline="", encoding="utf-8") as f:
        reader = csv.DictReader(f)
        # Try common column name variants.
        for row in reader:
            id_field = next((k for k in row if k.lower() in ("id", "filename", "file")), None)
            truth_field = next(
                (k for k in row if k.lower() in ("predicted", "morse", "text", "label", "truth")),
                None,
            )
            if id_field is None or truth_field is None:
                continue
            raw_id = (row[id_field] or "").strip()
            if not raw_id:
                continue
            cw_id = Path(raw_id).stem  # accept "cw001.wav" or "cw001"
            truth = _normalize_truth(row[truth_field] or "")
            if truth:
                out[cw_id] = truth
    return out


def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("--data", type=Path, default=DEFAULT_DATA,
                   help=f"Extracted dataset root (default: {DEFAULT_DATA})")
    p.add_argument("--out", type=Path, default=DEFAULT_OUT)
    args = p.parse_args()

    wavs_dir = args.data / "wavs"
    if not wavs_dir.exists():
        print(f"ERROR: {wavs_dir} not found. Run download.py first.", file=sys.stderr)
        sys.exit(2)

    # Prefer train.csv if it exists; otherwise read SampleSubmission.csv (some
    # competitions seed labels there).
    labels: dict[str, str] = {}
    for cand in ("train.csv", "SampleSubmission.csv", "sample_submission.csv"):
        labels.update(_read_csv_labels(args.data / cand))
    if not labels:
        print(
            "WARN: no labeled rows found in train.csv / SampleSubmission.csv. "
            "All entries will be marked split='test'.",
            file=sys.stderr,
        )

    rows: list[dict] = []
    for wav in sorted(wavs_dir.glob("cw*.wav")):
        cw_id = wav.stem
        truth = labels.get(cw_id)
        rows.append({
            "id": cw_id,
            "wav": str(wav.resolve()),
            "truth": truth,
            "split": "train" if truth is not None else "test",
        })

    args.out.parent.mkdir(parents=True, exist_ok=True)
    with args.out.open("w", encoding="utf-8") as f:
        for r in rows:
            f.write(json.dumps(r) + "\n")

    n_train = sum(1 for r in rows if r["split"] == "train")
    n_test = sum(1 for r in rows if r["split"] == "test")
    print(f"manifest -> {args.out}")
    print(f"  train (labeled) : {n_train}")
    print(f"  test  (held-out): {n_test}")
    print(f"  total           : {len(rows)}")


if __name__ == "__main__":
    main()
