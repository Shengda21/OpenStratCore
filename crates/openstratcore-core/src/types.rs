//! Data types mirroring schemas/{map,scenario}.schema.json plus runtime [`State`].
//! Keep these in lockstep with the schemas (contract-first; see CLAUDE.md rule 3).

use crate::hex::Axial;
use crate::time::Tick;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// ---------- Map ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Terrain {
    Open,
    Urban,
    Forest,
    River,
    /// 大河流 — heavier movement penalty than `River` (小河流); see 三.1.2.
    RiverLarge,
    Lake,
    Soft,
    Road,
    Rail,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Road {
    pub kind: String,
    #[serde(default)]
    pub connects: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HexCell {
    pub q: i32,
    pub r: i32,
    #[serde(default)]
    pub id: Option<String>,
    pub elevation: i32,
    pub terrain: Terrain,
    #[serde(default)]
    pub road: Option<Road>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Map {
    pub format: String,
    pub version: u32,
    pub name: String,
    #[serde(default)]
    pub elevation_unit_meters: Option<u32>,
    pub hexes: Vec<HexCell>,
}

impl Map {
    pub fn cell(&self, at: &Axial) -> Option<&HexCell> {
        self.hexes.iter().find(|h| h.q == at.q && h.r == at.r)
    }
}

// ---------- Scenario ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SideId {
    Red,
    Blue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnitType {
    Tank,
    Ifv,
    Infantry,
    Artillery,
    Uav,
    AttackHeli,
    TransportHeli,
    LoiteringMunition,
    ReconVehicle,
    RadarVehicle,
    Minelayer,
    Minesweeper,
    AaGun,
    AaMissileSquad,
    AaMissileVehicle,
    Ugv,
    Pickup,
    /// 三.22 天基侦察算子 — a passive support 算子: cannot move, attack, or be attacked; grants its
    /// side a fog-piercing track of enemy 算子 (lowest-id per hex, not 工事-hidden).
    SpaceRecon,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Armor {
    Composite,
    Heavy,
    Medium,
    Light,
    None,
}

impl Armor {
    /// Parse the snake_case armor string used in scenarios/config (e.g. a 工事's `armor`), or `None`
    /// if unrecognised.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "composite" => Some(Self::Composite),
            "heavy" => Some(Self::Heavy),
            "medium" => Some(Self::Medium),
            "light" => Some(Self::Light),
            "none" => Some(Self::None),
            _ => None,
        }
    }
}

/// 运输直升机 altitude state (三.16): 高空 / 低空 / 超低空 (+500 / +200 / +20 m of LOS height, see
/// `air.heli_altitude_meters`). Every non-heli carries the default 低空; the field is meaningful
/// only for `UnitType::TransportHeli`, where it drives how far the heli sees over terrain — and how
/// far it is seen — so a 超低空 heli is correctly harder to reveal (三.16b).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeliAlt {
    High,
    #[default]
    Low,
    VeryLow,
}

impl HeliAlt {
    /// Parse the `altitude` field of a `switch_altitude` order.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "high" => Some(Self::High),
            "low" => Some(Self::Low),
            "very_low" => Some(Self::VeryLow),
            _ => None,
        }
    }
    /// 三.16d — a switch is only allowed between neighbouring altitude states.
    pub fn adjacent(self, other: Self) -> bool {
        matches!(
            (self, other),
            (Self::High, Self::Low)
                | (Self::Low, Self::High)
                | (Self::Low, Self::VeryLow)
                | (Self::VeryLow, Self::Low)
        )
    }
    /// True for the default (低空) altitude — lets snapshots omit it for every ground unit.
    pub fn is_low(&self) -> bool {
        matches!(self, Self::Low)
    }
}

