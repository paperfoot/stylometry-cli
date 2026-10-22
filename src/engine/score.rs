//! Length-aware voice-match scoring: how close a text sits to a profile's
//! voice, measured at the text's own length. Shared by `compare` (the
//! verification verdict) and `check` (the draft score).
//!
//! The band is the calibrated length nearest the text's length without
//! exceeding it (Delta at 4,000 words is not Delta at 250). The verdict comes
//! from P(same author) alone, so the two can never contradict each other.
//! A text longer than twice the band is scored passage by passage and
//! summarized by the median: that per-passage list is the drift map of a
//! manuscript.

use serde::Serialize;

use crate::engine::calibrate::{
    self, BAND_KS, MIN_IMPOSTERS, P_ABSTAIN_HIGH, P_ABSTAIN_LOW, own_deltas, weighted_centroid,
};
use crate::engine::delta::cosine_delta;
use crate::engine::model::{ReferenceModel, reference_signature};
use crate::engine::profile::{BASE_CHUNK_WORDS, Profile, ProfileKind};
use crate::engine::{DEFAULT_MFW, DEFAULT_TRIGRAMS, text};
use crate::error::AppError;

/// Shortest text scored at all: below this Delta is noise.
pub const MIN_WORDS: usize = 150;
/// Uncalibrated "close": the text is at least as typical as the author's
/// least typical tenth of passages.
const CLOSE_TYPICALITY: f64 = 0.10;
const RANKING_LEN: usize = 5;

#[derive(Debug, Clone, Serialize)]
pub struct RankEntry {
    pub profile: String,
    pub cosine_delta: f64,
}

