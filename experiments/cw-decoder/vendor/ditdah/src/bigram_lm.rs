//! Character bigram language-model rescoring layer for the CW decoder.
//!
//! Composes a precomputed bigram transition table (trained from the ARRL
//! Code Practice corpus — see `experiments/cw-decoder/data/bigram_arrl.json`)
//! with the per-element acoustic posteriors emitted by the soft Viterbi
//! decoder. At each completed letter slot we enumerate every Morse code of
//! the observed element-length, compute its acoustic log-prob from the
//! per-element dot/dash log-probs, and run a standard bigram Viterbi over
//! the resulting lattice combining acoustic + lambda * LM scores.
//!
//! Activated when `DITDAH_BIGRAM_LM=1`. Lambda is read from
//! `DITDAH_BIGRAM_LAMBDA` (default 0.5).

use once_cell::sync::Lazy;
use serde::Deserialize;
use std::collections::HashMap;

/// Special token denoting a word-break ("space") in the bigram model.
const SPACE: char = ' ';

/// Bad-copy marker emitted when no Morse code matches an observed letter.
const BAD_COPY_MARKER: char = '*';

#[derive(Debug, Deserialize)]
struct RawBigram {
    vocab: Vec<String>,
    unigrams: HashMap<String, u64>,
    bigrams: HashMap<String, u64>,
}

/// Compiled bigram LM with Laplace-smoothed log-probabilities.
pub struct BigramLm {
    vocab: Vec<char>,
    /// log P(c2 | c1) keyed by (c1, c2). Missing pairs fall back to
    /// `default_log_prob_for(c1)`.
    log_p: HashMap<(char, char), f32>,
    /// Per-context (c1) fallback log-prob for unseen (c1, c2) pairs:
    /// log(1 / (count(c1) + V)).
    fallback: HashMap<char, f32>,
    /// Universal floor used when the previous character itself is unseen.
    floor: f32,
}

impl BigramLm {
    fn from_raw(raw: RawBigram) -> Self {
        let vocab: Vec<char> = raw.vocab.iter().filter_map(|s| s.chars().next()).collect();
        let v = vocab.len() as u64;

        // Per-context totals. We start from the JSON unigram counts and
        // also compute (count(c1) + V) for Laplace smoothing.
        let mut log_p = HashMap::with_capacity(raw.bigrams.len());
        for (k, n) in &raw.bigrams {
            let mut it = k.chars();
            let (Some(c1), Some(c2)) = (it.next(), it.next()) else {
                continue;
            };
            let total = raw.unigrams.get(&c1.to_string()).copied().unwrap_or(0) + v;
            // Laplace-smoothed: P(c2|c1) = (n + 1) / (count(c1) + V).
            let p = (*n as f32 + 1.0) / total as f32;
            log_p.insert((c1, c2), p.ln());
        }

        let mut fallback = HashMap::with_capacity(raw.unigrams.len());
        for (k, n) in &raw.unigrams {
            if let Some(c1) = k.chars().next() {
                let p = 1.0 / (*n as f32 + v as f32);
                fallback.insert(c1, p.ln());
            }
        }

        let floor = (1.0 / (v as f32 + 1.0)).ln();

        Self {
            vocab,
            log_p,
            fallback,
            floor,
        }
    }

    pub fn vocab(&self) -> &[char] {
        &self.vocab
    }

    pub fn log_transition(&self, prev: char, curr: char) -> f32 {
        if let Some(v) = self.log_p.get(&(prev, curr)) {
            return *v;
        }
        self.fallback.get(&prev).copied().unwrap_or(self.floor)
    }
}

const BIGRAM_JSON: &str = include_str!("../../../data/bigram_arrl.json");

static LM: Lazy<BigramLm> = Lazy::new(|| {
    let raw: RawBigram = serde_json::from_str(BIGRAM_JSON).expect("invalid bigram_arrl.json");
    BigramLm::from_raw(raw)
});

pub fn lm() -> &'static BigramLm {
    &LM
}

