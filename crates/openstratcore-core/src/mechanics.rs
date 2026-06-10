//! Game mechanics. Foundations (terrain/slope helpers, direct-fire wiring) are real;
//! LOS and observation are stubs being grown via `/add-rule` (rules 6/7).
//! Each entry point cites the ruleset section it implements.

use crate::hex::Axial;
use crate::prob::{build_provider, Outcome, ResolveContext};
use crate::rng::Rng;
use crate::rules::Rules;
use crate::types::{HexCell, Map, RuntimeUnit, SideId, State, Terrain, UnitState, UnitType};
use crate::{EngineError, Result};

/// Rule 1 — terrain time multiplier for vehicle movement (>1 = slower).
pub fn terrain_speed_factor(rules: &Rules, terrain: Terrain) -> f64 {
    let key = match terrain {
        Terrain::Forest => "forest",
        Terrain::Urban => "urban",
        Terrain::River => "river",
        Terrain::RiverLarge => "river_large",
        Terrain::Lake => "lake",
        Terrain::Soft => "soft",
        // Open, Road, Rail: no terrain penalty.
        Terrain::Open | Terrain::Road | Terrain::Rail => return 1.0,
    };
    rules
        .movement
        .get("terrain_speed_factor")
        .and_then(|t| t.get(key))
        .and_then(|v| v.as_f64())
        .unwrap_or(1.0)
}

/// Rule 1 — slope level from elevation difference (in elevation units).
pub fn slope_level(from_elev: i32, to_elev: i32) -> i32 {
    (to_elev - from_elev).abs()
}

/// Rule 1 — time in seconds to move ONE hex step, or None if impassable (slope too steep).
pub fn step_time_seconds(
    rules: &Rules,
    unit_type: UnitType,
    terrain: Terrain,
    from_elev: i32,
    to_elev: i32,
) -> Result<Option<f64>> {
    let base = base_speed(rules, unit_type)
        .ok_or_else(|| EngineError::Rules(format!("no base speed for {unit_type:?}")))?;

    // 沿路免修正 (三.1.2): on a road/rail hex a vehicle ignores terrain AND elevation
    // (不受高差影响) — so it moves at base speed and is NOT blocked by the off-road
    // impassable-slope limit (the road carries the gradient via bridges/switchbacks).
    // (Precise "along the road network" adjacency belongs to the pathing layer; here a
    // road/rail hex is the on-road condition, and roadblocks — 路障不可通行 — are enforced
    // where pathing resolves the move, not in this per-step speed helper.)
    if matches!(terrain, Terrain::Road | Terrain::Rail) {
        return Ok(Some(base));
    }

    let slope = slope_level(from_elev, to_elev);
    let impassable = rules
        .movement
        .get("slope_rule")
        .and_then(|s| s.get("impassable_above_level"))
        .and_then(|v| v.as_i64())
        .unwrap_or(5);
    if slope as i64 > impassable {
        return Ok(None);
    }
    let slope_factor = rules
        .movement
        .get("slope_rule")
        .and_then(|s| s.get("factor_per_level"))
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.get((slope.max(1) - 1) as usize))
        .and_then(|v| v.as_f64())
        .unwrap_or(1.0);

    Ok(Some(
        base * terrain_speed_factor(rules, terrain) * slope_factor,
    ))
}

fn base_speed(rules: &Rules, unit_type: UnitType) -> Option<f64> {
    let key = unit_type_key(unit_type);
    rules
        .movement
        .get("base_speed_sec_per_hex")
        .and_then(|m| m.get(key))
        .and_then(|v| v.as_f64())
}

/// The config key for a unit type (`base_speed_sec_per_hex`, `loadout`, …).
pub fn unit_type_key(t: UnitType) -> &'static str {
    use UnitType::*;
    match t {
        Tank => "tank",
        Ifv => "ifv",
        Infantry => "infantry",
        Artillery => "artillery",
        Uav => "uav",
        AttackHeli => "attack_heli",
        TransportHeli => "transport_heli",
        LoiteringMunition => "loitering_munition",
        ReconVehicle => "recon_vehicle",
        RadarVehicle => "radar_vehicle",
        Minelayer => "minelayer",
        Minesweeper => "minesweeper",
        AaGun => "aa_gun",
        AaMissileSquad => "aa_missile_squad",
        AaMissileVehicle => "aa_missile_vehicle",
        Ugv => "ugv",
        Pickup => "pickup",
        SpaceRecon => "space_recon",
    }
}

/// Rule 三.1.4 — speed multiplier (×base speed) for an infantry movement mode.
fn infantry_charge_speed_mult(rules: &Rules, key: &str) -> f64 {
    rules
        .movement
        .get("infantry_charge")
        .and_then(|c| c.get(key))
        .and_then(|c| c.get("speed_mult"))
        .and_then(|v| v.as_f64())
        .unwrap_or(1.0)
}

/// Rule 三.1.4 — time in seconds for ONE hex step on foot, or None if `mode` is not a
/// moving posture. Personnel are **terrain-independent** (no terrain factor enters here);
/// the only environmental modifier is 高差: an elevation difference over
/// `infantry_highdiff_meters_halfspeed` metres halves speed. Charge modes multiply speed
/// (一级 ×2 / 二级 ×4), 半速 halves it.
pub fn infantry_step_time_seconds(
    rules: &Rules,
    elevation_unit_meters: i32,
    from_elev: i32,
    to_elev: i32,
    mode: UnitState,
) -> Result<Option<f64>> {
    let base = base_speed(rules, UnitType::Infantry)
        .ok_or_else(|| EngineError::Rules("no base speed for infantry".into()))?;

    let speed_mult = match mode {
        UnitState::Normal | UnitState::Moving | UnitState::March => 1.0,
        UnitState::Half => 0.5,
        UnitState::Charge1 => infantry_charge_speed_mult(rules, "charge1"),
        UnitState::Charge2 => infantry_charge_speed_mult(rules, "charge2"),
        // 掩蔽 / 停止 / 被压制 are not moving postures.
        UnitState::Cover | UnitState::Stopped | UnitState::Suppressed => return Ok(None),
    };
    if speed_mult <= 0.0 {
        return Ok(None);
    }

    // Widen to i64 before subtracting so extreme i32 elevations cannot overflow/panic.
    let diff_m =
        (i64::from(to_elev) - i64::from(from_elev)).abs() * i64::from(elevation_unit_meters);
    let threshold = rules
        .movement
        .get("infantry_highdiff_meters_halfspeed")
        .and_then(|v| v.as_i64())
        .unwrap_or(60);
    let highdiff_time_factor = if diff_m > threshold { 2.0 } else { 1.0 };

    Ok(Some(base / speed_mult * highdiff_time_factor))
}

