//! Length-aware voice-match scoring: how close a text sits to a profile's
//! voice, measured at the text's own length. Shared by `compare` (the
//! verification verdict) and `check` (the draft score).
//!
//! FOUNDATION STUB: the verification lane implements `voice_match`.

use serde::Serialize;

use crate::engine::profile::Profile;
use crate::error::AppError;

#[derive(Debug, Clone, Serialize)]
pub struct VoiceMatch {
    /// Unicode word count of the scored text.
    pub words: usize,
    /// Analysis length (words) the score was computed at.
    pub band_words: usize,
    /// Cosine Delta from the text to the target's centroid at that length.
    pub cosine_delta: f64,
    /// Share of the target's own held-out passages of this length that sit
    /// FARTHER from the centroid than the text does: 0.5 = as typical as the
    /// author's median passage, 0.05 = less typical than 95% of them.
    pub typicality: f64,
    /// Calibrated P(same author) at this length, when the target is calibrated.
    pub p_same_author: Option<f64>,
    /// Profile (author or background) closest to the text.
    pub nearest_profile: String,
    /// same_author | different_author | inconclusive (calibrated), or
    /// *_uncalibrated / insufficient_background otherwise.
    pub verdict: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// Score `text` against `target`, using `all` (every loaded profile, target
/// included) as the reference set and imposter pool.
pub fn voice_match(target: &Profile, all: &[Profile], text: &str) -> Result<VoiceMatch, AppError> {
    let _ = (target, all, text);
    Err(AppError::InvalidInput("voice_match not implemented yet".into()))
}