pub fn enabled() -> bool {
    std::env::var("DITDAH_BIGRAM_LM")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

pub fn lambda() -> f32 {
    std::env::var("DITDAH_BIGRAM_LAMBDA")
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .unwrap_or(0.5)
}

/// One observation slot in the lattice — either a "letter" with candidates,
/// or an explicit word-break (which forces a SPACE in the output and
/// resets the LM context to SPACE).
#[derive(Debug, Clone)]
pub enum Slot {
    Letter {
        /// Per-element (log_p_dot, log_p_dash). Same length as the Morse
        /// pattern the soft decoder originally built.
        elements: Vec<(f32, f32)>,
        /// Greedy Morse string, used as a fallback if no LM candidate
        /// dominates and we want to recover the original character.
        greedy_morse: String,
    },
    WordBreak,
}

/// Set of (Morse-pattern -> char) for every alphanumeric+punctuation entry
/// the decoder recognizes. Built once at startup; keyed by length so the
/// rescorer only enumerates length-matched candidates per slot.
static MORSE_BY_LEN: Lazy<Vec<Vec<(&'static str, char)>>> = Lazy::new(|| {
    // Mirror the alphabet in `morse_to_char` (decoder.rs). Punctuation that
    // the bigram corpus actually contains is included so it can be a
    // candidate; unsupported punctuation is omitted (the greedy decoder
    // still emits them via morse_to_char fallback when LM is disabled).
    let alphabet: &[(&str, char)] = &[
        (".-", 'A'),
        ("-...", 'B'),
        ("-.-.", 'C'),
        ("-..", 'D'),
        (".", 'E'),
        ("..-.", 'F'),
        ("--.", 'G'),
        ("....", 'H'),
        ("..", 'I'),
        (".---", 'J'),
        ("-.-", 'K'),
        (".-..", 'L'),
        ("--", 'M'),
        ("-.", 'N'),
        ("---", 'O'),
        (".--.", 'P'),
        ("--.-", 'Q'),
        (".-.", 'R'),
        ("...", 'S'),
        ("-", 'T'),
        ("..-", 'U'),
        ("...-", 'V'),
        (".--", 'W'),
        ("-..-", 'X'),
        ("-.--", 'Y'),
        ("--..", 'Z'),
        (".----", '1'),
        ("..---", '2'),
        ("...--", '3'),
        ("....-", '4'),
        (".....", '5'),
        ("-....", '6'),
        ("--...", '7'),
        ("---..", '8'),
        ("----.", '9'),
        ("-----", '0'),
        ("-..-.", '/'),
        ("-...-", '='),
        (".-.-.-", '.'),
        ("--..--", ','),
        ("..--..", '?'),
    ];
    let mut by_len: Vec<Vec<(&'static str, char)>> = Vec::new();
    for &(m, c) in alphabet {
        let len = m.chars().count();
        if by_len.len() <= len {
            by_len.resize(len + 1, Vec::new());
        }
        by_len[len].push((m, c));
    }
    by_len
});

fn candidates_for(elements: &[(f32, f32)]) -> Vec<(char, f32)> {
    let len = elements.len();
    if len == 0 {
        return Vec::new();
    }
    let codes = MORSE_BY_LEN.get(len).map(|v| v.as_slice()).unwrap_or(&[]);
    let mut out = Vec::with_capacity(codes.len());
    for &(pattern, ch) in codes {
        let mut score = 0.0_f32;
        for (i, e) in pattern.chars().enumerate() {
            let (lp_dot, lp_dash) = elements[i];
            score += if e == '.' { lp_dot } else { lp_dash };
        }
        out.push((ch, score));
    }
    out
}

/// Run bigram Viterbi over the slot lattice and return the decoded string.
///
/// Lambda controls the LM weight — 0.0 reduces to pure acoustic argmax (per
/// slot), large lambda dominates with the LM. Word-break slots emit ' ' and
/// reset LM context to the SPACE token.
pub fn decode_lattice(slots: &[Slot], lambda: f32) -> String {
    let lm = lm();
    let v = lm.vocab().len();

    // beam[i] = (char, score, backpointer-into-prev-beam)
    // We keep at most one entry per character for the bigram order.
    type Beam = HashMap<char, (f32, Option<(usize, char)>)>;

    let mut beams: Vec<Beam> = Vec::with_capacity(slots.len());
    // Start state: previous "char" is SPACE (sentence start).
    let mut prev_beam: Beam = HashMap::new();
    prev_beam.insert(SPACE, (0.0, None));

    for (i, slot) in slots.iter().enumerate() {
        let mut beam: Beam = HashMap::with_capacity(v);
        match slot {
            Slot::WordBreak => {
                // Force SPACE; aggregate over previous chars using LM.
                let mut best: Option<(f32, char)> = None;
                for (&pc, &(ps, _)) in prev_beam.iter() {
                    let lm_lp = lm.log_transition(pc, SPACE);
                    let s = ps + lambda * lm_lp;
                    if best.is_none_or(|(b, _)| s > b) {
                        best = Some((s, pc));
                    }
                }
                if let Some((s, bp)) = best {
                    beam.insert(SPACE, (s, Some((i, bp))));
                }
            }
            Slot::Letter {
                elements,
                greedy_morse,
            } => {
                let mut cands = candidates_for(elements);
                if cands.is_empty() {
                    // Length doesn't correspond to any known Morse code;
                    // emit BAD_COPY_MARKER with a low neutral score so the
                    // bigram doesn't silently merge it into a real letter.
                    let _ = greedy_morse;
                    let mut best: Option<(f32, char)> = None;
                    for (&pc, &(ps, _)) in prev_beam.iter() {
                        if best.is_none_or(|(b, _)| ps > b) {
                            best = Some((ps, pc));
                        }
                    }
                    if let Some((s, bp)) = best {
                        beam.insert(BAD_COPY_MARKER, (s, Some((i, bp))));
                    }
                } else {
                    // Normalize candidate acoustic scores so the LM weight
                    // is comparable across slots: subtract the max acoustic
                    // log-prob within this slot. This is monotone-equivalent
                    // for argmax but keeps numeric scale in check.
                    let max_ac = cands
                        .iter()
                        .map(|&(_, s)| s)
                        .fold(f32::NEG_INFINITY, f32::max);
                    for c in &mut cands {
                        c.1 -= max_ac;
                    }

                    for &(curr, ac) in &cands {
                        let mut best: Option<(f32, char)> = None;
                        for (&pc, &(ps, _)) in prev_beam.iter() {
                            let lm_lp = lm.log_transition(pc, curr);
                            let s = ps + ac + lambda * lm_lp;
                            if best.is_none_or(|(b, _)| s > b) {
                                best = Some((s, pc));
                            }
                        }
                        if let Some((s, bp)) = best {
                            beam.insert(curr, (s, Some((i, bp))));
                        }
                    }
                }
            }
        }

        if beam.is_empty() {
            // Nothing to extend; carry the previous beam forward unchanged.
            beam = prev_beam.clone();
        }

        beams.push(beam.clone());
        prev_beam = beam;
    }

    if beams.is_empty() {
        return String::new();
    }

    // Find best terminal.
    let last = beams.last().unwrap();
    let (mut cur_char, _) =
        last.iter()
            .map(|(&c, &(s, _))| (c, s))
            .fold(
                (' ', f32::NEG_INFINITY),
                |acc, x| {
                    if x.1 > acc.1 { x } else { acc }
                },
            );

    // Backtrack.
    let mut chars: Vec<char> = Vec::with_capacity(beams.len());
    for i in (0..beams.len()).rev() {
        chars.push(cur_char);
        let (_, bp) = beams[i][&cur_char];
        if let Some((_, prev)) = bp {
            cur_char = prev;
        }
    }
    chars.reverse();

    // Collapse runs of SPACE into single spaces and trim.
    let mut out = String::with_capacity(chars.len());
    let mut last_space = true; // suppress leading spaces
    for c in chars {
        if c == SPACE {
            if !last_space {
                out.push(' ');
                last_space = true;
            }
        } else {
            out.push(c);
            last_space = false;
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lm_loads() {
        let l = lm();
        // Vocab should include uppercase letters at minimum.
        assert!(l.vocab().contains(&'E'));
        // T->H is common in English; should be > T->Q.
        let th = l.log_transition('T', 'H');
        let tq = l.log_transition('T', 'Q');
        assert!(th > tq, "TH ({th}) should be more likely than TQ ({tq})");
    }

    #[test]
    fn candidates_only_match_length() {
        // Length-1 slot should produce E and T candidates (and nothing else).
        let cands = candidates_for(&[(0.0, -1.0)]);
        let chars: Vec<char> = cands.iter().map(|&(c, _)| c).collect();
        assert!(chars.contains(&'E'));
        assert!(chars.contains(&'T'));
        assert_eq!(chars.len(), 2);
    }

    #[test]
    fn pure_acoustic_recovers_greedy() {
        // Lambda = 0 should reduce to per-slot acoustic argmax.
        // Construct a 3-element slot strongly preferring "..." (S).
        let slot = Slot::Letter {
            elements: vec![(0.0, -10.0), (0.0, -10.0), (0.0, -10.0)],
            greedy_morse: "...".into(),
        };
        let out = decode_lattice(&[slot], 0.0);
        assert_eq!(out, "S");
    }
}