/// Rule 三.1.4 — fatigue gained per hex moved in `mode` (一/二级冲锋 accrue fatigue).
pub fn infantry_fatigue_per_hex(rules: &Rules, mode: UnitState) -> i64 {
    let key = match mode {
        UnitState::Charge1 => "charge1",
        UnitState::Charge2 => "charge2",
        _ => return 0,
    };
    rules
        .movement
        .get("infantry_charge")
        .and_then(|c| c.get(key))
        .and_then(|c| c.get("fatigue_per_hex"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0)
}

/// Rule 三.1.4 — is `mode` permitted at the given fatigue level? The two fatigue levels are
/// structural rule constants (NOT config tunables): 一级疲劳 (fatigue ≥ 1) bars 二级冲锋, and
/// 二级疲劳 (fatigue ≥ 2) bars all movement. Non-moving postures are always allowed.
pub fn infantry_mode_allowed(mode: UnitState, fatigue: i64) -> bool {
    let is_move = matches!(
        mode,
        UnitState::Normal
            | UnitState::Moving
            | UnitState::March
            | UnitState::Half
            | UnitState::Charge1
            | UnitState::Charge2
    );
    if fatigue >= 2 && is_move {
        return false; // 二级疲劳禁机动
    }
    if fatigue >= 1 && matches!(mode, UnitState::Charge2) {
        return false; // 一级疲劳禁二级冲锋
    }
    true
}

/// Rule 三.1.4 — the 疲劳恢复 interval in seconds: every this many seconds idle removes 1 fatigue.
/// Floored at 1 so it is always a valid scheduling delay. Rules-as-data.
pub fn infantry_fatigue_decay_seconds(rules: &Rules) -> i64 {
    rules
        .movement
        .get("infantry_fatigue_decay_seconds")
        .and_then(|v| v.as_i64())
        .unwrap_or(75)
        .max(1)
}

/// Rule 三.1.4 — fatigue after resting `rest_seconds` (−1 per decay interval, floored at 0).
pub fn infantry_fatigue_after_rest(rules: &Rules, fatigue: i64, rest_seconds: i64) -> i64 {
    let interval = infantry_fatigue_decay_seconds(rules);
    (fatigue - rest_seconds / interval).max(0)
}

/// Rule 三.6 通视 (line of sight) between two hex centers. Adjacent hexes always have LOS
/// (相邻六角格总是通视); otherwise the straight center-to-center line is blocked iff some
/// intermediate hex rises strictly above the linearly-interpolated sight line. Per 高看低,
/// the higher piece is the observer and its endpoint elevation gains +1 level; intermediate
/// 居民地/丛林地 hexes add +1 to their blocking height (endpoints' own features are NOT
/// counted). All integer math — no float comparisons, so it is bit-deterministic.
pub fn line_of_sight(map: &Map, from: &Axial, to: &Axial) -> bool {
    line_of_sight_alt(map, from, to, 0, 0)
}

/// [`line_of_sight`] with the endpoints raised by `from_alt`/`to_alt` elevation LEVELS — used for
/// 空中单位 flying at +200 m (三.10/三.11/三.12), which see over intervening terrain.
pub fn line_of_sight_alt(map: &Map, from: &Axial, to: &Axial, from_alt: i64, to_alt: i64) -> bool {
    // An off-map endpoint has no line of sight (a missing *intermediate* hex on a sparse
    // map is treated as transparent below — schema-valid maps are dense).
    let (Some(c_from), Some(c_to)) = (map.cell(from), map.cell(to)) else {
        return false;
    };
    if from.distance(to) <= 1 {
        return true; // 相邻六角格总是通视
    }
    let (e_from, e_to) = (
        i64::from(c_from.elevation) + from_alt,
        i64::from(c_to.elevation) + to_alt,
    );

    // 高看低: raise the higher endpoint by +1 level for the sight line. Keep the line's
    // own orientation (line[0] == from .. line[n] == to).
    let (sight_from, sight_to) = match e_from.cmp(&e_to) {
        std::cmp::Ordering::Greater => (e_from + 1, e_to),
        std::cmp::Ordering::Less => (e_from, e_to + 1),
        std::cmp::Ordering::Equal => (e_from, e_to),
    };

    let line = from.line_to(to);
    let n = (line.len() - 1) as i64; // == distance, which is > 1 here
    for (i, hex) in line.iter().enumerate() {
        if i == 0 || i as i64 == n {
            continue; // endpoints do not obstruct the view to themselves
        }
        let Some(cell) = map.cell(hex) else { continue };
        let feature_bonus = i64::from(matches!(cell.terrain, Terrain::Urban | Terrain::Forest));
        let blocking = i64::from(cell.elevation) + feature_bonus;
        // Sight elevation at step i = sight_from + (sight_to - sight_from) * i / n. Compare
        // without dividing: blocking > sight  <=>  blocking*n > sight_from*n + (Δ)*i.
        let sight_scaled = sight_from * n + (sight_to - sight_from) * i as i64;
        if blocking * n > sight_scaled {
            return false; // 被更高高程的六角格阻挡
        }
    }
    true
}

/// Observation category of a unit as an OBSERVER (rows of the 三.7 distance table).
fn observer_category(t: UnitType) -> &'static str {
    use UnitType::*;
    match t {
        Infantry => "infantry",
        AttackHeli | TransportHeli => "attack_heli",
        Uav => "uav",
        LoiteringMunition => "loitering_munition",
        ReconVehicle => "recon_vehicle",
        AaMissileSquad => "aa_missile_squad",
        AaMissileVehicle => "aa_missile_vehicle",
        _ => "vehicle",
    }
}

/// Observation category of a unit as a TARGET (columns 步兵/车辆/直升机运输机/无人机/巡飞弹).
fn target_category(t: UnitType) -> &'static str {
    use UnitType::*;
    match t {
        Infantry => "infantry",
        AttackHeli | TransportHeli => "attack_heli",
        Uav => "uav",
        LoiteringMunition => "loitering_munition",
        _ => "vehicle",
    }
}

/// Whether a unit counts as a 车辆 (vehicle) target — used by the 三.7 cover-negation rule and the
/// 三.1.3 行军 gate (only a 车辆 may march).
pub fn is_vehicle(t: UnitType) -> bool {
    matches!(target_category(t), "vehicle")
}

// ---------- 三.4 堆叠 / 三.5 夺控 ----------

/// A 空中单位 (air): drones, helicopters, loitering munitions. Everything else is a 地面单位
/// (ground unit), which is what stacking (三.4) counts and capture-blocking (三.5a) cares about.
pub fn is_air(t: UnitType) -> bool {
    matches!(
        t,
        UnitType::Uav
            | UnitType::AttackHeli
            | UnitType::TransportHeli
            | UnitType::LoiteringMunition
    )
}

/// A 地面单位 (ground unit) — anything not 空中.
pub fn is_ground(t: UnitType) -> bool {
    !is_air(t)
}

/// 三.10/三.11/三.12/三.16b — a 空中单位's flight altitude in elevation LEVELS above its hex, or
/// 0 for a ground unit. A 运输直升机 reads its CURRENT altitude band (高空/低空/超低空 → +500/200/20 m,
/// `air.heli_altitude_meters[u.heli_alt]`) so a 超低空 heli correctly sees less and is seen less;
/// every other air unit uses `air.recon_altitude_meters`. Level size comes from the map.
pub fn air_altitude_levels(rules: &Rules, map: &Map, u: &RuntimeUnit) -> i64 {
    if !is_air(u.unit_type) {
        return 0;
    }
    let meters = if u.unit_type == UnitType::TransportHeli {
        let band = match u.heli_alt {
            crate::types::HeliAlt::High => "high",
            crate::types::HeliAlt::Low => "low",
            crate::types::HeliAlt::VeryLow => "very_low",
        };
        rules
            .air
            .get("heli_altitude_meters")
            .and_then(|v| v.get(band))
            .and_then(|v| v.as_i64())
            .unwrap_or(0) // unreachable for engine games — Engine::new validates the air block
    } else {
        rules
            .air
            .get("recon_altitude_meters")
            .and_then(|v| v.as_i64())
            .unwrap_or(0) // unreachable for engine games — Engine::new validates the air block
    };
    let unit = map.elevation_unit_meters.map(i64::from).unwrap_or(0);
    if unit <= 0 {
        0
    } else {
        // Quantise the altitude to elevation levels, rounded to nearest so a unit that does not
        // divide 200 m isn't biased downward. The dominant effect (air >> terrain) is robust either
        // way: +200 m is ~20 levels at the usual 10 m unit, far above any hex.
        (meters + unit / 2) / unit
    }
}

/// 三.10a/三.11a/三.12a — seconds per hex for a 空中单位: base speed only, ignoring terrain, slope,
/// and impassability (it flies). `None` if no base speed is configured for the type.
pub fn air_step_time_seconds(rules: &Rules, t: UnitType) -> Option<f64> {
    base_speed(rules, t)
}

/// 三.5c — whether a unit may execute 夺控: 空中单位, 炮兵, and the 天基侦察算子 cannot (the latter is a
/// passive off-board support 算子, 三.22b); every other ground unit can.
pub fn can_capture(t: UnitType) -> bool {
    is_ground(t) && !matches!(t, UnitType::Artillery | UnitType::SpaceRecon)
}

// ---------- 三.17 聚合 / 解聚 ----------

/// 三.17a/i — which 算子 may 聚合/解聚: 地面算子 only (三.17a), and 炮兵暂不支持 (三.17i). The two
/// participants of a 聚合 must additionally be the SAME unit type (三.17b 同类型) — checked by the
/// caller against this predicate on both.
pub fn supports_aggregation(t: UnitType) -> bool {
    is_ground(t) && t != UnitType::Artillery
}

/// 三.17b/g — 聚合 (aggregate) two SAME-type same-hex 算子 with squad counts `a`,`b` and ammo
/// `ammo_a`,`ammo_b`, given the per-算子 squad cap `max_squads` (rules-as-data, see
/// `Rules::max_squads`). Returns the merged `(squads, ammo)`: squads = a+b (三.17b 车/班数累加, must
/// be ≤ `max_squads`), ammo = ⌊(ammo_a+ammo_b)/2⌋ (三.17g 取两者平均向下取整). `None` if either input
/// is 0 or the sum would exceed the cap. (Type/state/同格 eligibility is the engine's gate, 三.17b/d.)
pub fn aggregate(a: u8, b: u8, ammo_a: u32, ammo_b: u32, max_squads: u8) -> Option<(u8, u32)> {
    if a == 0 || b == 0 {
        return None;
    }
    let squads = a.checked_add(b)?;
    if squads > max_squads {
        return None;
    }
    // Widen to u64 so an extreme ammo count cannot overflow (rule #4: no panic in lib code); the
    // floored average is ≤ max(ammo_a, ammo_b) ≤ u32::MAX, so the cast back is lossless.
    let ammo = ((u64::from(ammo_a) + u64::from(ammo_b)) / 2) as u32;
    Some((squads, ammo))
}

