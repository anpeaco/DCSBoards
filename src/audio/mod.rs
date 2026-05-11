//! Audio input pipeline. M4.2: cpal capture into a shared buffer; rubato
//! resamples to 16 kHz mono on stop so the STT engine (M4.3) gets the format
//! whisper expects.

pub mod capture;

pub use capture::{enumerate_inputs, open_default, open_named, AudioCapture, TARGET_RATE};
