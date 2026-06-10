//! Direct-fire adjudication pipelines (三.8 + 附1; see docs/rules/combat-model.md).
//!
//! Pipelines read the authoritative decision tables via [`crate::rules::Tables`] (rules-as-data;
//! every value comes from `config/tables/*.json`, never hardcoded) and draw randomness only
//! through [`crate::rng::Rng`] (硬规则 #1). They return a [`crate::prob::Outcome`].
//!
//! - Pipeline A — direct fire vs **vehicle** (2d6): attack level → 高度差修正 → result_vs_vehicle
//!   stage1 → roll 2d6 → stage2 base loss → vehicle_loss_correction armor/state adjustment.
//! - Pipeline B — direct fire vs **personnel** (result table 2d6 + a 1d6 result correction):
//!   attack level → result_vs_personnel → personnel_loss_correction.
//!
//! The full fire flow (射击条件/行进间/武器状态) is [`direct_fire`], which the engine's
//! `fire_direct` op calls; the pipelines stay pure over the tables so they test deterministically.

use crate::mechanics::can_observe;
use crate::prob::Outcome;
use crate::rng::Rng;
use crate::rules::Tables;
use crate::types::{Armor, Map, RuntimeUnit, SideId, Terrain, UnitState, UnitType, WeaponState};
use serde_json::Value;

// --- Combat telemetry (OBSERVABILITY ONLY) ----------------------------------------------------
// A write-only, OFF-BY-DEFAULT thread-local buffer that records each direct-fire adjudication for
// offline BALANCE ANALYSIS. It is PURELY observational: nothing in the simulation ever reads it, so
// it cannot perturb the deterministic run (硬规则 #1) — the records are themselves a deterministic
// function of the run. When disabled (the default for every test / replay / RL step), `record_direct`
// is a cheap no-op, so combat behaviour is byte-identical with telemetry off. The Engine turns it on
// only around a telemetry-gathering run and drains it; it NEVER feeds back into combat resolution
// (the PDF-faithful pipeline is unchanged — the calibratable `prob` provider remains a separate model).

/// One recorded direct-fire adjudication from the real `combat::resolve_direct_vs_*` pipeline.
#[derive(Debug, Clone)]
pub struct OutcomeRecord {
    /// Provider-style result-table id this adjudication corresponds to (analysis correspondence).
    pub table: &'static str,
    pub weapon: String,
    /// Effective attack level AFTER pre-draw corrections (高差/车数), as the result table saw it.
    pub attack_level: i64,
    /// Target armour label ("none" for personnel — they have no armour class).
    pub armor: &'static str,
    pub distance: i32,
    pub outcome: Outcome,
}

// The buffer is a **process/thread-global** analysis sink, not `Engine`-scoped: `record_direct` is
// reached from the free `resolve_direct_vs_*` functions that have no `Engine` handle. It is therefore
// intended for SINGLE-ENGINE OFFLINE runs (the balance tool drives one engine per process). Running two
// engines interleaved on the same thread would interleave their records — acceptable because telemetry
// is observational tooling, never read by the sim, so it cannot affect determinism either way. Each
// test runs on its own thread (own thread-local), so tests are mutually isolated; `outcome_log_disable`
// gives an explicit off/reset for long-lived hosts.
thread_local! {
    static OUTCOME_LOG: std::cell::RefCell<Option<Vec<OutcomeRecord>>> =
        const { std::cell::RefCell::new(None) };
}

/// Start recording direct-fire adjudications on THIS thread (clears any prior buffer).
pub fn outcome_log_enable() {
    OUTCOME_LOG.with(|c| *c.borrow_mut() = Some(Vec::new()));
}

/// Stop recording and discard any buffered records on THIS thread (explicit off/reset).
pub fn outcome_log_disable() {
    OUTCOME_LOG.with(|c| *c.borrow_mut() = None);
}

/// Take the buffered records (leaving recording ON so a per-step drain keeps collecting); empty if
/// telemetry was never enabled.
pub fn outcome_log_drain() -> Vec<OutcomeRecord> {
    OUTCOME_LOG.with(|c| {
        c.borrow_mut()
            .as_mut()
            .map(std::mem::take)
            .unwrap_or_default()
    })
}

fn armor_label(a: Armor) -> &'static str {
    match a {
        Armor::Composite => "composite",
        Armor::Heavy => "heavy",
        Armor::Medium => "medium",
        Armor::Light => "light",
        Armor::None => "none",
    }
}

fn record_direct(
    table: &'static str,
    weapon: &str,
    attack_level: i64,
    armor: Armor,
    distance: i32,
    outcome: Outcome,
) {
    OUTCOME_LOG.with(|c| {
        if let Some(v) = c.borrow_mut().as_mut() {
            v.push(OutcomeRecord {
                table,
                weapon: weapon.to_string(),
                attack_level,
                armor: armor_label(armor),
                distance,
                outcome,
            });
        }
    });
}

/// Shooter/target conditions that feed the `*_loss_correction.additive_random` tables. The
/// field names mirror the rule's modifier rows; an unset flag contributes nothing.
#[derive(Debug, Clone, Copy, Default)]
pub struct FireMods {
    pub shooter_suppressed: bool, // 被压制
    pub shooter_moving: bool,     // 机动中
    pub shooter_water_tank: bool, // 水上坦克 (vehicle table only)
    pub target_terrain: TargetTerrain,
    pub target_cover: bool,   // 掩蔽中
    pub target_moving: bool,  // 机动中
    pub target_stacked: bool, // 堆叠中
    pub target_march: bool,   // 行军中
}

/// The target's terrain category for the loss-correction `target_terrain` row.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TargetTerrain {
    #[default]
    Open,
    UrbanOrWorks, // 居民地/工事
    Forest,       // 丛林地
    Water,        // 水上
}

// ---------- table accessors ----------

fn table<'a>(tables: &'a Tables, name: &str, key: &str) -> Option<&'a Value> {
    tables.get(name).and_then(|t| t.payload(key))
}

/// 附1.1 — attack level of `weapon` firing at personnel, by squad count and distance.
fn attack_level_personnel(tables: &Tables, weapon: &str, squads: u8, distance: i32) -> Option<i64> {
    let arr = table(tables, "attack_level_vs_personnel", "weapons")?
        .get(weapon)?
        .get("by_squad")?
        .get(squads.to_string())?
        .as_array()?;
    // Index without clamping (matches attack_level_vehicle): a distance past the defined columns
    // — the ragged 步兵轻武器 rows stop short of `range` — is no attack, not the repeated last cell.
    let idx = distance.max(0) as usize;
    arr.get(idx).and_then(|v| v.as_i64())
}

/// 附1.2 — base attack level of `weapon` firing at a vehicle, by distance. For 步兵轻武器 the table
/// entry holds a `ref` to the 步兵轻武器对车辆 by-squad table (三.15g / 附1.3), keyed by the firer's
/// `squads`, so that path is resolved here too (this fixes infantry-vs-vehicle direct fire, not just
/// 同格交战).
fn attack_level_vehicle(tables: &Tables, weapon: &str, squads: u8, distance: i32) -> Option<i64> {
    let entry = table(tables, "attack_level_vs_vehicle", "weapons")?.get(weapon)?;
    let idx = distance.max(0) as usize;
    if let Some(levels) = entry.get("levels").and_then(|v| v.as_array()) {
        return levels.get(idx)?.as_i64();
    }
    let reff = entry.get("ref").and_then(|v| v.as_str())?;
    let table_id = reff.strip_suffix(".json").unwrap_or(reff);
    table(tables, table_id, "by_squad")?
        .get(squads.to_string())?
        .as_array()?
        .get(idx)
        .and_then(|v| v.as_i64())
}

/// 附1.4 — height-difference correction to the attack level. `height_diff` is SIGNED: `> 0`
/// = shooter is higher than the target (高看低 → bonus), `< 0` = shooter lower (低打高 →
/// penalty), `0` = level. The table stores only the magnitude (a non-positive value) keyed by
/// `|height_diff|` (1..8) × distance (1..12); the sign is applied here by direction of fire.
fn height_diff_correction(tables: &Tables, height_diff: i32, distance: i32) -> i64 {
    if height_diff == 0 {
        return 0;
    }
    let magnitude = table(tables, "height_diff_correction", "rows")
        .and_then(|r| r.get(height_diff.unsigned_abs().to_string()))
        .and_then(|row| row.as_array())
        .and_then(|row| row.get((distance.max(1) - 1) as usize))
        .and_then(|v| v.as_i64())
        .unwrap_or(0)
        .abs();
    if height_diff > 0 {
        magnitude // 高看低: bonus
    } else {
        -magnitude // 低打高: penalty
    }
}

/// 附1.5 段1 — corrected attack level (level_B) for a shooter with `vehicles` vehicles/squads
/// and raw attack level `level_a`. The staircase cell holds the *input* level and its 1-based
/// column index is the corrected level (more vehicles ⇒ staircase shifts right ⇒ higher level).
fn stage1_corrected(tables: &Tables, vehicles: u8, level_a: i64) -> Option<i64> {
    if level_a < 1 {
        return None;
    }
    let row = table(tables, "result_vs_vehicle", "data")?
        .get("stage1")?
        .get(vehicles.to_string())?
        .as_array()?;
    row.iter()
        .position(|c| c.as_i64() == Some(level_a))
        .map(|i| i as i64 + 1)
}

/// 附1.5 段2 — base vehicles-destroyed for a 2d6 `roll` and corrected level `level_b`.
fn stage2_base_loss(tables: &Tables, roll: u32, level_b: i64) -> i64 {
    if level_b < 1 {
        return 0;
    }
    table(tables, "result_vs_vehicle", "data")
        .and_then(|d| d.get("stage2"))
        .and_then(|s| s.get(roll.to_string()))
        .and_then(|row| row.as_array())
        .and_then(|row| row.get((level_b - 1) as usize))
        .and_then(|v| v.as_i64())
        .unwrap_or(0)
}

