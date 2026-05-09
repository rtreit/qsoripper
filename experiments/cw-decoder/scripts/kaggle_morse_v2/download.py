"""Download the Kaggle Morse Learning Machine Challenge v2 dataset.

Wraps `kaggle competitions download` so we have a single command + a stable
local layout:

    data/cw-samples/kaggle-morse-v2/
        archive.zip          (cached; safe to delete)
        wavs/
            cw001.wav
            cw002.wav
            ...
        SampleSubmission.csv
        train.csv            (if present)

Authentication: ~/.kaggle/kaggle.json must exist. See README.md.

Idempotent: re-running with the archive already extracted is a no-op.
"""

from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
import zipfile
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[4]
DEFAULT_DEST = REPO_ROOT / "data" / "cw-samples" / "kaggle-morse-v2"
COMPETITION = "morse-learning-machine-challenge-v2"


def _ensure_kaggle_cli() -> None:
    try:
        import kaggle  # noqa: F401
    except ImportError:
        print("ERROR: 'kaggle' package not installed. Run: pip install kaggle", file=sys.stderr)
        sys.exit(2)
    cred_path = Path(os.environ.get("KAGGLE_CONFIG_DIR", str(Path.home() / ".kaggle"))) / "kaggle.json"
    if not cred_path.exists():
        print(f"ERROR: Kaggle credentials not found at {cred_path}.", file=sys.stderr)
        print("See https://www.kaggle.com/settings/account to create a token.", file=sys.stderr)
        sys.exit(2)


def _already_extracted(dest: Path) -> bool:
    wavs = dest / "wavs"
    if not wavs.exists():
        return False
    n = sum(1 for _ in wavs.glob("cw*.wav"))
    return n >= 100


def main() -> None:
    p = argparse.ArgumentParser(description="Download Kaggle morse-v2 dataset.")
    p.add_argument("--dest", type=Path, default=DEFAULT_DEST)
    p.add_argument("--force", action="store_true",
                   help="Re-download even if data is already present.")
    args = p.parse_args()

    if not args.force and _already_extracted(args.dest):
        print(f"Already extracted at {args.dest}. Use --force to re-download.")
        return

    _ensure_kaggle_cli()
    args.dest.mkdir(parents=True, exist_ok=True)

    print(f"Downloading competition '{COMPETITION}' to {args.dest} ...")
    cmd = ["kaggle", "competitions", "download", "-c", COMPETITION, "-p", str(args.dest)]
    res = subprocess.run(cmd, check=False)
    if res.returncode != 0:
        print(
            "ERROR: kaggle CLI failed. Possible causes: not authenticated, did "
            "not accept competition rules, or network failure.",
            file=sys.stderr,
        )
        sys.exit(res.returncode)

    # Find the downloaded archive (kaggle CLI names it after the competition).
    archives = sorted(args.dest.glob("*.zip"))
    if not archives:
        print(f"ERROR: no .zip archive found in {args.dest}", file=sys.stderr)
        sys.exit(2)
    archive = archives[-1]
    wavs_dir = args.dest / "wavs"
    wavs_dir.mkdir(exist_ok=True)
    print(f"Extracting {archive.name} ...")
    with zipfile.ZipFile(archive) as z:
        for name in z.namelist():
            inner = Path(name)
            if inner.suffix.lower() == ".wav":
                target = wavs_dir / inner.name
                with z.open(name) as src, open(target, "wb") as dst:
                    shutil.copyfileobj(src, dst)
            elif inner.suffix.lower() in (".csv", ".txt"):
                target = args.dest / inner.name
                with z.open(name) as src, open(target, "wb") as dst:
                    shutil.copyfileobj(src, dst)
    n_wavs = sum(1 for _ in wavs_dir.glob("*.wav"))
    print(f"OK: extracted {n_wavs} WAVs to {wavs_dir}")
    print(f"Companion files (.csv/.txt) under {args.dest}")


if __name__ == "__main__":
    main()
