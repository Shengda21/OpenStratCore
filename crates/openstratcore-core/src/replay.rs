//! Replay recording. Mirrors schemas/replay.schema.json.
//! Canonical = header.seed + ordered commands (re-running reproduces the match).
//! Snapshots/events are optional aids for the viewer.

use crate::engine::Engine;
use crate::rules::Rules;
use crate::time::{secs_to_ticks, ticks_to_secs};
use crate::types::{Map, Scenario, SideId};
use crate::{EngineError, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Header {
    pub format: String,
    pub version: u32,
    pub red_name: String,
    pub blue_name: String,
    pub map_file: String,
    pub scenario_file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rules_file: Option<String>,
    pub seed: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rules_version: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<MatchResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchResult {
    pub winner: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandEntry {
    pub t: f64,
    pub side: String,
    /// The command/action object (shape per llm_tools.schema.json action).
    pub command: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub t: f64,
    pub state: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventLog {
    pub t: f64,
    pub kind: String,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Replay {
    pub header: Header,
    pub commands: Vec<CommandEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub snapshots: Vec<Snapshot>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<EventLog>,
}

/// Accumulates the canonical command stream (and optional snapshots/events) during a match.
#[derive(Debug, Clone)]
pub struct Recorder {
    pub header: Header,
    pub commands: Vec<CommandEntry>,
    pub snapshots: Vec<Snapshot>,
    pub events: Vec<EventLog>,
}

impl Recorder {
    pub fn new(header: Header) -> Self {
        Self {
            header,
            commands: Vec::new(),
            snapshots: Vec::new(),
            events: Vec::new(),
        }
    }

    pub fn record_command(&mut self, t: f64, side: &str, command: serde_json::Value) {
        self.commands.push(CommandEntry {
            t,
            side: side.to_string(),
            command,
        });
    }

    pub fn snapshot(&mut self, t: f64, state: serde_json::Value) {
        self.snapshots.push(Snapshot { t, state });
    }

    pub fn finish(self) -> Replay {
        Replay {
            header: self.header,
            commands: self.commands,
            snapshots: self.snapshots,
            events: self.events,
        }
    }
}

fn side_from_str(s: &str) -> Result<SideId> {
    match s {
        "red" => Ok(SideId::Red),
        "blue" => Ok(SideId::Blue),
        other => Err(EngineError::Command(format!("unknown side {other:?}"))),
    }
}

/// Replay a canonical [`Replay`] on a fresh engine seeded by `header.seed`, returning a
/// per-step snapshot trace. Time ordering (continuous-time DES; see docs/ARCHITECTURE.md):
/// internal events keep the engine's strict `(time, seq)` heap order, and the driver
/// interleaves the (time-sorted) command stream against them — every event strictly
/// before a command's tick fires first, then the clock advances to that tick and the
/// command is applied, so a command at tick `T` lands before any effect *scheduled* at
/// `T`. This policy is fixed, hence deterministic: two runs of the same `Replay` yield
/// byte-identical traces, which is exactly what `/replay-verify` checks (hard rule #7).
pub fn play(replay: &Replay, map: Map, scenario: Scenario, rules: Rules) -> Result<Vec<Snapshot>> {
    let mut engine = Engine::new(map, scenario, rules, replay.header.seed)?;
    let mut trace = Vec::new();

    for cmd in &replay.commands {
        let cmd_tick = secs_to_ticks(cmd.t);
        // Drain every event strictly before this command, snapshotting each.
        while let Some(ev_tick) = engine.peek_next_event_time() {
            if ev_tick >= cmd_tick {
                break;
            }
            engine.advance_to_next_event();
            trace.push(snapshot_at(&engine));
        }
        // Advance the clock to the command's tick so the snapshot time and any
        // freshly-scheduled timers are anchored at the moment the order is given.
        engine.advance_clock_to(cmd_tick);
        engine.submit(side_from_str(&cmd.side)?, &cmd.command, cmd_tick)?;
        trace.push(snapshot_at(&engine));
    }

    // Drain any events scheduled after the last command.
    while engine.advance_to_next_event().is_some() {
        trace.push(snapshot_at(&engine));
    }

    Ok(trace)
}

fn snapshot_at(engine: &Engine) -> Snapshot {
    Snapshot {
        t: ticks_to_secs(engine.clock()),
        state: engine.snapshot(),
    }
}

#[cfg(test)]
#[test]
fn roundtrip() {
    let map: Map =
        serde_json::from_str(include_str!("../../../scenarios/maps/demo_valley.map.json")).unwrap();
    let scenario: Scenario = serde_json::from_str(include_str!(
        "../../../scenarios/demo_skirmish.scenario.json"
    ))
    .unwrap();
    let rules = Rules::from_json_str(include_str!("../../../config/rules.default.json")).unwrap();

    let header = Header {
        format: "openstratcore.replay".to_string(),
        version: 1,
        red_name: "scripted-red".to_string(),
        blue_name: "scripted-blue".to_string(),
        map_file: "scenarios/maps/demo_valley.map.json".to_string(),
        scenario_file: "scenarios/demo_skirmish.scenario.json".to_string(),
        rules_file: None,
        seed: 12345,
        engine_version: None,
        rules_version: None,
        created_at: None,
        duration_seconds: None,
        result: None,
    };
    let commands = vec![
        CommandEntry {
            t: 0.0,
            side: "red".into(),
            // R-T1 walks (0,0) -> (1,0) -> (2,0) and settles Stopped on arrival.
            command: serde_json::json!({ "op": "move_to", "unitId": "R-T1", "target": { "q": 2, "r": 0 } }),
        },
        CommandEntry {
            t: 10.0,
            side: "red".into(),
            command: serde_json::json!({ "op": "wait" }),
        },
        CommandEntry {
            t: 30.0,
            side: "red".into(),
            command: serde_json::json!({ "op": "wait" }),
        },
        CommandEntry {
            t: 50.0,
            side: "blue".into(),
            command: serde_json::json!({ "op": "wait" }),
        },
    ];
    let replay = Replay {
        header,
        commands,
        snapshots: vec![],
        events: vec![],
    };

    // Run #1 on a fresh engine.
    let trace1 = play(&replay, map.clone(), scenario.clone(), rules.clone()).expect("play 1");
    // Serialize the canonical replay, reload it, and run #2 on another fresh same-seed engine.
    let json = serde_json::to_string(&replay).expect("serialize replay");
    let replay2: Replay = serde_json::from_str(&json).expect("reload replay");
    let trace2 = play(&replay2, map, scenario, rules).expect("play 2");

    assert!(!trace1.is_empty(), "trace must contain snapshots");
    assert_eq!(
        serde_json::to_value(&trace1).unwrap(),
        serde_json::to_value(&trace2).unwrap(),
        "replay is not deterministic — per-tick state diverged"
    );
    // Sanity: the scheduled stop transition actually fired by the end of the trace.
    let last = trace1.last().unwrap();
    assert_eq!(
        last.state["units"]["R-T1"]["state"],
        serde_json::json!("stopped")
    );
}
