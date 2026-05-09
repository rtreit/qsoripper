# WA6MOW four-toggle diagnostic

- Sample: `cq-pota-de-wa6mow.mp3`
- Truth: `NQ CQ POTA DE WA6MOW CQ POTA DE WA6MOW K`
- Needle: leading `WA` of the **first** `WA6MOW` transmission.

## Verdict

- **Verdict: WPM auto-lock is the bug.**  Forcing `--force-wpm 22` (≈ truth) recovers the leading WA of the first WA6MOW. Larger `pad_s` does not. This confirms the front-end short-burst WPM estimator seeds too low at startup (the `bayes-joint` agent's 11–13 WPM observation), and the entire first transmission is decoded against the wrong rate.

## Per-toggle results

| # | Toggle | 1st WA6MOW | 2nd WA6MOW | WA6MOW count | head before 1st hit |
|---|--------|:---------:|:---------:|:---:|---------|
| 1 | `1-file-fullpath-auto-wpm` | ❌ | ❌ | 1 | `NQCQPOTADE5RHTIOWCQPOTADE` |
| 2 | `2-region-default` | ❌ | ❌ | 1 | `NQCQPOTADE6MOWCQPOTADE` |
| 3 | `3-region-pad-0.30` | ❌ | ❌ | 1 | `NQCQPOTADE6MOWCQPOTADE` |
| 4 | `4-region-pad-0.50` | ❌ | ❌ | 1 | `NQCQPOTADE6MOWCQPOTADE` |
| 5 | `5-region-pad-1.00` | ❌ | ❌ | 1 | `NQCQPOTADESIRSITIOWCQPOTADE` |
| 6 | `6-region-force-wpm-22` | ✅ | ✅ | 2 | `NQCQPOTADE` |
| 7 | `7-region-force-pitch-760` | ❌ | ❌ | 1 | `NQCQPOTADESIRSITIOWCQPOTADE` |
| 8 | `8-region-manual-window-0-to-12s` | ❌ | ❌ | 0 | `NQCQPOTADE6MOWCQE` |
| 9 | `9-region-manual-window-force-wpm-22` | ❌ | ❌ | 1 | `NQCQPOTADE` |

## Decoded text per toggle

### 1-file-fullpath-auto-wpm

- args: `file C:\Users\randy\Git\qsoripper\data\cw-samples\training-set-a\cq-pota-de-wa6mow.mp3`
- decoded: `NQ CQPO TA DE 5RHTI OW CQ PO TA DEWA6M OW K`

### 2-region-default

- args: `stream-region --file C:\Users\randy\Git\qsoripper\data\cw-samples\training-set-a\cq-pota-de-wa6mow.mp3 --json --no-realtime`
- decoded: `NQ CQPOTA DE 6MOW CQ PO TADEWA6M OW K`

### 3-region-pad-0.30

- args: `stream-region --file C:\Users\randy\Git\qsoripper\data\cw-samples\training-set-a\cq-pota-de-wa6mow.mp3 --json --no-realtime --pad-s 0.30`
- decoded: `NQ CQPOTA DE 6MOW CQ PO TADEWA6M OW K`

### 4-region-pad-0.50

- args: `stream-region --file C:\Users\randy\Git\qsoripper\data\cw-samples\training-set-a\cq-pota-de-wa6mow.mp3 --json --no-realtime --pad-s 0.50`
- decoded: `NQ CQPOTA DE 6MOW CQ PO TADEWA6M OW K`

### 5-region-pad-1.00

- args: `stream-region --file C:\Users\randy\Git\qsoripper\data\cw-samples\training-set-a\cq-pota-de-wa6mow.mp3 --json --no-realtime --pad-s 1.00`
- decoded: `NQ CQ P O T A D E SIRSI TI OW C Q P O T A D E WA 6 M OW K`

### 6-region-force-wpm-22

- args: `stream-region --file C:\Users\randy\Git\qsoripper\data\cw-samples\training-set-a\cq-pota-de-wa6mow.mp3 --json --no-realtime --force-wpm 22`
- decoded: `NQ CQPOTADEWA6MOW CQPOTA DEWA6MOW K`

### 7-region-force-pitch-760

- args: `stream-region --file C:\Users\randy\Git\qsoripper\data\cw-samples\training-set-a\cq-pota-de-wa6mow.mp3 --json --no-realtime --force-pitch 760`
- decoded: `NQ CQ P O T A D E SIRSI TI OW C Q P O T A D E WA 6 M OW K`

### 8-region-manual-window-0-to-12s

- args: `stream-region --file C:\Users\randy\Git\qsoripper\data\cw-samples\training-set-a\cq-pota-de-wa6mow.mp3 --json --no-realtime --region-start-s 0 --region-end-s 12`
- decoded: `NQ CQPOTA DE 6MOW CQ E`

### 9-region-manual-window-force-wpm-22

- args: `stream-region --file C:\Users\randy\Git\qsoripper\data\cw-samples\training-set-a\cq-pota-de-wa6mow.mp3 --json --no-realtime --region-start-s 0 --region-end-s 12 --force-wpm 22`
- decoded: `NQ CQPOTADEWA6MOW CQE`