/// 附1.7 — base result cell of the personnel result table for a 2d6 `roll` and attack `level`.
fn personnel_result(tables: &Tables, roll: u32, level: i64) -> PersonnelCell {
    if level < 1 {
        return PersonnelCell::None;
    }
    let cell = table(tables, "result_vs_personnel", "rows")
        .and_then(|r| r.get(roll.to_string()))
        .and_then(|row| row.as_array())
        .and_then(|row| row.get((level - 1) as usize));
    match cell {
        Some(v) if v.as_str() == Some("压") => PersonnelCell::Suppress,
        Some(v) => match v.as_i64() {
            Some(n) if n > 0 => PersonnelCell::Loss(n),
            _ => PersonnelCell::None,
        },
        None => PersonnelCell::None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PersonnelCell {
    Loss(i64),
    Suppress,
    None,
}

// ---------- loss-correction (additive_random + by_modified_random) ----------

/// Sum the `additive_random` modifiers of `loss_table` that apply under `mods`. The additive
/// shifts the random number toward (negative) or away from (positive) a result.
fn additive_sum(tables: &Tables, loss_table: &str, mods: FireMods, vehicle: bool) -> i64 {
    let Some(add) = table(tables, loss_table, "additive_random") else {
        return 0;
    };
    let get = |section: &str, key: &str| -> i64 {
        add.get(section)
            .and_then(|s| s.get(key))
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
    };
    let mut s = 0;
    if mods.shooter_suppressed {
        s += get("shooter", "被压制");
    }
    if mods.shooter_moving {
        s += get("shooter", "机动中");
    }
    if vehicle && mods.shooter_water_tank {
        s += get("shooter", "水上坦克");
    }
    let terrain_key = match mods.target_terrain {
        TargetTerrain::Open => None,
        TargetTerrain::UrbanOrWorks => Some(if vehicle {
            "居民地/工事"
        } else {
            "居民地"
        }),
        TargetTerrain::Forest => Some("丛林地"),
        TargetTerrain::Water => Some("水上"),
    };
    if let Some(k) = terrain_key {
        s += get("target_terrain", k);
        // the personnel table also lists 工事 separately; treat UrbanOrWorks as 居民地 there.
    }
    if mods.target_cover {
        s += get("target_state", "掩蔽中");
    }
    if mods.target_moving {
        s += get("target_state", "机动中");
    }
    if mods.target_stacked {
        s += get("target_state", "堆叠中");
    }
    if mods.target_march {
        s += get("target_state", "行军中");
    }
    s
}

/// Does the range-key (e.g. "<=-3", "-2..-1", "0", "3..5", ">=12") contain `v`?
fn bucket_contains(key: &str, v: i64) -> bool {
    let parse = |s: &str| s.trim().parse::<i64>().ok();
    if let Some(n) = key.strip_prefix("<=").and_then(parse) {
        return v <= n;
    }
    if let Some(n) = key.strip_prefix(">=").and_then(parse) {
        return v >= n;
    }
    if let Some((lo, hi)) = key.split_once("..") {
        if let (Some(lo), Some(hi)) = (parse(lo), parse(hi)) {
            return v >= lo && v <= hi;
        }
    }
    parse(key) == Some(v)
}

/// Find the `by_modified_random` bucket value for `modified` in `loss_table`.
fn by_modified_random<'a>(
    tables: &'a Tables,
    loss_table: &str,
    modified: i64,
) -> Option<&'a Value> {
    let obj = table(tables, loss_table, "by_modified_random")?.as_object()?;
    obj.iter()
        .find(|(k, _)| bucket_contains(k, modified))
        .map(|(_, v)| v)
}

/// 附1.6 — armor/state adjustment to the vehicle loss, indexed by the modified 2d6 bucket and
/// the target armor column (`armor_order`).
fn vehicle_armor_adjust(tables: &Tables, modified_roll: i64, armor: Armor) -> i64 {
    let Some(order) =
        table(tables, "vehicle_loss_correction", "armor_order").and_then(|a| a.as_array())
    else {
        return 0;
    };
    let armor_zh = armor_key(armor);
    let Some(col) = order.iter().position(|a| a.as_str() == Some(armor_zh)) else {
        return 0;
    };
    by_modified_random(tables, "vehicle_loss_correction", modified_roll)
        .and_then(|row| row.as_array())
        .and_then(|row| row.get(col))
        .and_then(|v| v.as_i64())
        .unwrap_or(0)
}

fn armor_key(a: Armor) -> &'static str {
    match a {
        Armor::Composite => "复合装甲",
        Armor::Heavy => "重型装甲",
        Armor::Medium => "中型装甲",
        Armor::Light => "轻型装甲",
        Armor::None => "无装甲",
    }
}

// ---------- pipelines ----------

/// Pipeline B (三.8 / 附1.7+1.8) — resolve one direct-fire shot at personnel.
pub fn resolve_direct_vs_personnel(
    tables: &Tables,
    rng: &mut dyn Rng,
    weapon: &str,
    squads: u8,
    distance: i32,
    mods: FireMods,
) -> Outcome {
    // Draw the shot's fixed dice up front (2d6 result roll, then 1d6 correction) so that a
    // No-Effect shot still consumes the same dice — keeps a shared Rng stream stable across
    // shots regardless of input data (matters once T1.8 fires many shots off one Rng).
    let roll = rng.roll_sum(2); // 2..12, indexes the result table row
    let d1 = rng.d6() as i64; // 1d6 for the result correction
    let Some(level) = attack_level_personnel(tables, weapon, squads, distance) else {
        return Outcome::NoEffect;
    };
    let base = personnel_result(tables, roll, level);

    // Result correction: 1d6 + additive → by_modified_random → -1 / 0 / +1.
    let modified = d1 + additive_sum(tables, "personnel_loss_correction", mods, false);
    let corr = by_modified_random(tables, "personnel_loss_correction", modified)
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    // Combine. The rule does not spell out how ±1 folds into 压/无效, so: a numeric loss is
    // adjusted by `corr` (0 ⇒ still suppresses), a 压 is upgraded to a kill by a favourable
    // +1 else stays 压, and a miss stays a miss. (Documented as the one interpretive choice.)
    let outcome = match base {
        PersonnelCell::Loss(n) => {
            let final_loss = (n + corr).max(0);
            if final_loss > 0 {
                Outcome::Destroyed(final_loss as u8)
            } else {
                Outcome::Suppress
            }
        }
        PersonnelCell::Suppress => {
            if corr >= 1 {
                Outcome::Destroyed(corr as u8)
            } else {
                Outcome::Suppress
            }
        }
        PersonnelCell::None => Outcome::NoEffect,
    };
    // Personnel have no armour class; record "none" so the analysis can pool them.
    record_direct(
        "direct_vs_personnel",
        weapon,
        level,
        Armor::None,
        distance,
        outcome,
    );
    outcome
}

/// One direct-fire shot at a vehicle target (pipeline A inputs).
#[derive(Debug, Clone, Copy)]
pub struct VehicleShot<'a> {
    pub weapon: &'a str,
    pub vehicles: u8,
    pub distance: i32,
    pub height_diff: i32,
    pub armor: Armor,
    pub mods: FireMods,
}

/// Pipeline A (三.8 / 附1.2/1.4/1.5/1.6) — resolve one direct-fire shot at a vehicle.
pub fn resolve_direct_vs_vehicle(
    tables: &Tables,
    rng: &mut dyn Rng,
    shot: &VehicleShot,
) -> Outcome {
    // Drawn up front so a No-Effect shot consumes the same 2d6 — stable shared Rng stream (T1.8).
    let roll = rng.roll_sum(2); // 2..12
    let Some(base_level) = attack_level_vehicle(tables, shot.weapon, shot.vehicles, shot.distance)
    else {
        return Outcome::NoEffect;
    };
    let level_a = base_level + height_diff_correction(tables, shot.height_diff, shot.distance);
    let Some(level_b) = stage1_corrected(tables, shot.vehicles, level_a) else {
        return Outcome::NoEffect;
    };

    let base_loss = stage2_base_loss(tables, roll, level_b);
    let modified = roll as i64 + additive_sum(tables, "vehicle_loss_correction", shot.mods, true);
    let armor_adj = vehicle_armor_adjust(tables, modified, shot.armor);

    let dmg = (base_loss + armor_adj).clamp(0, 5);
    let outcome = if dmg > 0 {
        Outcome::Destroyed(dmg as u8) // 8.6: 毁伤同时压制
    } else if base_loss > 0 {
        Outcome::Suppress // table gave a loss but armor deflected it to 0 — still 压制 (p15/8.6)
    } else {
        Outcome::NoEffect
    };
    record_direct(
        "direct_vs_vehicle",
        shot.weapon,
        level_b,
        shot.armor,
        shot.distance,
        outcome,
    );
    outcome
}

// ---------- pipeline C: 间瞄 (indirect / artillery, 三.9 / 附2) ----------

/// 间瞄校射等级 (三.9.2): can a friendly spotter see the plan hex (and the target inside it)?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndirectSpotting {
    None,   // 无校射 — no friendly unit observes the hex
    InHex,  // 格内校射 — a unit sees the hex but not the target in it
    Target, // 目标校射 — a unit sees the hex AND the target
}

/// 炮型 — the `indirect_result` column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GunClass {
    Light,  // 轻型炮
    Medium, // 中型炮
    Heavy,  // 重型炮
}

/// 散布 outcome (三.9.3 d/e/f): where the salvo actually lands relative to the plan hex.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scatter {
    Hit,        // 命中 — the plan hex AND the target in it
    InHex,      // 散布 — the plan hex but not the target
    Hexes(u32), // 散布 n 格 — n hexes off the plan hex
}

/// The salvo's base result before per-target 战果修正 — shared by every unit in the impact hex.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndirectBase {
    Loss(i64), // 毁伤班数 (pre-correction)
    Suppress,  // 压制
    NoEffect,  // 无效
}

/// 战果修正 inputs for one target unit (附2.6).
#[derive(Debug, Clone, Copy)]
pub struct IndirectMods {
    pub target_terrain: TargetTerrain,
    pub target_cover: bool,
    pub target_moving_personnel: bool,
    pub target_stacked: bool,
    pub target_march: bool,
    pub armor: Armor,
    pub gun_count: u8,
}

/// One target's full 间瞄 result.
#[derive(Debug, Clone, Copy)]
pub struct IndirectShot {
    pub scatter: Scatter,
    pub outcome: Outcome,
}

fn gun_class_col(g: GunClass) -> usize {
    match g {
        GunClass::Light => 0,
        GunClass::Medium => 1,
        GunClass::Heavy => 2,
    }
}

/// Which `indirect_scatter.distance_buckets` index `dist` (hexes) falls into; clamps to the last.
fn indirect_distance_bucket(tables: &Tables, dist: i64) -> usize {
    let Some(buckets) =
        table(tables, "indirect_scatter", "distance_buckets").and_then(|b| b.as_array())
    else {
        return 0;
    };
    for (i, b) in buckets.iter().enumerate() {
        let Some(s) = b.as_str() else { continue };
        if let Some(lo) = s
            .strip_suffix('+')
            .and_then(|x| x.trim().parse::<i64>().ok())
        {
            if dist >= lo {
                return i;
            }
        } else if let Some((lo, hi)) = s.split_once('-') {
            if let (Ok(lo), Ok(hi)) = (lo.trim().parse::<i64>(), hi.trim().parse::<i64>()) {
                if dist >= lo && dist <= hi {
                    return i;
                }
            }
        }
    }
    buckets.len().saturating_sub(1)
}

