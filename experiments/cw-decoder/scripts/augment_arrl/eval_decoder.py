"""Validation script: decode a sample of augmented variants and compute CER vs SNR."""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import time
from pathlib import Path

import numpy as np

if __package__ in (None, ""):
    sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
    from augment_arrl import config as cfg_mod  # type: ignore
else:
    from . import config as cfg_mod


def normalize(s: str) -> str:
    s = s.upper()
    s = re.sub(r"[^A-Z0-9]+", " ", s)
    return re.sub(r"\s+", " ", s).strip()


def levenshtein(a: str, b: str) -> int:
    if len(a) < len(b):
        a, b = b, a
    prev = list(range(len(b) + 1))
    for i, ca in enumerate(a, 1):
        curr = [i]
        for j, cb in enumerate(b, 1):
            curr.append(min(prev[j] + 1, curr[-1] + 1, prev[j - 1] + (ca != cb)))
        prev = curr
    return prev[-1]


def decode(exe: Path, wav: Path, timeout_s: int = 90) -> str:
    p = subprocess.run(
        [str(exe), "stream-region", "--file", str(wav), "--json", "--no-realtime"],
        capture_output=True, text=True, timeout=timeout_s,
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


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--manifest", default=str(cfg_mod.AUGMENTED_MANIFEST_PATH))
    p.add_argument("--decoder", default=str(cfg_mod.DEFAULT_DECODER_BIN))
    p.add_argument("--n", type=int, default=100, help="how many variants to decode")
    p.add_argument("--seed", type=int, default=0xC0FFEE, help="sampling seed")
    p.add_argument("--out", default=str(Path(cfg_mod.AUGMENTED_MANIFEST_PATH).parent / "augment_eval.jsonl"))
    args = p.parse_args(argv)

    exe = Path(args.decoder)
    if not exe.exists():
        print(f"ERROR: decoder not found at {exe}", file=sys.stderr)
        return 2

    rows: list[dict] = []
    with open(args.manifest, "r", encoding="utf-8") as fh:
        for line in fh:
            line = line.strip()
            if line:
                rows.append(json.loads(line))
    if not rows:
        print(f"ERROR: empty manifest {args.manifest}", file=sys.stderr)
        return 2

    rng = np.random.default_rng(args.seed)
    idx = rng.choice(len(rows), size=min(args.n, len(rows)), replace=False)
    sample = [rows[int(i)] for i in idx]
    print(f"Eval: decoding {len(sample)} of {len(rows)} variants with {exe.name}")

    out_rows: list[dict] = []
    t0 = time.time()
    for i, row in enumerate(sample, 1):
        wav = Path(row["wav_path"])
        if not wav.exists():
            print(f"  skip (missing): {wav}", file=sys.stderr)
            continue
        truth = normalize(row["text"])
        try:
            hyp_raw = decode(exe, wav)
        except subprocess.TimeoutExpired:
            print(f"  timeout: {wav.name}", file=sys.stderr)
            continue
        hyp = normalize(hyp_raw)
        cer = levenshtein(truth, hyp) / max(1, len(truth))
        out_rows.append({
            "wav": str(wav).replace("\\", "/"),
            "chunk_id": row["chunk_id"],
            "augment_seed": row["augment_seed"],
            "snr_db": row["snr_db"],
            "watterson_profile": row["watterson_profile"],
            "applied": row["applied"],
            "cer": round(cer, 4),
            "src_wpm": row.get("src_wpm"),
        })
        if i % 10 == 0:
            done = len(out_rows)
            print(f"  {i}/{len(sample)} (done={done}, mean_cer={np.mean([r['cer'] for r in out_rows]):.3f}, "
                  f"{(time.time()-t0)/max(1,i):.1f}s/sample)", flush=True)

    Path(args.out).parent.mkdir(parents=True, exist_ok=True)
    with open(args.out, "w", encoding="utf-8") as fh:
        for r in out_rows:
            fh.write(json.dumps(r) + "\n")
    print(f"Wrote {len(out_rows)} eval rows -> {args.out}")

    # Bucket CER by SNR.
    by_snr: dict[float, list[float]] = {}
    for r in out_rows:
        by_snr.setdefault(float(r["snr_db"]), []).append(r["cer"])
    print("\nCER vs SNR (median):")
    for snr in sorted(by_snr):
        cers = by_snr[snr]
        med = float(np.median(cers))
        print(f"  SNR={snr:5.1f} dB  n={len(cers):3d}  median CER={med:.3f}  mean CER={float(np.mean(cers)):.3f}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
