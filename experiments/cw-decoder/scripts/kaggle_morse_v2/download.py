"""Download the Kaggle Morse Learning Machine Challenge v2 dataset.

Two auth paths are supported:

1. Bearer token via env var (recommended, .env-friendly):
       KAGGLE_API_TOKEN=KGAT....   (also reads .env at repo root)
   Uses the public Kaggle REST API directly. No `kaggle` CLI required.

2. Classic kaggle.json: ~/.kaggle/kaggle.json with username + key.
   Falls back to invoking the `kaggle` CLI.

Local layout:

    data/cw-samples/kaggle-morse-v2/
        archive.zip          (cached; safe to delete)
        wavs/
            cw001.wav
            cw002.wav
            ...
        SampleSubmission.csv

Idempotent: re-running with the archive already extracted is a no-op.

NOTE: Before files can be downloaded, the Kaggle account must accept the
competition rules **once** at:
  https://www.kaggle.com/competitions/morse-learning-machine-challenge-v2/rules
This is a one-click step that cannot be done via the API.
"""

from __future__ import annotations

import argparse
import io
import os
import shutil
import subprocess
import sys
import urllib.request
import zipfile
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[4]
DEFAULT_DEST = REPO_ROOT / "data" / "cw-samples" / "kaggle-morse-v2"
COMPETITION = "morse-learning-machine-challenge-v2"
KAGGLE_API = "https://www.kaggle.com/api/v1"


def _load_env_file(env_path: Path) -> None:
    """Lightweight .env loader (no python-dotenv dep). Adds keys to os.environ
    if they are not already set."""
    if not env_path.exists():
        return
    for raw in env_path.read_text(encoding="utf-8", errors="replace").splitlines():
        line = raw.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        k, v = line.split("=", 1)
        k, v = k.strip(), v.strip().strip('"').strip("'")
        if k and k not in os.environ:
            os.environ[k] = v


def _get_bearer_token() -> str | None:
    _load_env_file(REPO_ROOT / ".env")
    return os.environ.get("KAGGLE_API_TOKEN")


def _bearer_get(url: str, token: str, *, timeout: int = 600) -> bytes:
    headers = {"Authorization": f"Bearer {token}"}
    # Prefer `requests` (bundles certifi, avoids platform CA-bundle quirks).
    try:
        import requests  # type: ignore
        r = requests.get(url, headers=headers, timeout=timeout)
        if r.status_code != 200:
            err = urllib.error.HTTPError(url, r.status_code, r.reason, hdrs=None, fp=None)
            raise err
        return r.content
    except ImportError:
        pass
    req = urllib.request.Request(url, headers=headers)
    with urllib.request.urlopen(req, timeout=timeout) as resp:  # noqa: S310
        return resp.read()


def _try_classic_cli() -> bool:
    try:
        import kaggle  # noqa: F401, PLC0415
        return True
    except Exception:  # noqa: BLE001
        return False


def _already_extracted(dest: Path) -> bool:
    wavs = dest / "wavs"
    if not wavs.exists():
        return False
    return sum(1 for _ in wavs.glob("cw*.wav")) >= 100


def _extract_archive(archive: Path, dest: Path) -> int:
    wavs_dir = dest / "wavs"
    wavs_dir.mkdir(exist_ok=True)
    with zipfile.ZipFile(archive) as z:
        for name in z.namelist():
            inner = Path(name)
            if inner.suffix.lower() == ".wav":
                target = wavs_dir / inner.name
                with z.open(name) as src, open(target, "wb") as dst:
                    shutil.copyfileobj(src, dst)
            elif inner.suffix.lower() in (".csv", ".txt", ".py", ".m"):
                target = dest / inner.name
                with z.open(name) as src, open(target, "wb") as dst:
                    shutil.copyfileobj(src, dst)
    return sum(1 for _ in wavs_dir.glob("*.wav"))