fn parse_scatter(s: &str) -> Scatter {
    if s == "命中" {
        return Scatter::Hit;
    }
    if s == "散布" {
        return Scatter::InHex;
    }
    if let Some(n) = s
        .strip_prefix("散布")
        .and_then(|x| x.strip_suffix('格'))
        .and_then(|x| x.trim().parse::<u32>().ok())
    {
        return Scatter::Hexes(n);
    }
    Scatter::InHex // unknown cell → conservative scatter
}

/// 散布裁决 (附2.1-2.3): index by 校射 level + 随机数 (and, for no/格内校射, the distance bucket).
fn resolve_scatter(tables: &Tables, spotting: IndirectSpotting, dist: i64, roll: u32) -> Scatter {
    let section = match spotting {
        IndirectSpotting::None => "no_spotting",
        IndirectSpotting::InHex => "in_hex_spotting",
        IndirectSpotting::Target => "target_spotting_single",
    };
    let Some(cell) =
        table(tables, "indirect_scatter", section).and_then(|t| t.get(roll.to_string()))
    else {
        return Scatter::InHex;
    };
    let s = if let Some(arr) = cell.as_array() {
        let idx = indirect_distance_bucket(tables, dist);
        arr.get(idx).and_then(|v| v.as_str()).unwrap_or("散布")
    } else {
        cell.as_str().unwrap_or("散布")
    };
    parse_scatter(s)
}

/// 命中/偏离 result cell (附2.4-2.5): `hit` table for 命中, `miss` for 散布/散布n; by 随机数 + 炮型.
fn indirect_raw_result(tables: &Tables, hit: bool, gun: GunClass, roll: u32) -> IndirectBase {
    let section = if hit { "hit" } else { "miss" };
    let cell = table(tables, "indirect_result", section)
        .and_then(|r| r.get(roll.to_string()))
        .and_then(|row| row.as_array())
        .and_then(|row| row.get(gun_class_col(gun)));
    match cell {
        Some(v) if v.as_str() == Some("压制") => IndirectBase::Suppress,
        Some(v) if v.as_str() == Some("无效") => IndirectBase::NoEffect,
        Some(v) => match v.as_i64() {
            Some(n) if n > 0 => IndirectBase::Loss(n),
            _ => IndirectBase::NoEffect,
        },
        None => IndirectBase::NoEffect,
    }
}

/// Sum the `indirect_correction.additive_random` modifiers that apply to one target (附2.6).
fn indirect_additive(tables: &Tables, mods: &IndirectMods) -> i64 {
    let Some(add) = table(tables, "indirect_correction", "additive_random") else {
        return 0;
    };
    let get = |section: &str, key: &str| -> i64 {
        add.get(section)
            .and_then(|s| s.get(key))
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
    };
    let mut s = 0;
    match mods.target_terrain {
        TargetTerrain::UrbanOrWorks => s += get("target_terrain", "居民地"),
        TargetTerrain::Forest => s += get("target_terrain", "丛林地"),
        _ => {}
    }
    if mods.target_cover {
        s += get("target_state", "掩蔽中");
    }
    if mods.target_moving_personnel {
        s += get("target_state", "机动中_人员");
    }
    if mods.target_stacked {
        s += get("target_state", "堆叠");
    }
    if mods.target_march {
        s += get("target_state", "行军");
    }
    s += get("armor", armor_key(mods.armor)); // personnel ("无装甲") simply isn't in the table → 0
    s += get("gun_count", &mods.gun_count.clamp(1, 4).to_string());
    s
}

/// 1d6 战果修正 (附2.6): 1d6 + additive → `by_modified_random` bucket → -2..+1.
fn indirect_correction(tables: &Tables, mods: &IndirectMods, roll_1d6: u32) -> i64 {
    let modified = roll_1d6 as i64 + indirect_additive(tables, mods);
    by_modified_random(tables, "indirect_correction", modified)
        .and_then(|v| v.as_i64())
        .unwrap_or(0)
}

/// Resolve the salvo itself (三.9.3): two independent 2d6 diamonds — one for 散布, one for the
/// 命中/偏离 result (drawn up front for a stable Rng stream). The base result is shared by every
/// unit in the impact hex; each then takes its own [`apply_indirect_to_target`] correction.
/// (Two-roll interpretation — the rules don't pin the dice procedure; documented for review.)
pub fn resolve_indirect_plan(
    tables: &Tables,
    rng: &mut dyn Rng,
    spotting: IndirectSpotting,
    dist: i64,
    gun: GunClass,
) -> (Scatter, IndirectBase) {
    let scatter_roll = rng.roll_sum(2);
    let result_roll = rng.roll_sum(2);
    let scatter = resolve_scatter(tables, spotting, dist, scatter_roll);
    let base = indirect_raw_result(tables, scatter == Scatter::Hit, gun, result_roll);
    (scatter, base)
}

/// Apply the salvo's `base` to one target with its own 战果修正 (附2.6, 1d6). A positive corrected
/// loss destroys 班; a salvo that lands but corrects to 0 kills still 压制 the target.
pub fn apply_indirect_to_target(
    tables: &Tables,
    rng: &mut dyn Rng,
    base: IndirectBase,
    mods: &IndirectMods,
) -> Outcome {
    match base {
        IndirectBase::NoEffect => Outcome::NoEffect,
        IndirectBase::Suppress => Outcome::Suppress,
        IndirectBase::Loss(n) => {
            let corrected = n + indirect_correction(tables, mods, rng.d6());
            if corrected > 0 {
                Outcome::Destroyed(corrected.clamp(1, 5) as u8)
            } else {
                Outcome::Suppress
            }
        }
    }
}

/// Convenience: resolve a full 间瞄 salvo against a single target (plan + that target's
/// correction). The engine resolves the plan once, then applies it to every unit in the impact hex.
pub fn resolve_indirect(
    tables: &Tables,
    rng: &mut dyn Rng,
    spotting: IndirectSpotting,
    dist: i64,
    gun: GunClass,
    mods: &IndirectMods,
) -> IndirectShot {
    let (scatter, base) = resolve_indirect_plan(tables, rng, spotting, dist, gun);
    let outcome = apply_indirect_to_target(tables, rng, base, mods);
    IndirectShot { scatter, outcome }
}

// ---------- direct-fire flow (三.8: 射击条件 + 行进间 + 武器状态) ----------

/// Result of attempting a direct-fire shot through the full 三.8 flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FireOutcome {
    /// The shot could not be taken; the static reason is for the firing side's own log only.
    Rejected(&'static str),
    /// The shot resolved to this combat outcome.
    Resolved(Outcome),
}

/// Is `weapon` a 坦克主炮 (大号/中号直瞄炮)? Only tank main guns may fire on the move (8.3a).
fn is_tank_main_gun(weapon: &str) -> bool {
    matches!(weapon, "大号直瞄炮" | "中号直瞄炮")
}

/// Pipeline selector: infantry are personnel (pipeline B), everything else is a vehicle (A).
fn target_is_vehicle(t: UnitType) -> bool {
    !matches!(t, UnitType::Infantry)
}

fn in_motion(u: &RuntimeUnit) -> bool {
    matches!(
        u.state,
        UnitState::Moving | UnitState::March | UnitState::Charge1 | UnitState::Charge2
    )
}

/// Weapon range (hexes) for the given target kind, from the relevant attack-level table.
fn weapon_range(tables: &Tables, weapon: &str, vehicle: bool) -> Option<i64> {
    let name = if vehicle {
        "attack_level_vs_vehicle"
    } else {
        "attack_level_vs_personnel"
    };
    table(tables, name, "weapons")?
        .get(weapon)?
        .get("range")?
        .as_i64()
}

fn target_terrain(map: &Map, u: &RuntimeUnit) -> TargetTerrain {
    match map.cell(&u.pos).map(|c| c.terrain) {
        Some(Terrain::Urban) => TargetTerrain::UrbanOrWorks,
        Some(Terrain::Forest) => TargetTerrain::Forest,
        Some(Terrain::River | Terrain::RiverLarge | Terrain::Lake) => TargetTerrain::Water,
        _ => TargetTerrain::Open,
    }
}

/// 战果修正 flags (8.7) from the live shooter/target state and the target's terrain.
fn fire_mods(shooter: &RuntimeUnit, target: &RuntimeUnit, map: &Map) -> FireMods {
    FireMods {
        shooter_suppressed: shooter.state == UnitState::Suppressed,
        shooter_moving: in_motion(shooter),
        // 水上坦克 (附1.6): a tank firing while standing on a water hex. Table-keyed on 坦克, so
        // gate on the tank type exactly; the terrain class reuses the target-terrain mapping.
        shooter_water_tank: shooter.unit_type == UnitType::Tank
            && target_terrain(map, shooter) == TargetTerrain::Water,
        target_terrain: target_terrain(map, target),
        target_cover: target.state == UnitState::Cover,
        target_moving: in_motion(target),
        target_stacked: false, // stacking count lives in the engine; wired in T1.9/T1.10
        target_march: target.state == UnitState::March,
    }
}

/// Signed height difference shooter→target (>0 = shooter higher, 高看低) for 附1.4.
fn elevation_height_diff(map: &Map, shooter: &RuntimeUnit, target: &RuntimeUnit) -> i32 {
    let e = |u: &RuntimeUnit| map.cell(&u.pos).map(|c| c.elevation).unwrap_or(0);
    e(shooter) - e(target)
}

