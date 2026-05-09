"""Orchestrator: run all 5 pipeline stages with stage timings.

Usage:
    python run.py --speeds 15,20,25,30 --limit-per-speed 50

Resume by skipping completed stages:
    python run.py --skip-stages 0,1
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time
from pathlib import Path

from common import (
    CORPUS_ROOT,
    DEFAULT_SPEEDS,
    SCRIPT_DIR,
    configure_logging,
    parse_speeds_arg,
)

PERF_PATH = CORPUS_ROOT / "pipeline_perf.json"


def _run_module(module: str, args: list[str], logger) -> tuple[int, float]:
    cmd = [sys.executable, str(SCRIPT_DIR / module)] + args
    logger.info(f"=== running {module} {' '.join(args)} ===")
    t0 = time.monotonic()
    rc = subprocess.call(cmd, cwd=str(SCRIPT_DIR))
    dt = time.monotonic() - t0
    logger.info(f"=== {module} done rc={rc} in {dt:.1f}s ===")
    return rc, dt


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--speeds", default=None,
                        help=f"Comma-separated speeds. Default {','.join(str(s) for s in DEFAULT_SPEEDS)}.")
    parser.add_argument("--limit-per-speed", type=int, default=50)
    parser.add_argument("--workers", type=int, default=8,
                        help="Concurrent HTTP downloads (default 8).")
    parser.add_argument("--align-workers", type=int, default=os.cpu_count() or 4)
    parser.add_argument("--skip-stages", default="",
                        help="Comma-separated stage indices to skip (0=index, 1=download, 2=trim, 3=align, 4=manifest, 5=report).")
    args = parser.parse_args(argv)

    logger = configure_logging()
    speeds = parse_speeds_arg(args.speeds)
    speeds_arg = ",".join(str(s) for s in speeds)
    skip = {int(s) for s in args.skip_stages.split(",") if s.strip().isdigit()}

    stages: list[dict] = []
    t_start = time.monotonic()

    perf = {
        "speeds": speeds,
        "limit_per_speed": args.limit_per_speed,
        "workers": args.workers,
        "align_workers": args.align_workers,
        "stages": stages,
    }

    def stage(idx: int, name: str, module: str, extra: list[str], detail: str = "") -> int:
        if idx in skip:
            logger.info(f"--- skipping stage {idx} ({name}) ---")
            stages.append({"name": name, "seconds": 0.0, "detail": "skipped"})
            return 0
        rc, dt = _run_module(module, extra, logger)
        stages.append({"name": name, "seconds": round(dt, 2), "detail": detail})
        if name == "download":
            perf["download_seconds"] = dt
        elif name == "align":
            perf["align_seconds"] = dt
        return rc

    rc = stage(0, "build_index", "build_index.py", ["--speeds", speeds_arg])
    if rc and 0 not in skip:
        return rc

    rc = stage(1, "download", "download_parallel.py",
               ["--speeds", speeds_arg, "--limit-per-speed", str(args.limit_per_speed),
                "--workers", str(args.workers)],
               detail=f"workers={args.workers}, limit={args.limit_per_speed}/speed")
    if rc and 1 not in skip:
        logger.warning("download stage exited non-zero; continuing to use whatever was fetched")

    rc = stage(2, "trim", "trim_parallel.py",
               ["--speeds", speeds_arg, "--workers", str(args.align_workers)],
               detail=f"workers={args.align_workers}")
    if rc and 2 not in skip:
        logger.warning("trim stage exited non-zero; continuing")

    rc = stage(3, "align", "align_parallel.py",
               ["--speeds", speeds_arg, "--workers", str(args.align_workers)],
               detail=f"workers={args.align_workers}")
    if rc and 3 not in skip:
        logger.warning("align stage exited non-zero; continuing")

    stage(4, "manifest", "manifest.py", ["--speeds", speeds_arg])

    # Need session/chunk counts for report — compute from manifest now.
    from common import MANIFEST_PATH, read_jsonl, speed_dirname
    manifest_rows = read_jsonl(MANIFEST_PATH)
    perf["chunk_count"] = len(manifest_rows)
    sess_count = 0
    for s in speeds:
        td = CORPUS_ROOT / speed_dirname(s) / "trimmed"
        if td.exists():
            sess_count += len(list(td.glob("*.trim.json")))
    perf["session_count"] = sess_count

    perf["wall_total_s"] = round(time.monotonic() - t_start, 2)
    PERF_PATH.parent.mkdir(parents=True, exist_ok=True)
    PERF_PATH.write_text(json.dumps(perf, indent=2), encoding="utf-8")

    stage(5, "report", "report.py", ["--speeds", speeds_arg, "--perf-json", str(PERF_PATH)])

    perf["wall_total_s"] = round(time.monotonic() - t_start, 2)
    PERF_PATH.write_text(json.dumps(perf, indent=2), encoding="utf-8")

    logger.info("")
    logger.info("================ PIPELINE SUMMARY ================")
    for s in stages:
        logger.info(f"  {s['name']:<14} {s['seconds']:>8.1f}s  {s.get('detail', '')}")
    logger.info(f"  TOTAL          {perf['wall_total_s']:>8.1f}s")
    logger.info(f"  Sessions:      {sess_count}")
    logger.info(f"  Chunks:        {len(manifest_rows)}")
    logger.info("==================================================")
    return 0


if __name__ == "__main__":
    sys.exit(main())