def _download_via_bearer(dest: Path, token: str) -> None:
    archive = dest / "archive.zip"
    print(f"Downloading via Kaggle REST (Bearer token, {len(token)} chars) ...")
    url = f"{KAGGLE_API}/competitions/data/download-all/{COMPETITION}"
    try:
        data = _bearer_get(url, token)
    except urllib.error.HTTPError as e:  # type: ignore[attr-defined]
        if e.code == 403:
            print(
                "\nERROR: Kaggle returned 403 Forbidden.\n"
                "This means the account is authenticated but has not yet "
                "accepted the competition rules.\n\n"
                "FIX (one-time, browser-only):\n"
                f"  1. Visit https://www.kaggle.com/competitions/{COMPETITION}/rules\n"
                "  2. Click 'I Understand and Accept'.\n"
                "  3. Re-run this script.\n",
                file=sys.stderr,
            )
        else:
            print(f"\nERROR: HTTP {e.code} {e.reason}", file=sys.stderr)
        sys.exit(2)
    archive.write_bytes(data)
    print(f"  -> {archive} ({len(data) / 1024 / 1024:.1f} MB)")

    # Some competitions wrap each file in its own .zip inside the outer zip
    # (audio.zip nested inside the all-files archive). Extract recursively.
    print("Extracting (with nested zip handling) ...")
    n_wavs = _extract_archive(archive, dest)
    # Look for nested zips that contain WAVs.
    for inner_zip in list(dest.glob("*.zip")):
        if inner_zip.name == "archive.zip":
            continue
        try:
            n_wavs += _extract_archive(inner_zip, dest)
        except zipfile.BadZipFile:
            continue
    # Also check inside the outer archive for nested ZIPs the first pass missed
    # (some Kaggle bundles store audio.zip itself rather than the WAVs).
    with zipfile.ZipFile(archive) as z:
        for name in z.namelist():
            if name.lower().endswith(".zip"):
                with z.open(name) as src:
                    buf = io.BytesIO(src.read())
                with zipfile.ZipFile(buf) as inner:
                    for member in inner.namelist():
                        ip = Path(member)
                        if ip.suffix.lower() == ".wav":
                            target = (dest / "wavs") / ip.name
                            target.parent.mkdir(exist_ok=True)
                            with inner.open(member) as src2, open(target, "wb") as dst:
                                shutil.copyfileobj(src2, dst)
                                n_wavs += 1
    print(f"OK: extracted {n_wavs} WAVs to {dest / 'wavs'}")


def _download_via_classic_cli(dest: Path) -> None:
    print(f"Downloading via kaggle CLI (kaggle.json auth) to {dest} ...")
    cmd = ["kaggle", "competitions", "download", "-c", COMPETITION, "-p", str(dest)]
    res = subprocess.run(cmd, check=False)
    if res.returncode != 0:
        print(
            "ERROR: kaggle CLI failed. Either authenticate (~/.kaggle/kaggle.json), "
            "set KAGGLE_API_TOKEN in .env, or accept the competition rules.",
            file=sys.stderr,
        )
        sys.exit(res.returncode)
    archives = sorted(dest.glob("*.zip"))
    if not archives:
        print(f"ERROR: no .zip archive found in {dest}", file=sys.stderr)
        sys.exit(2)
    n_wavs = _extract_archive(archives[-1], dest)
    print(f"OK: extracted {n_wavs} WAVs to {dest / 'wavs'}")


def main() -> None:
    p = argparse.ArgumentParser(description="Download Kaggle morse-v2 dataset.")
    p.add_argument("--dest", type=Path, default=DEFAULT_DEST)
    p.add_argument("--force", action="store_true",
                   help="Re-download even if data is already present.")
    args = p.parse_args()

    if not args.force and _already_extracted(args.dest):
        print(f"Already extracted at {args.dest}. Use --force to re-download.")
        return

    args.dest.mkdir(parents=True, exist_ok=True)
    token = _get_bearer_token()
    if token:
        _download_via_bearer(args.dest, token)
    elif _try_classic_cli():
        _download_via_classic_cli(args.dest)
    else:
        print(
            "ERROR: no Kaggle credentials found.\n"
            "Either set KAGGLE_API_TOKEN in .env (recommended) or place "
            "kaggle.json under ~/.kaggle/.",
            file=sys.stderr,
        )
        sys.exit(2)


if __name__ == "__main__":
    main()


if __name__ == "__main__":
    main()