/// 三.8 direct-fire flow: check the firing conditions (8.5 + 8.3) then resolve via pipeline A or
/// B. Read-only over the units — the caller (engine) applies the [`Outcome`] and schedules
/// cooldown. Rejection reasons are returned to the *firing* side only (no fog leak).
pub fn direct_fire(
    tables: &Tables,
    map: &Map,
    rng: &mut dyn Rng,
    rules: &crate::rules::Rules,
    shooter: &RuntimeUnit,
    weapon: &str,
    target: &RuntimeUnit,
) -> FireOutcome {
    // 8.5a (FOG GATE — must be first): an unobserved target is indistinguishable from a
    // missing / own / dead one, so reject it before any shooter- or target-specific reason can
    // surface. Otherwise "weapon not ready" / "must be stopped" / "out of range" for a real but
    // hidden enemy would differ from the engine's generic "invalid fire target" for a fake id,
    // letting the firer probe the enemy order of battle (rule #5). The engine collapses this one
    // reason into "invalid fire target"; every check below therefore only runs for an already
    // observed target, so its reasons are safe to return to the firing side.
    if !can_observe(rules, map, shooter, target) {
        return FireOutcome::Rejected("target not observed");
    }
    // 8.5b: the weapon must be deployed (not 锁定 / 冷却中).
    if shooter.weapon_state != WeaponState::Deployed {
        return FireOutcome::Rejected("weapon not ready (locked or cooling)");
    }
    // 8.5c + 8.3: a ground unit must be 停止 (Stopped) to fire. The sole exception is a 坦克 firing
    // its 主炮 while genuinely 机动中 (Moving / 一二级冲锋) — NOT 行军, which bars all firing
    // (三.1.3e). So Normal / Half / Cover / Suppressed / March all cannot fire a non-main-gun, and
    // only a Tank may take the on-the-move shot (a non-tank can't wield a 坦克主炮).
    let moving_fire_ok = shooter.unit_type == UnitType::Tank
        && is_tank_main_gun(weapon)
        && matches!(
            shooter.state,
            UnitState::Moving | UnitState::Charge1 | UnitState::Charge2
        );
    if shooter.state != UnitState::Stopped && !moving_fire_ok {
        return FireOutcome::Rejected("must be stopped to fire (8.5c)");
    }
    let vehicle = target_is_vehicle(target.unit_type);
    let dist = shooter.pos.distance(&target.pos);
    let Some(range) = weapon_range(tables, weapon, vehicle) else {
        return FireOutcome::Rejected("weapon cannot engage this target");
    };
    if i64::from(dist) > range {
        return FireOutcome::Rejected("out of range");
    }

    let mods = fire_mods(shooter, target, map);
    let outcome = if vehicle {
        resolve_direct_vs_vehicle(
            tables,
            rng,
            &VehicleShot {
                weapon,
                vehicles: shooter.teams,
                distance: dist,
                height_diff: elevation_height_diff(map, shooter, target),
                armor: target.armor,
                mods,
            },
        )
    } else {
        resolve_direct_vs_personnel(tables, rng, weapon, shooter.teams, dist, mods)
    };
    FireOutcome::Resolved(outcome)
}

/// 三.10f 巡飞弹打击: resolve a deployed 巡飞弹's strike on `target` via the 直瞄对车 pipeline A. The
/// ruleset gives 巡飞弹 an attack-level row in 附1.2 (`attack_level_vs_vehicle` → range 2, level 5) but
/// NO dedicated result table, so the strike reuses the vs-vehicle result table (confirmed by PDF
/// inspection; this is the user-chosen pipeline-A model). Unlike [`direct_fire`] there is NO
/// weapon-state / 停止 gate — a 巡飞弹 strikes WHILE airborne (三.10f, 无需飞完全部机动路线). The 三.10b
/// 侦察/fog gate is the caller's (the engine collapses an unobserved target into a generic error,
/// rule #5 — same split as `aa_fire`). The caller applies the [`Outcome`] and consumes the munition
/// (one-shot). Read-only over the board.
pub fn loitering_strike(
    tables: &Tables,
    map: &Map,
    rng: &mut dyn Rng,
    munition: &RuntimeUnit,
    weapon: &str,
    target: &RuntimeUnit,
) -> FireOutcome {
    let dist = munition.pos.distance(&target.pos);
    let Some(range) = weapon_range(tables, weapon, true) else {
        return FireOutcome::Rejected("weapon cannot engage this target");
    };
    if i64::from(dist) > range {
        return FireOutcome::Rejected("out of range");
    }
    let mods = fire_mods(munition, target, map);
    let outcome = resolve_direct_vs_vehicle(
        tables,
        rng,
        &VehicleShot {
            weapon,
            vehicles: munition.teams,
            distance: dist,
            // 附1.4 高差 from terrain elevations; the +200 m air altitude bonus (三.10a) is deferred.
            height_diff: elevation_height_diff(map, munition, target),
            armor: target.armor,
            mods,
        },
    );
    FireOutcome::Resolved(outcome)
}

/// 三.20.1c/2c — resolve one DIRECT shot AT a 工事 (the structure itself; its garrison is 全程隐蔽 and
/// never individually targeted). The 工事 adjudicates as a vehicle-class target (附1) against its OWN
/// armour + 居民地/工事 terrain. Unlike [`direct_fire`], the fog gate is the 工事's exposure range
/// (`observe_range`, 12 格 for 车辆/战斗工事) + 通视 — NOT the unit recon table. The caller applies the
/// returned [`Outcome`] per 战果继承 (三.20.1e/2e). Read-only over the board.
#[allow(clippy::too_many_arguments)]
pub fn direct_fire_at_works(
    tables: &Tables,
    map: &Map,
    rng: &mut dyn Rng,
    rules: &crate::rules::Rules,
    shooter: &RuntimeUnit,
    weapon: &str,
    works_pos: crate::hex::Axial,
    works_armor: Armor,
    observe_range: i64,
) -> FireOutcome {
    // FOG GATE first (rule #5): the 工事 must be observed (within its exposure range + 通视), else it
    // is indistinguishable from a missing/unobservable id — the engine collapses this to the generic
    // "invalid fire target".
    let dist = shooter.pos.distance(&works_pos);
    let observed = observe_range > 0
        && i64::from(dist) <= observe_range
        && crate::mechanics::line_of_sight_alt(
            map,
            &shooter.pos,
            &works_pos,
            crate::mechanics::air_altitude_levels(rules, map, shooter),
            0,
        );
    if !observed {
        return FireOutcome::Rejected("target not observed");
    }
    if shooter.weapon_state != WeaponState::Deployed {
        return FireOutcome::Rejected("weapon not ready (locked or cooling)");
    }
    let moving_fire_ok = shooter.unit_type == UnitType::Tank
        && is_tank_main_gun(weapon)
        && matches!(
            shooter.state,
            UnitState::Moving | UnitState::Charge1 | UnitState::Charge2
        );
    if shooter.state != UnitState::Stopped && !moving_fire_ok {
        return FireOutcome::Rejected("must be stopped to fire (8.5c)");
    }
    let Some(range) = weapon_range(tables, weapon, true) else {
        return FireOutcome::Rejected("weapon cannot engage this target");
    };
    if i64::from(dist) > range {
        return FireOutcome::Rejected("out of range");
    }
    // The 工事 is a vehicle-class target in 居民地/工事 terrain, defended by its OWN armour (三.20.1d/2d).
    let height = map.cell(&shooter.pos).map(|c| c.elevation).unwrap_or(0)
        - map.cell(&works_pos).map(|c| c.elevation).unwrap_or(0);
    let mods = FireMods {
        target_terrain: TargetTerrain::UrbanOrWorks,
        ..Default::default()
    };
    let outcome = resolve_direct_vs_vehicle(
        tables,
        rng,
        &VehicleShot {
            weapon,
            vehicles: shooter.teams,
            distance: dist,
            height_diff: height,
            armor: works_armor,
            mods,
        },
    );
    FireOutcome::Resolved(outcome)
}

// ---------- pipeline: 引导射击 (三.14) ----------

/// 三.14a — units that can act as a 引导算子: 步兵 / 无人战车 / 无人机.
pub fn can_guide(t: UnitType) -> bool {
    matches!(t, UnitType::Infantry | UnitType::Ugv | UnitType::Uav)
}

/// 三.14e — only a 战车's 重型导弹 may be 被引导.
fn is_heavy_missile(weapon: &str) -> bool {
    weapon == "重型导弹"
}

/// 三.14d — is the 引导算子 NOT in a state to guide? Moving or 压制 always disqualifies; a 步兵/无人车
/// also needs a ready (Deployed) weapon; a 无人机 must be hovering (Stopped). The "executing another
/// command" / cooldown checks are time-based and enforced by the engine.
fn guide_unfit(guide: &RuntimeUnit) -> bool {
    if in_motion(guide) || guide.state == UnitState::Suppressed {
        return true;
    }
    match guide.unit_type {
        UnitType::Uav => guide.state != UnitState::Stopped, // 无人机必须悬停
        _ => guide.weapon_state != WeaponState::Deployed,   // 步兵/无人车 武器需展开
    }
}

/// 三.14 引导射击: a 引导算子 that OBSERVES `target` lets a 重型导弹 战车 fire on it WITHOUT the
/// vehicle's own 通视 (三.14a.ii 可不通视). Checks the guide/vehicle conditions (三.14a/c/d/e) then
/// resolves via pipeline A/B from the *vehicle*. The ownership link (步兵/无人车 → 隶属车辆 vs 无人机
/// → any heavy-missile vehicle, 三.14b) and the 75 s prep (三.14f) are enforced by the engine, where
/// the 隶属 relationship and the clock live.
// Mirrors `direct_fire`'s signature plus the separate 引导算子 — the parties are genuinely distinct.
#[allow(clippy::too_many_arguments)]
pub fn guided_fire(
    tables: &Tables,
    map: &Map,
    rng: &mut dyn Rng,
    rules: &crate::rules::Rules,
    guide: &RuntimeUnit,
    vehicle: &RuntimeUnit,
    weapon: &str,
    target: &RuntimeUnit,
) -> FireOutcome {
    if !is_heavy_missile(weapon) {
        return FireOutcome::Rejected("only the 重型导弹 supports guided fire (三.14e)");
    }
    if !can_guide(guide.unit_type) {
        return FireOutcome::Rejected("this unit cannot guide (三.14a)");
    }
    // 三.14a.i: the guide must genuinely observe the target.
    if !can_observe(rules, map, guide, target) {
        return FireOutcome::Rejected("the guide does not observe the target (三.14a.i)");
    }
    if guide_unfit(guide) {
        return FireOutcome::Rejected("the guide is not in a state to guide (三.14d)");
    }
    // 三.14c: the firing 战车 must have its 重型导弹 deployed & cooled and be stopped & idle.
    if vehicle.weapon_state != WeaponState::Deployed {
        return FireOutcome::Rejected("the missile vehicle's weapon is not ready (三.14c)");
    }
    if vehicle.state != UnitState::Stopped {
        return FireOutcome::Rejected("the missile vehicle must be stopped (三.14c)");
    }
    // 三.14a.ii: the target must be within the 重型导弹's range — but the vehicle needs NO 通视.
    let is_veh = target_is_vehicle(target.unit_type);
    let dist = vehicle.pos.distance(&target.pos);
    let Some(range) = weapon_range(tables, weapon, is_veh) else {
        return FireOutcome::Rejected("weapon cannot engage this target");
    };
    if i64::from(dist) > range {
        return FireOutcome::Rejected("target out of missile range (三.14a.ii)");
    }
    let mods = fire_mods(vehicle, target, map);
    let outcome = if is_veh {
        resolve_direct_vs_vehicle(
            tables,
            rng,
            &VehicleShot {
                weapon,
                vehicles: vehicle.teams,
                distance: dist,
                height_diff: elevation_height_diff(map, vehicle, target),
                armor: target.armor,
                mods,
            },
        )
    } else {
        resolve_direct_vs_personnel(tables, rng, weapon, vehicle.teams, dist, mods)
    };
    FireOutcome::Resolved(outcome)
}

