//! Stylometry engine: tokenization, feature counting, style counts, the
//! shared Burrows-Delta reference model, distance measures, calibration,
//! voice-match scoring, lexicons, and the profile store.

pub mod background;
pub mod calibrate;
pub mod checks;
pub mod delta;
pub mod features;
pub mod lexicon;
pub mod metrics;
pub mod model;
pub mod profile;
pub mod score;
pub mod store;
pub mod style;
pub mod text;
pub mod voice;

pub const DEFAULT_MFW: usize = 300;
pub const DEFAULT_TRIGRAMS: usize = 500;
