//! wasm-bindgen binding. The browser frontend runs the SAME engine locally.
//! Data crosses as JSON strings (contract = schemas/*.json).

use openstratcore_core::rules::Rules;
use openstratcore_core::time::{secs_to_ticks, ticks_to_secs};
use openstratcore_core::types::{Map, Scenario, SideId};
use openstratcore_core::Engine as CoreEngine;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub struct Engine {
    inner: CoreEngine,
}

#[wasm_bindgen]
impl Engine {
    #[wasm_bindgen(constructor)]
    pub fn new(
        map_json: &str,
        scenario_json: &str,
        rules_json: &str,
        seed: u32,
    ) -> Result<Engine, JsValue> {
        let map: Map = serde_json::from_str(map_json).map_err(err)?;
        let scenario: Scenario = serde_json::from_str(scenario_json).map_err(err)?;
        let rules = Rules::from_json_str(rules_json).map_err(err)?;
        let inner = CoreEngine::new(map, scenario, rules, seed as u64).map_err(err)?;
        Ok(Engine { inner })
    }

    pub fn submit(&mut self, side: &str, command_json: &str, t: f64) -> Result<(), JsValue> {
        let s = match side {
            "red" => SideId::Red,
            "blue" => SideId::Blue,
            _ => return Err(JsValue::from_str("side must be red/blue")),
        };
        let cmd: serde_json::Value = serde_json::from_str(command_json).map_err(err)?;
        self.inner.submit(s, &cmd, secs_to_ticks(t)).map_err(err)
    }

    pub fn step(&mut self, dt: f64) {
        self.inner.step(secs_to_ticks(dt));
    }

    #[wasm_bindgen(js_name = clockSeconds)]
    pub fn clock_seconds(&self) -> f64 {
        ticks_to_secs(self.inner.clock())
    }

    pub fn observe(&self, side: &str) -> Result<String, JsValue> {
        let s = match side {
            "red" => SideId::Red,
            "blue" => SideId::Blue,
            _ => return Err(JsValue::from_str("side must be red/blue")),
        };
        Ok(self.inner.observe(s).to_string())
    }

    pub fn snapshot(&self) -> String {
        self.inner.snapshot().to_string()
    }
}

fn err<E: std::fmt::Display>(e: E) -> JsValue {
    JsValue::from_str(&e.to_string())
}
