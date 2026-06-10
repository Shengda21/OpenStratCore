//! openstratcore-core — deterministic, seedable real-time land-warfare wargame engine.
//!
//! Invariants (see CLAUDE.md):
//! - All randomness goes through [`rng::Rng`]. No OS RNG, no wall clock, no hash-order reliance.
//! - All tunable numbers come from [`rules::Rules`] (loaded from config), never hardcoded.
//! - Library code returns [`EngineError`]; it must not panic. `todo!()` only in clearly-marked
//!   unimplemented mechanics that are being grown via the `/add-rule` workflow.

pub mod combat;
pub mod engine;
pub mod hex;
pub mod mechanics;
pub mod prob;
pub mod replay;
pub mod rng;
pub mod rules;
pub mod time;
pub mod types;
pub mod units;

pub use engine::Engine;
pub use hex::Axial;
pub use rules::Rules;
pub use time::Tick;
pub use types::{Map, Scenario, Side, State};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("parse error: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("invalid scenario: {0}")]
    Scenario(String),
    #[error("invalid command: {0}")]
    Command(String),
    #[error("rules config error: {0}")]
    Rules(String),
    #[error("not yet implemented: {0}")]
    Unimplemented(String),
}

pub type Result<T> = std::result::Result<T, EngineError>;
