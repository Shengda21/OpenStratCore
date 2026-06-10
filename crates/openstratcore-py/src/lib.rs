//! PyO3 binding. Exposes a thin `Engine` for Python (RL env + LLM harness).
//! All data crosses the boundary as JSON strings to keep the contract = schemas/*.json.

// pyo3's `#[pymethods]` macro inserts an identity `PyErr -> PyErr` `.into()` on
// `PyResult`-returning methods (submit/observe), which clippy flags as
// `useless_conversion`. It is macro-generated, not our code, so allow it crate-wide.
#![allow(clippy::useless_conversion)]

// Leading `::` disambiguates the dependency crate from the `#[pymodule] fn
// openstratcore_core` of the same name that pyo3 expands at the crate root (E0659).
use ::openstratcore_core::rules::Rules;
use ::openstratcore_core::time::{secs_to_ticks, ticks_to_secs};
use ::openstratcore_core::types::{Map, Scenario, SideId};
use ::openstratcore_core::Engine as CoreEngine;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

#[pyclass]
struct Engine {
    inner: CoreEngine,
}

fn side_from_str(s: &str) -> PyResult<SideId> {
    match s {
        "red" => Ok(SideId::Red),
        "blue" => Ok(SideId::Blue),
        _ => Err(PyValueError::new_err("side must be 'red' or 'blue'")),
    }
}

#[pymethods]
impl Engine {
    /// Construct from JSON strings for map, scenario, rules + an integer seed.
    #[new]
    fn new(map_json: &str, scenario_json: &str, rules_json: &str, seed: u64) -> PyResult<Self> {
        let map: Map = serde_json::from_str(map_json).map_err(to_py)?;
        let scenario: Scenario = serde_json::from_str(scenario_json).map_err(to_py)?;
        let rules = Rules::from_json_str(rules_json).map_err(to_py)?;
        let inner = CoreEngine::new(map, scenario, rules, seed).map_err(to_py)?;
        Ok(Self { inner })
    }

    /// Submit an order (JSON action string) at sim time `t` (seconds).
    fn submit(&mut self, side: &str, command_json: &str, t: f64) -> PyResult<()> {
        let cmd: serde_json::Value = serde_json::from_str(command_json).map_err(to_py)?;
        self.inner
            .submit(side_from_str(side)?, &cmd, secs_to_ticks(t))
            .map_err(to_py)
    }

    /// Advance the simulation by `dt` seconds.
    fn step(&mut self, dt: f64) {
        self.inner.step(secs_to_ticks(dt));
    }

    /// Jump to the next event; returns its time in seconds (None if the queue is empty).
    fn advance_to_next_event(&mut self) -> Option<f64> {
        self.inner.advance_to_next_event().map(ticks_to_secs)
    }

    fn clock_seconds(&self) -> f64 {
        ticks_to_secs(self.inner.clock())
    }

    /// Fog-of-war observation for `side`, as a JSON string.
    fn observe(&self, side: &str) -> PyResult<String> {
        let v = self.inner.observe(side_from_str(side)?);
        Ok(v.to_string())
    }

    /// Full state snapshot, as a JSON string.
    fn snapshot(&self) -> String {
        self.inner.snapshot().to_string()
    }

    /// Telemetry (balance analysis): start recording every direct-fire adjudication. Observational
    /// only — does not change the simulation. ADMIN/OFFLINE, like `snapshot` — not the agent surface;
    /// records carry no side/unit/position, so they cannot leak un-observed units. Pair with
    /// `drain_outcome_log`; the buffer is thread-global (one engine per analysis thread).
    fn enable_outcome_log(&self) {
        self.inner.enable_outcome_log();
    }

    /// Stop telemetry and discard buffered records on this thread (explicit off/reset).
    fn disable_outcome_log(&self) {
        self.inner.disable_outcome_log();
    }

    /// Drain recorded direct-fire adjudications as a JSON-array string of
    /// `{table, weapon, attackLevel, armor, distance, outcome}` (schema: schemas/telemetry.schema.json).
    /// Leaves recording on.
    fn drain_outcome_log(&self) -> String {
        self.inner.drain_outcome_log().to_string()
    }
}

fn to_py<E: std::fmt::Display>(e: E) -> PyErr {
    PyValueError::new_err(e.to_string())
}

#[pymodule]
fn openstratcore_core(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Engine>()?;
    Ok(())
}