// ---------- 同格交战 (三.15) ----------

/// The default weapons a unit of type `t` carries (三.15a loadout, rules-as-data via `rules.loadout`).
pub fn unit_loadout(rules: &crate::rules::Rules, t: UnitType) -> Vec<String> {
    rules
        .loadout
        .get(crate::mechanics::unit_type_key(t))
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|w| w.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// 三.15a target-priority rank (lower = higher): 坦克 > 战车(步战车) > 其他车辆 > 人员.
fn same_hex_priority_rank(t: UnitType) -> u8 {
    match t {
        UnitType::Tank => 0,                                // 坦克
        UnitType::Ifv => 1,                                 // 战车
        UnitType::Infantry | UnitType::AaMissileSquad => 3, // 人员
        _ => 2,                                             // 其他车辆
    }
}

/// 三.15a — choose the highest-priority enemy GROUND unit in the hex for an attacker on `side`:
/// 坦克 > 战车 > 其他车辆 > 人员; ties broken by fewest 班组数, then at random (seeded `rng`).
pub fn pick_same_hex_target<'a>(
    rng: &mut dyn Rng,
    side: SideId,
    in_hex: &[&'a RuntimeUnit],
) -> Option<&'a RuntimeUnit> {
    let mut enemies: Vec<&RuntimeUnit> = in_hex
        .iter()
        .copied()
        // 三.2/三.15: a 被载 unit rides inside its carrier and is not independently targetable.
        // 三.22b: the off-board 天基侦察算子 is never a 同格 target either.
        .filter(|u| {
            u.alive
                && u.side != side
                && u.is_on_board()
                && crate::mechanics::is_ground(u.unit_type)
                && crate::mechanics::can_be_targeted(u.unit_type)
        })
        .collect();
    if enemies.is_empty() {
        return None;
    }
    let best_rank = enemies
        .iter()
        .map(|u| same_hex_priority_rank(u.unit_type))
        .min()
        .unwrap_or(u8::MAX);
    enemies.retain(|u| same_hex_priority_rank(u.unit_type) == best_rank);
    let min_teams = enemies.iter().map(|u| u.teams).min().unwrap_or(0);
    enemies.retain(|u| u.teams == min_teams);
    let idx = if enemies.len() > 1 {
        rng.next_u32_below(enemies.len() as u32) as usize
    } else {
        0
    };
    enemies.get(idx).copied()
}

/// The best (highest attack level) weapon from `weapons` against a target, at the given distance.
pub fn best_weapon<'a>(
    tables: &Tables,
    weapons: &[&'a str],
    target_vehicle: bool,
    attacker_squads: u8,
    distance: i32,
) -> Option<&'a str> {
    weapons
        .iter()
        .copied()
        .filter_map(|w| {
            let lvl = if target_vehicle {
                attack_level_vehicle(tables, w, attacker_squads, distance)
            } else {
                attack_level_personnel(tables, w, attacker_squads, distance)
            };
            lvl.map(|l| (l, w))
        })
        .max_by_key(|(l, _)| *l)
        .map(|(_, w)| w)
}

/// 三.15 同格交战 — `attacker` fires its best weapon at distance 0 at `target`, via pipeline A/B.
/// Returns `NoEffect` if the attacker has no weapon that can engage that target.
pub fn same_hex_engage(
    tables: &Tables,
    map: &Map,
    rng: &mut dyn Rng,
    attacker_weapons: &[&str],
    attacker: &RuntimeUnit,
    target: &RuntimeUnit,
) -> Outcome {
    let target_veh = target_is_vehicle(target.unit_type);
    let Some(weapon) = best_weapon(tables, attacker_weapons, target_veh, attacker.teams, 0) else {
        return Outcome::NoEffect;
    };
    let mods = fire_mods(attacker, target, map);
    if target_veh {
        resolve_direct_vs_vehicle(
            tables,
            rng,
            &VehicleShot {
                weapon,
                vehicles: attacker.teams,
                distance: 0,
                height_diff: 0,
                armor: target.armor,
                mods,
            },
        )
    } else {
        resolve_direct_vs_personnel(tables, rng, weapon, attacker.teams, 0, mods)
    }
}

// ---------- 流水线 D：防空 (三.18 / 附3) ----------

/// 附3.1 防空攻击等级 — `weapon` firing at an air target `distance` hexes away. Returns `None` when
/// the shot is impossible: target beyond 防空射程, inside 最小射程, or any cell printed as 0 (the
/// d0/d1 cells of the two missiles). A distance past the printed columns but within range uses the
/// flat far tail (`tail_value`, see `aa_attack_level.json` — 附3.1 is a scrollable widget so the PDF
/// only prints d0..d19 and the long 车载 tail is the visible plateau).
fn aa_attack_level(tables: &Tables, weapon: &str, distance: i32) -> Option<i64> {
    let entry = table(tables, "aa_attack_level", "weapons")?.get(weapon)?;
    let range = entry.get("range").and_then(|v| v.as_i64())?;
    if distance < 0 || i64::from(distance) > range {
        return None; // 超出防空射程
    }
    let levels = entry.get("levels").and_then(|v| v.as_array())?;
    let idx = distance as usize;
    let level = if idx < levels.len() {
        levels.get(idx)?.as_i64()?
    } else {
        // within range but past the printed columns → the constant far tail.
        entry.get("tail_value").and_then(|v| v.as_i64())?
    };
    // attack level 0 = cannot engage (最小射程 cells).
    (level > 0).then_some(level)
}

/// 附3.2 歼灭阈值 for a 2d6 `roll`: the air target is 歼灭 iff 攻击等级 ≥ threshold(roll). The
/// threshold is the 钻石型 curve (symmetric about roll 7, the hardest to hit).
fn aa_kill_threshold(tables: &Tables, roll: u32) -> Option<i64> {
    table(tables, "aa_result", "kill_threshold_by_random")?
        .get(roll.to_string())?
        .as_i64()
}

/// One防空 adjudication: roll 2d6, 歼灭 iff 攻击等级 ≥ kill-threshold(roll) (附3.2).
fn aa_adjudicate(tables: &Tables, rng: &mut dyn Rng, level: i64) -> bool {
    let roll = rng.roll_sum(2); // 2..12
                                // A complete, verified table covers 2..12; an (impossible) missing cell is treated as
                                // unreachable rather than aborting the salvo mid-way (no panic, deterministic).
    let threshold = aa_kill_threshold(tables, roll).unwrap_or(i64::MAX);
    level >= threshold
}

/// Outcome of a 防空 salvo: whether the air target was 歼灭 (destroyed), and how many of the
/// adjudications scored a kill (out of `adjudications`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AaOutcome {
    pub destroyed: bool,
    pub adjudications: u8,
    pub kills: u8,
}

/// 流水线 D 防空 (三.18 / 附3): a 防空算子 with `shooters` 车/班 fires once at a single air target
/// `distance` hexes away. 三.18f: 一次动作的裁决次数 = 当前车/班数 — so the salvo runs `shooters`
/// independent 附3.2 adjudications, and the target dies if ANY of them 歼灭s it (an air unit is a
/// single airframe — 歼灭 = killed). Returns `None` when there is no valid salvo: zero shooters, or
/// the target is out of range / inside 最小射程 / on a 0-level cell — and in every `None` case NO
/// dice are drawn, keeping the Rng stream stable when the engine rejects an impossible AA order.
///
/// This is the PURE adjudication core, exactly like [`direct_fire`]: it does NOT check fog-of-war.
/// The engine's (deferred) `aa_fire` op is responsible — before calling — for the 三.18e
/// observation gate (自带雷达 sees air at 50 格, 无人机 减半 25 格, 巡飞弹 不可观察) and for the
/// 射速/弹药上限 (15s×10 / 75s×4 / 15s×4, `timings.json.fire_intervals`). Calling this for an
/// unobserved target would leak `destroyed/kills` (rule #5), so the engine MUST gate it.
pub fn air_defense(
    tables: &Tables,
    rng: &mut dyn Rng,
    weapon: &str,
    shooters: u8,
    distance: i32,
) -> Option<AaOutcome> {
    if shooters == 0 {
        return None; // 三.18f: a 0-车/班 (dead/empty) operator has no salvo — and draws no dice.
    }
    let level = aa_attack_level(tables, weapon, distance)?;
    let mut kills = 0u8;
    for _ in 0..shooters {
        if aa_adjudicate(tables, rng, level) {
            kills = kills.saturating_add(1);
        }
    }
    Some(AaOutcome {
        destroyed: kills > 0,
        adjudications: shooters,
        kills,
    })
}

// ---------- 流水线 E：雷场 (三.21 / 附4) ----------

/// 三.21 雷场 target category for the 附4.1 damage table: 人员 (步兵/防空导弹小队) or 车辆 (every other
/// GROUND unit). Only ground units ever enter a 雷场 (空中单位 fly over), so the 车辆 default is safe;
/// the engine only calls the 雷场 path for a unit physically entering the hex.
pub fn minefield_category(t: UnitType) -> &'static str {
    if matches!(t, UnitType::Infantry | UnitType::AaMissileSquad) {
        "人员"
    } else {
        "车辆"
    }
}

/// 附4.1 — base 雷场 损伤 (班数) for a `category` (车辆/人员) at a 2d6 `roll`.
fn minefield_base_damage(tables: &Tables, category: &str, roll: u32) -> i64 {
    table(tables, "minefield", "damage_by_random")
        .and_then(|d| d.get(category))
        .and_then(|c| c.get(roll.to_string()))
        .and_then(|v| v.as_i64())
        .unwrap_or(0)
}

/// 附4.2 — 装甲修正 to the 雷场 损伤 at a 2d6 `roll`. A `null` cell means no correction (0).
fn minefield_armor_correction(tables: &Tables, armor: Armor, roll: u32) -> i64 {
    table(tables, "minefield", "armor_correction_by_random")
        .and_then(|a| a.get(armor_key(armor)))
        .and_then(|c| c.get(roll.to_string()))
        .and_then(|v| v.as_i64()) // JSON null → None → 0
        .unwrap_or(0)
}