/// 三.17c — 解聚 (split) the squads of an `n`-squad 算子 into two new 算子: 4→(2,2), 3→(2,1),
/// 2→(1,1) (the larger half leads). `None` if n is not splittable: a 1-squad 算子, or any n above
/// the per-算子 cap `max_squads` (no valid 算子 can hold more — guards against malformed input).
pub fn split_squads(n: u8, max_squads: u8) -> Option<(u8, u8)> {
    if n < 2 || n > max_squads {
        return None;
    }
    Some((n - n / 2, n / 2)) // 4→(2,2), 3→(2,1), 2→(1,1)
}

/// 三.17c/g — 解聚 an `(n, ammo)` 算子 into two new `(squads, ammo)` 算子 under the cap `max_squads`.
/// Squads split per [`split_squads`]; crucially BOTH new 算子 keep the ORIGINAL ammo count — 三.17g:
/// "解聚后两个新算子弹药数与原算子相同" (split does NOT divide ammo, unlike 聚合 which averages it).
pub fn split(n: u8, ammo: u32, max_squads: u8) -> Option<((u8, u32), (u8, u32))> {
    let (a, b) = split_squads(n, max_squads)?;
    Some(((a, ammo), (b, ammo)))
}

// ---------- 三.1.3 车辆行军 (march) ----------

/// 三.1.3a/b — seconds per hex for a 车辆 marching on a road of class `road_kind` (`country`/`normal`/
/// `grade`, matching `Road::kind` and `movement.march_speed_kmh`). Speed is `march_speed_kmh` over the
/// physical hex size (`hex_size_meters`); 三.1.3b 行军不受地形/高差影响 so terrain/slope never enter.
/// `None` if the class has no march speed (e.g. `rail` — vehicles don't march on rail).
pub fn march_step_time_seconds(rules: &Rules, road_kind: &str) -> Option<f64> {
    let kmh = rules
        .movement
        .get("march_speed_kmh")
        .and_then(|m| m.get(road_kind))
        .and_then(|v| v.as_f64())?;
    if kmh <= 0.0 {
        return None;
    }
    // metres / (km/h → m/s) = metres * 3600 / (kmh * 1000) = metres * 3.6 / kmh.
    Some(f64::from(rules.hex_size_meters) * 3.6 / kmh)
}

/// 三.1.3b/e — whether a `cell` is a valid march step toward edge direction `dir` (0..5): the cell
/// must carry a road that exits toward `dir` (只能沿道路走向规划行军路线，不能离开道路). A hex without a
/// road, or a road that doesn't connect that way, is not marchable in that direction.
pub fn road_allows_direction(cell: &HexCell, dir: u8) -> bool {
    cell.road
        .as_ref()
        .is_some_and(|r| r.connects.contains(&dir))
}

/// 三.1.3d — whether a marching unit may advance INTO `hex`: blocked if the hex holds any live,
/// non-carried unit that is NOT itself marching (一格存在停止的棋子或非行军的棋子则行军被阻挡). A column
/// of marching units does not self-block; carried passengers ride inside and don't count.
///
/// AUTHORITATIVE predicate over the TRUE board (like [`stacking_allows_entry`]): the engine
/// adjudicates the halt with it and MUST fog-filter the outward "march blocked" signal so a hidden
/// enemy blocker isn't revealed (rule #5). Side-agnostic per 三.1.3d's text — an enemy STOPPED unit
/// blocks; an enemy MARCHING unit does NOT block here, since entering its hex instead triggers 同格
/// 交战 (三.15) on arrival (a separate mechanic), so enemy contact is handled there.
pub fn march_allows_entry(state: &State, hex: Axial) -> bool {
    !state
        .units
        .values()
        .any(|u| u.alive && u.is_on_board() && u.pos == hex && u.state != UnitState::March)
}

// ---------- 三.20 工事 (fortifications) ----------

/// 三.20c — the three permanent-工事 kinds. 人员单位 enter 人员工事 (战斗/隐蔽); 车辆单位 (incl. a
/// troop-carrying vehicle, 三.20c 载人车辆按车辆处理) enter 车辆工事.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FortKind {
    /// 车辆工事 — vehicles; 12-hex hide/fire; the vehicle does NOT inherit fire results (三.20.1e).
    Vehicle,
    /// 人员战斗工事 — personnel; 12-hex hide/fire; the troops DO inherit fire results (三.20.2e).
    PersonnelCombat,
    /// 人员隐蔽工事 — personnel; observable/targetable only 本格, cannot fire out, terrain-immune (三.20.3).
    PersonnelConcealment,
}

impl FortKind {
    /// Parse a scenario `Facility.kind` string (schemas/scenario.schema.json) into a 工事 kind, or
    /// `None` if the facility is not a 工事 (e.g. a minefield/roadblock/indirect_zone).
    pub fn from_facility_kind(kind: &str) -> Option<Self> {
        match kind {
            "works_vehicle" => Some(Self::Vehicle),
            "works_infantry_combat" => Some(Self::PersonnelCombat),
            "works_infantry_hidden" => Some(Self::PersonnelConcealment),
            _ => None,
        }
    }
}

/// 人员单位 (班-based: 步兵 / 防空导弹小队) — everyone else on the ground is a 车辆单位.
fn is_personnel(t: UnitType) -> bool {
    matches!(t, UnitType::Infantry | UnitType::AaMissileSquad)
}

/// 三.20c — whether `unit_type` may garrison a `kind` 工事 (人员→人员工事, 车辆→车辆工事; 空中单位
/// never). A troop-carrying vehicle still counts as a 车辆 (三.20c) — carriage is handled by the
/// engine, this predicate sees the carrier's own type.
pub fn fortification_admits(kind: FortKind, unit_type: UnitType) -> bool {
    if !is_ground(unit_type) {
        return false;
    }
    match kind {
        FortKind::Vehicle => !is_personnel(unit_type),
        FortKind::PersonnelCombat | FortKind::PersonnelConcealment => is_personnel(unit_type),
    }
}

/// 三.20.1a/20.3a — the range (hexes) at which a garrisoned 工事 can be observed AND fired upon by
/// the enemy: `exposed_range` (rules-as-data, 12 格) for 车辆/战斗工事, but only 本格 (0) for a 隐蔽工事.
pub fn fortification_observe_range(kind: FortKind, exposed_range: i64) -> i64 {
    match kind {
        FortKind::Vehicle | FortKind::PersonnelCombat => exposed_range,
        FortKind::PersonnelConcealment => 0, // 本格 only (三.20.3a)
    }
}

/// 三.20.1b/20.3b/c — whether a garrison may 直瞄射击 out of its 工事 (and, symmetrically, be fired
/// upon): true for 车辆/战斗工事, false for 隐蔽工事 (全程隐蔽且不可射击/被射击, 三.20.3b/c).
pub fn fortification_can_engage(kind: FortKind) -> bool {
    !matches!(kind, FortKind::PersonnelConcealment)
}

/// 三.20.1e/20.2e — whether the garrison inherits a fire result against the 工事: 人员 in a 战斗工事
/// take it directly (e), a 车辆 in a 车辆工事 does NOT (the 工事 absorbs it). 隐蔽工事 can't be fired
/// upon (三.20.3c), so this is moot for it (false).
pub fn fortification_inherits_fire(kind: FortKind) -> bool {
    matches!(kind, FortKind::PersonnelCombat)
}

/// 三.20.1a/20.3a — whether the 工事 hex's terrain modifiers apply to observation/fire against it:
/// yes for 车辆/战斗工事, no for a 隐蔽工事 (不受所在格地形属性影响, 三.20.3a).
pub fn fortification_terrain_applies(kind: FortKind) -> bool {
    !matches!(kind, FortKind::PersonnelConcealment)
}

/// 三.20e/20.x.f — whether a 工事 must expel ALL its garrison (瞬时退出, 暴露): the occupants' total
/// 班数 exceeds the 工事's REMAINING capacity (reduced by prior fire results, 三.20.1c). Equality is
/// fine — capacity is 5 (三.20d) and shrinks as the 工事 is hit.
pub fn fortification_over_capacity(occupant_squads: u32, remaining_capacity: u32) -> bool {
    occupant_squads > remaining_capacity
}

// ---------- 三.21 雷场 (minefield) ----------

/// 三.21b/c — only a 火箭布雷车 (`Minelayer`) may 布雷 (lay a 雷场 hex).
pub fn can_lay_mines(t: UnitType) -> bool {
    t == UnitType::Minelayer
}

/// 三.21f — a 雷场通路 is opened by a 扫雷车 (`Minesweeper`) or a 坦克 (`Tank`) passing the hex at half
/// speed; no other unit can clear a path.
pub fn can_clear_minefield(t: UnitType) -> bool {
    matches!(t, UnitType::Minesweeper | UnitType::Tank)
}

/// 一.1.3 间瞄炮火区 — only a 地面算子 (人员/车辆) is adjudicated when crossing into/out of an active
/// barrage zone (一.1.3 "地面算子进出炮火区将受到间瞄火力裁决"); 空中单位 fly over it untouched.
pub fn barrage_zone_triggers(t: UnitType) -> bool {
    is_ground(t)
}