/// One band-length passage of a long text.
#[derive(Debug, Clone, Serialize)]
pub struct PassageScore {
    /// 1-based line of the passage's first word in the scored text.
    pub start_line: usize,
    pub words: usize,
    pub cosine_delta: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p_same_author: Option<f64>,
    pub nearest_profile: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct VoiceMatch {
    /// Unicode word count of the scored text.
    pub words: usize,
    /// Analysis length (words) the score was computed at.
    pub band_words: usize,
    /// Cosine Delta from the text to the target's centroid at that length
    /// (the median over passages for a long text).
    pub cosine_delta: f64,
    /// Share of the target's own held-out passages of this length that sit
    /// FARTHER from the centroid than the text does: 0.5 = as typical as the
    /// author's median passage, 0.05 = less typical than 95% of them.
    pub typicality: f64,
    /// Calibrated P(same author) at this length, when the target is calibrated.
    pub p_same_author: Option<f64>,
    /// Profile (author or background) closest to the text.
    pub nearest_profile: String,
    /// same_author | different_author | inconclusive (calibrated, from P),
    /// close_uncalibrated | far_uncalibrated (uncalibrated or a blend), or
    /// insufficient_background (nothing to compare against).
    pub verdict: String,
    /// The profile set changed since calibration (scores still use the
    /// frozen reference, so they are reproducible; re-calibrate to refresh).
    pub calibration_stale: bool,
    /// Nearest profiles, closest first.
    pub ranking: Vec<RankEntry>,
    /// Per-passage scores when the text is longer than twice the band.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub passages: Vec<PassageScore>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// Round to `digits` decimals (agents read these; 1e-12 noise helps nobody).
pub fn round(x: f64, digits: i32) -> f64 {
    let f = 10f64.powi(digits);
    (x * f).round() / f
}

/// Largest band not exceeding the text's length, else the smallest.
fn pick_band(bands: &[usize], words: usize) -> usize {
    bands
        .iter()
        .copied()
        .filter(|&b| b <= words)
        .max()
        .or_else(|| bands.iter().copied().min())
        .unwrap_or(BASE_CHUNK_WORDS)
}

fn verdict_from_p(p: f64) -> &'static str {
    if p > P_ABSTAIN_HIGH {
        "same_author"
    } else if p < P_ABSTAIN_LOW {
        "different_author"
    } else {
        "inconclusive"
    }
}

/// Score `text` against `target`, using `all` (every loaded profile, target
/// included) as the reference set and imposter pool.
pub fn voice_match(target: &Profile, all: &[Profile], text: &str) -> Result<VoiceMatch, AppError> {
    let bin = env!("CARGO_BIN_NAME");
    let words = text::count_words(text);
    if words < MIN_WORDS {
        return Err(AppError::input(
            format!("text has {words} words; need ≥ {MIN_WORDS} words for a voice match"),
            "Pass a longer passage (a full scene or more); 1,000+ words gives a much firmer verdict",
        ));
    }
    let refs = calibrate::reference_set(all);
    let is_blend = target.kind == ProfileKind::Blend;
    let cal = target.calibration.as_ref().filter(|c| !is_blend && !c.bands.is_empty());
    let mut warnings = Vec::new();

    // Band and model: the frozen calibrated reference, or a live one.
    let live: ReferenceModel;
    let (model, logistic, band_auc) = match cal {
        Some(c) => {
            let bw = pick_band(&c.bands.iter().map(|b| b.words).collect::<Vec<_>>(), words);
            let b = c.bands.iter().find(|b| b.words == bw).expect("picked from bands");
            (&b.reference, Some((b.slope, b.intercept)), Some(b.auc))
        }
        None => {
            let ks: Vec<usize> = BAND_KS
                .iter()
                .copied()
                .filter(|&k| target.spans(k).len() >= 3)
                .map(|k| k * BASE_CHUNK_WORDS)
                .collect();
            let bw = pick_band(&ks, words);
            live = ReferenceModel::build(&refs, DEFAULT_MFW, DEFAULT_TRIGRAMS, bw / BASE_CHUNK_WORDS);
            (&live, None, None)
        }
    };
    let band_words = model.k * BASE_CHUNK_WORDS;
    if words < band_words {
        warnings.push(format!(
            "text ({words} words) is shorter than the shortest {} length ({band_words} words): directional only",
            if cal.is_some() { "calibrated" } else { "analysis" }
        ));
    }

    // Candidates for "nearest": every non-blend profile, plus a blend target.
    let mut names: Vec<String> = Vec::new();
    let mut centroids: Vec<Vec<f64>> = Vec::new();
    let target_centroid = weighted_centroid(model, target);
    names.push(target.name.clone());
    centroids.push(target_centroid);
    for p in refs.iter().filter(|p| p.name != target.name) {
        if p.spans(model.k).is_empty() {
            continue;
        }
        names.push(p.name.clone());
        centroids.push(weighted_centroid(model, p));
    }
    let deltas_to_all = |t: &str| -> Vec<f64> {
        let z = model.zscore(&model.vectorize_text(t));
        centroids.iter().map(|c| cosine_delta(&z, c)).collect()
    };
    let nearest = |ds: &[f64]| -> String {
        let i = (0..ds.len())
            .min_by(|&a, &b| ds[a].total_cmp(&ds[b]))
            .unwrap_or(0);
        names[i].clone()
    };
    let p_of = |d: f64| logistic.map(|(s, i)| round(calibrate::probability(d, s, i), 3));

    // Whole text, or passage by passage for a long text.
    let mut passages = Vec::new();
    let summary: Vec<f64> = if words > 2 * band_words {
        let paras = text::paragraphs(text);
        let chunks = text::chunk_paragraphs(&paras, band_words);
        let lines = start_lines(text, &chunks);
        let mut per: Vec<Vec<f64>> = Vec::new();
        for (chunk, line) in chunks.iter().zip(lines) {
            let joined = chunk.join("\n\n");
            let n = text::count_words(&joined);
            let ds = deltas_to_all(&joined);
            passages.push(PassageScore {
                start_line: line,
                words: n,
                cosine_delta: round(ds[0], 4),
                p_same_author: p_of(ds[0]),
                nearest_profile: nearest(&ds),
            });
            per.push(ds);
        }
        (0..names.len())
            .map(|j| median(per.iter().map(|ds| ds[j]).collect()))
            .collect()
    } else {
        deltas_to_all(text)
    };
    let delta = summary[0];

    let own = own_deltas(model, target);
    let total_w: f64 = own.weights.iter().sum();
    let typicality = if total_w > 0.0 {
        own.deltas
            .iter()
            .zip(&own.weights)
            .filter(|(d, _)| **d > delta)
            .map(|(_, w)| w)
            .sum::<f64>()
            / total_w
    } else {
        warnings.push("too few of the profile's own passages at this length to measure typicality".into());
        0.0
    };

    let mut order: Vec<usize> = (0..names.len()).collect();
    order.sort_by(|&a, &b| summary[a].total_cmp(&summary[b]));
    let ranking: Vec<RankEntry> = order
        .iter()
        .take(RANKING_LEN)
        .map(|&i| RankEntry {
            profile: names[i].clone(),
            cosine_delta: round(summary[i], 4),
        })
        .collect();
    let nearest_profile = names[order[0]].clone();

    let p_same_author = p_of(delta);
    let mut calibration_stale = false;
    let verdict = if let (Some(p), Some(c)) = (p_same_author, cal) {
        calibration_stale = reference_signature(&refs, DEFAULT_MFW, DEFAULT_TRIGRAMS) != c.ref_signature;
        if calibration_stale {
            warnings.push(format!(
                "profiles were added or changed since calibration; scores use the frozen reference, re-run `{bin} calibrate {}` to refresh",
                target.name
            ));
        }
        if c.imposters < MIN_IMPOSTERS {
            warnings.push(format!(
                "calibrated against only {} imposter(s): unseen authors may pass as same_author; run `{bin} background fetch` then `{bin} calibrate {}`",
                c.imposters, target.name
            ));
        }
        if let Some(auc) = band_auc.filter(|a| *a < 0.9) {
            warnings.push(format!(
                "at {band_words} words this profile's verifier has AUC {auc:.2}: treat the verdict as directional; longer text verifies better"
            ));
        }
        verdict_from_p(p).to_string()
    } else if names.len() < 2 {
        warnings.push(format!(
            "no other profiles to compare against: install reference authors with `{bin} background fetch`"
        ));
        "insufficient_background".to_string()
    } else {
        let own_side = nearest_profile == target.name
            || target.components.iter().any(|(n, _)| *n == nearest_profile);
        if !is_blend {
            warnings.push(format!(
                "not calibrated: no P(same author); run `{bin} calibrate {}`",
                target.name
            ));
        }
        if typicality >= CLOSE_TYPICALITY && own_side {
            "close_uncalibrated".to_string()
        } else {
            "far_uncalibrated".to_string()
        }
    };

    Ok(VoiceMatch {
        words,
        band_words,
        cosine_delta: round(delta, 4),
        typicality: round(typicality, 3).max(0.0),
        p_same_author,
        nearest_profile,
        verdict,
        calibration_stale,
        ranking,
        passages,
        warnings,
    })
}

fn median(mut v: Vec<f64>) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(f64::total_cmp);
    let m = v.len() / 2;
    if v.len() % 2 == 1 { v[m] } else { (v[m - 1] + v[m]) / 2.0 }
}

