//! Stylometry engine: tokenization, feature counting, the shared Burrows-Delta
//! reference model, distance measures, calibration, and the profile store.

pub mod calibrate;
pub mod delta;
pub mod features;
pub mod model;
pub mod profile;
pub mod store;
pub mod text;

pub const DEFAULT_CHUNK_WORDS: usize = 1500;
pub const DEFAULT_MFW: usize = 300;
pub const DEFAULT_TRIGRAMS: usize = 500;