// ---------- 三.22 天基侦察 (space-based recon) ----------

/// 三.22 — the 天基侦察算子.
pub fn is_space_recon(t: UnitType) -> bool {
    t == UnitType::SpaceRecon
}

/// 三.22b — whether a unit may be a fire target at all: the 天基侦察算子 cannot 被攻击 (everything else
/// can, subject to the usual range/observation gates).
pub fn can_be_targeted(t: UnitType) -> bool {
    t != UnitType::SpaceRecon
}

/// 三.22c — the enemy 算子 that `viewer_side`'s 天基侦察 reveals: ONLY if it has a live 天基侦察算子, the
/// lowest-id enemy live, non-carried 算子 in EACH occupied hex (一格只显编号最小的一个), as a sorted id
/// list. The reduced view (no 车班数/弹药/战损, 三.22c) and the "NOT directly observed, no direct fire"
/// rule (三.22d) are applied where the engine renders/uses this — it must not enable direct fire.
///
/// NOTE 三.22c 不在工事内: 工事 garrison occupancy is not board state yet (T4.1 was predicates only),
/// so the 工事 exclusion is applied once garrisons exist — deferred with the 工事 engine op.
pub fn space_recon_reveals(state: &State, viewer_side: SideId) -> Vec<String> {
    let has_recon = state
        .units
        .values()
        .any(|u| u.alive && u.side == viewer_side && u.unit_type == UnitType::SpaceRecon);
    if !has_recon {
        return Vec::new();
    }
    use std::collections::BTreeMap;
    let mut lowest: BTreeMap<Axial, &str> = BTreeMap::new();
    for u in state.units.values() {
        if u.alive && u.side != viewer_side && u.is_on_board() {
            lowest
                .entry(u.pos)
                .and_modify(|cur| {
                    if u.id.as_str() < *cur {
                        *cur = u.id.as_str();
                    }
                })
                .or_insert(u.id.as_str());
        }
    }
    let mut ids: Vec<String> = lowest.values().map(|s| s.to_string()).collect();
    ids.sort();
    ids
}

/// 三.4a — whether `side` may move a unit INTO `dest`: the destination must hold fewer than `cap`
/// of `side`'s live ground units, NOT counting the mover itself (a unit already standing there
/// stays legal). Pure predicate for the pathing layer (T1.10) and the rule test.
pub fn stacking_allows_entry(
    state: &State,
    side: SideId,
    dest: Axial,
    mover_id: &str,
    cap: i64,
) -> bool {
    let occupied = state
        .units
        .values()
        .filter(|u| {
            u.alive
                && u.side == side
                && is_ground(u.unit_type)
                && !is_space_recon(u.unit_type) // 三.22b: the 天基侦察算子 is off-board, not stacked
                && u.is_on_board() // 三.2: a 被载 unit rides inside its carrier, not the hex
                && u.pos == dest
                && u.id != mover_id
        })
        .count();
    (occupied as i64) < cap
}

/// 三.5a — whether the capture zone is clear: no live ENEMY ground unit (any side other than
/// `capturing_side`) lies within `radius` hexes of the objective `center` (radius 1 = center + the
/// 6 neighbors). Adjudicated on the true board (the rule is defined over real positions); callers
/// surface only a generic pass/fail so a hidden enemy's exact position is never revealed (rule #5).
pub fn capture_zone_clear(
    state: &State,
    center: Axial,
    capturing_side: SideId,
    radius: i64,
) -> bool {
    !state.units.values().any(|u| {
        u.alive
            && u.side != capturing_side
            && is_ground(u.unit_type)
            && !is_space_recon(u.unit_type) // 三.22b: the 天基侦察算子 is off-board, doesn't contest 夺控
            && u.is_on_board() // a 被载 unit's carrier already counts; don't double-count it
            && i64::from(center.distance(&u.pos)) <= radius
    })
}

// ---------- 三.3 掩蔽 (cover) ----------
// Most of 三.3 is engine-side state machinery (a 75 s transition that a 坦克 firing (三.3b) or a
// 压制 (三.3f) interrupts; firing/moving leaves an established cover (三.3c); 压制 cannot start a
// cover but also does not lift an established one — 三.3e + p05-g). These pure predicates capture
// the parts a caller decides; the [`concealment`] test drives the rest through the engine.

/// 三.3e — may a unit *begin* a 掩蔽 transition? Not while it is 被压制.
pub fn can_enter_cover(suppressed: bool) -> bool {
    !suppressed
}

/// 三.3d — does a 引导射击 break the firer's 掩蔽? The 引导 (guiding) unit keeps cover; the
/// 被引导 (guided) unit loses it. (Guided fire itself lands in T2.6; this is its cover rule.)
pub fn guided_fire_breaks_cover(is_guided: bool) -> bool {
    is_guided
}

fn elev_at(map: &Map, pos: &Axial) -> i64 {
    i64::from(map.cell(pos).map(|c| c.elevation).unwrap_or(0))
}

fn terrain_at(map: &Map, pos: &Axial) -> Option<Terrain> {
    map.cell(pos).map(|c| c.terrain)
}

/// Rule 三.7 观察 — whether `observer` can currently observe `target`. Looks up the base
/// distance in `rules.observation[observer][target]` (a number of hexes, the string
/// "adjacent" = 当前/相邻格, or absent = 不可观察), requires 通视 ([`line_of_sight`]) for any
/// ranged observation, then applies the distance modifiers: 掩蔽 halves the observed distance
/// (unless the target is a 车辆 in a hex lower than the observer — 掩蔽对观察无效), and a target
/// in a 居民地/丛林地 hex halves it as well. Returns false if anything is unobservable.
pub fn can_observe(rules: &Rules, map: &Map, observer: &RuntimeUnit, target: &RuntimeUnit) -> bool {
    let entry = rules
        .observation
        .get(observer_category(observer.unit_type))
        .and_then(|m| m.get(target_category(target.unit_type)));
    let dist = observer.pos.distance(&target.pos);
    let base = match entry {
        None => return false, // 不可观察 (absent from the table)
        Some(v) if v.as_str() == Some("adjacent") => return dist <= 1, // 当前/相邻格
        Some(v) => match v.as_i64() {
            Some(n) => n,
            None => return false,
        },
    };

    // The table distances assume 通视 (line of sight); without it, no observation. 空中单位 fly above
    // their hex (UAV/直升机 +200 m, a heli per its altitude band), so raise each air endpoint — it
    // sees over terrain, and is seen against it (三.16b for the altitude-dependent heli case).
    let obs_alt = air_altitude_levels(rules, map, observer);
    let tgt_alt = air_altitude_levels(rules, map, target);
    if !line_of_sight_alt(map, &observer.pos, &target.pos, obs_alt, tgt_alt) {
        return false;
    }

    let mut eff = base;
    // 掩蔽: halve — unless the target is a 车辆 lower than the observer (掩蔽对观察无效).
    if target.state == UnitState::Cover {
        let cover_negated =
            is_vehicle(target.unit_type) && elev_at(map, &target.pos) < elev_at(map, &observer.pos);
        if !cover_negated {
            eff /= 2;
        }
    }
    // Target in a 居民地/丛林地 hex: halve again (modifiers stack multiplicatively — each
    // listed reduction applies; shorter distance errs toward fog, never toward a leak).
    if matches!(
        terrain_at(map, &target.pos),
        Some(Terrain::Urban) | Some(Terrain::Forest)
    ) {
        eff /= 2;
    }

    // A positive base distance never floors below 1: if a unit is observable at all and has
    // 通视, it can at least be seen adjacent. (Guards the base-2 UAV/巡飞弹 rows from being
    // halved twice to 0, which would wrongly hide an adjacent covered/forest target.)
    i64::from(dist) <= eff.max(1)
}

/// Rule 8 — resolve one direct-fire shot. Minimal-real: distance -> weapon attack level ->
/// ProbProvider -> Outcome. Modifier application is the /add-rule refinement target.
pub fn resolve_direct_fire(
    rules: &Rules,
    shooter_pos: &Axial,
    target_pos: &Axial,
    weapon_id: &str,
    rng: &mut dyn Rng,
) -> Result<Outcome> {
    let weapon = rules
        .weapons
        .get(weapon_id)
        .ok_or_else(|| EngineError::Command(format!("unknown weapon {weapon_id}")))?;
    let dist = shooter_pos.distance(target_pos);
    if dist > weapon.range {
        return Ok(Outcome::NoEffect);
    }
    let table_id = weapon
        .result_table
        .clone()
        .ok_or_else(|| EngineError::Rules(format!("weapon {weapon_id} has no resultTable")))?;

    // Base attack level from distance (flat form). Row-per-crew + modifiers: TODO(/add-rule 8).
    let base_level = weapon.flat_attack_level(dist).unwrap_or(0);
    // TODO(/add-rule 8): apply height-diff (modifiers.heightDiffByDistance) and combat-result
    // modifiers (shooter state / target terrain / target state / armor) to get effective level.
    let effective_level = base_level;

    let provider = build_provider(rules, &table_id);
    let ctx = ResolveContext {
        table_id,
        attack_level: effective_level,
    };
    Ok(provider.resolve(&ctx, rng))
}