/// 流水线 E 雷场 (三.21e / 附4) for a fixed 2d6 `roll`: 损伤 = base(车辆/人员 category of `unit_type`) +
/// 装甲修正(armor), floored at 0 (班数 destroyed). Typed on `UnitType` (not a stringly category) so a
/// caller can't silently mis-key it; split out so tests can pin exact cells without the Rng.
pub fn minefield_damage_for_roll(
    tables: &Tables,
    unit_type: UnitType,
    armor: Armor,
    roll: u32,
) -> u32 {
    let dmg = minefield_base_damage(tables, minefield_category(unit_type), roll)
        + minefield_armor_correction(tables, armor, roll);
    dmg.max(0) as u32
}

/// 流水线 E 雷场 (三.21e / 附4): a unit entering an UN-cleared 雷场 hex (no friendly 通路) takes a
/// 毁伤裁决 — roll 2d6, then `minefield_damage_for_roll`. (三.21i: moving ALONG a friendly path —
/// personnel freely, vehicles at half speed — draws NO adjudication; that gate is the engine's.)
pub fn minefield_damage(
    tables: &Tables,
    rng: &mut dyn Rng,
    unit_type: UnitType,
    armor: Armor,
) -> u32 {
    let roll = rng.roll_sum(2); // 2..12
    minefield_damage_for_roll(tables, unit_type, armor, roll)
}

// ---------- 一.1.3 间瞄炮火区 (indirect barrage zone) ----------

/// 一.1.3 设施: a 地面算子 crossing INTO or OUT OF an active 间瞄炮火区 takes an 间瞄火力裁决 (详见间瞄
/// 射击规则和裁决表). The barrage already covers the zone hex, so the crossing unit is adjudicated as
/// a 命中 on its hex (interpretive default — the zone IS the impact area; no fresh 散布 roll): resolve
/// the 附2 result for the zone's `gun` class, then the per-target 战果修正 from `mods` — i.e. the same
/// machinery as a landed 间瞄 salvo (T2.3), reused. 空中算子 fly over (see `barrage_zone_triggers`).
pub fn barrage_zone_adjudicate(
    tables: &Tables,
    rng: &mut dyn Rng,
    gun: GunClass,
    mods: &IndirectMods,
) -> Outcome {
    let roll = rng.roll_sum(2); // 命中 the zone hex → the 附2 result row
    let base = indirect_raw_result(tables, true, gun, roll);
    apply_indirect_to_target(tables, rng, base, mods)
}

// ---------- tests (done-when: combat::direct_vs_personnel / combat::direct_vs_vehicle) ----------

