"""Stage 4: aggregate per-session chunk JSONL into the master manifest."""

from __future__ import annotations

import argparse
import sys

from common import (
    CORPUS_ROOT,
    DEFAULT_SPEEDS,
    MANIFEST_PATH,
    SAMPLE_MANIFEST_PATH,
    configure_logging,
    parse_speeds_arg,
    read_jsonl,
    speed_dirname,
    write_jsonl,
)


def collect(speeds: list[float]) -> list[dict]:
    rows: list[dict] = []
    for wpm in speeds:
        chunks_dir = CORPUS_ROOT / speed_dirname(wpm) / "chunks"
        if not chunks_dir.exists():
            continue
        for jsonl in sorted(chunks_dir.glob("*.chunks.jsonl")):
            rows.extend(read_jsonl(jsonl))
    return rows


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--speeds", default=None)
    parser.add_argument("--sample-size", type=int, default=20)
    args = parser.parse_args(argv)

    logger = configure_logging()
    speeds = parse_speeds_arg(args.speeds)
    rows = collect(speeds)
    if not rows:
        logger.warning("manifest: no chunks found")
        return 1

    n = write_jsonl(MANIFEST_PATH, rows)
    logger.info(f"manifest: wrote {n} entries -> {MANIFEST_PATH}")

    # Sample = round-robin per speed to guarantee coverage.
    by_speed: dict[float, list[dict]] = {}
    for r in rows:
        by_speed.setdefault(r["wpm"], []).append(r)

    sample: list[dict] = []
    seen: set[str] = set()
    speeds_sorted = sorted(by_speed.keys())
    idx = 0
    while len(sample) < args.sample_size and any(by_speed.values()):
        s = speeds_sorted[idx % len(speeds_sorted)]
        bucket = by_speed[s]
        if bucket:
            r = bucket.pop(0)
            if r["wav_path"] not in seen:
                sample.append(r)
                seen.add(r["wav_path"])
        else:
            speeds_sorted = [x for x in speeds_sorted if by_speed[x]]
            if not speeds_sorted:
                break
            continue
        idx += 1

    write_jsonl(SAMPLE_MANIFEST_PATH, sample[:args.sample_size])
    logger.info(f"manifest: wrote {len(sample[:args.sample_size])} sample entries -> {SAMPLE_MANIFEST_PATH}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