/// Rule 三.1 movement state machine (T1.1): five mutually-exclusive movement modes,
/// a 75 s transition between them, and the stop penalty (no re-tasking mid-transition).
#[cfg(test)]
#[test]
fn movement_states() {
    use crate::engine::Engine;
    use crate::rules::Rules;
    use crate::types::{Map, Scenario, SideId, UnitState};

    let map: Map =
        serde_json::from_str(include_str!("../../../scenarios/maps/demo_valley.map.json")).unwrap();
    let scenario: Scenario = serde_json::from_str(include_str!(
        "../../../scenarios/demo_skirmish.scenario.json"
    ))
    .unwrap();
    let rules = Rules::from_json_str(include_str!("../../../config/rules.default.json")).unwrap();
    let mut e = Engine::new(map, scenario, rules, 1).unwrap();

    let unit = "R-I1"; // an infantry unit present in the demo scenario
    let set_mode =
        |mode: &str| serde_json::json!({ "op": "set_mode", "unitId": unit, "mode": mode });
    assert_eq!(e.state.units[unit].state, UnitState::Normal);

    // (1) the switch takes a full 75 s (7500 cs) — not before. (掩蔽 is a settable posture; 半速 and
    // 一二级冲锋 are per-MOVE bursts, exercised in engine::half_speed_move / engine::charge_and_fatigue.)
    e.submit(SideId::Red, &set_mode("cover"), 0).unwrap();
    e.step(7400);
    assert_ne!(
        e.state.units[unit].state,
        UnitState::Cover,
        "mode must not switch before the 75 s transition completes"
    );
    // (2) stop penalty / mutual exclusion: no new movement order while mid-transition.
    assert!(
        e.submit(SideId::Red, &set_mode("normal"), 7400).is_err(),
        "must reject re-tasking during a transition"
    );
    e.step(200); // cross the 7500 cs boundary
    assert_eq!(e.state.units[unit].state, UnitState::Cover);

    // (3) mutually exclusive: switching back to 常速 replaces 掩蔽 (a unit holds exactly one state).
    let now = e.clock();
    e.submit(SideId::Red, &set_mode("normal"), now).unwrap();
    e.step(7500);
    assert_eq!(e.state.units[unit].state, UnitState::Normal);

    // (4) stop order -> 75 s "机动→停止" transition, no ops meanwhile, then Stopped.
    let now = e.clock();
    e.submit(
        SideId::Red,
        &serde_json::json!({ "op": "stop", "unitId": unit }),
        now,
    )
    .unwrap();
    assert!(
        e.submit(SideId::Red, &set_mode("normal"), now).is_err(),
        "no operations during the stop transition"
    );
    e.step(7500);
    assert_eq!(e.state.units[unit].state, UnitState::Stopped);
}

/// Rule 三.1.2 vehicle movement speed (T1.2): per-hex time = base × terrain factor ×
/// slope factor; road/rail are exempt (沿路免修正); slope level > 5 is impassable.
#[cfg(test)]
#[test]
fn vehicle_speed() {
    use crate::rules::Rules;
    use crate::types::{Terrain, UnitType};

    let r = Rules::from_json_str(include_str!("../../../config/rules.default.json")).unwrap();
    let t = |terrain: Terrain, from: i32, to: i32| {
        step_time_seconds(&r, UnitType::Tank, terrain, from, to).unwrap()
    };

    // Tank base = 20 s/hex (config base_speed_sec_per_hex.tank); open + flat = base.
    assert_eq!(t(Terrain::Open, 0, 0), Some(20.0));
    // Terrain time factors: 丛林 ×2, 居民 ×3, 小河 ×2, 大河 ×4, 松软 ×4.
    assert_eq!(t(Terrain::Forest, 0, 0), Some(40.0));
    assert_eq!(t(Terrain::Urban, 0, 0), Some(60.0));
    assert_eq!(t(Terrain::River, 0, 0), Some(40.0));
    assert_eq!(t(Terrain::RiverLarge, 0, 0), Some(80.0));
    assert_eq!(t(Terrain::Soft, 0, 0), Some(80.0));
    // Slope levels 2..5 = ×2/3/4/5 time (open terrain so factor is pure slope).
    assert_eq!(t(Terrain::Open, 0, 2), Some(40.0));
    assert_eq!(t(Terrain::Open, 0, 3), Some(60.0));
    assert_eq!(t(Terrain::Open, 0, 4), Some(80.0));
    assert_eq!(t(Terrain::Open, 0, 5), Some(100.0));
    // Slope > 5 is impassable.
    assert_eq!(t(Terrain::Open, 0, 6), None);
    // 沿路免修正: a road hex ignores both terrain and slope.
    assert_eq!(t(Terrain::Road, 0, 5), Some(20.0));
    // Intentional (三.1.2 不受高差影响): a road carries even an off-road-impassable slope
    // (> 5) at base speed — road/rail are never blocked by elevation difference.
    assert_eq!(t(Terrain::Road, 0, 6), Some(20.0));
    assert_eq!(t(Terrain::Rail, 0, 8), Some(20.0));
}

/// Rule 三.1.4 personnel movement (T1.3): terrain-independent; 高差 > 60 m halves speed;
/// 一/二级冲锋 speed multipliers and fatigue; fatigue gating; 75 s fatigue decay.
#[cfg(test)]
#[test]
fn infantry_move() {
    use crate::rules::Rules;
    use crate::types::UnitState;

    let r = Rules::from_json_str(include_str!("../../../config/rules.default.json")).unwrap();
    let t = |from: i32, to: i32, mode: UnitState| {
        infantry_step_time_seconds(&r, 10, from, to, mode).unwrap()
    };

    // Base infantry = 144 s/hex; terrain plays no part (terrain-independent).
    assert_eq!(t(0, 0, UnitState::Normal), Some(144.0));
    // 高差 > 60 m halves speed (×2 time): elevation unit 10 m, so Δ7 = 70 m > 60.
    assert_eq!(t(0, 7, UnitState::Normal), Some(288.0));
    // Boundary: Δ6 = 60 m is NOT > 60 → no penalty.
    assert_eq!(t(0, 6, UnitState::Normal), Some(144.0));
    // Charge: 一级 ×2 speed (÷2 time), 二级 ×4 speed (÷4 time).
    assert_eq!(t(0, 0, UnitState::Charge1), Some(72.0));
    assert_eq!(t(0, 0, UnitState::Charge2), Some(36.0));
    // 半速: ×0.5 speed (×2 time).
    assert_eq!(t(0, 0, UnitState::Half), Some(288.0));
    // Combine: 一级冲锋 up a > 60 m climb = 144 ÷2 ×2 = 144.
    assert_eq!(t(0, 7, UnitState::Charge1), Some(144.0));
    // 掩蔽 is not a moving posture.
    assert_eq!(t(0, 0, UnitState::Cover), None);

    // Fatigue: +1 per charging hex, 0 otherwise.
    assert_eq!(infantry_fatigue_per_hex(&r, UnitState::Charge1), 1);
    assert_eq!(infantry_fatigue_per_hex(&r, UnitState::Charge2), 1);
    assert_eq!(infantry_fatigue_per_hex(&r, UnitState::Normal), 0);
    // Gating: 一级疲劳 (≥1) bars 二级冲锋; 二级疲劳 (≥2) bars all movement.
    assert!(!infantry_mode_allowed(UnitState::Charge2, 1));
    assert!(infantry_mode_allowed(UnitState::Charge1, 1));
    assert!(!infantry_mode_allowed(UnitState::Normal, 2));
    assert!(infantry_mode_allowed(UnitState::Normal, 0));
    // 75 s decay: −1 fatigue per 75 s of rest (floored at 0).
    assert_eq!(infantry_fatigue_after_rest(&r, 3, 150), 1);
    assert_eq!(infantry_fatigue_after_rest(&r, 1, 300), 0);
}