/// A unit's posture. The five mutually-exclusive *movement modes* of 三.1 are
/// `Normal`(正常机动) / `Charge1`(一级冲锋) / `Charge2`(二级冲锋) / `Cover`(掩蔽) /
/// `Half`(半速); `March`(行军) is the vehicle road mode (三.1.3). `Moving`/`Stopped`
/// are motion phases and `Suppressed` is also carried as `RuntimeUnit.suppressed_until`.
/// serde names match the `mode` vocabulary in schemas/llm_tools.schema.json.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnitState {
    Normal,
    Moving,
    Stopped,
    Cover,
    March,
    Suppressed,
    Charge1,
    Charge2,
    Half,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScenarioUnit {
    pub id: String,
    #[serde(rename = "type")]
    pub unit_type: UnitType,
    #[serde(default)]
    pub armor: Option<Armor>,
    #[serde(default = "one")]
    pub teams: u8,
    pub at: Axial,
    #[serde(default)]
    pub facing: u8,
    #[serde(default)]
    pub state: Option<UnitState>,
    #[serde(default)]
    pub carried_by: Option<String>,
    /// 三.13b: for a 无人战车, the id of its 隶属 manned vehicle (see [`RuntimeUnit::affiliated_to`]).
    #[serde(default)]
    pub affiliated_to: Option<String>,
}

