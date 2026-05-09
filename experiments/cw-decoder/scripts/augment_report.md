# Augmentation manifest report

- variants: **6000**
- unique source chunks: **200**
- total audio hours: **106.95**
- median variant duration: **59.3 s**

## Impairment coverage

| impairment | variants | fraction |
|---|---:|---:|
| awgn | 6000 | 100.00% |
| wpm_scale | 6000 | 100.00% |
| pitch_shift | 6000 | 100.00% |
| jitter | 6000 | 100.00% |
| wpm_drift | 6000 | 100.00% |
| watterson | 3896 | 64.93% |
| qsb | 3357 | 55.95% |
| pink_noise | 2755 | 45.92% |
| vfo_chirp | 1843 | 30.72% |
| pitch_drift | 1826 | 30.43% |
| qrm | 1785 | 29.75% |
| farnsworth | 1194 | 19.90% |
| agc_pumping | 572 | 9.53% |
| birdies | 314 | 5.23% |
| impulse | 58 | 0.97% |

## SNR distribution

| SNR (dB) | variants |
|---:|---:|
| 0.0 | 1008 |
| 5.0 | 1045 |
| 10.0 | 1011 |
| 15.0 | 972 |
| 20.0 | 975 |
| 30.0 | 989 |

## Watterson profile distribution

| profile | variants |
|---|---:|
| off | 2104 |
| poor | 1308 |
| moderate | 1294 |
| good | 1294 |

## Decoder eval (n=98)

| SNR (dB) | n | median CER | mean CER | p90 CER |
|---:|---:|---:|---:|---:|
| 0.0 | 18 | 0.641 | 0.644 | 1.116 |
| 5.0 | 12 | 0.420 | 0.458 | 0.977 |
| 10.0 | 11 | 0.262 | 0.368 | 0.664 |
| 15.0 | 18 | 0.609 | 0.624 | 1.102 |
| 20.0 | 13 | 0.537 | 0.586 | 0.828 |
| 30.0 | 26 | 0.638 | 0.635 | 0.955 |

## CER by Watterson profile

| profile | n | median CER | mean CER |
|---|---:|---:|---:|
| off | 36 | 0.341 | 0.487 |
| good | 22 | 0.316 | 0.445 |
| moderate | 22 | 0.668 | 0.794 |
| poor | 18 | 0.656 | 0.651 |

## Manifest schema (per row)

```
'wav_path': 'C:/Users/randy/Git/qsoripper-experiments/augment-arrl/data/
'src_wav_path': 'data/cw-samples/arrl-archive/20wpm/chunks/230905_0008.wav'
'text': 'WITH THE = END OF 20 WPM TEXT = QST DE W1AW <'
'src_wpm': 20.0
'augment_seed': 0
'chunk_id': '20wpm_230905_0008'
'seed_u32': 3904143378
'src_duration_s': 23.318
'duration_s': 22.7
'sample_rate': 8000
'snr_db': 0.0
'watterson_profile': 'off'
'applied': list
'params': dict
```