/// Rule 三.6 通视 (T1.4): clear / blocked / adjacent-always-visible, plus 高看低 and the
/// 居民地/丛林地 +1 line modifier. (`cargo test mechanics::line_of_sight` matches this.)
#[cfg(test)]
#[test]
fn line_of_sight_cases() {
    use crate::hex::Axial;
    use crate::types::{HexCell, Map};

    fn cell(q: i32, r: i32, elevation: i32, terrain: Terrain) -> HexCell {
        HexCell {
            q,
            r,
            id: None,
            elevation,
            terrain,
            road: None,
        }
    }
    let mk = |hexes: Vec<HexCell>| Map {
        format: "openstratcore.map".into(),
        version: 1,
        name: "los-test".into(),
        elevation_unit_meters: Some(10),
        hexes,
    };
    let a = Axial::new(0, 0);
    let b = Axial::new(3, 0); // distance 3; the line passes through (1,0) and (2,0)

    // (1) clear: flat open terrain along the line is visible.
    let clear = mk(vec![
        cell(0, 0, 0, Terrain::Open),
        cell(1, 0, 0, Terrain::Open),
        cell(2, 0, 0, Terrain::Open),
        cell(3, 0, 0, Terrain::Open),
    ]);
    assert!(line_of_sight(&clear, &a, &b), "flat line should be visible");

    // (2) blocked: an intermediate hill (elev 2) above the elev-0 sight line.
    let hill = mk(vec![
        cell(0, 0, 0, Terrain::Open),
        cell(1, 0, 0, Terrain::Open),
        cell(2, 0, 2, Terrain::Open),
        cell(3, 0, 0, Terrain::Open),
    ]);
    assert!(!line_of_sight(&hill, &a, &b), "an intermediate hill blocks");

    // (2b) 居民地/丛林地 add +1 on the line and block even at equal elevation.
    let forest = mk(vec![
        cell(0, 0, 0, Terrain::Open),
        cell(1, 0, 0, Terrain::Forest),
        cell(2, 0, 0, Terrain::Open),
        cell(3, 0, 0, Terrain::Open),
    ]);
    assert!(
        !line_of_sight(&forest, &a, &b),
        "forest on the line (+1) blocks"
    );

    // (3) adjacent always visible — even with a tall wall on the neighbour.
    let adj = Axial::new(1, 0);
    let wall = mk(vec![
        cell(0, 0, 0, Terrain::Open),
        cell(1, 0, 9, Terrain::Urban),
    ]);
    assert!(
        line_of_sight(&wall, &a, &adj),
        "adjacent hexes always have LOS"
    );

    // 高看低: a +1 rise that blocks a flat observer is cleared by a high observer.
    let rise = |from_elev| {
        mk(vec![
            cell(0, 0, from_elev, Terrain::Open),
            cell(1, 0, 0, Terrain::Open),
            cell(2, 0, 1, Terrain::Open),
            cell(3, 0, 0, Terrain::Open),
        ])
    };
    assert!(
        !line_of_sight(&rise(0), &a, &b),
        "a +1 rise blocks a flat observer"
    );
    assert!(
        line_of_sight(&rise(3), &a, &b),
        "a high observer (+1) sees over the same rise"
    );

    // 高看低 +1 follows the higher hex regardless of argument order: here the HIGH end is
    // `to` (3,0), and LOS is symmetric (it is a property of the pair, not of "who looks").
    let high_target = mk(vec![
        cell(0, 0, 0, Terrain::Open),
        cell(1, 0, 0, Terrain::Open),
        cell(2, 0, 1, Terrain::Open),
        cell(3, 0, 3, Terrain::Open),
    ]);
    assert!(
        line_of_sight(&high_target, &a, &b),
        "a high target (+1) is seen over the rise"
    );
    assert_eq!(
        line_of_sight(&high_target, &a, &b),
        line_of_sight(&high_target, &b, &a),
        "LOS must be symmetric in its endpoints"
    );

    // An off-map endpoint has no line of sight.
    assert!(!line_of_sight(&clear, &a, &Axial::new(9, 9)));

    // Long-diagonal 通视 must be symmetric (the old f64 line_to diverged at ~(7,8)/(8,7),
    // making observation depend on argument order). With an integer line_to both endpoint
    // orders sample the same cells.
    let mut grid: Vec<HexCell> = Vec::new();
    for q in 0..=11 {
        for r in 0..=11 {
            grid.push(cell(
                q,
                r,
                if (q, r) == (7, 8) { 4 } else { 0 },
                Terrain::Open,
            ));
        }
    }
    let g = mk(grid);
    let (p, d) = (Axial::new(0, 0), Axial::new(11, 11));
    assert_eq!(line_of_sight(&g, &p, &d), line_of_sight(&g, &d, &p));
}

/// Rule 三.7 — a covered target in a 居民地/丛林地 hex adjacent to a base-2 observer (UAV /
/// 巡飞弹) stays visible: the two halvings must not floor the 2-hex range to 0 (regression for
/// the false-hide the adversarial review found).
#[cfg(test)]
#[test]
fn can_observe_short_range_floor() {
    use crate::hex::Axial;
    use crate::types::{Armor, HexCell, SideId, WeaponState};

    let rules = Rules::from_json_str(include_str!("../../../config/rules.default.json")).unwrap();
    fn unit(id: &str, side: SideId, ut: UnitType, q: i32, r: i32, state: UnitState) -> RuntimeUnit {
        RuntimeUnit {
            id: id.into(),
            side,
            unit_type: ut,
            armor: Armor::None,
            teams: 1,
            pos: Axial::new(q, r),
            facing: 0,
            state,
            weapon_state: WeaponState::Deployed,
            busy_until: 0,
            suppressed_until: 0,
            alive: true,
            carried_by: None,
            affiliated_to: None,
            heli_alt: crate::types::HeliAlt::Low,
            inside_facility: None,
            fatigue: 0,
        }
    }
    let cell = |q: i32, r: i32, terrain: Terrain| HexCell {
        q,
        r,
        id: None,
        elevation: 0,
        terrain,
        road: None,
    };
    let mk = |hexes: Vec<HexCell>| Map {
        format: "openstratcore.map".into(),
        version: 1,
        name: "obs".into(),
        elevation_unit_meters: Some(10),
        hexes,
    };

    let uav = unit("uav", SideId::Red, UnitType::Uav, 0, 0, UnitState::Normal);
    // Adjacent infantry, in cover, in a forest hex: base 2 → cover ÷2 → forest ÷2 = 0, but the
    // max(1) clamp keeps an observable-at-all target visible at distance 1.
    let near = unit(
        "near",
        SideId::Blue,
        UnitType::Infantry,
        1,
        0,
        UnitState::Cover,
    );
    let map_near = mk(vec![cell(0, 0, Terrain::Open), cell(1, 0, Terrain::Forest)]);
    assert!(
        can_observe(&rules, &map_near, &uav, &near),
        "an adjacent covered forest target must stay visible to a UAV"
    );

    // But a plain target beyond the 2-hex base is still hidden.
    let far = unit(
        "far",
        SideId::Blue,
        UnitType::Infantry,
        3,
        0,
        UnitState::Normal,
    );
    let map_far = mk((0..=3).map(|q| cell(q, 0, Terrain::Open)).collect());
    assert!(
        !can_observe(&rules, &map_far, &uav, &far),
        "distance 3 exceeds the UAV's 2-hex base range"
    );
}

