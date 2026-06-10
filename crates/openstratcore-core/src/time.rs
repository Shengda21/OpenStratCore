//! Integer fixed-point logical clock.
//!
//! The kernel measures time in **centiseconds (cs) as `i64`** — never `f64` — so
//! replays are bit-identical across platforms (CLAUDE.md hard rule #1; see
//! docs/ARCHITECTURE.md "连续时间引擎约定"). Durations come from rules-as-data
//! config in *seconds*; they are converted to ticks exactly once at ingest and
//! the logical layer never sees floating-point time again.

/// Logical clock tick. `1 second = 100 cs`. The whole kernel uses this type.
pub type Tick = i64;

/// Centiseconds per second.
pub const CS_PER_SEC: Tick = 100;

/// Convert a duration in seconds (rules-as-data config / external API) to integer
/// ticks, rounding to the nearest centisecond. The rule timings are whole or
/// simple values (75, 150, 25, …) so this conversion is exact in practice; the
/// `round` only guards against float representation noise and never leaves a
/// fractional tick in the logical layer.
pub fn secs_to_ticks(secs: f64) -> Tick {
    (secs * CS_PER_SEC as f64).round() as Tick
}

/// Convert ticks back to seconds. Used **only** at serialization / external
/// observation boundaries (replay JSON, agent observations) — never inside the
/// event loop.
pub fn ticks_to_secs(ticks: Tick) -> f64 {
    ticks as f64 / CS_PER_SEC as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_seconds_round_trip_exactly() {
        for s in [0.0, 25.0, 75.0, 150.0, 300.0, 1200.0] {
            let t = secs_to_ticks(s);
            assert_eq!(t, (s as Tick) * CS_PER_SEC);
            assert_eq!(ticks_to_secs(t), s);
        }
    }

    #[test]
    fn rounds_to_nearest_centisecond() {
        assert_eq!(secs_to_ticks(0.005), 1); // 0.5 cs rounds up to 1
        assert_eq!(secs_to_ticks(2.5), 250);
    }
}
