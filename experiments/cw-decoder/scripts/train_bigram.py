"""Train a character bigram LM from the ARRL Code Practice corpus.

Reads manifest.jsonl, normalizes text (uppercase, [A-Z0-9 /=]), counts
unigrams and bigrams over the joined corpus (with explicit ' ' between
manifest entries), and emits JSON consumed at runtime by the bigram-LM
rescorer in vendor/ditdah/src/decoder.rs.
"""

from __future__ import annotations

import json
import re
import sys
from collections import Counter
from pathlib import Path


# Vocabulary used for the LM. We keep the alphanumerics + a small set of
# punctuation that morse_to_char actually emits and that appears in real CW
# traffic. Anything outside this is normalized to space.
VOCAB = list("ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 /=.,?")


def normalize(text: str) -> str:
    text = text.upper()
    text = re.sub(r"[^A-Z0-9 /=.,?]+", " ", text)
    text = re.sub(r"\s+", " ", text).strip()
    return text


def main(manifest_path: Path, out_path: Path) -> None:
    chunks: list[str] = []
    total_chars = 0
    with manifest_path.open("r", encoding="utf-8") as fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            obj = json.loads(line)
            t = normalize(obj.get("text", ""))
            if t:
                chunks.append(t)
                total_chars += len(t)

    # Join chunks with spaces so bigrams across chunk boundaries don't
    # spuriously couple unrelated text.
    corpus = " ".join(chunks)

    unigrams: Counter[str] = Counter()
    bigrams: Counter[str] = Counter()

    vocab_set = set(VOCAB)
    prev: str | None = None
    for ch in corpus:
        if ch not in vocab_set:
            ch = " "
        unigrams[ch] += 1
        if prev is not None:
            bigrams[prev + ch] += 1
        prev = ch

    out = {
        "vocab": VOCAB,
        "corpus_chars": total_chars,
        "joined_chars": len(corpus),
        "num_chunks": len(chunks),
        "unigrams": dict(unigrams),
        "bigrams": dict(bigrams),
    }
    out_path.write_text(json.dumps(out, indent=1, sort_keys=True), encoding="utf-8")
    print(f"manifest entries: {len(chunks)}")
    print(f"total truth chars: {total_chars}")
    print(f"joined chars: {len(corpus)}")
    print(f"unique unigrams: {len(unigrams)}")
    print(f"unique bigrams: {len(bigrams)}")
    print(f"wrote {out_path}")


if __name__ == "__main__":
    if len(sys.argv) != 3:
        print("usage: train_bigram.py <manifest.jsonl> <out.json>")
        sys.exit(1)
    main(Path(sys.argv[1]), Path(sys.argv[2]))