/// 三.3 掩蔽 (T2.1). Drives the engine end-to-end: cover takes 75 s (a); a 坦克 firing (b) or a
/// 压制 (f) interrupts a cover transition; firing leaves an established cover (c); a 被压制 unit
/// cannot begin a cover (e); and 压制 does NOT lift an established cover (p05-g). The 引导射击
/// cover rule (d) is a pure predicate. Suppression uses seed 1 (a 大号直瞄炮 at point blank
/// reliably 压制 the infantry without killing it).
#[cfg(test)]
#[test]
fn concealment() {
    use crate::engine::Engine;
    use crate::rules::Rules;
    use crate::types::{HexCell, Map, Scenario, ScenarioUnit, Side, Sides};

    // d (三.3d) is a pure decision: a 被引导 unit's guided fire breaks cover; a 引导 unit's keeps it.
    assert!(guided_fire_breaks_cover(true));
    assert!(!guided_fire_breaks_cover(false));
    assert!(can_enter_cover(false));
    assert!(!can_enter_cover(true));

    let map = Map {
        format: "openstratcore.map".into(),
        version: 1,
        name: "cov".into(),
        elevation_unit_meters: Some(10),
        hexes: (0..=2)
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
    let unit = |id: &str, ut: UnitType, q: i32| ScenarioUnit {
        id: id.into(),
        unit_type: ut,
        armor: if matches!(ut, UnitType::Tank) {
            Some(crate::types::Armor::Medium)
        } else {
            None
        },
        // 4 vehicles in the firing 坦克 班 + 3 personnel 班 in the infantry: at point blank, seed 1
        // 大号直瞄炮 reliably 压制 (not kills) the infantry — see the probe behind this test.
        teams: if matches!(ut, UnitType::Tank) { 4 } else { 3 },
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
            name: "cov".into(),
            map: "cov".into(),
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
            objectives: vec![],
            facilities: vec![],
        };
        Engine::new(map.clone(), scenario, rules.clone(), 1).unwrap()
    };
    let drain = |e: &mut Engine| while e.advance_to_next_event().is_some() {};
    let st = |e: &Engine, id: &str| e.state.units.get(id).unwrap().state;
    let cover = |id: &str| serde_json::json!({ "op": "set_mode", "unitId": id, "mode": "cover" });
    let fire = |id: &str, tgt: &str| serde_json::json!({ "op": "fire_direct", "unitId": id, "weapon": "大号直瞄炮", "targetUnit": tgt });

    // (a) 掩蔽 transition completes after its 75 s — the unit ends in Cover.
    let mut e = make(vec![unit("R", UnitType::Tank, 0)], vec![]);
    e.submit(SideId::Red, &cover("R"), 0).unwrap();
    drain(&mut e);
    assert_eq!(st(&e, "R"), UnitState::Cover, "三.3a: cover is reached");

    // (e) a 被压制 unit cannot begin a cover.
    let mut e = make(vec![unit("R", UnitType::Tank, 0)], vec![]);
    if let Some(u) = e.state.units.get_mut("R") {
        u.suppressed_until = 100_000;
    }
    assert!(
        e.submit(SideId::Red, &cover("R"), 0).is_err(),
        "三.3e: no cover while suppressed"
    );

    // (c) firing from an established cover leaves it.
    let mut e = make(
        vec![unit("R", UnitType::Tank, 0)],
        vec![unit("B", UnitType::Tank, 1)],
    );
    e.submit(SideId::Red, &cover("R"), 0).unwrap();
    drain(&mut e);
    assert_eq!(st(&e, "R"), UnitState::Cover);
    let now = e.clock();
    e.submit(SideId::Red, &fire("R", "B"), now).unwrap();
    assert_ne!(st(&e, "R"), UnitState::Cover, "三.3c: firing leaves cover");

    // (b) a 坦克 firing mid-transition interrupts the cover (it never reaches Cover).
    let mut e = make(
        vec![unit("R", UnitType::Tank, 0)],
        vec![unit("B", UnitType::Tank, 1)],
    );
    e.submit(SideId::Red, &cover("R"), 0).unwrap();
    e.submit(SideId::Red, &fire("R", "B"), 0).unwrap();
    drain(&mut e);
    assert_ne!(
        st(&e, "R"),
        UnitState::Cover,
        "三.3b: tank fire interrupts the cover transition"
    );

    // (f) 压制 mid-transition interrupts the cover. The fire applies synchronously, so right after
    // it the unit is Suppressed (not Cover); draining past the would-be transition never lands it.
    let mut e = make(
        vec![unit("RI", UnitType::Infantry, 0)],
        vec![unit("B", UnitType::Tank, 1)],
    );
    e.submit(SideId::Red, &cover("RI"), 0).unwrap();
    e.submit(SideId::Blue, &fire("B", "RI"), 0).unwrap();
    assert_eq!(
        st(&e, "RI"),
        UnitState::Suppressed,
        "三.3f: suppression interrupts the cover transition"
    );
    drain(&mut e);
    assert_ne!(
        st(&e, "RI"),
        UnitState::Cover,
        "三.3f: the cover never lands"
    );

    // (p05-g) 压制 does NOT lift an already-established cover.
    let mut e = make(
        vec![unit("RI", UnitType::Infantry, 0)],
        vec![unit("B", UnitType::Tank, 1)],
    );
    e.submit(SideId::Red, &cover("RI"), 0).unwrap();
    drain(&mut e);
    assert_eq!(st(&e, "RI"), UnitState::Cover);
    let now = e.clock();
    e.submit(SideId::Blue, &fire("B", "RI"), now).unwrap();
    assert_eq!(
        st(&e, "RI"),
        UnitState::Cover,
        "p05-g: established cover survives suppression"
    );
    assert!(
        e.state.units.get("RI").unwrap().suppressed_until > now,
        "but the unit is still marked suppressed by the timer"
    );
    // A 被压制 unit cannot fire, even from cover (8.8 — the Cover posture must not be a loophole).
    assert!(
        e.submit(SideId::Red, &fire("RI", "B"), now).is_err(),
        "a suppressed-in-cover unit cannot fire"
    );
    // And the cover must outlast the suppression: draining past SuppressEnd leaves it in Cover.
    drain(&mut e);
    assert_eq!(
        st(&e, "RI"),
        UnitState::Cover,
        "p05-g: cover persists after the suppression timer ends"
    );
}