/// 直瞄对人员流水线 B (T1.6): across the 11 possible 2d6 rolls a single attack level yields all
/// three outcome kinds — 毁伤(Destroyed) / 压制(Suppress) / 无效(NoEffect) — deterministically.
#[cfg(test)]
#[test]
fn direct_vs_personnel() {
    use crate::rng::PcgRng;
    let tables = Tables::load_embedded().unwrap();
    // 大号直瞄炮, 3 squads ⇒ attack level 5 (any distance). Column 5 of result_vs_personnel has
    // 压 / a number / blank across the 2..12 rows, so neutral fire yields each outcome.
    let mut rng = PcgRng::from_seed(7);
    let mut destroyed = false;
    let mut suppress = false;
    let mut no_effect = false;
    for _ in 0..400 {
        match resolve_direct_vs_personnel(
            &tables,
            &mut rng,
            "大号直瞄炮",
            3,
            0,
            FireMods::default(),
        ) {
            Outcome::Destroyed(n) => {
                assert!(n >= 1);
                destroyed = true;
            }
            Outcome::Suppress => suppress = true,
            Outcome::NoEffect => no_effect = true,
            Outcome::Kill => {}
        }
    }
    assert!(destroyed, "expected at least one 毁伤 result");
    assert!(suppress, "expected at least one 压制 result");
    assert!(no_effect, "expected at least one 无效 result");

    // Out-of-table / nonsense weapon ⇒ NoEffect, never a panic.
    let mut r2 = PcgRng::from_seed(1);
    assert_eq!(
        resolve_direct_vs_personnel(&tables, &mut r2, "不存在", 2, 0, FireMods::default()),
        Outcome::NoEffect
    );
    // Determinism: same seed ⇒ same outcome stream.
    let seq = |seed| {
        let mut r = PcgRng::from_seed(seed);
        (0..50)
            .map(|_| {
                resolve_direct_vs_personnel(
                    &tables,
                    &mut r,
                    "大号直瞄炮",
                    3,
                    0,
                    FireMods::default(),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(seq(42), seq(42));
}

/// 直瞄对车辆流水线 A (T1.7): a strong gun close in destroys vehicles, a weak hit is no-effect,
/// and the whole pipeline is deterministic under a fixed seed.
#[cfg(test)]
#[test]
fn direct_vs_vehicle() {
    use crate::rng::PcgRng;
    let tables = Tables::load_embedded().unwrap();
    // 大号直瞄炮 at d0 = attack level 10; with 3 vehicles stage1 lifts it well up the stage2
    // columns, so most 2d6 rolls destroy at least one tank.
    let shot = |armor| VehicleShot {
        weapon: "大号直瞄炮",
        vehicles: 3,
        distance: 0,
        height_diff: 0,
        armor,
        mods: FireMods::default(),
    };

    let mut rng = PcgRng::from_seed(3);
    let mut destroyed = false;
    for _ in 0..400 {
        match resolve_direct_vs_vehicle(&tables, &mut rng, &shot(Armor::Medium)) {
            Outcome::Destroyed(n) => {
                assert!((1..=5).contains(&n));
                destroyed = true;
            }
            Outcome::NoEffect => {}
            _ => {}
        }
    }
    assert!(destroyed, "a level-10 gun up close should destroy vehicles");

    // A weapon out of range / unknown ⇒ NoEffect (no panic).
    let mut r2 = PcgRng::from_seed(1);
    assert_eq!(
        resolve_direct_vs_vehicle(
            &tables,
            &mut r2,
            &VehicleShot {
                weapon: "巡飞弹",
                vehicles: 1,
                distance: 18,
                height_diff: 0,
                armor: Armor::Composite,
                mods: FireMods::default(),
            }
        ),
        Outcome::NoEffect,
        "巡飞弹 (range 2) cannot reach distance 18"
    );

    // Armor matters: against 复合装甲 the same shot destroys no more than against 无装甲.
    let count = |armor| {
        let mut r = PcgRng::from_seed(99);
        (0..300)
            .filter_map(
                |_| match resolve_direct_vs_vehicle(&tables, &mut r, &shot(armor)) {
                    Outcome::Destroyed(n) => Some(n as u32),
                    _ => None,
                },
            )
            .sum::<u32>()
    };
    assert!(
        count(Armor::Composite) <= count(Armor::None),
        "composite armor must not take more losses than no armor"
    );

    // Determinism.
    let seq = |seed| {
        let mut r = PcgRng::from_seed(seed);
        (0..50)
            .map(|_| resolve_direct_vs_vehicle(&tables, &mut r, &shot(Armor::Medium)))
            .collect::<Vec<_>>()
    };
    assert_eq!(seq(5), seq(5));
}

/// 战果遥测 (A2 — 分析用，不改模型)：direct-fire `OUTCOME_LOG` 默认关闭、启用后逐次裁决各记一条，
/// 且**纯观测**——启用它在固定 seed 下绝不能改变裁决结果（硬规则 #1 重放一致）。每个测试线程有
/// 独立的 thread-local 缓冲，故此测试自带隔离。
#[cfg(test)]
#[test]
fn outcome_telemetry_is_observational() {
    use crate::rng::PcgRng;
    let tables = Tables::load_embedded().unwrap();
    let shot = VehicleShot {
        weapon: "大号直瞄炮",
        vehicles: 3,
        distance: 0,
        height_diff: 0,
        armor: Armor::Medium,
        mods: FireMods::default(),
    };

    // 默认关闭：裁决不留任何记录。
    let mut r0 = PcgRng::from_seed(7);
    let off: Vec<_> = (0..20)
        .map(|_| resolve_direct_vs_vehicle(&tables, &mut r0, &shot))
        .collect();
    assert!(
        outcome_log_drain().is_empty(),
        "遥测默认关闭——未 enable 不应有记录"
    );

    // 启用后：同一 seed ⇒ 完全相同的结果流（观测不消耗 rng），且每次裁决恰好捕获一条。
    outcome_log_enable();
    let mut r1 = PcgRng::from_seed(7);
    let on: Vec<_> = (0..20)
        .map(|_| resolve_direct_vs_vehicle(&tables, &mut r1, &shot))
        .collect();
    let recs = outcome_log_drain();
    assert_eq!(off, on, "启用遥测不得扰动战斗结果（硬规则 #1）");
    assert_eq!(recs.len(), 20, "每次裁决恰好一条记录");
    for (rec, o) in recs.iter().zip(&on) {
        assert_eq!(rec.table, "direct_vs_vehicle");
        assert_eq!(rec.armor, "medium");
        assert_eq!(rec.weapon, "大号直瞄炮");
        assert_eq!(&rec.outcome, o, "记录的结果须等于实际裁决结果");
    }

    // drain() 保持记录开启：再裁一次会被继续捕获。
    let mut r2 = PcgRng::from_seed(7);
    let _ = resolve_direct_vs_vehicle(&tables, &mut r2, &shot);
    assert_eq!(outcome_log_drain().len(), 1, "drain 后仍处于记录态");
}

/// 间瞄流水线 C (T2.3, 三.9 / 附2): scatter classification (命中 / 散布 / 散布n格), the 命中-vs-偏离
/// result tables, and a full salvo that destroys 班 — the same damage a friendly unit in the
/// impact hex would take (三.9.3c 对己方也生效).
#[cfg(test)]
#[test]
fn indirect_fire() {
    use crate::rng::PcgRng;
    let tables = Tables::load_embedded().unwrap();

    // 散布裁决 (附2.1-2.3): specific (校射, 随机数, 距离) cells classify deterministically.
    assert_eq!(
        resolve_scatter(&tables, IndirectSpotting::None, 5, 4),
        Scatter::Hit
    ); // no_spotting[4][bucket 1-20] = 命中
    assert_eq!(
        resolve_scatter(&tables, IndirectSpotting::None, 5, 2),
        Scatter::Hexes(1)
    ); // [2][1-20] = 散布1格
    assert_eq!(
        resolve_scatter(&tables, IndirectSpotting::None, 70, 2),
        Scatter::Hexes(4)
    ); // [2][61+] = 散布4格
    assert_eq!(
        resolve_scatter(&tables, IndirectSpotting::None, 5, 5),
        Scatter::InHex
    ); // [5][1-20] = 散布
    assert_eq!(
        resolve_scatter(&tables, IndirectSpotting::Target, 99, 6),
        Scatter::Hit
    ); // target_spotting[6] = 命中
    assert_eq!(
        resolve_scatter(&tables, IndirectSpotting::Target, 99, 2),
        Scatter::InHex
    ); // target_spotting[2] = 散布

    // 命中 vs 偏离 result (附2.4-2.5): the hit table is far deadlier than the miss table.
    assert_eq!(
        indirect_raw_result(&tables, true, GunClass::Heavy, 2),
        IndirectBase::Loss(3)
    ); // hit[2][重型炮] = 3
    assert_eq!(
        indirect_raw_result(&tables, true, GunClass::Light, 7),
        IndirectBase::Suppress
    ); // hit[7][轻型炮] = 压制
    assert_eq!(
        indirect_raw_result(&tables, false, GunClass::Medium, 5),
        IndirectBase::NoEffect
    ); // miss[5][中型炮] = 无效

    // A full salvo on exposed personnel: across the 2d6 stream it yields 命中, 散布n格 and 毁伤 — the
    // last being exactly the friendly-fire damage a unit in the impact hex suffers (side-agnostic).
    let exposed = IndirectMods {
        target_terrain: TargetTerrain::Open,
        target_cover: false,
        target_moving_personnel: false,
        target_stacked: false,
        target_march: false,
        armor: Armor::None,
        gun_count: 4,
    };
    let mut rng = PcgRng::from_seed(7);
    let (mut saw_hit, mut saw_scatter_n, mut saw_destroyed) = (false, false, false);
    for _ in 0..600 {
        let shot = resolve_indirect(
            &tables,
            &mut rng,
            IndirectSpotting::None,
            5,
            GunClass::Heavy,
            &exposed,
        );
        match shot.scatter {
            Scatter::Hit => saw_hit = true,
            Scatter::Hexes(n) => {
                assert!(n >= 1);
                saw_scatter_n = true;
            }
            Scatter::InHex => {}
        }
        if let Outcome::Destroyed(k) = shot.outcome {
            assert!((1..=5).contains(&k));
            saw_destroyed = true;
        }
    }
    assert!(saw_hit, "expected at least one 命中");
    assert!(saw_scatter_n, "expected at least one 散布n格");
    assert!(
        saw_destroyed,
        "expected a 毁伤 — the friendly-fire damage a unit in the area takes (三.9.3c)"
    );

    // 战果修正 determinism: same seed ⇒ same correction stream for a protected target.
    let protected = IndirectMods {
        target_cover: true,
        armor: Armor::Composite,
        gun_count: 1,
        ..exposed
    };
    let seq = |seed| {
        let mut r = PcgRng::from_seed(seed);
        (0..30)
            .map(|_| apply_indirect_to_target(&tables, &mut r, IndirectBase::Loss(3), &protected))
            .collect::<Vec<_>>()
    };
    assert_eq!(seq(11), seq(11));
}

/// 引导射击 (T2.6, 三.14): a 无人机 that observes the target lets a 重型导弹 战车 fire on it without
/// the vehicle's own 通视; the guide/vehicle conditions (三.14a/c/d/e) and the missile range gate it.
#[cfg(test)]
#[test]
fn guided_fire_pipeline() {
    use crate::hex::Axial;
    use crate::rng::PcgRng;
    use crate::rules::Rules;
    use crate::types::{HexCell, Map, SideId, Terrain};

    let map = Map {
        format: "openstratcore.map".into(),
        version: 1,
        name: "g".into(),
        elevation_unit_meters: Some(10),
        hexes: (0..=25)
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
    let rules = Rules::from_json_str(include_str!("../../../config/rules.default.json")).unwrap();
    let u = |id: &str, side, ut, q: i32, ws| RuntimeUnit {
        id: id.into(),
        side,
        unit_type: ut,
        armor: Armor::Medium,
        teams: 3,
        pos: Axial::new(q, 0),
        facing: 0,
        state: UnitState::Stopped,
        weapon_state: ws,
        busy_until: 0,
        suppressed_until: 0,
        alive: true,
        carried_by: None,
        affiliated_to: None,
        heli_alt: crate::types::HeliAlt::Low,
        inside_facility: None,
        fatigue: 0,
    };
    let tables = Tables::load_embedded().unwrap();
    let mut rng = PcgRng::from_seed(5);
    // Target tank at d5; the UAV guide adjacent to it (observes); the missile vehicle far back but
    // within the 重型导弹 20-hex range (and with NO line of sight needed).
    let target = u("T", SideId::Blue, UnitType::Tank, 5, WeaponState::Deployed);
    let guide = u("G", SideId::Red, UnitType::Uav, 4, WeaponState::Deployed);
    let veh = u("V", SideId::Red, UnitType::Tank, 0, WeaponState::Deployed);

    // Valid: it resolves to some combat outcome.
    assert!(matches!(
        guided_fire(
            &tables,
            &map,
            &mut rng,
            &rules,
            &guide,
            &veh,
            "重型导弹",
            &target
        ),
        FireOutcome::Resolved(_)
    ));

    let reason = |o| match o {
        FireOutcome::Rejected(r) => r,
        FireOutcome::Resolved(_) => "RESOLVED",
    };
    // 三.14e: only the 重型导弹 may be guided.
    assert_eq!(
        reason(guided_fire(
            &tables,
            &map,
            &mut rng,
            &rules,
            &guide,
            &veh,
            "大号直瞄炮",
            &target
        )),
        "only the 重型导弹 supports guided fire (三.14e)"
    );
    // 三.14a.i: a guide that cannot observe the target (a UAV 5 hexes away, recon range 2).
    let far_guide = u("G2", SideId::Red, UnitType::Uav, 0, WeaponState::Deployed);
    assert_eq!(
        reason(guided_fire(
            &tables,
            &map,
            &mut rng,
            &rules,
            &far_guide,
            &veh,
            "重型导弹",
            &target
        )),
        "the guide does not observe the target (三.14a.i)"
    );
    // 三.14c: the missile vehicle's weapon must be ready.
    let cooling = u("V2", SideId::Red, UnitType::Tank, 0, WeaponState::Cooling);
    assert_eq!(
        reason(guided_fire(
            &tables,
            &map,
            &mut rng,
            &rules,
            &guide,
            &cooling,
            "重型导弹",
            &target
        )),
        "the missile vehicle's weapon is not ready (三.14c)"
    );
    // 三.14a.ii: the target out of the 重型导弹's 20-hex range (vehicle at d0, target at d25).
    let far_target = u(
        "T2",
        SideId::Blue,
        UnitType::Tank,
        25,
        WeaponState::Deployed,
    );
    let near_guide = u("G3", SideId::Red, UnitType::Uav, 24, WeaponState::Deployed);
    assert_eq!(
        reason(guided_fire(
            &tables,
            &map,
            &mut rng,
            &rules,
            &near_guide,
            &veh,
            "重型导弹",
            &far_target
        )),
        "target out of missile range (三.14a.ii)"
    );
    // 三.14d: a moving guide cannot guide.
    let mut moving = guide.clone();
    moving.state = UnitState::Moving;
    assert_eq!(
        reason(guided_fire(
            &tables,
            &map,
            &mut rng,
            &rules,
            &moving,
            &veh,
            "重型导弹",
            &target
        )),
        "the guide is not in a state to guide (三.14d)"
    );
}

/// 同格交战 (T2.8, 三.15): the loadout, the 三.15a target priority (坦克 > … > 人员, fewest 班组 on
/// ties), the best-weapon pick, and the distance-0 resolution.
#[cfg(test)]
#[test]
fn same_hex() {
    use crate::hex::Axial;
    use crate::rng::PcgRng;
    use crate::rules::Rules;

    let rules = Rules::from_json_str(include_str!("../../../config/rules.default.json")).unwrap();
    let tables = Tables::load_embedded().unwrap();

    // Loadout (rules-as-data): a tank carries its 大号直瞄炮, infantry the 步兵轻武器.
    assert_eq!(
        unit_loadout(&rules, UnitType::Tank),
        vec!["大号直瞄炮".to_string()]
    );
    assert_eq!(
        unit_loadout(&rules, UnitType::Infantry),
        vec!["步兵轻武器".to_string()]
    );

    let map = Map {
        format: "openstratcore.map".into(),
        version: 1,
        name: "sh".into(),
        elevation_unit_meters: Some(10),
        hexes: vec![crate::types::HexCell {
            q: 0,
            r: 0,
            id: None,
            elevation: 0,
            terrain: Terrain::Open,
            road: None,
        }],
    };
    let u = |id: &str, side, ut, teams| RuntimeUnit {
        id: id.into(),
        side,
        unit_type: ut,
        armor: Armor::Medium,
        teams,
        pos: Axial::new(0, 0),
        facing: 0,
        state: UnitState::Stopped,
        weapon_state: WeaponState::Deployed,
        busy_until: 0,
        suppressed_until: 0,
        alive: true,
        carried_by: None,
        affiliated_to: None,
        heli_alt: crate::types::HeliAlt::Low,
        inside_facility: None,
        fatigue: 0,
    };
    // The hex holds a Red attacker and three Blue defenders: a 2-team tank, a 1-team tank, infantry.
    let big_tank = u("BT", SideId::Blue, UnitType::Tank, 2);
    let small_tank = u("ST", SideId::Blue, UnitType::Tank, 1);
    let inf = u("BI", SideId::Blue, UnitType::Infantry, 1);
    let red = u("R", SideId::Red, UnitType::Tank, 3);
    let in_hex = [&big_tank, &small_tank, &inf, &red];

    let mut rng = PcgRng::from_seed(1);
    // 三.15a: a 坦克 outranks the infantry, and among the two tanks the fewer-班组 one is chosen.
    let target = pick_same_hex_target(&mut rng, SideId::Red, &in_hex).unwrap();
    assert_eq!(target.id, "ST", "三.15a: 坦克 priority, then fewest 班组");

    // best weapon vs a vehicle at d0 is the tank's 大号直瞄炮.
    let red_weapons = unit_loadout(&rules, UnitType::Tank);
    let wrefs: Vec<&str> = red_weapons.iter().map(String::as_str).collect();
    assert_eq!(best_weapon(&tables, &wrefs, true, 3, 0), Some("大号直瞄炮"));

    // 三.15g: 步兵轻武器 vs a 车辆 resolves through the by-squad ref table (this also fixes infantry-
    // vs-vehicle direct fire generally) — it must NOT silently become None.
    let inf_w = unit_loadout(&rules, UnitType::Infantry);
    let iw: Vec<&str> = inf_w.iter().map(String::as_str).collect();
    assert_eq!(
        best_weapon(&tables, &iw, true, 3, 0),
        Some("步兵轻武器"),
        "三.15g: infantry can engage a vehicle with its 步兵轻武器"
    );

    // Distance-0 engagement destroys vehicles often (level-10 gun point blank).
    let mut destroyed = false;
    let mut r2 = PcgRng::from_seed(2);
    for _ in 0..200 {
        if let Outcome::Destroyed(n) =
            same_hex_engage(&tables, &map, &mut r2, &wrefs, &red, &small_tank)
        {
            assert!((1..=5).contains(&n));
            destroyed = true;
        }
    }
    assert!(
        destroyed,
        "三.15a: point-blank 同格交战 destroys the target tank"
    );

    // With only enemy infantry present, the personnel pipeline is used (no panic, can resolve).
    let only_inf = [&inf, &red];
    let t2 = pick_same_hex_target(&mut rng, SideId::Red, &only_inf).unwrap();
    assert_eq!(t2.id, "BI");
    let _ = same_hex_engage(&tables, &map, &mut r2, &wrefs, &red, &inf);
}

/// 附1.4 高度差修正 sign by direction (高看低取正 / 低打高取负), locking the previously
/// unreachable 取正 branch found in adversarial review.
#[cfg(test)]
#[test]
fn height_diff_directions() {
    let tables = Tables::load_embedded().unwrap();
    // height-diff level 1 at distance 1: the table stores magnitude 2 (cell -2).
    assert_eq!(
        height_diff_correction(&tables, 0, 1),
        0,
        "level fire: no correction"
    );
    assert_eq!(
        height_diff_correction(&tables, -1, 1),
        -2,
        "低打高: penalty"
    );
    assert_eq!(height_diff_correction(&tables, 1, 1), 2, "高看低: bonus");
    // Magnitude is symmetric in direction.
    assert_eq!(
        height_diff_correction(&tables, 5, 7),
        -height_diff_correction(&tables, -5, 7)
    );
}

/// 三.8 direct-fire flow (T1.8): the firing conditions (weapon ready, stopped unless 坦克主炮,
/// observed, in range) gate the shot before it resolves.
#[cfg(test)]
#[test]
fn direct_fire_flow() {
    use crate::hex::Axial;
    use crate::rng::PcgRng;
    use crate::types::{HexCell, Map, SideId};

    let tables = Tables::load_embedded().unwrap();
    let rules =
        crate::rules::Rules::from_json_str(include_str!("../../../config/rules.default.json"))
            .unwrap();
    let row = |hill_at: Option<i32>| Map {
        format: "openstratcore.map".into(),
        version: 1,
        name: "fire".into(),
        elevation_unit_meters: Some(10),
        hexes: (0..=6)
            .map(|q| HexCell {
                q,
                r: 0,
                id: None,
                elevation: if Some(q) == hill_at { 6 } else { 0 },
                terrain: Terrain::Open,
                road: None,
            })
            .collect(),
    };
    let unit = |id: &str, side, q, state, ws| RuntimeUnit {
        id: id.to_string(),
        side,
        unit_type: UnitType::Tank,
        armor: Armor::Medium,
        teams: 3,
        pos: Axial::new(q, 0),
        facing: 0,
        state,
        weapon_state: ws,
        busy_until: 0,
        suppressed_until: 0,
        alive: true,
        carried_by: None,
        affiliated_to: None,
        heli_alt: crate::types::HeliAlt::Low,
        inside_facility: None,
        fatigue: 0,
    };
    let shooter = |state, ws| unit("S", SideId::Red, 0, state, ws);
    let target = unit(
        "T",
        SideId::Blue,
        3,
        UnitState::Stopped,
        WeaponState::Deployed,
    );
    let map = row(None);
    let mut rng = PcgRng::from_seed(1);
    let ready = || shooter(UnitState::Stopped, WeaponState::Deployed);

    // Stopped tank, deployed 大号直瞄炮, target observed at dist 3 within range 18 -> resolves.
    assert!(matches!(
        direct_fire(
            &tables,
            &map,
            &mut rng,
            &rules,
            &ready(),
            "大号直瞄炮",
            &target
        ),
        FireOutcome::Resolved(_)
    ));
    // Cooling weapon -> rejected.
    assert_eq!(
        direct_fire(
            &tables,
            &map,
            &mut rng,
            &rules,
            &shooter(UnitState::Stopped, WeaponState::Cooling),
            "大号直瞄炮",
            &target
        ),
        FireOutcome::Rejected("weapon not ready (locked or cooling)")
    );
    // Moving + a non-tank-main gun -> rejected; moving + 坦克主炮 (行进间射击) -> allowed.
    assert_eq!(
        direct_fire(
            &tables,
            &map,
            &mut rng,
            &rules,
            &shooter(UnitState::Moving, WeaponState::Deployed),
            "速射炮",
            &target
        ),
        FireOutcome::Rejected("must be stopped to fire (8.5c)")
    );
    assert!(matches!(
        direct_fire(
            &tables,
            &map,
            &mut rng,
            &rules,
            &shooter(UnitState::Moving, WeaponState::Deployed),
            "大号直瞄炮",
            &target
        ),
        FireOutcome::Resolved(_)
    ));
    // Out of weapon range: 火箭筒 (range 4) at a target 6 hexes away (still observed at 25).
    let far = unit(
        "F",
        SideId::Blue,
        6,
        UnitState::Stopped,
        WeaponState::Deployed,
    );
    assert_eq!(
        direct_fire(&tables, &map, &mut rng, &rules, &ready(), "火箭筒", &far),
        FireOutcome::Rejected("out of range")
    );
    // Target with no line of sight (a hill at q=2 blocks the view to q=3) -> not observed.
    let blocked = row(Some(2));
    assert_eq!(
        direct_fire(
            &tables,
            &blocked,
            &mut rng,
            &rules,
            &ready(),
            "大号直瞄炮",
            &target
        ),
        FireOutcome::Rejected("target not observed")
    );
}

/// 流水线 D 防空 (三.18 / 附3, T3.4): attack-level lookup (incl. 最小射程 / 平台尾值 / 超射程),
/// the 附3.2 歼灭阈值 diamond, and the 三.18f 多次裁决 = 车/班数 salvo.
#[cfg(test)]
#[test]
fn air_defense_pipeline() {
    use crate::rng::PcgRng;
    let tables = Tables::load_embedded().unwrap();

    // --- 附3.1 攻击等级 (double-entry cross-verified) ---
    // 防空高炮 (range 20, no tail): d0=9, ramps to a 2-plateau; past the printed d0..d19 there is
    // no listed cell, so the exact range edge and beyond are "no shot".
    assert_eq!(aa_attack_level(&tables, "防空高炮", 0), Some(9));
    assert_eq!(aa_attack_level(&tables, "防空高炮", 7), Some(4));
    assert_eq!(aa_attack_level(&tables, "防空高炮", 19), Some(2));
    assert_eq!(aa_attack_level(&tables, "防空高炮", 20), None);
    assert_eq!(aa_attack_level(&tables, "防空高炮", 21), None);
    // 便携防空导弹 (range 20, 最小射程 2): d0/d1 = 0 → no shot; d2=5, d5=7, d7=6, d8=4.
    assert_eq!(aa_attack_level(&tables, "便携防空导弹", 0), None);
    assert_eq!(aa_attack_level(&tables, "便携防空导弹", 1), None);
    assert_eq!(aa_attack_level(&tables, "便携防空导弹", 2), Some(5));
    assert_eq!(aa_attack_level(&tables, "便携防空导弹", 5), Some(7));
    assert_eq!(aa_attack_level(&tables, "便携防空导弹", 7), Some(6));
    assert_eq!(aa_attack_level(&tables, "便携防空导弹", 8), Some(4));
    // 车载防空导弹 (range 50, 最小射程 2): eight 7s (d3..d10), then the 6-plateau (incl. the off-page
    // tail out to d50); d51 is beyond range.
    assert_eq!(aa_attack_level(&tables, "车载防空导弹", 2), Some(6));
    assert_eq!(aa_attack_level(&tables, "车载防空导弹", 10), Some(7));
    assert_eq!(aa_attack_level(&tables, "车载防空导弹", 11), Some(6));
    assert_eq!(aa_attack_level(&tables, "车载防空导弹", 25), Some(6)); // tail
    assert_eq!(aa_attack_level(&tables, "车载防空导弹", 50), Some(6)); // tail, range edge
    assert_eq!(aa_attack_level(&tables, "车载防空导弹", 51), None); // out of range

    // --- 附3.2 歼灭阈值 (diamond, symmetric about the hardest roll 7) ---
    assert_eq!(aa_kill_threshold(&tables, 7), Some(11)); // hardest to hit
    assert_eq!(aa_kill_threshold(&tables, 2), Some(1)); // easiest
    assert_eq!(aa_kill_threshold(&tables, 12), Some(2));
    // A level-2 weapon 歼灭s on the extreme rolls but never on the central 7.
    assert!(2 >= aa_kill_threshold(&tables, 2).unwrap()); // roll 2 (thr 1) → kill
    assert!(2 >= aa_kill_threshold(&tables, 12).unwrap()); // roll 12 (thr 2) → kill
    assert!(2 < aa_kill_threshold(&tables, 7).unwrap()); // roll 7 (thr 11) → no kill

    // --- 三.18f 多次裁决 = 车/班数 ---
    // No valid salvo draws no dice and returns None (out of range / 最小射程 / zero shooters).
    let mut rng = PcgRng::from_seed(1);
    assert_eq!(air_defense(&tables, &mut rng, "防空高炮", 4, 25), None);
    assert_eq!(air_defense(&tables, &mut rng, "便携防空导弹", 4, 1), None);
    assert_eq!(air_defense(&tables, &mut rng, "防空高炮", 0, 0), None); // 三.18f: 0 车/班

    // A salvo runs exactly `shooters` adjudications; kills never exceed them.
    let mut rng = PcgRng::from_seed(7);
    let salvo = air_defense(&tables, &mut rng, "防空高炮", 4, 0).unwrap();
    assert_eq!(salvo.adjudications, 4);
    assert!(salvo.kills <= 4);
    assert_eq!(salvo.destroyed, salvo.kills > 0);

    // Determinism: same seed + same order ⇒ identical outcome.
    let run = |seed| {
        let mut r = PcgRng::from_seed(seed);
        air_defense(&tables, &mut r, "车载防空导弹", 3, 5).unwrap()
    };
    assert_eq!(run(42), run(42));

    // 三.18f scaling: a 12-车 高炮 salvo at point-blank (level 9 — only the central roll 7 fails,
    // threshold 11) destroys the target for any normal seed (12 consecutive 7s is ~0).
    let mut rng = PcgRng::from_seed(5);
    let big = air_defense(&tables, &mut rng, "防空高炮", 12, 0).unwrap();
    assert!(
        big.destroyed,
        "a 12-shooter level-9 salvo should 歼灭 the air target"
    );
    assert!(big.kills >= 1);
}