fn one() -> u8 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Side {
    pub name: String,
    pub units: Vec<ScenarioUnit>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sides {
    pub red: Side,
    pub blue: Side,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlPoint {
    pub id: String,
    pub at: Axial,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub priority: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Facility {
    pub kind: String,
    pub at: Axial,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub armor: Option<String>,
    #[serde(default)]
    pub capacity: Option<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Scenario {
    pub format: String,
    pub version: u32,
    pub name: String,
    pub map: String,
    #[serde(default)]
    pub rules: Option<String>,
    #[serde(default)]
    pub time_limit_seconds: Option<u32>,
    pub sides: Sides,
    #[serde(default)]
    pub objectives: Vec<ControlPoint>,
    #[serde(default)]
    pub facilities: Vec<Facility>,
}

// ---------- Runtime State ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WeaponState {
    Deployed,
    Locked,
    Cooling,
}

/// Live unit during a match (scenario unit + mutable runtime fields).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeUnit {
    pub id: String,
    pub side: SideId,
    pub unit_type: UnitType,
    pub armor: Armor,
    pub teams: u8,
    pub pos: Axial,
    pub facing: u8,
    pub state: UnitState,
    pub weapon_state: WeaponState,
    /// Logical clock tick at which the current transition completes; 0 if idle.
    pub busy_until: Tick,
    /// Logical clock tick until which the unit is suppressed; 0 if not suppressed.
    pub suppressed_until: Tick,
    pub alive: bool,
    /// 三.2 carriage: the id of the vehicle currently carrying this unit (the unit rides "inside"),
    /// or `None` when on the ground. A carried unit follows its vehicle's hex and cannot be ordered
    /// independently — only `dismount`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carried_by: Option<String>,
    /// 三.13b 无人战车 隶属关系: the id of the 有人战车 (manned vehicle) this UGV belongs to. Unlike
    /// `carried_by` (which clears on dismount) this is a permanent command link: it gates whom a
    /// dismounted UGV may guide (三.13c / 三.14b) and means the UGV is annihilated with it (三.13d).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub affiliated_to: Option<String>,
    /// 三.16 运输直升机 altitude (`Low`/低空 for every non-heli). Drives LOS-over-terrain via
    /// `air.heli_altitude_meters`, so a 超低空 heli is correctly harder to see and sees less.
    #[serde(default, skip_serializing_if = "HeliAlt::is_low")]
    pub heli_alt: HeliAlt,
    /// 三.20 工事: the id of the 工事 (fortification) this unit is garrisoned inside, or `None` when
    /// on the open board. Like `carried_by`, a garrisoned unit is 全程隐蔽 (not independently
    /// observable/targetable) and cannot be ordered except to `exit_facility`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inside_facility: Option<String>,
    /// 三.1.4 活体疲劳 (infantry charge fatigue). Accrues +`fatigue_per_hex` per 冲锋 hex entered,
    /// decays −1 every `infantry_fatigue_decay_seconds` while idle. `0` = 无疲劳, `1` = 一级疲劳
    /// (bars 二级冲锋), `≥2` = 二级疲劳 (bars all movement until it decays). Only ever non-zero for
    /// infantry; gated by `mechanics::infantry_mode_allowed`.
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub fatigue: i64,
}

/// serde skip helper: omit `fatigue` from snapshots when it is the `0` default.
fn is_zero_i64(v: &i64) -> bool {
    *v == 0
}

impl RuntimeUnit {
    /// Whether the unit is independently present on the open board — NOT riding inside a vehicle
    /// (三.2) and NOT garrisoned inside a 工事 (三.20). Such a unit is the one that observation,
    /// targeting, 同格交战, stacking and capture all count; a 被载/驻守 unit is hidden inside and is
    /// surfaced only by its carrier / on exit (rule #5).
    pub fn is_on_board(&self) -> bool {
        self.carried_by.is_none() && self.inside_facility.is_none()
    }
}

/// 三.20 工事 (a fortification on the board). Loaded from `Scenario.facilities`; units garrison it
/// via `enter_facility` and leave via `exit_facility`. Keyed by a stable engine-assigned id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeFacility {
    pub id: String,
    /// Scenario `kind` string (e.g. "works_infantry_hidden") — parsed to a `FortKind` by mechanics.
    pub kind: String,
    pub at: Axial,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<SideId>,
    /// 三.20d garrison capacity in 班 (squads); defaults to the rules `fortification.capacity_squads`.
    /// For a 车辆工事 it SHRINKS as the 工事 is hit (三.20.1c); for a 人员工事 it is the fixed limit.
    pub capacity: u8,
    /// 三.20.1d/20.2d 工事防护 (heavy/medium): the armor applied when the 工事 itself is fired upon
    /// (NOT the occupant's). `Armor::None` for an un-armoured 工事.
    #[serde(default = "armor_none")]
    pub armor: Armor,
}

fn armor_none() -> Armor {
    Armor::None
}

/// 三.21 雷场 (a minefield on one hex). Loaded from `Scenario.facilities` (kind "minefield") and laid
/// at runtime by a 火箭布雷车. A mined hex damages any 地面单位 that enters it (附4) — for BOTH sides
/// (三.21e) — and persists until a 通路 is cleared through it (三.21f, the `cleared` flag).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeMinefield {
    pub at: Axial,
    /// The side that laid it (mines affect both sides regardless); `None` for a neutral/preset field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<SideId>,
    /// 三.21f/h — the sides that have opened a 通路 (cleared lane) through this hex by passing at half
    /// speed with a 扫雷车/坦克. A lane is single-side-visible: only a side IN this set passes without
    /// a 雷场裁决 (人员 any speed, 车辆 at half speed, 三.21i); the other side still adjudicates.
    /// (The lane is modelled per-hex per-side; the 同道路 directional binding 三.21g is deferred.)
    #[serde(default, skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    pub cleared_by: std::collections::BTreeSet<SideId>,
}

/// Full match state. All collections are ordered for determinism.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    /// Logical clock in centisecond ticks (see [`crate::time`]). Never f64.
    pub clock: Tick,
    pub units: BTreeMap<String, RuntimeUnit>,
    /// Control-point id -> owner side (or None for neutral).
    pub control: BTreeMap<String, Option<SideId>>,
    /// 三.20 工事 keyed by engine-assigned id; ordered for determinism.
    #[serde(default)]
    pub facilities: BTreeMap<String, RuntimeFacility>,
    /// 三.21 雷场 keyed by hex; ordered for determinism.
    #[serde(default)]
    pub minefields: BTreeMap<Axial, RuntimeMinefield>,
}

impl State {
    pub fn units_of(&self, side: SideId) -> impl Iterator<Item = &RuntimeUnit> {
        self.units
            .values()
            .filter(move |u| u.side == side && u.alive)
    }
}