/// 三.2 上下车 (T2.2). Drives the engine: a stopped, co-located passenger boards a carrier over
/// 75 s, then rides with it and cannot be ordered independently; dismount reverses it; and a 压制
/// of the passenger (三.2d) interrupts an in-flight mount. Validation guards (same hex / stopped /
/// not suppressed) round it out. Suppression uses seed 1 (point-blank 大号直瞄炮 压制 the infantry).
#[cfg(test)]
#[test]
fn mount_dismount() {
    use crate::engine::Engine;
    use crate::rules::Rules;
    use crate::types::{Armor, HexCell, Map, Scenario, ScenarioUnit, Side, Sides};

    let map = Map {
        format: "openstratcore.map".into(),
        version: 1,
        name: "mnt".into(),
        elevation_unit_meters: Some(10),
        hexes: (0..=1)
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
    let unit = |id: &str, ut: UnitType, q: i32, state: UnitState| ScenarioUnit {
        id: id.into(),
        unit_type: ut,
        armor: match ut {
            UnitType::Tank => Some(Armor::Medium),
            UnitType::Ifv => Some(Armor::Light),
            _ => None,
        },
        teams: if matches!(ut, UnitType::Tank) { 4 } else { 3 },
        at: Axial::new(q, 0),
        facing: 0,
        state: Some(state),
        carried_by: None,
        affiliated_to: None,
    };
    let rules = Rules::from_json_str(include_str!("../../../config/rules.default.json")).unwrap();
    let make = |red: Vec<ScenarioUnit>, blue: Vec<ScenarioUnit>| {
        let scenario = Scenario {
            format: "openstratcore.scenario".into(),
            version: 1,
            name: "mnt".into(),
            map: "mnt".into(),
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
            objectives: vec![],
            facilities: vec![],
        };
        Engine::new(map.clone(), scenario, rules.clone(), 1).unwrap()
    };
    let drain = |e: &mut Engine| while e.advance_to_next_event().is_some() {};
    let carried = |e: &Engine, id: &str| e.state.units.get(id).unwrap().carried_by.clone();
    let pos = |e: &Engine, id: &str| e.state.units.get(id).unwrap().pos;
    let mount =
        |p: &str, v: &str| serde_json::json!({ "op": "mount", "unitId": p, "vehicleId": v });
    let dismount = |p: &str| serde_json::json!({ "op": "dismount", "unitId": p });
    let stopped = UnitState::Stopped;

    // (a) a co-located, stopped infantry boards an IFV after the 75 s 上车.
    let mut e = make(
        vec![
            unit("RI", UnitType::Infantry, 0, stopped),
            unit("V", UnitType::Ifv, 0, stopped),
        ],
        vec![],
    );
    e.submit(SideId::Red, &mount("RI", "V"), 0).unwrap();
    assert_eq!(carried(&e, "RI"), None, "still boarding mid-transition");
    drain(&mut e);
    assert_eq!(
        carried(&e, "RI"),
        Some("V".to_string()),
        "三.2: RI is aboard V"
    );

    // (b) a 被载 unit cannot be ordered independently; (c) it rides with the carrier.
    assert!(
        e.submit(
            SideId::Red,
            &serde_json::json!({ "op": "move_to", "unitId": "RI", "target": { "q": 1, "r": 0 } }),
            e.clock()
        )
        .is_err(),
        "三.2: a mounted unit takes no independent order"
    );
    let now = e.clock();
    e.submit(
        SideId::Red,
        &serde_json::json!({ "op": "move_to", "unitId": "V", "target": { "q": 1, "r": 0 } }),
        now,
    )
    .unwrap();
    drain(&mut e);
    assert_eq!(pos(&e, "V"), Axial::new(1, 0));
    assert_eq!(
        pos(&e, "RI"),
        Axial::new(1, 0),
        "三.2: passenger rides along"
    );

    // (d) dismount returns the passenger to the ground at the carrier's hex.
    let now = e.clock();
    e.submit(SideId::Red, &dismount("RI"), now).unwrap();
    drain(&mut e);
    assert_eq!(carried(&e, "RI"), None, "三.2: RI dismounted");
    assert_eq!(pos(&e, "RI"), Axial::new(1, 0));

    // (e) 三.2a: mount needs co-location and both stopped.
    let mut e = make(
        vec![
            unit("RI", UnitType::Infantry, 0, stopped),
            unit("V", UnitType::Ifv, 1, stopped),
        ],
        vec![],
    );
    assert!(
        e.submit(SideId::Red, &mount("RI", "V"), 0).is_err(),
        "三.2a: cannot mount across hexes"
    );
    let mut e = make(
        vec![
            unit("RI", UnitType::Infantry, 0, UnitState::Moving),
            unit("V", UnitType::Ifv, 0, stopped),
        ],
        vec![],
    );
    assert!(
        e.submit(SideId::Red, &mount("RI", "V"), 0).is_err(),
        "三.2a: a moving unit cannot mount"
    );

    // (f) 三.2c: a 被压制 unit cannot start a 上下车.
    let mut e = make(
        vec![
            unit("RI", UnitType::Infantry, 0, stopped),
            unit("V", UnitType::Ifv, 0, stopped),
        ],
        vec![],
    );
    if let Some(u) = e.state.units.get_mut("RI") {
        u.suppressed_until = 100_000;
    }
    assert!(
        e.submit(SideId::Red, &mount("RI", "V"), 0).is_err(),
        "三.2c: no mount while suppressed"
    );

    // (g) 三.2d: 压制 of the passenger interrupts an in-flight mount.
    let mut e = make(
        vec![
            unit("RI", UnitType::Infantry, 0, stopped),
            unit("V", UnitType::Ifv, 0, stopped),
        ],
        vec![unit("B", UnitType::Tank, 1, stopped)],
    );
    e.submit(SideId::Red, &mount("RI", "V"), 0).unwrap();
    e.submit(
        SideId::Blue,
        &serde_json::json!({ "op": "fire_direct", "unitId": "B", "weapon": "大号直瞄炮", "targetUnit": "RI" }),
        0,
    )
    .unwrap();
    drain(&mut e);
    assert_eq!(
        carried(&e, "RI"),
        None,
        "三.2d: suppression interrupted the mount — RI never boarded"
    );

    // (h) only a man-portable squad may board — a vehicle cannot ride a vehicle.
    let mut e = make(
        vec![
            unit("T", UnitType::Tank, 0, stopped),
            unit("V", UnitType::Ifv, 0, stopped),
        ],
        vec![],
    );
    assert!(
        e.submit(SideId::Red, &mount("T", "V"), 0).is_err(),
        "三.2: a tank cannot mount an IFV"
    );

    // (j) dismount is refused when the carrier's hex is already at the 三.4 stacking cap.
    let mut e = make(
        vec![
            unit("RI", UnitType::Infantry, 0, stopped),
            unit("V", UnitType::Ifv, 0, stopped),
            unit("G1", UnitType::Tank, 0, stopped),
            unit("G2", UnitType::Tank, 0, stopped),
            unit("G3", UnitType::Tank, 0, stopped),
        ],
        vec![],
    );
    e.submit(SideId::Red, &mount("RI", "V"), 0).unwrap();
    drain(&mut e);
    assert_eq!(carried(&e, "RI"), Some("V".to_string()));
    // Hex (0,0) now holds V + G1 + G2 + G3 = 4 ground units (RI is inside). Dismount has no room.
    let now = e.clock();
    assert!(
        e.submit(SideId::Red, &dismount("RI"), now).is_err(),
        "三.4: cannot dismount into a full hex"
    );

    // (i) a destroyed carrier takes its embarked passengers with it (documented interpretive
    // default; rules are silent). seed 1 lets the point-blank 大号直瞄炮 wreck the 1-vehicle IFV.
    let ifv1 = ScenarioUnit {
        id: "V".into(),
        unit_type: UnitType::Ifv,
        armor: Some(Armor::None),
        teams: 1,
        at: Axial::new(0, 0),
        facing: 0,
        state: Some(stopped),
        carried_by: None,
        affiliated_to: None,
    };
    let mut e = make(
        vec![unit("RI", UnitType::Infantry, 0, stopped), ifv1],
        vec![unit("B", UnitType::Tank, 1, stopped)],
    );
    e.submit(SideId::Red, &mount("RI", "V"), 0).unwrap();
    drain(&mut e);
    assert_eq!(carried(&e, "RI"), Some("V".to_string()), "RI is aboard V");
    let now = e.clock();
    e.submit(
        SideId::Blue,
        &serde_json::json!({ "op": "fire_direct", "unitId": "B", "weapon": "大号直瞄炮", "targetUnit": "V" }),
        now,
    )
    .unwrap();
    assert!(
        !e.state.units.get("V").unwrap().alive,
        "the IFV was destroyed"
    );
    assert!(
        !e.state.units.get("RI").unwrap().alive,
        "三.2 carrier death: the embarked squad is lost with the wreck"
    );
}

/// 三.1.3 车辆行军 (T3.6): road-class march speed (km/h → 秒/格 over hex_size, terrain-free), the
/// 沿路走向 direction rule, and the 三.1.3d blocking rule (a stopped/non-marching unit ahead halts
/// the column). The 75 s transitions / weapon-lock-first / road-route pathing are the engine op's
/// job; the per-hex march SPEED is wired into `Engine::step_ticks`.
#[cfg(test)]
#[test]
fn march() {
    use crate::hex::Axial;
    use crate::types::{Armor, Road, WeaponState};
    use std::collections::BTreeMap;

    let rules = Rules::from_json_str(include_str!("../../../config/rules.default.json")).unwrap();
    assert_eq!(rules.hex_size_meters, 200);

    // 三.1.3a/b: a 200 m hex at 乡村40 / 一般60 / 等级90 km/h → 18 / 12 / 8 s per hex (no terrain).
    assert_eq!(march_step_time_seconds(&rules, "country"), Some(18.0));
    assert_eq!(march_step_time_seconds(&rules, "normal"), Some(12.0));
    assert_eq!(march_step_time_seconds(&rules, "grade"), Some(8.0));
    assert_eq!(march_step_time_seconds(&rules, "rail"), None); // vehicles don't march on rail
    assert_eq!(march_step_time_seconds(&rules, "bogus"), None);

    // 三.1.3b/e: march only follows the road's own exit directions; a road-less hex is unmarchable.
    let cell = |road: Option<Road>| HexCell {
        q: 0,
        r: 0,
        id: None,
        elevation: 0,
        terrain: Terrain::Open,
        road,
    };
    let road_cell = cell(Some(Road {
        kind: "normal".into(),
        connects: vec![2, 5],
    }));
    assert!(road_allows_direction(&road_cell, 2));
    assert!(road_allows_direction(&road_cell, 5));
    assert!(!road_allows_direction(&road_cell, 0)); // the road does not exit this way
    assert!(!road_allows_direction(&cell(None), 2)); // no road → not marchable

    // 三.1.3d: a stopped / non-marching unit in the next hex blocks; a marching column does not
    // self-block; a carried passenger rides inside and does not count.
    let ru = |id: &str, st: UnitState, carried: Option<&str>| RuntimeUnit {
        id: id.into(),
        side: SideId::Red,
        unit_type: UnitType::Tank,
        armor: Armor::Medium,
        teams: 1,
        pos: Axial::new(1, 0),
        facing: 0,
        state: st,
        weapon_state: WeaponState::Deployed,
        busy_until: 0,
        suppressed_until: 0,
        alive: true,
        carried_by: carried.map(|s| s.into()),
        affiliated_to: None,
        heli_alt: crate::types::HeliAlt::Low,
        inside_facility: None,
        fatigue: 0,
    };
    let mk = |us: Vec<RuntimeUnit>| {
        let mut m = BTreeMap::new();
        for u in us {
            m.insert(u.id.clone(), u);
        }
        State {
            clock: 0,
            units: m,
            control: BTreeMap::new(),
            facilities: BTreeMap::new(),
            minefields: BTreeMap::new(),
        }
    };
    let here = Axial::new(1, 0);
    assert!(march_allows_entry(&mk(vec![]), here)); // empty hex → free
    assert!(march_allows_entry(
        &mk(vec![ru("M", UnitState::March, None)]),
        here
    )); // a marching column doesn't self-block
    assert!(!march_allows_entry(
        &mk(vec![ru("S", UnitState::Stopped, None)]),
        here
    )); // a stopped unit blocks
    assert!(!march_allows_entry(
        &mk(vec![ru("N", UnitState::Normal, None)]),
        here
    )); // any non-marching unit blocks
    assert!(march_allows_entry(
        &mk(vec![ru("P", UnitState::Stopped, Some("V"))]),
        here
    )); // a carried passenger rides inside → ignored
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::PcgRng;

    fn rules() -> Rules {
        Rules::from_json_str(include_str!("../../../config/rules.default.json")).unwrap()
    }

    #[test]
    fn vehicle_slower_in_forest() {
        let r = rules();
        let open = step_time_seconds(&r, UnitType::Tank, Terrain::Open, 0, 0)
            .unwrap()
            .unwrap();
        let forest = step_time_seconds(&r, UnitType::Tank, Terrain::Forest, 0, 0)
            .unwrap()
            .unwrap();
        assert!(forest > open);
    }

    #[test]
    fn steep_slope_impassable() {
        let r = rules();
        // elevation jump of 6 units exceeds impassable_above_level=5
        assert_eq!(
            step_time_seconds(&r, UnitType::Tank, Terrain::Open, 0, 6).unwrap(),
            None
        );
    }

    #[test]
    fn direct_fire_in_range_is_deterministic() {
        let r = rules();
        let a = Axial::new(0, 0);
        let b = Axial::new(3, 0);
        let mut r1 = PcgRng::from_seed(5);
        let mut r2 = PcgRng::from_seed(5);
        let o1 = resolve_direct_fire(&r, &a, &b, "tank_main_big", &mut r1).unwrap();
        let o2 = resolve_direct_fire(&r, &a, &b, "tank_main_big", &mut r2).unwrap();
        assert_eq!(o1, o2);
    }
}