/// 1-based line where each passage starts: its first few words located in
/// the text's whitespace-token stream, in order.
fn start_lines(text: &str, passages: &[Vec<String>]) -> Vec<usize> {
    let toks: Vec<(&str, usize)> = text
        .lines()
        .enumerate()
        .flat_map(|(i, l)| l.split_whitespace().map(move |t| (t, i + 1)))
        .collect();
    let mut cursor = 0;
    passages
        .iter()
        .map(|p| {
            let head: Vec<&str> = p
                .first()
                .map(|s| s.split_whitespace().take(6).collect())
                .unwrap_or_default();
            let found = (cursor..toks.len()).find(|&i| {
                head.iter()
                    .enumerate()
                    .all(|(j, h)| toks.get(i + j).is_some_and(|t| t.0 == *h))
            });
            match found {
                Some(i) => {
                    cursor = i + 1;
                    toks[i].1
                }
                None => toks.get(cursor).map(|t| t.1).unwrap_or(1),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn band_is_largest_not_exceeding_length() {
        let b = [250, 500, 1000, 2000, 4000];
        assert_eq!(pick_band(&b, 900), 500);
        assert_eq!(pick_band(&b, 1000), 1000);
        assert_eq!(pick_band(&b, 200), 250);
        assert_eq!(pick_band(&b, 90_000), 4000);
    }

    #[test]
    fn verdict_follows_probability_only() {
        assert_eq!(verdict_from_p(0.9), "same_author");
        assert_eq!(verdict_from_p(0.5), "inconclusive");
        assert_eq!(verdict_from_p(0.29), "different_author");
    }

    #[test]
    fn start_lines_track_passages() {
        let t = "one two three four\nfive six\n\nseven eight nine ten\n";
        let p = vec![vec!["one two three four five six".to_string()], vec!["seven eight nine ten".to_string()]];
        assert_eq!(start_lines(t, &p), vec![1, 4]);
    }
}
