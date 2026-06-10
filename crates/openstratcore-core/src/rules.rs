//! Rules-as-data. Mirrors schemas/rules.schema.json. Flexible numeric blocks
//! (attack-level arrays, result-table cells, modifiers) stay as `serde_json::Value`
//! so authors can extend tables in config without touching this struct.

use crate::EngineError;
use crate::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableSource {
    pub page: i64,
    #[serde(default)]
    pub appendix: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleTable {
    pub id: String,
    #[serde(default)]
    pub title_zh: String,
    pub source: TableSource,
    pub verified: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify_note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hex_size_meters: Option<i64>,
    #[serde(flatten)]
    pub data: BTreeMap<String, serde_json::Value>,
}

impl RuleTable {
    pub fn payload(&self, key: &str) -> Option<&serde_json::Value> {
        self.data.get(key)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tables {
    pub by_name: BTreeMap<String, RuleTable>,
}

impl Tables {
    pub const TABLE_FILES: [&'static str; 18] = [
        "observation_distance.json",
        "unit_speed.json",
        "terrain_movement.json",
        "attack_level_vs_personnel.json",
        "attack_level_vs_vehicle.json",
        "attack_level_infantry_vs_vehicle.json",
        "height_diff_correction.json",
        "result_vs_vehicle.json",
        "vehicle_loss_correction.json",
        "result_vs_personnel.json",
        "personnel_loss_correction.json",
        "indirect_scatter.json",
        "indirect_result.json",
        "indirect_correction.json",
        "aa_attack_level.json",
        "aa_result.json",
        "minefield.json",
        "timings.json",
    ];

    pub fn load_embedded() -> crate::Result<Tables> {
        let embedded: [(&str, &str); 18] = [
            (
                "observation_distance",
                include_str!("../../../config/tables/observation_distance.json"),
            ),
            (
                "unit_speed",
                include_str!("../../../config/tables/unit_speed.json"),
            ),
            (
                "terrain_movement",
                include_str!("../../../config/tables/terrain_movement.json"),
            ),
            (
                "attack_level_vs_personnel",
                include_str!("../../../config/tables/attack_level_vs_personnel.json"),
            ),
            (
                "attack_level_vs_vehicle",
                include_str!("../../../config/tables/attack_level_vs_vehicle.json"),
            ),
            (
                "attack_level_infantry_vs_vehicle",
                include_str!("../../../config/tables/attack_level_infantry_vs_vehicle.json"),
            ),
            (
                "height_diff_correction",
                include_str!("../../../config/tables/height_diff_correction.json"),
            ),
            (
                "result_vs_vehicle",
                include_str!("../../../config/tables/result_vs_vehicle.json"),
            ),
            (
                "vehicle_loss_correction",
                include_str!("../../../config/tables/vehicle_loss_correction.json"),
            ),
            (
                "result_vs_personnel",
                include_str!("../../../config/tables/result_vs_personnel.json"),
            ),
            (
                "personnel_loss_correction",
                include_str!("../../../config/tables/personnel_loss_correction.json"),
            ),
            (
                "indirect_scatter",
                include_str!("../../../config/tables/indirect_scatter.json"),
            ),
            (
                "indirect_result",
                include_str!("../../../config/tables/indirect_result.json"),
            ),
            (
                "indirect_correction",
                include_str!("../../../config/tables/indirect_correction.json"),
            ),
            (
                "aa_attack_level",
                include_str!("../../../config/tables/aa_attack_level.json"),
            ),
            (
                "aa_result",
                include_str!("../../../config/tables/aa_result.json"),
            ),
            (
                "minefield",
                include_str!("../../../config/tables/minefield.json"),
            ),
            (
                "timings",
                include_str!("../../../config/tables/timings.json"),
            ),
        ];

        let mut by_name = BTreeMap::new();
        for (stem, json) in embedded {
            let table = serde_json::from_str::<RuleTable>(json).map_err(|err| {
                EngineError::Rules(format!("failed to parse embedded table {stem}: {err}"))
            })?;
            if by_name.insert(stem.to_string(), table).is_some() {
                return Err(EngineError::Rules(format!(
                    "duplicate embedded table {stem}"
                )));
            }
        }

        if by_name.len() != Self::TABLE_FILES.len() {
            return Err(EngineError::Rules(format!(
                "expected {} embedded tables, loaded {}",
                Self::TABLE_FILES.len(),
                by_name.len()
            )));
        }

        Ok(Tables { by_name })
    }

    pub fn get(&self, name: &str) -> Option<&RuleTable> {
        self.by_name.get(name)
    }

    pub fn len(&self) -> usize {
        self.by_name.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }

    pub fn unverified(&self) -> Vec<&str> {
        self.by_name
            .iter()
            .filter_map(|(name, table)| (!table.verified).then_some(name.as_str()))
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Timing {
    #[serde(default = "tick_default")]
    pub decision_tick_seconds: f64,
    #[serde(flatten)]
    pub durations: BTreeMap<String, f64>,
}

fn tick_default() -> f64 {
    5.0
}

fn default_hex_size_meters() -> u32 {
    200
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Weapon {
    pub kind: String,
    pub range: i32,
    #[serde(default)]
    pub moving_fire: bool,
    // Contract-first: schemas/rules.schema.json ($defs/weapon) names these two fields in
    // camelCase (resultTable / attackLevelByDistance) while the rest stay snake_case, so we
    // rename per-field rather than rename_all (which would wrongly camelCase moving_fire).
    #[serde(default, rename = "resultTable")]
    pub result_table: Option<String>,
    /// Attack level by distance. Flat array, or rows per crew-count. Kept generic.
    #[serde(default, rename = "attackLevelByDistance")]
    pub attack_level_by_distance: serde_json::Value,
    #[serde(default)]
    pub shots: Option<u32>,
    #[serde(default)]
    pub interval: Option<u32>,
}

impl Weapon {
    /// Attack level at a given distance, reading the flat-array form.
    /// Row-per-crew form is resolved in mechanics where crew count is known.
    pub fn flat_attack_level(&self, distance: i32) -> Option<i64> {
        let arr = self.attack_level_by_distance.as_array()?;
        let v = arr.get(distance as usize)?;
        v.as_i64()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderSpec {
    pub kind: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbConfig {
    pub providers: BTreeMap<String, ProviderSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rules {
    pub format: String,
    pub version: u32,
    /// Physical hex size in metres (三.1.3 行军 km/h → 秒/格; tables also assume 200 m).
    #[serde(default = "default_hex_size_meters")]
    pub hex_size_meters: u32,
    pub timing: Timing,
    /// Movement block kept generic; mechanics read the fields it needs.
    pub movement: serde_json::Value,
    #[serde(default)]
    pub observation: serde_json::Value,
    /// 夺控/堆叠 constants (三.4/三.5). Generic so mechanics read the fields they need.
    #[serde(default)]
    pub control: serde_json::Value,
    /// 空中单位 constants (三.10–三.12: flight altitude, etc.). Generic block.
    #[serde(default)]
    pub air: serde_json::Value,
    /// 三.20 工事 constants (capacity, exposed range). Generic block.
    #[serde(default)]
    pub fortification: serde_json::Value,
    /// 三.21 雷场 constants (lay range/count, observe range). Generic block.
    #[serde(default)]
    pub minefield: serde_json::Value,
    /// Default weapons per ground unit type (三.15a 同格交战 auto-weapon-pick). Generic map.
    #[serde(default)]
    pub loadout: serde_json::Value,
    pub weapons: BTreeMap<String, Weapon>,
    #[serde(default)]
    pub combat_result_tables: serde_json::Value,
    #[serde(default)]
    pub modifiers: serde_json::Value,
    pub prob: ProbConfig,
}

impl Rules {
    pub fn from_json_str(s: &str) -> Result<Self> {
        Ok(serde_json::from_str(s)?)
    }

    pub fn load_embedded_tables() -> crate::Result<Tables> {
        Tables::load_embedded()
    }

    /// Look up a result table's `cells` block and dice count by table id.
    pub fn result_table(&self, id: &str) -> Option<&serde_json::Value> {
        self.combat_result_tables.get(id)
    }

    pub fn timing(&self, key: &str) -> Option<f64> {
        self.timing.durations.get(key).copied()
    }

    /// Max own ground units per hex (三.4a). `None` if the config omits it (the caller errors
    /// rather than assuming a default — rules-as-data, hard rule #2).
    pub fn stacking_cap(&self) -> Option<i64> {
        self.control
            .get("stacking_max_ground_units_per_hex")?
            .as_i64()
    }

    /// Hex radius around an objective center that must be free of enemy ground units to capture
    /// (三.5a; 1 = center + 6 neighbors). `None` if the config omits it.
    pub fn capture_radius(&self) -> Option<i64> {
        self.control.get("capture_no_enemy_radius_hexes")?.as_i64()
    }

    /// 三.17b — the per-算子 squad cap (车/班数上限) that a 聚合 must not exceed.
    pub fn max_squads(&self) -> Option<i64> {
        self.control.get("max_squads_per_unit")?.as_i64()
    }

    /// 三.20d — per-工事 garrison capacity in 人/车班数.
    pub fn fortification_capacity(&self) -> Option<i64> {
        self.fortification.get("capacity_squads")?.as_i64()
    }

    /// 三.20.1a — hex range at which a 车辆/战斗工事 can be observed & fired upon (隐蔽工事 is 本格-only).
    pub fn fortification_exposed_range(&self) -> Option<i64> {
        self.fortification.get("exposed_range_hexes")?.as_i64()
    }

    /// 三.21c — range (hexes) within which a 火箭布雷车 may lay a 雷场 hex.
    pub fn minefield_lay_range(&self) -> Option<i64> {
        self.minefield.get("lay_range_hexes")?.as_i64()
    }

    /// 三.21c — max 布雷 actions per 火箭布雷车.
    pub fn minefield_max_lays(&self) -> Option<i64> {
        self.minefield.get("max_lays_per_vehicle")?.as_i64()
    }

    /// 三.21a — 雷场 observation distance (遮蔽地形 halves it, applied by the observer logic).
    pub fn minefield_observe_range(&self) -> Option<i64> {
        self.minefield.get("observe_range_hexes")?.as_i64()
    }
}

/// 三.4 堆叠 + 三.5 夺控 (T1.9). Module-level so `cargo test rules::capture_and_stack` selects it.
#[cfg(test)]
#[test]
fn capture_and_stack() {
    use crate::engine::Engine;
    use crate::hex::Axial;
    use crate::mechanics::stacking_allows_entry;
    use crate::types::{
        ControlPoint, HexCell, Map, Scenario, ScenarioUnit, Side, SideId, Sides, Terrain,
        UnitState, UnitType,
    };
    use crate::EngineError;

    // A 7-hex row q=0..=6 (r=0), flat & open. The objective sits at the middle hex (5,0).
    let map = Map {
        format: "openstratcore.map".into(),
        version: 1,
        name: "cap".into(),
        elevation_unit_meters: Some(10),
        hexes: (0..=6)
            .map(|q| HexCell {
                q,
                r: 0,
                id: None,
                elevation: 0,
                terrain: Terrain::Open,
                road: None,
            })
            .collect(),
    };
    let cp = ControlPoint {
        id: "CP".into(),
        at: Axial::new(5, 0),
        owner: None,
        priority: None,
    };
    let unit = |id: &str, ut: UnitType, q: i32| ScenarioUnit {
        id: id.into(),
        unit_type: ut,
        armor: None,
        teams: 2,
        at: Axial::new(q, 0),
        facing: 0,
        state: Some(UnitState::Stopped),
        carried_by: None,
        affiliated_to: None,
    };
    let rules = Rules::from_json_str(include_str!("../../../config/rules.default.json")).unwrap();
    let make = |red: Vec<ScenarioUnit>, blue: Vec<ScenarioUnit>| {
        let scenario = Scenario {
            format: "openstratcore.scenario".into(),
            version: 1,
            name: "cap".into(),
            map: "cap".into(),
            rules: None,
            time_limit_seconds: None,
            sides: Sides {
                red: Side {
                    name: "Red".into(),
                    units: red,
                },
                blue: Side {
                    name: "Blue".into(),
                    units: blue,
                },
            },
            objectives: vec![cp.clone()],
            facilities: vec![],
        };
        Engine::new(map.clone(), scenario, rules.clone(), 1).unwrap()
    };
    let capture = |target: &str| serde_json::json!({ "op": "capture", "unitId": target });
    let at = |q| Axial::new(q, 0);

    // ---------- 三.4 堆叠 ----------
    // (0,0): a full stack of 4 ground units. (1,0): 3 tanks + 1 air unit.
    let e = make(
        vec![
            unit("S1", UnitType::Tank, 0),
            unit("S2", UnitType::Tank, 0),
            unit("S3", UnitType::Tank, 0),
            unit("S4", UnitType::Tank, 0),
            unit("A1", UnitType::Uav, 1),
            unit("G1", UnitType::Tank, 1),
            unit("G2", UnitType::Tank, 1),
            unit("G3", UnitType::Tank, 1),
        ],
        vec![],
    );
    let cap = e
        .rules
        .stacking_cap()
        .expect("config carries the stacking cap");
    assert_eq!(cap, 4, "三.4a cap is 4 ground units per hex");
    // The 5th own ground unit is refused entry to a hex already holding 4.
    assert!(!stacking_allows_entry(
        &e.state,
        SideId::Red,
        at(0),
        "S5",
        cap
    ));
    // But the 4 already there are legal — the mover is excluded from its own count.
    assert!(stacking_allows_entry(
        &e.state,
        SideId::Red,
        at(0),
        "S4",
        cap
    ));
    // Air units do NOT occupy a ground slot: a 4th tank may still enter the 3-tank+UAV hex.
    assert!(stacking_allows_entry(
        &e.state,
        SideId::Red,
        at(1),
        "G4",
        cap
    ));

    // ---------- 三.5 夺控 ----------
    // (a) zone clear (nearest enemy 5 hexes away) → capture flips control to Red.
    let mut e = make(
        vec![unit("CAP", UnitType::Infantry, 5)],
        vec![unit("E_far", UnitType::Tank, 0)],
    );
    e.submit(SideId::Red, &capture("CAP"), 0).unwrap();
    assert_eq!(e.state.control["CP"], Some(SideId::Red));

    // (b) an enemy ground unit one hex away (4,0) contests the point → refused, control unchanged.
    let mut e = make(
        vec![unit("CAP", UnitType::Infantry, 5)],
        vec![unit("E_adj", UnitType::Tank, 4)],
    );
    assert!(matches!(
        e.submit(SideId::Red, &capture("CAP"), 0),
        Err(EngineError::Command(_))
    ));
    assert_eq!(e.state.control["CP"], None);

    // (c) 三.5c: 空中单位 and 炮兵 cannot capture even a clear point.
    for ut in [UnitType::Uav, UnitType::Artillery] {
        let mut e = make(vec![unit("X", ut, 5)], vec![]);
        assert!(e.submit(SideId::Red, &capture("X"), 0).is_err());
        assert_eq!(e.state.control["CP"], None);
    }

    // (d) 三.5b/d: a 压制 unit still captures — no wait, no need to be stopped.
    let mut e = make(vec![unit("CAP", UnitType::Infantry, 5)], vec![]);
    if let Some(u) = e.state.units.get_mut("CAP") {
        u.state = UnitState::Suppressed;
        u.suppressed_until = 9_999;
    }
    e.submit(SideId::Red, &capture("CAP"), 0).unwrap();
    assert_eq!(e.state.control["CP"], Some(SideId::Red));

    // (e) a ground unit NOT on the objective center cannot capture it.
    let mut e = make(vec![unit("CAP", UnitType::Infantry, 4)], vec![]);
    assert!(e.submit(SideId::Red, &capture("CAP"), 0).is_err());
    assert_eq!(e.state.control["CP"], None);

    // (f) a destroyed unit cannot capture (require_own_unit rejects dead units).
    let mut e = make(vec![unit("CAP", UnitType::Infantry, 5)], vec![]);
    if let Some(u) = e.state.units.get_mut("CAP") {
        u.alive = false;
    }
    assert!(e.submit(SideId::Red, &capture("CAP"), 0).is_err());
    assert_eq!(e.state.control["CP"], None);
}

/// 三.17 聚合 / 解聚 (T3.5): the pure squad/ammo arithmetic — 聚合 sums squads (capped) and
/// floor-averages ammo (三.17g); 解聚 splits 4=2+2/3=2+1/2=1+1 keeping ammo (三.17g); 炮兵 unsupported
/// (三.17i). The 75 s timer / 压制·同格 interruption / 巡飞弹·乘员 handling (三.17b/d/e/f) are the
/// engine op's job, deferred.
#[cfg(test)]
#[test]
fn aggregate_split() {
    use crate::mechanics::{aggregate, split, split_squads, supports_aggregation};
    use crate::types::UnitType;

    // 三.17i: 炮兵 cannot 聚合/解聚; other ground 算子 can; 空中单位 cannot (not 地面算子).
    assert!(!supports_aggregation(UnitType::Artillery));
    assert!(supports_aggregation(UnitType::Infantry));
    assert!(supports_aggregation(UnitType::Tank));
    assert!(supports_aggregation(UnitType::AaMissileSquad));
    assert!(!supports_aggregation(UnitType::Uav));
    assert!(!supports_aggregation(UnitType::AttackHeli));

    // The squad cap is rules-as-data (三.17b 总班数 ≤4).
    let rules = Rules::from_json_str(include_str!("../../../config/rules.default.json")).unwrap();
    let cap = rules.max_squads().unwrap() as u8;
    assert_eq!(cap, 4);

    // 三.17b/g 聚合: squads sum (≤ cap), ammo = ⌊(a+b)/2⌋.
    assert_eq!(aggregate(2, 2, 4, 4, cap), Some((4, 4))); // 2+2=4, ⌊(4+4)/2⌋=4
    assert_eq!(aggregate(1, 2, 3, 4, cap), Some((3, 3))); // ⌊7/2⌋=3 (floor)
    assert_eq!(aggregate(1, 1, 0, 5, cap), Some((2, 2))); // ⌊5/2⌋=2
    assert_eq!(aggregate(3, 2, 4, 4, cap), None); // 5 > cap 4 → refused (三.17b)
    assert_eq!(aggregate(0, 2, 4, 4, cap), None); // a 0-squad 算子 is not a participant

    // 三.17c 解聚 modes: 4→2+2, 3→2+1, 2→1+1; a 1-squad 算子 cannot split; an over-cap n is rejected.
    assert_eq!(split_squads(4, cap), Some((2, 2)));
    assert_eq!(split_squads(3, cap), Some((2, 1)));
    assert_eq!(split_squads(2, cap), Some((1, 1)));
    assert_eq!(split_squads(1, cap), None);
    assert_eq!(split_squads(cap + 1, cap), None); // no valid 算子 exceeds the cap

    // 三.17g: BOTH halves keep the ORIGINAL ammo (split does NOT divide ammo).
    assert_eq!(split(4, 7, cap), Some(((2, 7), (2, 7))));
    assert_eq!(split(3, 5, cap), Some(((2, 5), (1, 5))));
    assert_eq!(split(1, 9, cap), None);

    // 聚合 then 解聚 round-trips the squad count (ammo follows the two different 三.17g rules).
    let (sq, _ammo) = aggregate(2, 2, 6, 2, cap).unwrap();
    assert_eq!(split_squads(sq, cap), Some((2, 2)));
}

/// 三.20 工事 (T4.1): the three kinds' garrison eligibility, exposed observe/fire range (12 vs 本格),
/// fire-out / inheritance / terrain rules, and the 超容退出 rule. The 进入/离开 75 s ops + hiding from
/// observation + same-hex-on-entry (三.20.3d) are the engine op's job, deferred.
#[cfg(test)]
#[test]
fn fortifications() {
    use crate::mechanics::{
        fortification_admits, fortification_can_engage, fortification_inherits_fire,
        fortification_observe_range, fortification_over_capacity, fortification_terrain_applies,
        FortKind::{PersonnelCombat, PersonnelConcealment, Vehicle},
    };
    use crate::types::UnitType;

    // rules-as-data: capacity 5 (三.20d), exposed range 12 (三.20.1a), transition 75 (三.20b).
    let rules = Rules::from_json_str(include_str!("../../../config/rules.default.json")).unwrap();
    let cap = rules.fortification_capacity().unwrap();
    let range = rules.fortification_exposed_range().unwrap();
    assert_eq!(cap, 5);
    assert_eq!(range, 12);
    assert_eq!(rules.timing("fortification_transition"), Some(75.0));

    // 三.20c: 人员→人员工事, 车辆→车辆工事; 空中单位 never.
    assert!(fortification_admits(Vehicle, UnitType::Tank));
    assert!(fortification_admits(Vehicle, UnitType::AaGun)); // 高炮 is a 车辆单位
    assert!(!fortification_admits(Vehicle, UnitType::Infantry));
    assert!(fortification_admits(PersonnelCombat, UnitType::Infantry));
    assert!(fortification_admits(
        PersonnelCombat,
        UnitType::AaMissileSquad
    )); // 导弹小队 is 人员
    assert!(fortification_admits(
        PersonnelConcealment,
        UnitType::Infantry
    ));
    assert!(!fortification_admits(PersonnelConcealment, UnitType::Tank));
    assert!(!fortification_admits(Vehicle, UnitType::Uav)); // 空中单位 never

    // 三.20.1a/20.3a: 车辆/战斗工事 expose at `range`; a 隐蔽工事 only 本格 (0).
    assert_eq!(fortification_observe_range(Vehicle, range), 12);
    assert_eq!(fortification_observe_range(PersonnelCombat, range), 12);
    assert_eq!(fortification_observe_range(PersonnelConcealment, range), 0);

    // 三.20.1b/20.3b/c: fire out of / be fired upon — yes for 车辆/战斗, no for 隐蔽.
    assert!(fortification_can_engage(Vehicle));
    assert!(fortification_can_engage(PersonnelCombat));
    assert!(!fortification_can_engage(PersonnelConcealment));

    // 三.20.1e/20.2e: a 车辆 in 车辆工事 does NOT inherit fire; 人员 in 战斗工事 DO.
    assert!(!fortification_inherits_fire(Vehicle));
    assert!(fortification_inherits_fire(PersonnelCombat));
    assert!(!fortification_inherits_fire(PersonnelConcealment));

    // 三.20.3a: a 隐蔽工事 ignores its hex terrain; the others don't.
    assert!(fortification_terrain_applies(Vehicle));
    assert!(fortification_terrain_applies(PersonnelCombat));
    assert!(!fortification_terrain_applies(PersonnelConcealment));

    // 三.20e/f: garrison 班数 over the (possibly shrunk) remaining capacity ⇒ all expelled, exposed.
    assert!(!fortification_over_capacity(3, 5)); // fits
    assert!(!fortification_over_capacity(5, 5)); // exactly full is fine
    assert!(fortification_over_capacity(6, 5)); // over the base cap
    assert!(fortification_over_capacity(3, 2)); // capacity shrank below the garrison ⇒ expel
}

/// 三.21 雷场 流水线 E (T4.2): the 附4 损伤+装甲修正 adjudication (exact cells), the 车辆/人员 category
/// split, and the 布雷/开辟 unit predicates + rules-as-data constants. The 通路单向/单侧可见
/// (三.21g/h), 半速沿通路免裁决 (三.21i) and 布雷车 75 s×3 op are the engine's job, deferred.
#[cfg(test)]
#[test]
fn minefield() {
    use crate::combat::{minefield_category, minefield_damage, minefield_damage_for_roll};
    use crate::mechanics::{can_clear_minefield, can_lay_mines};
    use crate::rng::PcgRng;
    use crate::types::{Armor, UnitType};

    // rules-as-data (三.21a/c): lay range 50, max 3 lays, observe 10, lay time 75 s.
    let rules = Rules::from_json_str(include_str!("../../../config/rules.default.json")).unwrap();
    assert_eq!(rules.minefield_lay_range(), Some(50));
    assert_eq!(rules.minefield_max_lays(), Some(3));
    assert_eq!(rules.minefield_observe_range(), Some(10));
    assert_eq!(rules.timing("minefield_lay"), Some(75.0));

    // 附4.1 category split: 步兵/防空导弹小队 are 人员, everything else on the ground is 车辆.
    assert_eq!(minefield_category(UnitType::Tank), "车辆");
    assert_eq!(minefield_category(UnitType::AaGun), "车辆");
    assert_eq!(minefield_category(UnitType::Infantry), "人员");
    assert_eq!(minefield_category(UnitType::AaMissileSquad), "人员");

    let tables = Tables::load_embedded().unwrap();
    let veh = UnitType::Tank; // a 车辆-category unit; 人员 uses 步兵
    let inf = UnitType::Infantry;
    let d = |ut, armor, roll| minefield_damage_for_roll(&tables, ut, armor, roll);
    // 附4.1 base damage (车辆): only rolls 6/7 (=1), 11 (=2), 12 (=1) hurt; safe rolls are 0.
    assert_eq!(d(veh, Armor::None, 4), 0); // safe cell
    assert_eq!(d(veh, Armor::None, 6), 1); // base 1, 无装甲 corr 0
    assert_eq!(d(inf, Armor::None, 6), 1);
    assert_eq!(d(veh, Armor::Heavy, 7), 1); // base 1, 重型 null → 0
                                            // 附4.2 armor correction stacks on the base (floored at 0):
    assert_eq!(d(veh, Armor::Composite, 11), 1); // base 2 + 复合 -1
    assert_eq!(d(veh, Armor::None, 2), 2); // base 0 + 无装甲 +2
    assert_eq!(d(veh, Armor::Composite, 2), 0); // base 0 + 复合 -2 → floored to 0
    assert_eq!(d(veh, Armor::Medium, 12), 2); // base 1 + 中型 +1
    assert_eq!(d(veh, Armor::Light, 2), 1); // base 0 + 轻型 +1

    // 三.21b/c/f: who lays / who clears a path.
    assert!(can_lay_mines(UnitType::Minelayer));
    assert!(!can_lay_mines(UnitType::Tank));
    assert!(can_clear_minefield(UnitType::Minesweeper));
    assert!(can_clear_minefield(UnitType::Tank)); // 坦克 may open a path at half speed
    assert!(!can_clear_minefield(UnitType::Infantry));
    assert!(!can_clear_minefield(UnitType::Minelayer));

    // The 2d6 salvo form is deterministic for a fixed seed and matches its fixed-roll cell.
    let mut r = PcgRng::from_seed(9);
    let a = minefield_damage(&tables, &mut r, veh, Armor::Medium);
    let mut r2 = PcgRng::from_seed(9);
    assert_eq!(a, minefield_damage(&tables, &mut r2, veh, Armor::Medium));
}

/// 一.1.3 间瞄炮火区 (T4.4): a 地面算子 crossing into/out of an active barrage zone takes an 间瞄火力
/// 裁决 — the trigger (ground-only) and the adjudication reusing the 间瞄 pipeline (附2 / T2.3). The
/// zone as a facility on the board + the enter/exit detection are the engine's job, deferred.
#[cfg(test)]
#[test]
fn barrage_zone() {
    use crate::combat::{barrage_zone_adjudicate, GunClass, IndirectMods, TargetTerrain};
    use crate::mechanics::barrage_zone_triggers;
    use crate::prob::Outcome;
    use crate::rng::PcgRng;
    use crate::types::{Armor, UnitType};

    // 一.1.3: only 地面算子 are adjudicated crossing a 间瞄炮火区; 空中算子 fly over untouched.
    assert!(barrage_zone_triggers(UnitType::Tank));
    assert!(barrage_zone_triggers(UnitType::Infantry));
    assert!(barrage_zone_triggers(UnitType::AaGun));
    assert!(!barrage_zone_triggers(UnitType::Uav));
    assert!(!barrage_zone_triggers(UnitType::AttackHeli));
    assert!(!barrage_zone_triggers(UnitType::LoiteringMunition));
    assert!(!barrage_zone_triggers(UnitType::TransportHeli));

    let tables = Tables::load_embedded().unwrap();
    let mods = IndirectMods {
        target_terrain: TargetTerrain::Open,
        target_cover: false,
        target_moving_personnel: true,
        target_stacked: false,
        target_march: false,
        armor: Armor::None,
        gun_count: 4,
    };

    // Determinism: same seed ⇒ same Outcome (replay-safe).
    let run = |seed| {
        let mut r = PcgRng::from_seed(seed);
        barrage_zone_adjudicate(&tables, &mut r, GunClass::Heavy, &mods)
    };
    assert_eq!(run(11), run(11));

    // The crossing genuinely runs the 附2 result pipeline: across many rolls an exposed (开阔/机动/
    // 无装甲) target sees BOTH 毁伤 and non-毁伤 verdicts — the table drives it, not a constant.
    let mut r = PcgRng::from_seed(3);
    let (mut destroyed, mut other) = (false, false);
    for _ in 0..400 {
        match barrage_zone_adjudicate(&tables, &mut r, GunClass::Heavy, &mods) {
            Outcome::Destroyed(n) => {
                assert!((1..=5).contains(&n));
                destroyed = true;
            }
            Outcome::Suppress | Outcome::NoEffect => other = true,
            Outcome::Kill => {}
        }
    }
    assert!(
        destroyed && other,
        "一.1.3: the 附2 result table drives a mix of 毁伤/压制/无效 on crossing"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = include_str!("../../../config/rules.default.json");

    #[test]
    fn loads_default_rules() {
        let r = Rules::from_json_str(SAMPLE).expect("parse default rules");
        assert_eq!(r.format, "openstratcore.rules");
        assert!(r.weapons.contains_key("tank_main_big"));
        assert_eq!(r.timing("move_to_stop"), Some(75.0));
        assert!(r.prob.providers.contains_key("direct_vs_vehicle"));
    }

    #[test]
    fn weapon_attack_level_decreases_with_distance() {
        let r = Rules::from_json_str(SAMPLE).unwrap();
        let w = &r.weapons["tank_main_big"];
        assert_eq!(w.flat_attack_level(0), Some(10));
        assert!(w.flat_attack_level(0).unwrap() >= w.flat_attack_level(18).unwrap());
    }

    #[test]
    fn loads_all_18_tables() {
        let t = Tables::load_embedded().expect("load all 18 tables");
        assert_eq!(t.len(), 18, "must deserialize all 18 decision tables");
        for name in Tables::TABLE_FILES
            .iter()
            .map(|f| f.trim_end_matches(".json"))
        {
            let tbl = t
                .get(name)
                .unwrap_or_else(|| panic!("missing table {name}"));
            assert!(!tbl.data.is_empty(), "{name} has no payload");
            assert_eq!(tbl.hex_size_meters, Some(200), "{name} hex_size_meters");
        }
        assert!(t.get("unit_speed").unwrap().payload("units").is_some());
        assert!(t.get("timings").unwrap().payload("seconds").is_some());
        assert!(t
            .get("observation_distance")
            .unwrap()
            .payload("rows")
            .is_some());
        assert!(t
            .get("result_vs_vehicle")
            .unwrap()
            .payload("data")
            .is_some());
        // result_vs_vehicle's two-stage cells are entered + cross-verified; whatever tables
        // are still unverified must all be in the known verification backlog (it only shrinks).
        assert!(t.get("result_vs_vehicle").unwrap().verified);
        assert!(t.get("unit_speed").unwrap().verified);
        let backlog = [
            "attack_level_vs_vehicle",
            "height_diff_correction",
            "result_vs_personnel",
        ];
        for name in t.unverified() {
            assert!(
                backlog.contains(&name),
                "unexpected unverified table {name}"
            );
        }
    }
}
