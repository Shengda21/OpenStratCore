//! The simulator: deterministic discrete-event loop with a decision tick.
//!
//! Structure is real; order *scheduling* for the full rule set is grown via `/add-rule`.
//! Unhandled commands return `EngineError::Unimplemented` (never panic — CLAUDE.md rule 4).

use crate::combat::{self, FireOutcome};
use crate::hex::Axial;
use crate::prob::Outcome;
use crate::rng::{PcgRng, Rng};
use crate::rules::{Rules, Tables};
use crate::time::{secs_to_ticks, ticks_to_secs, Tick};
use crate::types::{
    Armor, HeliAlt, Map, RuntimeUnit, Scenario, SideId, State, UnitState, UnitType, WeaponState,
};
use crate::{EngineError, Result};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};

/// Internal timed events processed by the loop.
#[derive(Debug, Clone)]
pub enum Event {
    /// A posture transition (三.1 / 三.3) lands the unit in state `to`. `cover_gen` is `Some` only
    /// for a 掩蔽 transition; if it no longer matches `cover_transition[unit]` the transition was
    /// interrupted (三.3b/f) and this event is a no-op.
    TransitionComplete {
        unit: String,
        to: UnitState,
        cover_gen: Option<u64>,
    },
    WeaponReady {
        unit: String,
    },
    SuppressEnd {
        unit: String,
    },
    /// A moving unit reaches hex `to` and then plans the next hop toward `target` (三.1 pathing).
    /// `gen` is the unit's move generation at scheduling time; a later order bumps the generation,
    /// so a stale step is dropped instead of teleporting a unit that has since been re-tasked.
    MoveArrive {
        unit: String,
        gen: u64,
        to: Axial,
        target: Axial,
    },
    /// A 上下车 (三.2) finishes: the passenger boards or leaves its vehicle. `gen` guards against a
    /// transition the 压制 of either party has already interrupted (三.2d/e).
    MountComplete {
        passenger: String,
        gen: u64,
    },
    /// A 间瞄 plan's 150 s 飞行 ends → adjudicate the salvo (三.9.3) and enter the 300 s 爆炸 phase.
    IndirectFly {
        plan: u64,
    },
    /// A 间瞄 plan's 300 s 爆炸 ends → the impact hex stops adjudicating (三.9.1b).
    IndirectBoomEnd {
        plan: u64,
    },
    /// A 巡飞弹's 75 s 发射 finishes → it deploys off its carrier (三.10d).
    LoiteringLaunched {
        munition: String,
    },
    /// A deployed 巡飞弹 reaches its 1200 s 巡飞时长 → self-destruct (三.10g).
    LoiteringSelfDestruct {
        munition: String,
    },
    /// A 炮兵校射雷达's 75 s 开机 finishes (三.19d) → it goes ON. `gen` drops an interrupted spin-up.
    RadarReady {
        vehicle: String,
        gen: u64,
    },
    /// A 运输直升机's 75 s 切换高度 finishes (三.16d) → it settles at altitude `to`.
    HeliAltReady {
        heli: String,
        to: HeliAlt,
    },
    /// A 同格交战 (三.15) round on `hex`: each eligible ground unit there fires once at its priority
    /// enemy; the round reschedules every 25 s until one side no longer occupies the hex (三.15f).
    SameHexEngage {
        hex: Axial,
        gen: u64,
    },
    /// A unit's 75 s 进入工事 (三.20b) finishes → it is garrisoned inside `facility` (全程隐蔽). `gen`
    /// drops a transition invalidated meanwhile (e.g. a forced over-capacity expulsion / 三.20.3d).
    FacilityEnterReady {
        unit: String,
        facility: String,
        gen: u64,
    },
    /// A unit's 75 s 离开工事 (三.20b) finishes → it leaves the 工事 onto the open board. `gen` drops a
    /// transition invalidated meanwhile (forced expulsion / 三.20.3d / a fresh facility order).
    FacilityExitReady {
        unit: String,
        gen: u64,
    },
    /// A 火箭布雷车's 75 s 布雷 (三.21c) finishes → a 雷场 appears at `at`. `gen` drops a 布雷 superseded
    /// by a fresh order.
    MineLayReady {
        unit: String,
        at: Axial,
        gen: u64,
    },
    /// 三.1.4 活体疲劳恢复: while a unit is idle, its 疲劳 decays −1 every
    /// `infantry_fatigue_decay_seconds`. `gen` is the unit's 疲劳 generation when scheduled — any
    /// fresh 冲锋 hex or new movement order bumps it, so a decay tick queued before the unit started
    /// moving again no-ops (the unit was not actually resting for the full interval).
    FatigueDecay {
        unit: String,
        gen: u64,
    },
    /// A 三.17 聚合/解聚 75 s transition finishes for `unit` → merge in (聚合) or fission out (解聚).
    /// `gen` drops a transition aborted meanwhile (压制 三.17d / 同格 / a fresh order).
    AggSplitComplete {
        unit: String,
        gen: u64,
    },
    /// Re-evaluate control-point ownership.
    CaptureCheck,
}

/// A 同格交战 (三.15) in progress on a hex. `gen` invalidates stale rounds (so a re-entry after an
/// engagement ended starts fresh); `dry_rounds` counts rounds that destroyed no 班, capping a
/// no-progress stalemate so the sim always terminates (hard rule #1).
#[derive(Debug, Clone, Copy)]
struct SameHexState {
    gen: u64,
    dry_rounds: u32,
}

/// Termination guard: a 同格交战 that destroys no 班 for this many consecutive rounds is a stalemate
/// (its matchup cannot resolve) and ends. Generous — lethal point-blank combat resets it on the
/// first kill, so a can-kill engagement never reaches it.
const SAME_HEX_MAX_DRY_ROUNDS: u32 = 16;

/// A 间瞄 (三.9) plan in flight or detonating. `impact` is `Some` once the 飞行 has resolved into a
/// 爆炸 over that hex; `base`/`gun_count` are then the salvo result reused for units entering the
/// hex during the 300 s 爆炸 (三.9.1b).
#[derive(Debug, Clone)]
struct IndirectPlan {
    artillery: String,
    plan_hex: Axial,
    impact: Option<Axial>,
    base: crate::combat::IndirectBase,
    gun_count: u8,
}

/// Which direction a 上下车 (三.2) transition is going.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MountKind {
    Mount,
    Dismount,
}

/// A 上下车 (三.2) in progress, keyed by passenger id.
#[derive(Debug, Clone)]
struct MountInTransit {
    gen: u64,
    vehicle: String,
    kind: MountKind,
}

/// A 三.17 聚合/解聚 in progress, keyed by the initiating (own) unit id. `聚合` consumes `other` into
/// the initiator; `解聚` fissions the initiator into itself + one spawned child.
#[derive(Debug, Clone)]
enum AggSplitKind {
    /// 聚合: merge the named other unit's 班 into the initiator, then despawn the other.
    Aggregate { other: String },
    /// 解聚: split the initiator's 班 into a smaller half (kept) + a spawned child.
    Split,
}

/// A 聚合/解聚 (三.17) transition in flight, keyed by initiating unit id (mirrors `MountInTransit`).
#[derive(Debug, Clone)]
struct AggSplitInTransit {
    gen: u64,
    kind: AggSplitKind,
}

#[derive(Debug, Clone)]
struct Timed {
    t: Tick,
    seq: u64,
    event: Event,
}

// Min-heap by (time, seq) for a stable, deterministic order. Integer `Tick` keys
// give a total order (no f64 partial_cmp), so "先到先裁" is replayable byte-for-byte.
impl PartialEq for Timed {
    fn eq(&self, o: &Self) -> bool {
        self.t == o.t && self.seq == o.seq
    }
}
impl Eq for Timed {}
impl Ord for Timed {
    fn cmp(&self, o: &Self) -> Ordering {
        // Reverse so the BinaryHeap (max-heap) pops the earliest (time, seq) first.
        o.t.cmp(&self.t).then(o.seq.cmp(&self.seq))
    }
}
impl PartialOrd for Timed {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

pub struct Engine {
    pub map: Map,
    pub scenario: Scenario,
    pub rules: Rules,
    /// The 18 authoritative decision tables (combat etc.), loaded once.
    pub tables: Tables,
    pub state: State,
    rng: PcgRng,
    queue: BinaryHeap<Timed>,
    seq: u64,
    /// Per-unit move generation; bumped on each new movement order so in-flight
    /// [`Event::MoveArrive`] steps from a superseded order are dropped (lazy invalidation).
    move_gen: BTreeMap<String, u64>,
    /// Units mid-掩蔽转换 → the generation of that transition (三.3). Present only while a unit is
    /// converting to Cover; a 坦克 firing (三.3b) or being 压制 (三.3f) interrupts it by removing
    /// the entry, so the still-queued `TransitionComplete` (tagged with the old gen) no-ops.
    cover_transition: BTreeMap<String, u64>,
    cover_gen_ctr: u64,
    /// Passengers mid-上下车 (三.2) → the in-flight transition. A 压制 of the passenger or its
    /// vehicle drops the entry (三.2d/e) so the queued `MountComplete` no-ops.
    mount_op: BTreeMap<String, MountInTransit>,
    mount_gen_ctr: u64,
    /// 三.20 工事 进入/离开 transition generation per unit — bumped on each facility order AND on a
    /// forced over-capacity expulsion / 三.20.3d exposure, so a stale Facility{Enter,Exit}Ready no-ops.
    facility_gen: BTreeMap<String, u64>,
    /// 三.21 布雷 generation per 布雷车 (a fresh lay order invalidates a queued MineLayReady) and the
    /// per-vehicle 布雷 count (三.21c max_lays_per_vehicle).
    minelayer_gen: BTreeMap<String, u64>,
    minelayer_lays: BTreeMap<String, u32>,
    /// 火箭布雷车 currently mid-布雷 (one at a time per unit, reject_if_busy); a 压制 (三.21d) aborts it.
    minelay_in_flight: BTreeSet<String>,
    /// 三.1.4 活体疲劳 generation per unit — bumped whenever the unit accrues a 冲锋 hex or takes a
    /// fresh movement order, so a queued `FatigueDecay` from an earlier rest no-ops (the unit did not
    /// actually sit idle for the full `infantry_fatigue_decay_seconds`). At most one decay tick is in
    /// flight per unit at any time.
    fatigue_gen: BTreeMap<String, u64>,
    /// 三.17 聚合/解聚 transitions in flight, keyed by the initiating unit id (mirrors `mount_op`).
    aggsplit_op: BTreeMap<String, AggSplitInTransit>,
    /// Per-unit 聚合/解聚 generation — bumped on a fresh order and on a 压制/同格 abort (三.17d), so a
    /// queued `AggSplitComplete` from a superseded/aborted transition no-ops.
    aggsplit_gen: BTreeMap<String, u64>,
    /// Monotonic counter for 解聚-spawned child ids (`{parent}#{n}`). Deterministic across replay (it
    /// advances only in the deterministic event order), so re-running the same commands yields the same
    /// child ids. Scenario ids may not contain `#` (validated at load), so spawned ids never collide.
    spawn_ctr: u64,
    /// Active 间瞄 (三.9) plans by id (flying or detonating). A 炮兵 may hold up to two (one 爆炸 +
    /// one 飞行); the 300 s cooldown enforces the spacing.
    indirect_plans: BTreeMap<u64, IndirectPlan>,
    /// Per-炮兵 间瞄 cooldown (三.9.1c): the earliest tick it may plan again.
    indirect_cd: BTreeMap<String, Tick>,
    indirect_plan_ctr: u64,
    /// Deployed 巡飞弹 (三.10) → its 发射车. Loaded (not-yet-launched) munitions instead ride via
    /// `carried_by`; either way the munition dies with its carrier (三.10g/h).
    loitering_parent: BTreeMap<String, String>,
    /// A 巡飞弹's 巡飞 area (三.10 `targetArea`), recorded at launch for its recon flight/strike.
    loitering_target: BTreeMap<String, Axial>,
    /// 炮兵校射雷达 vehicles currently 开机 (三.19c). Their校射 effect is live only while the
    /// vehicle is also un-suppressed (三.19f, auto-recovers).
    radar_on: BTreeSet<String>,
    /// Radar vehicles mid-开机 → the spin-up generation; 压制 (三.19d) drops the entry.
    radar_spinup: BTreeMap<String, u64>,
    radar_gen_ctr: u64,
    /// Active 同格交战 (三.15) hexes → engagement state (generation + stalemate counter).
    same_hex: BTreeMap<Axial, SameHexState>,
    same_hex_ctr: u64,
    /// Per-unit 先入 order for 同格交战 firing priority (三.15a): lower = entered its current hex
    /// earlier; units present from setup default to 0 (oldest).
    same_hex_order: BTreeMap<String, u64>,
    entry_seq: u64,
    /// 三.18g: cumulative AA shots fired by each 防空算子 (capped at air.aa[type].max_shots).
    aa_shots: BTreeMap<String, u32>,
    /// 三.4a stacking cap, resolved once from `rules.control` (rules-as-data, no hardcode).
    stacking_cap: i64,
}

impl Engine {
    /// Build a fresh match from parsed map + scenario + rules + seed.
    pub fn new(map: Map, scenario: Scenario, rules: Rules, seed: u64) -> Result<Self> {
        let mut units = std::collections::BTreeMap::new();
        for (side, list) in [
            (SideId::Red, &scenario.sides.red.units),
            (SideId::Blue, &scenario.sides.blue.units),
        ] {
            for u in list {
                if map.cell(&u.at).is_none() {
                    return Err(EngineError::Scenario(format!(
                        "unit {} placed off-map at {:?}",
                        u.id, u.at
                    )));
                }
                units.insert(
                    u.id.clone(),
                    RuntimeUnit {
                        id: u.id.clone(),
                        side,
                        unit_type: u.unit_type,
                        armor: u.armor.unwrap_or(Armor::None),
                        teams: u.teams,
                        pos: u.at,
                        facing: u.facing,
                        state: u.state.unwrap_or(UnitState::Normal),
                        weapon_state: WeaponState::Deployed,
                        busy_until: 0,
                        suppressed_until: 0,
                        alive: true,
                        carried_by: u.carried_by.clone(),
                        affiliated_to: u.affiliated_to.clone(),
                        heli_alt: HeliAlt::Low,
                        inside_facility: None,
                        fatigue: 0,
                    },
                );
            }
        }
        let mut control = std::collections::BTreeMap::new();
        for cp in &scenario.objectives {
            control.insert(cp.id.clone(), None);
        }
        // 三.20: load 工事 (fortifications) from the scenario into runtime state, keyed by a stable
        // engine id "FAC{idx}". Non-工事 facilities (minefield/roadblock/indirect_zone) belong to
        // other subsystems and are skipped here. Capacity defaults to fortification.capacity_squads.
        let fort_capacity =
            u8::try_from(rules.fortification_capacity().unwrap_or(0).max(0)).unwrap_or(0);
        let mut facilities = std::collections::BTreeMap::new();
        for (idx, f) in scenario.facilities.iter().enumerate() {
            if crate::mechanics::FortKind::from_facility_kind(&f.kind).is_none() {
                continue;
            }
            if map.cell(&f.at).is_none() {
                return Err(EngineError::Scenario(format!(
                    "工事 #{idx} ({}) is placed off-map at {:?}",
                    f.kind, f.at
                )));
            }
            let owner = f.owner.as_deref().and_then(|o| match o {
                "red" => Some(SideId::Red),
                "blue" => Some(SideId::Blue),
                _ => None,
            });
            let id = format!("FAC{idx}");
            facilities.insert(
                id.clone(),
                crate::types::RuntimeFacility {
                    id,
                    kind: f.kind.clone(),
                    at: f.at,
                    owner,
                    capacity: f.capacity.unwrap_or(fort_capacity),
                    // 三.20.1d/20.2d 工事防护 (heavy/medium); absent → no armour.
                    armor: f
                        .armor
                        .as_deref()
                        .and_then(Armor::parse)
                        .unwrap_or(Armor::None),
                },
            );
        }
        // 三.21: load preset 雷场 (facility kind "minefield") into runtime state, keyed by hex.
        let mut minefields = std::collections::BTreeMap::new();
        for f in scenario.facilities.iter() {
            if f.kind != "minefield" {
                continue;
            }
            if map.cell(&f.at).is_none() {
                return Err(EngineError::Scenario(format!(
                    "雷场 is placed off-map at {:?}",
                    f.at
                )));
            }
            let owner = f.owner.as_deref().and_then(|o| match o {
                "red" => Some(SideId::Red),
                "blue" => Some(SideId::Blue),
                _ => None,
            });
            minefields.insert(
                f.at,
                crate::types::RuntimeMinefield {
                    at: f.at,
                    owner,
                    cleared_by: std::collections::BTreeSet::new(),
                },
            );
        }
        let stacking_cap = rules.stacking_cap().ok_or_else(|| {
            EngineError::Rules("missing control.stacking_max_ground_units_per_hex".into())
        })?;
        // 三.10–三.12 flight altitude is part of the rules contract — fail loudly if a rules file
        // omits it, rather than silently flying air units at ground level (no see-over-terrain).
        if rules
            .air
            .get("recon_altitude_meters")
            .and_then(|v| v.as_i64())
            .is_none()
        {
            return Err(EngineError::Rules(
                "missing air.recon_altitude_meters".into(),
            ));
        }
        // 三.16b: a 运输直升机's LOS now depends on its altitude band, so each of 高空/低空/超低空
        // must be present — otherwise `air_altitude_levels` would silently fly the heli at ground
        // level (a fog hazard rather than a loud failure).
        for band in ["high", "low", "very_low"] {
            if rules
                .air
                .get("heli_altitude_meters")
                .and_then(|v| v.get(band))
                .and_then(|v| v.as_i64())
                .is_none()
            {
                return Err(EngineError::Rules(format!(
                    "missing air.heli_altitude_meters.{band}"
                )));
            }
        }
        // 三.17 解聚 spawns child ids as `{parent}#{n}`; reserve `#` so a spawned id can never collide
        // with a scenario-authored one. (Checked once at load — the engine never emits a `#`-free spawn.)
        for u in units.values() {
            if u.id.contains('#') {
                return Err(EngineError::Scenario(format!(
                    "unit id {:?} may not contain '#' (reserved for 三.17 解聚 child ids)",
                    u.id
                )));
            }
        }
        // 三.1.3: 行军 is a 车辆 road mode, so a scenario may not pre-place a march-INELIGIBLE unit
        // (人员/空中/炮兵) in 行军 — that would let the march mechanics (road speed/route/三.1.3e) trust
        // a type that can never legally march. A 车辆 starting in 行军 is harmless (off-road or on a
        // non-marchable road it simply cannot move), so it is allowed; the entry op gates the rest.
        for u in units.values() {
            if u.state == UnitState::March
                && !(crate::mechanics::is_vehicle(u.unit_type)
                    && u.unit_type != UnitType::Artillery)
            {
                return Err(EngineError::Scenario(format!(
                    "{} ({:?}) cannot start in 行军 — only a 车辆 marches (三.1.3)",
                    u.id, u.unit_type
                )));
            }
            // 三.1 半速 (三.1.4 一二级冲锋) are per-MOVE speeds, never standing postures — a unit may not
            // be placed in Half/Charge1/Charge2 at load (a tank standing in 冲锋 would bypass the
            // infantry-only / per-move contract). They are reachable only via `move_to mode:…`.
            if matches!(
                u.state,
                UnitState::Half | UnitState::Charge1 | UnitState::Charge2
            ) {
                return Err(EngineError::Scenario(format!(
                    "{} cannot start in {:?} — 半速/一二级冲锋 are per-move speeds (move_to mode:…), not postures",
                    u.id, u.state
                )));
            }
        }
        // 三.14b: only a 步兵 or 无人战车 has a 隶属车辆, and when set it must name exactly one same-side,
        // manned (ground, non-UGV, non-personnel) vehicle — never itself, an enemy, or a missing/air
        // id. (Validated at load so the runtime guide (三.14b) and 三.13d death logic can trust it.)
        for u in units.values() {
            let Some(parent_id) = &u.affiliated_to else {
                // 三.13b: a 无人战车 MUST be 隶属 to a manned vehicle — a UGV without one is invalid.
                if u.unit_type == UnitType::Ugv {
                    return Err(EngineError::Scenario(format!(
                        "无人战车 {} must declare a 隶属 vehicle (affiliatedTo) — 三.13b",
                        u.id
                    )));
                }
                continue;
            };
            if !matches!(u.unit_type, UnitType::Ugv | UnitType::Infantry) {
                return Err(EngineError::Scenario(format!(
                    "only 步兵/无人战车 may have a 隶属 vehicle, but {} ({:?}) does",
                    u.id, u.unit_type
                )));
            }
            if parent_id == &u.id {
                return Err(EngineError::Scenario(format!("{} is 隶属 to itself", u.id)));
            }
            let parent = units.get(parent_id).ok_or_else(|| {
                EngineError::Scenario(format!("{} is 隶属 to unknown unit {parent_id}", u.id))
            })?;
            let manned_vehicle = crate::mechanics::is_ground(parent.unit_type)
                && !matches!(
                    parent.unit_type,
                    UnitType::Ugv | UnitType::Infantry | UnitType::AaMissileSquad
                );
            if parent.side != u.side || !manned_vehicle {
                return Err(EngineError::Scenario(format!(
                    "{} must be 隶属 to a same-side manned vehicle, not {parent_id} ({:?})",
                    u.id, parent.unit_type
                )));
            }
        }
        // 三.2 搭载关系 (carried_by) load sanity — a 搭载 unit must name a known, same-side, non-self
        // carrier. Kept deliberately light because `carried_by` is DUAL-USE: besides 三.2 班 passengers
        // it also marks a 三.10 巡飞弹 loaded on its 发射车 (a munition is not `can_be_passenger`), so we
        // do NOT constrain passenger/carrier TYPES here (the mount op 三.2 / launch 三.10 enforce those).
        for u in units.values() {
            let Some(carrier_id) = &u.carried_by else {
                continue;
            };
            if carrier_id == &u.id {
                return Err(EngineError::Scenario(format!("{} is 搭载 on itself", u.id)));
            }
            let carrier = units.get(carrier_id).ok_or_else(|| {
                EngineError::Scenario(format!("{} is 搭载 on unknown unit {carrier_id}", u.id))
            })?;
            if carrier.side != u.side {
                return Err(EngineError::Scenario(format!(
                    "{} is 搭载 on {carrier_id}, which is not on the same side (三.2)",
                    u.id
                )));
            }
        }
        let mut engine = Self {
            map,
            scenario,
            rules,
            tables: Tables::load_embedded()?,
            state: State {
                clock: 0,
                units,
                control,
                facilities,
                minefields,
            },
            rng: PcgRng::from_seed(seed),
            queue: BinaryHeap::new(),
            seq: 0,
            move_gen: BTreeMap::new(),
            cover_transition: BTreeMap::new(),
            cover_gen_ctr: 0,
            mount_op: BTreeMap::new(),
            mount_gen_ctr: 0,
            facility_gen: BTreeMap::new(),
            minelayer_gen: BTreeMap::new(),
            minelayer_lays: BTreeMap::new(),
            minelay_in_flight: BTreeSet::new(),
            fatigue_gen: BTreeMap::new(),
            aggsplit_op: BTreeMap::new(),
            aggsplit_gen: BTreeMap::new(),
            spawn_ctr: 0,
            indirect_plans: BTreeMap::new(),
            indirect_cd: BTreeMap::new(),
            indirect_plan_ctr: 0,
            loitering_parent: BTreeMap::new(),
            loitering_target: BTreeMap::new(),
            radar_on: BTreeSet::new(),
            radar_spinup: BTreeMap::new(),
            radar_gen_ctr: 0,
            same_hex: BTreeMap::new(),
            same_hex_ctr: 0,
            same_hex_order: BTreeMap::new(),
            entry_seq: 0,
            aa_shots: BTreeMap::new(),
            stacking_cap,
        };
        // 三.15: units placed co-located with the enemy at setup engage immediately.
        let start_hexes: BTreeSet<Axial> = engine
            .state
            .units
            .values()
            .map(|u| u.pos)
            .collect::<BTreeSet<_>>();
        for hex in start_hexes {
            engine.maybe_trigger_same_hex(hex, 0);
        }
        Ok(engine)
    }

    fn schedule(&mut self, t: Tick, event: Event) {
        self.seq += 1;
        self.queue.push(Timed {
            t,
            seq: self.seq,
            event,
        });
    }

    /// Start a new movement order for `id`, returning the fresh generation. Any in-flight
    /// [`Event::MoveArrive`] tagged with an older generation becomes a no-op (lazy invalidation).
    fn bump_move_gen(&mut self, id: &str) -> u64 {
        let g = self.move_gen.entry(id.to_string()).or_insert(0);
        *g = g.wrapping_add(1); // never panics; 2^64 orders for one unit is unreachable
        *g
    }

    /// Start a new 工事 进入/离开 transition for `id` (or invalidate any in-flight one), returning the
    /// fresh generation. A queued Facility{Enter,Exit}Ready tagged with an older gen no-ops — so a
    /// forced expulsion / 三.20.3d exposure (which also bumps) can't be undone by a stale event.
    fn bump_facility_gen(&mut self, id: &str) -> u64 {
        let g = self.facility_gen.entry(id.to_string()).or_insert(0);
        *g = g.wrapping_add(1);
        *g
    }

    /// Start a new 布雷 (三.21c) for a 布雷车 (or invalidate an in-flight one), returning the fresh
    /// generation; a stale `MineLayReady` then no-ops.
    fn bump_minelayer_gen(&mut self, id: &str) -> u64 {
        let g = self.minelayer_gen.entry(id.to_string()).or_insert(0);
        *g = g.wrapping_add(1);
        *g
    }

    /// Bump a unit's 三.1.4 疲劳 generation, invalidating any in-flight `FatigueDecay`. Returns the
    /// fresh generation. Called when a unit starts moving (rest interrupted) and when arming a fresh
    /// decay tick.
    fn bump_fatigue_gen(&mut self, id: &str) -> u64 {
        let g = self.fatigue_gen.entry(id.to_string()).or_insert(0);
        *g = g.wrapping_add(1);
        *g
    }

    /// Bump a unit's 三.17 聚合/解聚 generation, invalidating any in-flight `AggSplitComplete`.
    fn bump_aggsplit_gen(&mut self, id: &str) -> u64 {
        let g = self.aggsplit_gen.entry(id.to_string()).or_insert(0);
        *g = g.wrapping_add(1);
        *g
    }

    /// 三.17d — abort any in-flight 聚合/解聚 that `id` is part of (the initiator, or the consumed
    /// `other` of a 聚合): drop the transition, bump the gen so the queued `AggSplitComplete` no-ops,
    /// and free both units' busy windows. Called on 压制 (三.17d) and when a 同格交战 opens on the hex.
    fn interrupt_aggsplit(&mut self, id: &str) {
        // The transition is keyed by initiator; `id` may instead be the consumed party of a 聚合.
        let initiators: Vec<String> = self
            .aggsplit_op
            .iter()
            .filter_map(|(init, op)| {
                let hit = init == id
                    || matches!(&op.kind, AggSplitKind::Aggregate { other } if other == id);
                hit.then(|| init.clone())
            })
            .collect();
        for init in initiators {
            if let Some(op) = self.aggsplit_op.remove(&init) {
                self.bump_aggsplit_gen(&init);
                if let Some(u) = self.state.units.get_mut(&init) {
                    u.busy_until = 0;
                }
                if let AggSplitKind::Aggregate { other } = op.kind {
                    if let Some(u) = self.state.units.get_mut(&other) {
                        u.busy_until = 0;
                    }
                }
            }
        }
    }

    /// 三.1.4 — (re)arm the 疲劳恢复 clock for an idle unit: if it carries any 疲劳, schedule the next
    /// −1 decay `infantry_fatigue_decay_seconds` out, tagged with a fresh 疲劳 generation so an
    /// earlier decay tick no-ops. A no-op when 疲劳 is already 0 (nothing to recover).
    fn arm_fatigue_decay(&mut self, id: &str) {
        let fatigue = self.state.units.get(id).map(|u| u.fatigue).unwrap_or(0);
        if fatigue <= 0 {
            return;
        }
        let gen = self.bump_fatigue_gen(id);
        let secs = crate::mechanics::infantry_fatigue_decay_seconds(&self.rules);
        let at = self.state.clock.saturating_add(secs_to_ticks(secs as f64));
        self.schedule(
            at,
            Event::FatigueDecay {
                unit: id.to_string(),
                gen,
            },
        );
    }

    /// Halt a unit in place: drop back to Stopped and clear its busy window.
    fn settle_stopped(&mut self, id: &str) {
        if let Some(u) = self.state.units.get_mut(id) {
            // A 行军 unit that halts (reached its target, ran out of road, or was blocked by 三.1.3d)
            // stays in 行军 — "marching, currently stopped on the road". Leaving 行军 is a separate
            // set_mode (三.1.3f, 75 s). Every other mover settles to Stopped.
            if u.state != UnitState::March {
                u.state = UnitState::Stopped;
            }
            u.busy_until = 0;
        }
        // 三.1.4: the unit is now idle — start its 疲劳恢复 clock (no-op unless it carries 疲劳).
        self.arm_fatigue_decay(id);
    }

    /// Is the unit under 压制 at tick `t`? Tracked both by the `suppressed_until` timer and (when
    /// not in established 掩蔽) the `Suppressed` posture.
    fn is_suppressed(&self, id: &str, t: Tick) -> bool {
        self.state
            .units
            .get(id)
            .is_some_and(|u| u.suppressed_until > t || u.state == UnitState::Suppressed)
    }

    /// 三.3b/f — interrupt an in-progress 掩蔽转换: forget the pending transition (its queued
    /// `TransitionComplete` becomes a stale no-op) and clear the busy window, leaving the unit in
    /// its pre-Cover posture. Returns true if a cover transition was actually in flight.
    fn interrupt_cover(&mut self, id: &str) -> bool {
        if self.cover_transition.remove(id).is_some() {
            if let Some(u) = self.state.units.get_mut(id) {
                u.busy_until = 0;
            }
            true
        } else {
            false
        }
    }

    /// Is the unit currently riding inside a vehicle (三.2)?
    fn is_carried(&self, id: &str) -> bool {
        self.state
            .units
            .get(id)
            .is_some_and(|u| u.carried_by.is_some())
    }

    /// A 被载 unit cannot be ordered independently (三.2) — only `dismount`.
    fn reject_if_mounted(&self, id: &str) -> Result<()> {
        if self.is_carried(id) {
            return Err(EngineError::Command(format!(
                "{id} is mounted; dismount first"
            )));
        }
        Ok(())
    }

    /// 三.2d/e — when `id` is 压制, abort any 上下车 it is part of: a Mount is cancelled if the
    /// passenger OR the vehicle is suppressed; a Dismount only if the vehicle is. Frees both busy
    /// windows; the queued `MountComplete` then no-ops on its stale generation.
    fn interrupt_mounts_on_suppress(&mut self, id: &str) {
        let hit: Vec<String> = self
            .mount_op
            .iter()
            .filter_map(|(p, m)| {
                let interrupts = match m.kind {
                    MountKind::Mount => p == id || m.vehicle == id,
                    MountKind::Dismount => m.vehicle == id,
                };
                interrupts.then(|| p.clone())
            })
            .collect();
        for p in hit {
            if let Some(m) = self.mount_op.remove(&p) {
                if let Some(u) = self.state.units.get_mut(&p) {
                    u.busy_until = 0;
                }
                if let Some(v) = self.state.units.get_mut(&m.vehicle) {
                    v.busy_until = 0;
                }
            }
        }
        // 三.21d: a 压制 also aborts an in-flight 布雷 (the 火箭布雷车 must stay unsuppressed). Bump the
        // gen so the queued MineLayReady no-ops, and free the unit. Checked here (not only at the
        // 75 s completion) so it aborts even if the 压制 lifts before the lay would have finished.
        if self.minelay_in_flight.remove(id) {
            self.bump_minelayer_gen(id);
            if let Some(u) = self.state.units.get_mut(id) {
                u.busy_until = 0;
            }
        }
        // 三.17d: a 压制 also aborts an in-flight 聚合/解聚 (whether `id` is the initiator or the
        // consumed party of a 聚合).
        self.interrupt_aggsplit(id);
    }

    /// 三.2 (carrier death): a destroyed `vehicle` abandons any 上下车 it was part of and takes its
    /// embarked passengers with it. A unit still mid-*mount* (not yet aboard, carried_by == None)
    /// survives on the ground; a unit aboard (or mid-dismount, carried_by == vehicle) is destroyed.
    fn handle_carrier_death(&mut self, vehicle: &str) {
        let aborting: Vec<String> = self
            .mount_op
            .iter()
            .filter(|(_, m)| m.vehicle == vehicle)
            .map(|(p, _)| p.clone())
            .collect();
        for p in &aborting {
            self.mount_op.remove(p);
            if let Some(u) = self.state.units.get_mut(p) {
                u.busy_until = 0;
            }
        }
        let aboard: Vec<String> = self
            .state
            .units
            .iter()
            .filter(|(_, u)| u.carried_by.as_deref() == Some(vehicle) && u.alive)
            .map(|(id, _)| id.clone())
            .collect();
        for p in aboard {
            if let Some(u) = self.state.units.get_mut(&p) {
                u.alive = false;
                u.teams = 0;
                u.busy_until = 0;
            }
        }
        // 三.10g/h: a deployed 巡飞弹 self-destructs with its 发射车 (loaded ones die via `aboard`).
        let munitions: Vec<String> = self
            .loitering_parent
            .iter()
            .filter(|(_, p)| p.as_str() == vehicle)
            .map(|(m, _)| m.clone())
            .collect();
        for m in munitions {
            self.loitering_parent.remove(&m);
            self.loitering_target.remove(&m);
            if let Some(u) = self.state.units.get_mut(&m) {
                u.alive = false;
                u.teams = 0;
            }
        }
        // 三.13d: a 无人战车 is annihilated together with its 隶属 manned vehicle, wherever it stands
        // (dismounted ones aren't caught by `aboard`). `handle_carrier_death` is the shared sink for
        // every combat-caused death (direct/guided/间瞄/同格 all route through `apply_direct_fire`),
        // so the UGV dies regardless of what killed its parent. Only the UGV dies this way — a 步兵
        // guide may also be 隶属 to a vehicle (三.14b) but 三.13d is UGV-specific, so it is exempt.
        let affiliated: Vec<(String, Axial)> = self
            .state
            .units
            .iter()
            .filter(|(_, u)| {
                u.alive
                    && u.unit_type == UnitType::Ugv
                    && u.affiliated_to.as_deref() == Some(vehicle)
            })
            .map(|(id, u)| (id.clone(), u.pos))
            .collect();
        for (ugv, hex) in affiliated {
            if let Some(u) = self.state.units.get_mut(&ugv) {
                u.alive = false;
                u.teams = 0;
                u.busy_until = 0;
            }
            // The UGV's own death may also have cleared a 同格交战 it was part of (三.15f).
            self.end_same_hex_if_resolved(hex);
        }
    }

    /// 三.9.2 校射等级 for a plan hex: 目标校射 if a friendly 校射 unit (本方地面单位 or 无人机) can
    /// observe the ENEMY target in the hex; else 格内校射 if one has 通视 to the hex; else 无校射.
    pub(crate) fn spotting_level(&self, plan_hex: Axial, side: SideId) -> combat::IndirectSpotting {
        let spotters: Vec<&RuntimeUnit> = self
            .state
            .units
            .values()
            .filter(|u| u.alive && u.side == side && u.is_on_board() && can_spot(u.unit_type))
            .collect();
        // 格内目标 is the ENEMY being ranged on — a friendly unit in the hex grants no 目标校射, and a
        // 通视-but-unobservable enemy correctly stays hidden through can_observe (no fog leak, #5).
        let enemy_in_hex: Vec<&RuntimeUnit> = self
            .state
            .units
            .values()
            .filter(|u| u.alive && u.is_on_board() && u.side != side && u.pos == plan_hex)
            .collect();
        let observes_target = spotters.iter().any(|s| {
            enemy_in_hex
                .iter()
                .any(|tgt| crate::mechanics::can_observe(&self.rules, &self.map, s, tgt))
        });
        if observes_target {
            return combat::IndirectSpotting::Target;
        }
        let has_los = spotters
            .iter()
            .any(|s| crate::mechanics::line_of_sight(&self.map, &s.pos, &plan_hex));
        if has_los {
            return combat::IndirectSpotting::InHex;
        }
        // 三.19c/f: an 开机 friendly 炮兵校射雷达 floors all 间瞄 to 格内校射 — but only while the radar
        // vehicle itself is un-suppressed (三.19f, the effect auto-recovers when 压制 lifts).
        let clock = self.state.clock;
        let radar_active = self.radar_on.iter().any(|v| {
            self.state
                .units
                .get(v)
                .is_some_and(|u| u.alive && u.side == side)
                && !self.is_suppressed(v, clock)
        });
        if radar_active {
            combat::IndirectSpotting::InHex
        } else {
            combat::IndirectSpotting::None
        }
    }

    /// Build a unit's 战果修正 inputs (附2.6). `gun_count` is the firing 炮兵's 火炮数量 (its 班数).
    fn indirect_mods_for(&self, id: &str, gun_count: u8) -> Option<combat::IndirectMods> {
        let u = self.state.units.get(id)?;
        use crate::types::Terrain;
        let target_terrain = match self.map.cell(&u.pos).map(|c| c.terrain) {
            Some(Terrain::Urban) => combat::TargetTerrain::UrbanOrWorks,
            Some(Terrain::Forest) => combat::TargetTerrain::Forest,
            Some(Terrain::River | Terrain::RiverLarge | Terrain::Lake) => {
                combat::TargetTerrain::Water
            }
            _ => combat::TargetTerrain::Open,
        };
        let in_motion = matches!(
            u.state,
            UnitState::Moving | UnitState::March | UnitState::Charge1 | UnitState::Charge2
        );
        let stacked = self
            .state
            .units
            .values()
            .filter(|o| {
                o.alive
                    && o.is_on_board()
                    && crate::mechanics::is_ground(o.unit_type)
                    && o.pos == u.pos
            })
            .count()
            > 1;
        Some(combat::IndirectMods {
            target_terrain,
            target_cover: u.state == UnitState::Cover,
            target_moving_personnel: u.unit_type == UnitType::Infantry && in_motion,
            target_stacked: stacked,
            target_march: u.state == UnitState::March,
            armor: u.armor,
            gun_count,
        })
    }

    /// Apply a resolved 间瞄 salvo to every unit standing on `impact` — BOTH sides (三.9.3c 对己方
    /// 也生效). Each unit takes its own 1d6 战果修正.
    fn apply_indirect_at(
        &mut self,
        impact: Axial,
        base: combat::IndirectBase,
        gun_count: u8,
        t: Tick,
    ) {
        let victims: Vec<String> = self
            .state
            .units
            .values()
            // 三.22b: the off-board 天基侦察算子 is never a combat victim (it shares no hex on the
            // real board); excluding it here closes the 间瞄 path the command-level gate can't.
            .filter(|u| {
                u.alive
                    && u.is_on_board()
                    && crate::mechanics::can_be_targeted(u.unit_type)
                    && u.pos == impact
            })
            .map(|u| u.id.clone())
            .collect();
        for v in victims {
            if let Some(mods) = self.indirect_mods_for(&v, gun_count) {
                let outcome =
                    combat::apply_indirect_to_target(&self.tables, &mut self.rng, base, &mods);
                let _ = self.apply_direct_fire(&v, outcome, t); // config is valid; never errs here
            }
        }
        // 三.20.1c/2c: a 间瞄 salvo landing on a 车辆/战斗工事's hex ALSO hits the 工事 (the structure —
        // its 全程隐蔽 garrison is never an individual victim above, is_on_board excludes it), adjudicated
        // vs the 工事's own armour + 居民地/工事 terrain, with the result inherited per kind. A 隐蔽工事
        // is immune (三.20.3c, range 0). Resolved AFTER unit victims for a stable Rng order.
        // Pick the first CAN-ENGAGE 工事 on the hex (a non-engage 隐蔽工事 sharing the hex must not
        // mask a 车辆/战斗工事 behind it — find_map skips it rather than stopping at the first).
        let works = self.state.facilities.values().find_map(|f| {
            if f.at != impact {
                return None;
            }
            let k = crate::mechanics::FortKind::from_facility_kind(&f.kind)?;
            crate::mechanics::fortification_can_engage(k).then(|| (f.id.clone(), f.armor, k))
        });
        if let Some((fid, armor, kind)) = works {
            let mods = combat::IndirectMods {
                target_terrain: combat::TargetTerrain::UrbanOrWorks,
                target_cover: false,
                target_moving_personnel: false,
                target_stacked: false,
                target_march: false,
                armor,
                gun_count,
            };
            let outcome =
                combat::apply_indirect_to_target(&self.tables, &mut self.rng, base, &mods);
            if kind == crate::mechanics::FortKind::Vehicle {
                self.apply_fire_to_vehicle_works(&fid, outcome);
            } else {
                let _ = self.apply_fire_to_personnel_works(&fid, outcome, t);
            }
        }
    }

    /// 三.9.3 间瞄裁决: resolve 校射 → 散布 → 命中/偏离 result, detonate over the impact hex (which may
    /// have 散布 off the plan hex), damage everyone there, and open the 300 s 爆炸 window.
    fn adjudicate_indirect(&mut self, plan_id: u64, t: Tick) {
        let Some(plan) = self.indirect_plans.get(&plan_id) else {
            return;
        };
        let plan_hex = plan.plan_hex;
        let artillery = plan.artillery.clone();
        let (side, art_pos, teams) = match self.state.units.get(&artillery) {
            Some(u) if u.alive => (u.side, u.pos, u.teams),
            _ => {
                self.indirect_plans.remove(&plan_id);
                return;
            }
        };
        let spotting = self.spotting_level(plan_hex, side);
        let dist = i64::from(art_pos.distance(&plan_hex));
        // No per-unit artillery sub-type is modelled yet, so all 炮兵 fire as 中型炮 (documented).
        let (scatter, base) = combat::resolve_indirect_plan(
            &self.tables,
            &mut self.rng,
            spotting,
            dist,
            combat::GunClass::Medium,
        );
        // Draw the 散布 direction unconditionally to keep the Rng stream branch-stable.
        let dir = self.rng.next_u32_below(6) as usize;
        let impact = match scatter {
            combat::Scatter::Hexes(n) => {
                let mut h = plan_hex;
                for _ in 0..n {
                    h = h.neighbor(dir);
                }
                h
            }
            _ => plan_hex,
        };
        if let Some(p) = self.indirect_plans.get_mut(&plan_id) {
            p.impact = Some(impact);
            p.base = base;
            p.gun_count = teams;
        }
        self.apply_indirect_at(impact, base, teams, t);
        let boom = secs_to_ticks(self.rules.timing("indirect_boom").unwrap_or(0.0));
        self.schedule(
            t.saturating_add(boom),
            Event::IndirectBoomEnd { plan: plan_id },
        );
    }

    /// Which sides have a live, on-ground (not 被载) 地面 unit on `hex` (三.15 trigger/end condition).
    fn sides_on_hex(&self, hex: Axial) -> (bool, bool) {
        let mut red = false;
        let mut blue = false;
        for u in self.state.units.values() {
            if u.alive
                && u.pos == hex
                && u.is_on_board()
                && crate::mechanics::is_ground(u.unit_type)
                && crate::mechanics::can_be_targeted(u.unit_type)
            // 三.22b: 天基 isn't a 同格 combatant
            {
                match u.side {
                    SideId::Red => red = true,
                    SideId::Blue => blue = true,
                }
            }
        }
        (red, blue)
    }

    /// 三.15a — `attacker` fires its loadout's best weapon at distance 0 at `target_id` and then
    /// enters the 武器冷却 the 同格交战 shot incurs (三.15b). Returns true if a 班 was destroyed (for
    /// the stalemate counter).
    fn same_hex_shot(&mut self, attacker_id: &str, target_id: &str, t: Tick) -> bool {
        let attacker = self.state.units.get(attacker_id).cloned();
        let target = self.state.units.get(target_id).cloned();
        let (Some(attacker), Some(target)) = (attacker, target) else {
            return false;
        };
        if !attacker.alive || !target.alive {
            return false;
        }
        let before = target.teams;
        let weapons = combat::unit_loadout(&self.rules, attacker.unit_type);
        let wrefs: Vec<&str> = weapons.iter().map(String::as_str).collect();
        let outcome = combat::same_hex_engage(
            &self.tables,
            &self.map,
            &mut self.rng,
            &wrefs,
            &attacker,
            &target,
        );
        let _ = self.apply_direct_fire(target_id, outcome, t);
        // 三.15b: the firing unit's weapon goes onto cooldown (replacing any prior cooldown).
        if let Ok(cd) = self.require_timing("weapon_cooldown_direct") {
            let cd = secs_to_ticks(cd);
            if let Some(u) = self.state.units.get_mut(attacker_id) {
                u.weapon_state = WeaponState::Cooling;
            }
            self.schedule(
                t.saturating_add(cd),
                Event::WeaponReady {
                    unit: attacker_id.to_string(),
                },
            );
        }
        self.state
            .units
            .get(target_id)
            .map(|u| u.teams < before)
            .unwrap_or(true)
    }

    /// Is a unit eligible to fire in 同格交战 (三.15b)? Not 行军中, and not an un-dismounted 被载 unit.
    fn same_hex_can_fire(&self, id: &str) -> bool {
        self.state
            .units
            .get(id)
            .is_some_and(|u| u.alive && u.state != UnitState::March && u.is_on_board())
    }

    /// Live 地面 units on `hex` in 先入 order (三.15a): earliest-entered first, id as a stable tie.
    fn same_hex_attackers(&self, hex: Axial) -> Vec<String> {
        let mut v: Vec<(&String, u64)> = self
            .state
            .units
            .values()
            .filter(|u| {
                u.alive
                    && u.pos == hex
                    && u.is_on_board()
                    && crate::mechanics::is_ground(u.unit_type)
                    && crate::mechanics::can_be_targeted(u.unit_type) // 三.22b: 天基 doesn't fight 同格
            })
            .map(|u| (&u.id, self.same_hex_order.get(&u.id).copied().unwrap_or(0)))
            .collect();
        v.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(b.0)));
        v.into_iter().map(|(id, _)| id.clone()).collect()
    }

    /// 三.15 同格交战: if both sides' 地面 units now occupy `hex` and none is active there, start one —
    /// interrupting in-progress 上下车/掩蔽/雷达 transitions (三.15e), cancelling existing 武器冷却
    /// (三.15b), and running an immediate round (三.15b) that then recurs every 25 s.
    fn maybe_trigger_same_hex(&mut self, hex: Axial, t: Tick) {
        if self.same_hex.contains_key(&hex) {
            return;
        }
        let (red, blue) = self.sides_on_hex(hex);
        if !(red && blue) {
            return;
        }
        self.same_hex_ctr = self.same_hex_ctr.wrapping_add(1);
        let gen = self.same_hex_ctr;
        self.same_hex
            .insert(hex, SameHexState { gen, dry_rounds: 0 });
        // 三.15e/b: interrupt every in-hex unit's 上下车 / 掩蔽 / 雷达 transition and clear its cooldown.
        let here: Vec<String> = self
            .state
            .units
            .values()
            .filter(|u| u.alive && u.pos == hex)
            .map(|u| u.id.clone())
            .collect();
        for id in here {
            self.interrupt_cover(&id);
            self.interrupt_mounts_on_suppress(&id);
            self.radar_spinup.remove(&id);
            if let Some(u) = self.state.units.get_mut(&id) {
                if u.weapon_state == WeaponState::Cooling {
                    u.weapon_state = WeaponState::Deployed; // 三.15b: existing cooldown cancelled
                }
            }
        }
        self.run_same_hex_round(hex, gen, t);
    }

    /// Run one 同格交战 round on `hex` (三.15a/b): each eligible 地面 unit, in 先入 order, fires once at
    /// its priority enemy; reschedule +25 s unless one side has left/died (三.15f) or the engagement
    /// has gone too many rounds without a kill (stalemate guard — hard rule #1).
    fn run_same_hex_round(&mut self, hex: Axial, gen: u64, t: Tick) {
        if self.same_hex.get(&hex).map(|s| s.gen) != Some(gen) {
            return; // a stale round for an engagement that ended/restarted
        }
        let (red, blue) = self.sides_on_hex(hex);
        if !(red && blue) {
            self.same_hex.remove(&hex);
            return;
        }
        let mut any_kill = false;
        for atk in self.same_hex_attackers(hex) {
            if !self.same_hex_can_fire(&atk) {
                continue;
            }
            let side = match self.state.units.get(&atk) {
                Some(u) => u.side,
                None => continue,
            };
            let in_hex: Vec<RuntimeUnit> = self
                .state
                .units
                .values()
                .filter(|u| u.alive && u.pos == hex && crate::mechanics::is_ground(u.unit_type))
                .cloned()
                .collect();
            let refs: Vec<&RuntimeUnit> = in_hex.iter().collect();
            if let Some(target) = combat::pick_same_hex_target(&mut self.rng, side, &refs) {
                let target_id = target.id.clone();
                if self.same_hex_shot(&atk, &target_id, t) {
                    any_kill = true;
                }
            }
        }
        let (red, blue) = self.sides_on_hex(hex);
        if !(red && blue) {
            self.same_hex.remove(&hex);
            return;
        }
        // Stalemate guard (hard rule #1): stop a no-progress engagement so the sim terminates.
        let dry = match self.same_hex.get_mut(&hex) {
            Some(s) => {
                s.dry_rounds = if any_kill { 0 } else { s.dry_rounds + 1 };
                s.dry_rounds
            }
            None => return,
        };
        if dry >= SAME_HEX_MAX_DRY_ROUNDS {
            self.same_hex.remove(&hex);
            return;
        }
        let interval = secs_to_ticks(
            self.rules
                .timing("same_hex_engage_interval")
                .unwrap_or(25.0),
        );
        self.schedule(
            t.saturating_add(interval),
            Event::SameHexEngage { hex, gen },
        );
    }

    /// Re-evaluate a hex an engagement may have just emptied of one side; end it if so (三.15f).
    fn end_same_hex_if_resolved(&mut self, hex: Axial) {
        if self.same_hex.contains_key(&hex) {
            let (red, blue) = self.sides_on_hex(hex);
            if !(red && blue) {
                self.same_hex.remove(&hex);
            }
        }
    }

    /// 三.15d — a unit leaving an active 同格交战 hex draws one punishment shot from every enemy
    /// 地面 unit still in that hex.
    fn same_hex_punish(&mut self, leaver: &str, hex: Axial, t: Tick) {
        let leaver_side = match self.state.units.get(leaver) {
            Some(u) => u.side,
            None => return,
        };
        let enemies: Vec<String> = self
            .state
            .units
            .values()
            .filter(|u| {
                u.alive
                    && u.pos == hex
                    && u.side != leaver_side
                    && crate::mechanics::is_ground(u.unit_type)
                    && crate::mechanics::can_be_targeted(u.unit_type) // 三.22b: 天基 fires no 同格 shot
                    && u.state != UnitState::March
                    && u.is_on_board()
            })
            .map(|u| u.id.clone())
            .collect();
        for atk in enemies {
            if !self.state.units.get(leaver).is_some_and(|u| u.alive) {
                break; // the leaver is already dead
            }
            self.same_hex_shot(&atk, leaver, t);
        }
    }

    /// Current altitude of a 运输直升机 (三.16); a unit with no recorded altitude flies 低空.
    fn heli_alt(&self, id: &str) -> HeliAlt {
        self.state
            .units
            .get(id)
            .map(|u| u.heli_alt)
            .unwrap_or(HeliAlt::Low)
    }

    /// Begin a 上下车 (三.2): validate co-location/stopped/unsuppressed, then schedule the 75 s
    /// `MountComplete`. Both parties go busy and accept no other command meanwhile (三.2b).
    fn begin_mount(
        &mut self,
        passenger: &str,
        vehicle: &str,
        kind: MountKind,
        t: Tick,
    ) -> Result<()> {
        self.reject_if_busy(passenger, t)?;
        self.reject_if_busy(vehicle, t)?;
        // 三.2c: neither party may be 被压制 when starting.
        if self.is_suppressed(passenger, t) || self.is_suppressed(vehicle, t) {
            return Err(EngineError::Command(
                "cannot mount/dismount while suppressed (三.2c)".into(),
            ));
        }
        let (p_pos, p_state, p_carried, p_type, p_side, p_teams) =
            match self.state.units.get(passenger) {
                Some(u) => (
                    u.pos,
                    u.state,
                    u.carried_by.clone(),
                    u.unit_type,
                    u.side,
                    u.teams,
                ),
                None => return Err(EngineError::Command("no controllable unit".into())),
            };
        let (v_pos, v_state, v_type) = match self.state.units.get(vehicle) {
            Some(u) => (u.pos, u.state, u.unit_type),
            None => return Err(EngineError::Command("no controllable unit".into())),
        };
        // 三.16e: 索降装/卸载 onto a 运输直升机 needs the heli at 超低空 over 开阔地 (Open terrain).
        if v_type == UnitType::TransportHeli {
            if self.heli_alt(vehicle) != HeliAlt::VeryLow {
                return Err(EngineError::Command(
                    "the 运输直升机 must be at 超低空 to load/unload (三.16e)".into(),
                ));
            }
            if self.map.cell(&v_pos).map(|c| c.terrain) != Some(crate::types::Terrain::Open) {
                return Err(EngineError::Command(
                    "索降 needs 开阔地 (open terrain) (三.16e)".into(),
                ));
            }
        }
        match kind {
            MountKind::Mount => {
                if p_carried.is_some() {
                    return Err(EngineError::Command("unit is already mounted".into()));
                }
                // Only a dismounted man-portable squad rides a vehicle — not another vehicle.
                // (三.2 doesn't enumerate this; an interpretive default pending the 三.16/三.17
                // carrier-capacity rules, which will also bound how many may board.)
                if !can_be_passenger(p_type) {
                    return Err(EngineError::Command(
                        "only dismounted infantry-type squads can mount a vehicle".into(),
                    ));
                }
                if !is_carrier(v_type) {
                    return Err(EngineError::Command(
                        "that unit cannot carry passengers".into(),
                    ));
                }
                // 三.2a: same hex and both stopped.
                if p_pos != v_pos {
                    return Err(EngineError::Command(
                        "passenger and vehicle must share a hex to mount (三.2a)".into(),
                    ));
                }
                if p_state != UnitState::Stopped || v_state != UnitState::Stopped {
                    return Err(EngineError::Command(
                        "both must be stopped to mount (三.2a)".into(),
                    ));
                }
                // 三.16f: load needs the 运输直升机 空载, and the squad count ≤ its carry capacity.
                if v_type == UnitType::TransportHeli {
                    let occupied = self
                        .state
                        .units
                        .values()
                        .any(|u| u.alive && u.carried_by.as_deref() == Some(vehicle));
                    if occupied {
                        return Err(EngineError::Command(
                            "the 运输直升机 must be 空载 to load (三.16f)".into(),
                        ));
                    }
                    let cap = self
                        .rules
                        .air
                        .get("heli_capacity_squads")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    if u64::from(p_teams) > cap {
                        return Err(EngineError::Command(
                            "too many 班 for the 运输直升机 — 解聚 first (三.16f)".into(),
                        ));
                    }
                }
            }
            MountKind::Dismount => {
                // 三.4: the passenger returns to the ground at the carrier's hex, so that hex must
                // have room under the stacking cap (the carried passenger is not yet counted).
                if !crate::mechanics::stacking_allows_entry(
                    &self.state,
                    p_side,
                    v_pos,
                    passenger,
                    self.stacking_cap,
                ) {
                    return Err(EngineError::Command(
                        "no room to dismount here — the hex stack is full (三.4)".into(),
                    ));
                }
                // 三.2a 双方停止 + 同格: the carrier must be halted and the passenger must be where
                // the carrier is (a 被载 unit rides with it, so this normally holds — defensive).
                if v_state != UnitState::Stopped {
                    return Err(EngineError::Command(
                        "the vehicle must be stopped to dismount (三.2a)".into(),
                    ));
                }
                if p_pos != v_pos {
                    return Err(EngineError::Command(
                        "passenger is not co-located with its carrier (三.2a)".into(),
                    ));
                }
            }
        }
        let dur = secs_to_ticks(self.require_timing("mount_dismount")?);
        let done = t.saturating_add(dur);
        self.mount_gen_ctr = self.mount_gen_ctr.wrapping_add(1);
        let gen = self.mount_gen_ctr;
        self.mount_op.insert(
            passenger.to_string(),
            MountInTransit {
                gen,
                vehicle: vehicle.to_string(),
                kind,
            },
        );
        if let Some(u) = self.state.units.get_mut(passenger) {
            u.busy_until = done;
        }
        if let Some(u) = self.state.units.get_mut(vehicle) {
            u.busy_until = done;
        }
        self.schedule(
            done,
            Event::MountComplete {
                passenger: passenger.to_string(),
                gen,
            },
        );
        Ok(())
    }

    /// Ticks to walk `from -> to` for `ut`, or None if that hop is impassable (slope too steep,
    /// off-map, or no configured speed). Vehicles pay terrain+slope (三.1.2); infantry are
    /// terrain-independent and pay only 高差 (三.1.4). The entered hex's terrain/elevation drives
    /// the cost. Reads rules-as-data via [`crate::mechanics`].
    fn step_ticks(&self, id: &str, ut: UnitType, from: Axial, to: Axial) -> Option<Tick> {
        let from_c = self.map.cell(&from)?;
        let to_c = self.map.cell(&to)?;
        // 空中单位 fly at base speed over any terrain (三.10a/三.11a/三.12a) — no slope/impassability.
        if crate::mechanics::is_air(ut) {
            return match crate::mechanics::air_step_time_seconds(&self.rules, ut) {
                Some(s) if s > 0.0 => {
                    // 三.16c: a 运输直升机 in 超低空 moves at half speed (double the time).
                    let s =
                        if ut == UnitType::TransportHeli && self.heli_alt(id) == HeliAlt::VeryLow {
                            s * 2.0
                        } else {
                            s
                        };
                    Some(secs_to_ticks(s))
                }
                _ => None,
            };
        }
        // 三.1.2g 路障: a 路障 facility makes its hex 不可通过 for GROUND 算子 (人员/车辆) — air flew over
        // above (only ground is blocked). Treated like impassable terrain: step_ticks returns None, so
        // plan_hop halts the mover at the hex BEFORE the 路障. 路障 are static board features (never a
        // 工事, so not in `state.facilities`, and never created mid-sim) — read straight from the
        // immutable scenario, so this plan-time check needs no MoveArrive re-check.
        if self
            .scenario
            .facilities
            .iter()
            .any(|f| f.kind == "roadblock" && f.at == to)
        {
            return None;
        }
        // 三.1.3a/b: a 车辆 in 行军 moves at its ROAD's speed (乡村/一般/等级 km/h), ignoring terrain &
        // slope. plan_hop's route check (沿路走向) guarantees `from` carries a road whenever the unit
        // is marching, so a missing road here means it cannot advance (it settles stopped).
        if self
            .state
            .units
            .get(id)
            .is_some_and(|u| u.state == UnitState::March)
        {
            return from_c.road.as_ref().and_then(|road| {
                crate::mechanics::march_step_time_seconds(&self.rules, &road.kind)
                    .filter(|s| *s > 0.0)
                    .map(secs_to_ticks)
            });
        }
        // The unit's live movement mode (preserved across a move by move_to). 三.1.4 infantry speed
        // multipliers (半速 / 一二级冲锋) key off it; a 车辆 in 半速 doubles its terrain time.
        let mode = self
            .state
            .units
            .get(id)
            .map(|u| u.state)
            .unwrap_or(UnitState::Moving);
        let secs = if matches!(ut, UnitType::Infantry) {
            let em = self
                .map
                .elevation_unit_meters
                .map(|v| v as i32)
                .unwrap_or(0);
            crate::mechanics::infantry_step_time_seconds(
                &self.rules,
                em,
                from_c.elevation,
                to_c.elevation,
                mode,
            )
        } else {
            let base = crate::mechanics::step_time_seconds(
                &self.rules,
                ut,
                to_c.terrain,
                from_c.elevation,
                to_c.elevation,
            );
            // 三.1: a 车辆 in 半速 (Half) takes twice as long (half speed) — e.g. clearing a 雷场 通路.
            if mode == UnitState::Half {
                base.map(|opt| opt.map(|s| s * 2.0))
            } else {
                base
            }
        };
        match secs {
            Ok(Some(s)) if s > 0.0 => Some(secs_to_ticks(s)),
            _ => None,
        }
    }

    /// Plan the next hex of a move toward `target` (三.1). Settles the unit as Stopped on arrival,
    /// on an impassable hop, or against a full 堆叠 (三.4); otherwise schedules the next
    /// [`Event::MoveArrive`]. A stale generation or a dead/absent unit abandons the move.
    fn plan_hop(&mut self, id: &str, gen: u64, target: Axial, now: Tick) {
        if self.move_gen.get(id).copied() != Some(gen) {
            return;
        }
        let (ut, unit_side, cur, alive) = match self.state.units.get(id) {
            Some(u) => (u.unit_type, u.side, u.pos, u.alive),
            None => return,
        };
        if !alive {
            return;
        }
        if cur == target {
            self.settle_stopped(id);
            return;
        }
        // 三.1.4 活体疲劳: a 冲锋ing 步兵 that has accrued too much 疲劳 can no longer sustain its
        // movement (一级疲劳 bars 二级冲锋; 二级疲劳 bars ALL movement). It halts where it stands — the
        // player must re-issue a permitted mode once it rests. No-op for vehicles / fatigue-free
        // infantry (whose current mode is always allowed).
        if matches!(ut, UnitType::Infantry) {
            let (mode, fatigue) = self
                .state
                .units
                .get(id)
                .map(|u| (u.state, u.fatigue))
                .unwrap_or((UnitState::Moving, 0));
            if !crate::mechanics::infantry_mode_allowed(mode, fatigue) {
                self.settle_stopped(id);
                return;
            }
        }
        let path = cur.line_to(&target);
        let Some(&next) = path.get(1) else {
            self.settle_stopped(id);
            return;
        };
        // 三.1.3b/e: a marching 车辆 may only follow the road — the current hex's road must exit toward
        // `next` AND `next`'s road must connect BACK (a continuous road segment, not a one-way config
        // or a dead-end stub). 三.1.3d: a stopped or non-marching unit in `next` blocks the column.
        // All pure predicates over the true board; the outward "stopped" signal is fog-filtered like
        // any other halt.
        let is_march = self
            .state
            .units
            .get(id)
            .is_some_and(|u| u.state == UnitState::March);
        if is_march {
            let on_route = hex_dir(cur, next).is_some_and(|d| {
                let exits = self
                    .map
                    .cell(&cur)
                    .is_some_and(|c| crate::mechanics::road_allows_direction(c, d));
                // `next` must carry the road back toward `cur` (the opposite hex direction (d+3)%6).
                let connects_back = self
                    .map
                    .cell(&next)
                    .is_some_and(|c| crate::mechanics::road_allows_direction(c, (d + 3) % 6));
                exits && connects_back
            });
            if !on_route || !crate::mechanics::march_allows_entry(&self.state, next) {
                self.settle_stopped(id);
                return;
            }
        }
        // Impassable terrain/slope, or the destination stack is already full (三.4a): halt here.
        let Some(step) = self.step_ticks(id, ut, cur, next) else {
            self.settle_stopped(id);
            return;
        };
        if !crate::mechanics::stacking_allows_entry(
            &self.state,
            unit_side,
            next,
            id,
            self.stacking_cap,
        ) {
            self.settle_stopped(id);
            return;
        }
        let arrive = now.saturating_add(step.max(1));
        if let Some(u) = self.state.units.get_mut(id) {
            u.busy_until = arrive;
        }
        self.schedule(
            arrive,
            Event::MoveArrive {
                unit: id.to_string(),
                gen,
                to: next,
                target,
            },
        );
    }

    /// Fetch a rule timing in seconds or fail. Timings are rules-as-data (CLAUDE.md
    /// rule #2): a missing key is a config error, never a silently-invented default.
    fn require_timing(&self, key: &str) -> Result<f64> {
        self.rules
            .timing(key)
            .ok_or_else(|| EngineError::Rules(format!("missing rule timing {key:?}")))
    }

    /// Resolve a unit that `side` is allowed to command. Returns the SAME generic error for
    /// "no such unit", "not your unit", AND "dead unit", so a command's error never reveals an
    /// enemy/unobserved unit's existence or state (CLAUDE.md rule #5, no fog leak) and a destroyed
    /// unit can no longer act. Call this before any state-dependent check.
    fn require_own_unit(&self, side: SideId, id: &str) -> Result<()> {
        match self.state.units.get(id) {
            Some(u) if u.side == side && u.alive => Ok(()),
            _ => Err(EngineError::Command(format!("no controllable unit {id:?}"))),
        }
    }

    /// Reject a movement order while the unit is still completing a transition: a unit
    /// in transition takes no new movement order until `busy_until` (三.1 — the stop
    /// transition and mode switches block re-tasking for their 75 s duration).
    fn reject_if_busy(&self, id: &str, t: Tick) -> Result<()> {
        if self.state.units.get(id).is_some_and(|u| u.busy_until > t) {
            return Err(EngineError::Command(format!(
                "{id} is mid-transition; cannot accept a new movement order yet"
            )));
        }
        Ok(())
    }

    /// 三.1.3e — a 车辆 in 行军 cannot 射击/引导射击/上下车/发射巡飞弹 (it must first 停止行军, 三.1.3f). The
    /// 离开道路 ban is enforced separately by `plan_hop`'s road-route check.
    fn reject_if_marching(&self, id: &str) -> Result<()> {
        if self
            .state
            .units
            .get(id)
            .is_some_and(|u| u.state == UnitState::March)
        {
            return Err(EngineError::Command(format!(
                "{id} is 行军; stop the march first (三.1.3e)"
            )));
        }
        Ok(())
    }

    /// 三.20 — a unit garrisoned inside a 工事 is inert (全程隐蔽): it cannot move, fire, board, or
    /// capture; its only valid order is `exit_facility`. The message is the unit's own state, so it
    /// is target-independent and leaks nothing (rule #5).
    fn reject_if_in_facility(&self, id: &str) -> Result<()> {
        if self
            .state
            .units
            .get(id)
            .is_some_and(|u| u.inside_facility.is_some())
        {
            return Err(EngineError::Command(format!(
                "{id} is inside a 工事; exit it first (三.20)"
            )));
        }
        Ok(())
    }

    /// 三.17 — common eligibility for a unit initiating 聚合/解聚: own, idle (not busy) & Stopped,
    /// unsuppressed, not mounted/garrisoned/marching, a ground non-炮兵 (`supports_aggregation`), free
    /// of carriage/巡飞弹 entanglement (三.17e/f — carried passengers / a live 巡飞弹 aloft are resolved
    /// separately, not here), and not in an active 同格交战 (三.17d). Returns its (type, pos, 班数).
    fn aggsplit_check(&self, side: SideId, id: &str, t: Tick) -> Result<(UnitType, Axial, u8)> {
        self.require_own_unit(side, id)?;
        self.reject_if_mounted(id)?;
        self.reject_if_in_facility(id)?;
        self.reject_if_marching(id)?;
        self.reject_if_busy(id, t)?;
        if self.is_suppressed(id, t) {
            return Err(EngineError::Command(format!(
                "{id} cannot 聚合/解聚 while 压制 (三.17d)"
            )));
        }
        let (ut, pos, teams, state) = match self.state.units.get(id) {
            Some(u) if u.alive => (u.unit_type, u.pos, u.teams, u.state),
            _ => return Err(EngineError::Command("no controllable unit".into())),
        };
        if !crate::mechanics::supports_aggregation(ut) {
            return Err(EngineError::Command(
                "this unit type cannot 聚合/解聚 (三.17a/i: 地面算子, 非炮兵)".into(),
            ));
        }
        if state != UnitState::Stopped {
            return Err(EngineError::Command(
                "must be stopped to 聚合/解聚 (三.17b)".into(),
            ));
        }
        if self.same_hex.contains_key(&pos) {
            return Err(EngineError::Command(
                "cannot 聚合/解聚 during a 同格交战 (三.17d)".into(),
            ));
        }
        // 三.17f: a unit carrying 搭载 must unload first (passengers 聚合/解聚 independently).
        if self
            .state
            .units
            .values()
            .any(|p| p.alive && p.carried_by.as_deref() == Some(id))
        {
            return Err(EngineError::Command(
                "unload 搭载 before 聚合/解聚 (三.17f)".into(),
            ));
        }
        // 三.17e: a 发射车 with a 巡飞弹 still aloft cannot 聚合/解聚.
        if self
            .loitering_parent
            .values()
            .any(|launcher| launcher == id)
        {
            return Err(EngineError::Command(
                "a 发射车 with a 巡飞弹 aloft cannot 聚合/解聚 (三.17e)".into(),
            ));
        }
        Ok((ut, pos, teams))
    }

    /// The `FortKind` of the 工事 unit `u` is garrisoned in (or `None` if it is on the open board).
    fn garrison_kind(&self, u: &RuntimeUnit) -> Option<crate::mechanics::FortKind> {
        u.inside_facility
            .as_deref()
            .and_then(|fid| self.state.facilities.get(fid))
            .and_then(|f| crate::mechanics::FortKind::from_facility_kind(&f.kind))
    }

    /// 三.20.1b/2b — whether a garrison may fire/observe OUT: true for a 车辆/战斗工事 (it stays 全程隐蔽
    /// while doing so), false for a 隐蔽工事 garrison and any on-board unit (which observes normally).
    fn garrison_can_fire_out(&self, u: &RuntimeUnit) -> bool {
        self.garrison_kind(u)
            .is_some_and(crate::mechanics::fortification_can_engage)
    }

    /// Whether `side` can observe the 工事 `fac` (三.20.1a/2a): some on-board own unit lies within the
    /// kind's exposure range (12 格 for 车辆/战斗工事; 0 / 本格 for a 隐蔽工事, never surfaced here) AND
    /// has 通视 to it. Used to surface + target an ENEMY 工事 without leaking unobservable ones (rule #5).
    fn facility_observed_by(&self, fac: &crate::types::RuntimeFacility, side: SideId) -> bool {
        let Some(kind) = crate::mechanics::FortKind::from_facility_kind(&fac.kind) else {
            return false;
        };
        let exposed = self.rules.fortification_exposed_range().unwrap_or(0);
        let range = crate::mechanics::fortification_observe_range(kind, exposed);
        if range <= 0 {
            return false; // 隐蔽工事: revealed only by an enemy ENTERING its hex (三.20.3d), not at range
        }
        self.state.units_of(side).any(|o| {
            // A 车辆/战斗工事 garrison observes OUT too (三.20.1b/2b), matching the enemy-unit observer
            // gate — so it can surface (and thus target by id) an enemy 工事 it sees.
            (o.is_on_board() || self.garrison_can_fire_out(o))
                && i64::from(o.pos.distance(&fac.at)) <= range
                && crate::mechanics::line_of_sight_alt(
                    &self.map,
                    &o.pos,
                    &fac.at,
                    crate::mechanics::air_altitude_levels(&self.rules, &self.map, o),
                    0,
                )
        })
    }

    /// 三.20.1c/2c — fire DIRECTLY at a 工事. The 工事 (not its 全程隐蔽 garrison) is the target,
    /// adjudicated as a vehicle vs its own armour (`combat::direct_fire_at_works`); the result then
    /// inherits per kind. COMMIT W6 wires only 人员战斗工事 (三.20.2e, occupants inherit directly);
    /// 车辆工事 (三.20.1, capacity-absorb) is deferred. Fog-safe: a missing/friendly/non-战斗/
    /// out-of-range 工事 all collapse to the generic "invalid fire target" (rule #5).
    fn fire_direct_at_facility(
        &mut self,
        side: SideId,
        shooter_id: &str,
        weapon: &str,
        fac_id: &str,
        t: Tick,
    ) -> Result<()> {
        let mut sh = match self.state.units.get(shooter_id) {
            Some(u) => u.clone(),
            None => return Err(EngineError::Command("no controllable unit".into())),
        };
        // 三.3c: firing leaves 掩蔽 — adjudicate as Stopped (only mutate the real unit once it fires).
        let was_in_cover = sh.state == UnitState::Cover;
        if was_in_cover {
            sh.state = UnitState::Stopped;
        }
        // Only an ENEMY fireable 工事 (车辆/战斗, fortification_can_engage) is targetable; every other
        // case (missing / friendly / 隐蔽) is folded into the generic fog error (rule #5).
        let target = self
            .state
            .facilities
            .get(fac_id)
            .cloned()
            .and_then(|f| crate::mechanics::FortKind::from_facility_kind(&f.kind).map(|k| (f, k)))
            .filter(|(f, k)| {
                crate::mechanics::fortification_can_engage(*k)
                    && f.owner.is_some()
                    && f.owner != Some(side)
            });
        let Some((fac, kind)) = target else {
            return Err(EngineError::Command("invalid fire target".into()));
        };
        let range = crate::mechanics::fortification_observe_range(
            kind,
            self.rules.fortification_exposed_range().unwrap_or(0),
        );
        match combat::direct_fire_at_works(
            &self.tables,
            &self.map,
            &mut self.rng,
            &self.rules,
            &sh,
            weapon,
            fac.at,
            fac.armor,
            range,
        ) {
            FireOutcome::Rejected(reason) => {
                // "target not observed" leaks an unobserved 工事's existence — collapse it (rule #5);
                // other reasons describe the firer's own unit, safe to surface.
                let msg = if reason == "target not observed" {
                    "invalid fire target"
                } else {
                    reason
                };
                Err(EngineError::Command(msg.into()))
            }
            FireOutcome::Resolved(outcome) => {
                if was_in_cover {
                    if let Some(u) = self.state.units.get_mut(shooter_id) {
                        if u.state == UnitState::Cover {
                            u.state = UnitState::Stopped;
                        }
                    }
                }
                if sh.unit_type == UnitType::Tank {
                    self.interrupt_cover(shooter_id);
                }
                // 战果继承 splits by kind (三.20.1e/2e): a 人员战斗工事's garrison inherits the result
                // directly; a 车辆工事 absorbs it into its capacity (the vehicles inside are untouched).
                if kind == crate::mechanics::FortKind::Vehicle {
                    self.apply_fire_to_vehicle_works(fac_id, outcome);
                } else {
                    self.apply_fire_to_personnel_works(fac_id, outcome, t)?;
                }
                let cd = secs_to_ticks(self.require_timing("weapon_cooldown_direct")?);
                if let Some(u) = self.state.units.get_mut(shooter_id) {
                    u.weapon_state = WeaponState::Cooling;
                }
                self.schedule(
                    t.saturating_add(cd),
                    Event::WeaponReady {
                        unit: shooter_id.to_string(),
                    },
                );
                Ok(())
            }
        }
    }

    /// 三.20.2e — the 人员战斗工事 garrison directly inherits a fire result: a 班 loss is dealt across
    /// the occupants in id order (the rule is silent on order; id-order is deterministic), a 压制
    /// result suppresses every occupant. (A 人员工事's capacity is fixed — only a 车辆工事 shrinks, so
    /// there is no over-capacity expulsion here.)
    fn apply_fire_to_personnel_works(
        &mut self,
        fac_id: &str,
        outcome: Outcome,
        t: Tick,
    ) -> Result<()> {
        let occupants: Vec<String> = self
            .state
            .units
            .iter()
            .filter(|(_, u)| u.alive && u.inside_facility.as_deref() == Some(fac_id))
            .map(|(id, _)| id.clone())
            .collect();
        match outcome {
            Outcome::Destroyed(n) => {
                let mut remaining = n;
                for occ in occupants {
                    if remaining == 0 {
                        break;
                    }
                    let teams = self.state.units.get(&occ).map(|u| u.teams).unwrap_or(0);
                    let k = remaining.min(teams);
                    if k > 0 {
                        self.apply_direct_fire(&occ, Outcome::Destroyed(k), t)?;
                        remaining -= k;
                    }
                }
            }
            Outcome::Kill => {
                for occ in occupants {
                    self.apply_direct_fire(&occ, Outcome::Kill, t)?;
                }
            }
            Outcome::Suppress => {
                for occ in occupants {
                    self.apply_direct_fire(&occ, Outcome::Suppress, t)?;
                }
            }
            Outcome::NoEffect => {}
        }
        Ok(())
    }

    /// 三.20.1c/1e — fire against a 车辆工事: the vehicles inside do NOT inherit; the 工事 ABSORBS the
    /// hit, its remaining capacity shrinking by the 班 loss (压制/无效 leave it intact). 三.20.1f/20e:
    /// if the garrison no longer fits the reduced capacity, ALL occupants are expelled INSTANTLY (no
    /// 75 s 转换) onto the 工事's hex, 暴露 — and a 同格交战 opens if an enemy is already there.
    fn apply_fire_to_vehicle_works(&mut self, fac_id: &str, outcome: Outcome) {
        let loss = match outcome {
            Outcome::Destroyed(n) => n,
            Outcome::Kill => u8::MAX,
            Outcome::Suppress | Outcome::NoEffect => 0,
        };
        if loss == 0 {
            return;
        }
        if let Some(f) = self.state.facilities.get_mut(fac_id) {
            f.capacity = f.capacity.saturating_sub(loss);
        }
        let cap = self
            .state
            .facilities
            .get(fac_id)
            .map(|f| u32::from(f.capacity))
            .unwrap_or(0);
        let occupants: Vec<String> = self
            .state
            .units
            .iter()
            .filter(|(_, u)| u.alive && u.inside_facility.as_deref() == Some(fac_id))
            .map(|(id, _)| id.clone())
            .collect();
        let occupied: u32 = occupants
            .iter()
            .filter_map(|id| self.state.units.get(id))
            .map(|u| u32::from(u.teams))
            .sum();
        if crate::mechanics::fortification_over_capacity(occupied, cap) {
            let mut hex = None;
            for occ in &occupants {
                if let Some(u) = self.state.units.get_mut(occ) {
                    hex = Some(u.pos);
                    u.inside_facility = None;
                    u.busy_until = 0;
                }
                // Invalidate any in-flight 进入/离开工事 event for this unit (it just left forcibly).
                self.bump_facility_gen(occ);
                // Keep the 同格 entry order it had while garrisoned (stamped on entry); only assign one
                // if somehow missing — overwriting it would wrongly drop the defender behind a later
                // enemy in 先入序.
                if !self.same_hex_order.contains_key(occ) {
                    self.entry_seq = self.entry_seq.wrapping_add(1);
                    self.same_hex_order.insert(occ.clone(), self.entry_seq);
                }
            }
            if let Some(hex) = hex {
                self.maybe_trigger_same_hex(hex, self.state.clock);
            }
        }
    }

    /// Apply a resolved direct-fire [`Outcome`] to the target (8.6/8.8): destroyed teams reduce
    /// the unit (killed at 0); a hit (any destroy or a 压制 result) suppresses the survivors for
    /// 压制持续 seconds, with a `SuppressEnd` scheduled to clear it.
    pub(crate) fn apply_direct_fire(
        &mut self,
        target_id: &str,
        outcome: Outcome,
        t: Tick,
    ) -> Result<()> {
        let suppress_ticks = secs_to_ticks(self.require_timing("suppress_duration")?);
        // First settle teams/kills and decide whether the survivors get suppressed.
        let mut do_suppress = false;
        let mut died = false;
        if let Some(target) = self.state.units.get_mut(target_id) {
            match outcome {
                Outcome::Destroyed(n) => {
                    target.teams = target.teams.saturating_sub(n);
                    if target.teams == 0 {
                        target.alive = false;
                        died = true;
                    } else {
                        do_suppress = true;
                    }
                }
                Outcome::Kill => {
                    target.teams = 0;
                    target.alive = false;
                    died = true;
                }
                Outcome::Suppress => do_suppress = true,
                Outcome::NoEffect => {}
            }
        }
        if died {
            // 三.2 (carrier death — the rules are silent; documented interpretive default): a
            // destroyed carrier takes its embarked passengers with it, and any in-flight 上下车 it
            // was part of is abandoned. Keeps the invariant "no live passenger references a dead
            // carrier" and avoids units stuck inside a wreck.
            self.handle_carrier_death(target_id);
            // 三.15f: this death may have cleared one side from a 同格交战 hex.
            if let Some(hex) = self.state.units.get(target_id).map(|u| u.pos) {
                self.end_same_hex_if_resolved(hex);
            }
        }
        if do_suppress {
            // 三.3f: 压制 interrupts a 掩蔽转换 in progress (the unit never reaches Cover).
            self.interrupt_cover(target_id);
            // 三.2d/e: 压制 also aborts a 上下车 the unit (passenger or carrier) is part of.
            self.interrupt_mounts_on_suppress(target_id);
            // 三.19d: 压制 interrupts a 炮兵校射雷达's 开机 (a live 开机 just loses its effect — 三.19f —
            // and auto-recovers when 压制 lifts, so radar_on is left intact here).
            if self.radar_spinup.remove(target_id).is_some() {
                if let Some(u) = self.state.units.get_mut(target_id) {
                    u.busy_until = 0;
                }
            }
            let until = t.saturating_add(suppress_ticks);
            if let Some(target) = self.state.units.get_mut(target_id) {
                target.suppressed_until = until;
                // p05-g: 压制 does NOT lift an already-established 掩蔽 — keep the Cover posture and
                // let only the timer mark the unit as suppressed. Otherwise enter the Suppressed
                // posture.
                if target.state != UnitState::Cover {
                    target.state = UnitState::Suppressed;
                }
            }
            self.schedule(
                until,
                Event::SuppressEnd {
                    unit: target_id.to_string(),
                },
            );
        }
        // 三.16h 运输直升机整体战损: a 运输直升机 fired upon (only vulnerable 在超低空/装卸载/飞行中) is
        // adjudicated AS A WHOLE — the same 毁伤/压制 result applies to every loaded 班 (同等损失). This
        // is heli-specific: a ground 车辆 carrier instead SHIELDS its 搭载 from partial loss (三.2),
        // forwarding only total destruction. When the heli itself dies its cargo already dies with it
        // via `handle_carrier_death`, so forward here ONLY on a survived hit (partial 战损 or 压制).
        if !died
            && matches!(outcome, Outcome::Destroyed(_) | Outcome::Suppress)
            && self
                .state
                .units
                .get(target_id)
                .is_some_and(|u| u.unit_type == UnitType::TransportHeli)
        {
            let cargo: Vec<String> = self
                .state
                .units
                .iter()
                // Only loaded 班 (`can_be_passenger`) share the loss — 三.16h 装载单位 are 索降 troops, not
                // any 巡飞弹 a 发射车 might also ride via `carried_by`. This filter ALSO bounds the
                // recursion to depth-1: a 班 is never a carrier, so the forwarded `apply_direct_fire`
                // never re-enters this branch (no heli-carries-heli chain can recurse).
                .filter(|(_, u)| {
                    u.alive
                        && u.carried_by.as_deref() == Some(target_id)
                        && can_be_passenger(u.unit_type)
                })
                .map(|(id, _)| id.clone())
                .collect();
            for c in cargo {
                self.apply_direct_fire(&c, outcome, t)?;
            }
        }
        Ok(())
    }

    /// Submit an order at simulation tick `t`. Returns Unimplemented for ops not yet built.
    /// `cmd` is a JSON action (llm_tools.schema.json shape). Callers convert any
    /// seconds-valued timestamps to ticks (see [`crate::time::secs_to_ticks`]).
    pub fn submit(&mut self, side: SideId, cmd: &serde_json::Value, t: Tick) -> Result<()> {
        let op = cmd
            .get("op")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EngineError::Command("missing op".into()))?;
        // 三.20: a unit garrisoned inside a 工事 is inert — except that a 车辆/战斗工事 garrison MAY
        // 直瞄/防空 fire OUT while staying 全程隐蔽 (三.20.1b/2b); a 隐蔽工事 garrison cannot fire at all
        // (三.20.3b). 间瞄 (plan_indirect) from a 工事 is NOT permitted (only 直瞄 is named). Every other
        // order needs `exit_facility` first. Gated on the unit's OWN side so an enemy's garrison
        // status never leaks (rule #5): an order naming an enemy unit falls through to require_own_unit.
        if !matches!(op, "exit_facility" | "wait") {
            if let Ok(uid) = unit_id(cmd) {
                if let Some(fid) = self
                    .state
                    .units
                    .get(&uid)
                    .filter(|u| u.side == side)
                    .and_then(|u| u.inside_facility.clone())
                {
                    let can_fire_out = self
                        .state
                        .facilities
                        .get(&fid)
                        .and_then(|f| crate::mechanics::FortKind::from_facility_kind(&f.kind))
                        .is_some_and(crate::mechanics::fortification_can_engage);
                    let firing = matches!(op, "fire_direct" | "aa_fire");
                    if !(can_fire_out && firing) {
                        return Err(EngineError::Command(format!(
                            "{uid} is inside a 工事; exit it first (三.20)"
                        )));
                    }
                }
            }
        }
        match op {
            "wait" => Ok(()),
            "stop" => {
                let id = unit_id(cmd)?;
                self.require_own_unit(side, &id)?;
                self.reject_if_mounted(&id)?;
                self.reject_if_busy(&id, t)?;
                let stop_t = secs_to_ticks(self.require_timing("move_to_stop")?);
                let done = t.saturating_add(stop_t);
                if let Some(u) = self.state.units.get_mut(&id) {
                    u.busy_until = done;
                }
                self.schedule(
                    done,
                    Event::TransitionComplete {
                        unit: id,
                        to: UnitState::Stopped,
                        cover_gen: None,
                    },
                );
                Ok(())
            }
            "move_to" => {
                // 三.1 movement: walk the unit hex-by-hex toward `target`. Each hop's duration is
                // the terrain/slope speed (vehicles, mechanics::step_time_seconds) or the 高差
                // speed (infantry); entry is gated by 堆叠 (三.4) and impassability. Exactly one
                // MoveArrive is in flight, tagged with the unit's move generation so a follow-up
                // order cleanly supersedes the rest of the path (no teleporting a re-tasked unit).
                let id = unit_id(cmd)?;
                self.require_own_unit(side, &id)?;
                self.reject_if_mounted(&id)?;
                self.reject_if_busy(&id, t)?;
                let target = parse_axial(cmd.get("target"))
                    .ok_or_else(|| EngineError::Command("move_to needs target {q,r}".into()))?;
                if self.map.cell(&target).is_none() {
                    return Err(EngineError::Command("move_to target is off-map".into()));
                }
                // A move to the hex the unit already occupies is a no-op: do nothing BEFORE touching
                // any generation. Otherwise it would needlessly draw a 三.15d punishment shot, turn off
                // a 雷达, reset the 三.1.4 疲劳恢复 clock, and re-arm a fresh `FatigueDecay` (leaving the
                // prior stale tick queued) — none of which a non-move should cause.
                if self.state.units.get(&id).map(|u| u.pos) == Some(target) {
                    return Ok(());
                }
                // 三.19e: a 炮兵校射雷达 ordered to 机动 turns OFF immediately (re-spin-up needed).
                self.radar_on.remove(&id);
                // 三.15d: a unit leaving an active 同格交战 draws a punishment shot from each enemy in
                // the hex; if it is destroyed, the move does not happen.
                if let Some(from) = self.state.units.get(&id).map(|u| u.pos) {
                    if self.same_hex.contains_key(&from) {
                        self.same_hex_punish(&id, from, t);
                        if !self.state.units.get(&id).is_some_and(|u| u.alive) {
                            return Ok(());
                        }
                    }
                }
                // 三.1 per-move speed modes: `mode:"half"` (半速 — e.g. a 车辆 opening a 雷场 通路,
                // 三.21f) and `mode:"charge1"/"charge2"` (一二级冲锋, 三.1.4) make THIS move proceed in
                // that posture with NO 75 s set_mode transition. Like 半速, 冲锋 is a per-MOVE burst, not
                // a standing posture (a unit "in charge" while idle is meaningless and must not bar
                // firing). `normal`/absent = generic motion.
                let move_mode = match cmd.get("mode").and_then(|v| v.as_str()) {
                    None | Some("normal") => None,
                    Some("half") => Some(UnitState::Half),
                    Some("charge1") => Some(UnitState::Charge1),
                    Some("charge2") => Some(UnitState::Charge2),
                    Some(other) => {
                        return Err(EngineError::Command(format!(
                        "move_to mode {other:?} is not a per-move speed (use half/charge1/charge2)"
                    )))
                    }
                };
                // 三.1.4 gating. 冲锋 is an INFANTRY-only burst; reject it for anything else. For
                // infantry, the chosen posture must be permitted at the unit's 活体疲劳: 一级疲劳 bars
                // 二级冲锋 and 二级疲劳 bars ALL movement (even a normal step). Reject up front so the
                // order never silently downgrades or accepts-then-instantly-halts.
                if let Some(u) = self.state.units.get(&id) {
                    let is_charge =
                        matches!(move_mode, Some(UnitState::Charge1 | UnitState::Charge2));
                    if is_charge && u.unit_type != UnitType::Infantry {
                        return Err(EngineError::Command("只有步兵可冲锋 (三.1.4)".into()));
                    }
                    if u.unit_type == UnitType::Infantry {
                        let eff = move_mode.unwrap_or(UnitState::Moving);
                        if !crate::mechanics::infantry_mode_allowed(eff, u.fatigue) {
                            return Err(EngineError::Command(
                                "活体疲劳过高,无法以该姿态机动 (三.1.4)".into(),
                            ));
                        }
                    }
                }
                let gen = self.bump_move_gen(&id);
                // 三.1.4: starting a move interrupts 疲劳恢复 — invalidate any queued FatigueDecay (it is
                // re-armed when the unit settles).
                self.bump_fatigue_gen(&id);
                if let Some(u) = self.state.units.get_mut(&id) {
                    // 行军 (三.1.3) overrides any per-move mode: a marching 车辆 keeps marching (firing
                    // stays barred, 三.1.3e) and IGNORES mode:"half"/"charge*" — it must 停止行军 first
                    // (set_mode), so a per-move mode can't bypass the road rules. Otherwise the chosen
                    // per-move posture (Half / Charge1 / Charge2) is set for the move (step_ticks keys
                    // its speed off this state), or generic motion. settle_stopped restores Stopped.
                    if u.state != UnitState::March {
                        u.state = move_mode.unwrap_or(UnitState::Moving);
                    }
                }
                self.plan_hop(&id, gen, target, t);
                Ok(())
            }
            "set_mode" => {
                // Rule 三.1: switch movement mode. The five modes are mutually exclusive
                // (a unit holds exactly one `UnitState`), and the switch takes a 75 s
                // transition during which the unit accepts no further movement order.
                let id = unit_id(cmd)?;
                self.require_own_unit(side, &id)?;
                self.reject_if_mounted(&id)?;
                self.reject_if_busy(&id, t)?;
                let mode = cmd
                    .get("mode")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| EngineError::Command("set_mode missing mode".into()))?;
                let to = movement_state_from_mode(mode)?;
                // 三.1 半速 is a per-MOVE speed (`move_to mode:"half"`), not a persistent posture — so it
                // is never reachable via set_mode (a standing Half unit can't fire, which 半速 must not
                // imply). The clean contract: Half exists only for the duration of a half-speed move.
                if to == UnitState::Half {
                    return Err(EngineError::Command(
                        "半速 is a per-move choice — use move_to mode:\"half\", not set_mode"
                            .into(),
                    ));
                }
                // 三.1.4 一二级冲锋 is likewise a per-MOVE burst (`move_to mode:"charge1"/"charge2"`), not
                // a persistent posture — a standing "charge" unit is meaningless and would wrongly bar
                // its fire. So it is never reachable via set_mode.
                if matches!(to, UnitState::Charge1 | UnitState::Charge2) {
                    return Err(EngineError::Command(
                        "一二级冲锋 is a per-move choice — use move_to mode:\"charge1\"/\"charge2\" (三.1.4)"
                            .into(),
                    ));
                }
                // 三.3e: a 被压制 unit cannot begin a 掩蔽 transition.
                if to == UnitState::Cover && self.is_suppressed(&id, t) {
                    return Err(EngineError::Command(
                        "cannot take cover while suppressed (三.3e)".into(),
                    ));
                }
                // 三.1.3 行军 is a 车辆 road mode with extra entry preconditions and a longer transition.
                let dur = if to == UnitState::March {
                    let unit = self.state.units.get(&id);
                    let utype = unit.map(|u| u.unit_type);
                    let upos = unit.map(|u| u.pos);
                    // Only a (non-炮兵) 车辆 marches: 人员/空中 have no 行军 mode, and 炮兵不能机动 (三.1.2f).
                    if !matches!(utype, Some(ty) if crate::mechanics::is_vehicle(ty) && ty != UnitType::Artillery)
                    {
                        return Err(EngineError::Command("only a 车辆 may 行军 (三.1.3)".into()));
                    }
                    // 三.1.3a/b: the 车辆 must be on a road hex whose kind has a configured 行军 speed.
                    // This rejects 铁路 / any road kind for which `march_step_time_seconds` is None —
                    // entering 行军 there would trap the unit (no valid hop speed, so it can't move).
                    let marchable = upos
                        .and_then(|p| self.map.cell(&p))
                        .and_then(|c| c.road.as_ref())
                        .and_then(|r| {
                            crate::mechanics::march_step_time_seconds(&self.rules, &r.kind)
                        })
                        .is_some_and(|s| s > 0.0);
                    if !marchable {
                        return Err(EngineError::Command(
                            "must be on a marchable road hex to 行军 (三.1.3a/b)".into(),
                        ));
                    }
                    // 三.1.3c + 三.1.3f: the 车辆 locks its weapon first (75 s) then converts to 行军
                    // (75 s) — a 150 s entry cost.
                    secs_to_ticks(self.require_timing("weapon_lock")?).saturating_add(
                        secs_to_ticks(self.require_timing("move_state_transition")?),
                    )
                } else {
                    secs_to_ticks(self.require_timing("move_state_transition")?)
                };
                let done = t.saturating_add(dur);
                if let Some(u) = self.state.units.get_mut(&id) {
                    u.busy_until = done;
                    // 三.1.3c: commit to 行军 IMMEDIATELY (not at completion) so the unit cannot
                    // fire/引导射击/发射巡飞弹 during the 150 s lock+transition window — its weapon is
                    // already locking. It still cannot MOVE until free (busy_until); TransitionComplete
                    // then just clears the busy flag (it re-sets the same March state, idempotently).
                    if to == UnitState::March {
                        u.state = UnitState::March;
                    }
                }
                // Tag a 掩蔽 transition with a fresh generation so a 坦克 firing (三.3b) or a
                // 压制 (三.3f) can interrupt it before it completes.
                let cover_gen = if to == UnitState::Cover {
                    // wrapping_add never panics; 2^64 cover transitions is unreachable. A new
                    // transition can only start once the unit is free (reject_if_busy above), so a
                    // stale cover entry never coexists with a fresh order.
                    self.cover_gen_ctr = self.cover_gen_ctr.wrapping_add(1);
                    let g = self.cover_gen_ctr;
                    self.cover_transition.insert(id.clone(), g);
                    Some(g)
                } else {
                    None
                };
                self.schedule(
                    done,
                    Event::TransitionComplete {
                        unit: id,
                        to,
                        cover_gen,
                    },
                );
                Ok(())
            }
            "fire_direct" => {
                // 三.8 direct fire: check conditions + resolve via combat::direct_fire, apply the
                // outcome to the target, then put the weapon on a 75 s cooldown.
                let id = unit_id(cmd)?;
                self.require_own_unit(side, &id)?;
                self.reject_if_mounted(&id)?; // 三.2: a 被载 unit fires through its vehicle, not itself
                                              // A 被压制 unit cannot fire. The posture gate also bars the Suppressed posture, but a
                                              // unit 压制 while in 掩蔽 keeps the Cover posture (p05-g) and we re-posture Cover to
                                              // Stopped for adjudication below — so the 压制 timer must be checked explicitly here.
                if self.is_suppressed(&id, t) {
                    return Err(EngineError::Command("cannot fire while suppressed".into()));
                }
                let weapon = cmd
                    .get("weapon")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| EngineError::Command("fire_direct missing weapon".into()))?
                    .to_string();
                // 三.20.1c/2c: a `targetFacility` aims at a 工事 (the structure is the target; its
                // 全程隐蔽 garrison inherits the result per 战果继承). A `targetUnit` aims at a unit.
                if let Some(tf) = cmd.get("targetFacility").and_then(|v| v.as_str()) {
                    let tf = tf.to_string();
                    return self.fire_direct_at_facility(side, &id, &weapon, &tf, t);
                }
                let target_id = cmd
                    .get("targetUnit")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| EngineError::Command("fire_direct missing targetUnit".into()))?
                    .to_string();
                // Read-only copies for the adjudication (released before we mutate state). A
                // generic error for a missing/own/dead target avoids leaking enemy ids (rule #5).
                let mut shooter = match self.state.units.get(&id) {
                    Some(u) => u.clone(),
                    None => return Err(EngineError::Command("no controllable unit".into())),
                };
                // 三.3c: firing leaves 掩蔽. Adjudicate as if already Stopped so the posture gate
                // passes; the real state is only changed once the shot actually resolves (a
                // rejected attempt is not a shot, so it must not break cover).
                let was_in_cover = shooter.state == UnitState::Cover;
                if was_in_cover {
                    shooter.state = UnitState::Stopped;
                }
                let target = match self.state.units.get(&target_id) {
                    // A 被载 enemy rides inside its carrier and is not independently targetable —
                    // fold it into the SAME generic error as a missing/own/dead id so its embarked
                    // existence cannot be probed (三.2 + rule #5). Engage the carrier instead.
                    Some(u)
                        if u.side != side
                            && u.alive
                            && u.is_on_board()
                            && crate::mechanics::can_be_targeted(u.unit_type) =>
                    {
                        u.clone()
                    }
                    _ => return Err(EngineError::Command("invalid fire target".into())),
                };
                // 三.15c: an outside unit cannot 直瞄 INTO an active 同格交战 hex (only a unit inside the
                // engagement engages its co-occupants; 间瞄 against the hex is unaffected).
                if self.same_hex.contains_key(&target.pos) && shooter.pos != target.pos {
                    return Err(EngineError::Command(
                        "cannot fire into a 同格交战 (三.15c)".into(),
                    ));
                }
                match combat::direct_fire(
                    &self.tables,
                    &self.map,
                    &mut self.rng,
                    &self.rules,
                    &shooter,
                    &weapon,
                    &target,
                ) {
                    FireOutcome::Rejected(reason) => {
                        // "target not observed" would confirm that a real-but-unobserved enemy
                        // exists at that id; collapse it into the SAME generic error as a
                        // missing/own/dead target so it can't be used to probe the enemy order
                        // of battle (rule #5). Other reasons describe the firer's own unit or an
                        // already-observed target, so they are safe to surface to the firer.
                        let leaks = reason == "target not observed";
                        let msg = if leaks { "invalid fire target" } else { reason };
                        Err(EngineError::Command(msg.into()))
                    }
                    FireOutcome::Resolved(outcome) => {
                        // The shot was actually taken, so it leaves 掩蔽 (三.3c) or, for a 坦克
                        // firing mid-掩蔽转换, interrupts that transition (三.3b).
                        if was_in_cover {
                            if let Some(u) = self.state.units.get_mut(&id) {
                                if u.state == UnitState::Cover {
                                    u.state = UnitState::Stopped;
                                }
                            }
                        }
                        if shooter.unit_type == UnitType::Tank {
                            self.interrupt_cover(&id); // no-op unless mid-cover-transition
                        }
                        self.apply_direct_fire(&target_id, outcome, t)?;
                        let cd = secs_to_ticks(self.require_timing("weapon_cooldown_direct")?);
                        if let Some(u) = self.state.units.get_mut(&id) {
                            u.weapon_state = WeaponState::Cooling;
                        }
                        self.schedule(t.saturating_add(cd), Event::WeaponReady { unit: id });
                        Ok(())
                    }
                }
            }
            "aa_fire" => {
                // 三.18 防空射击: a 防空算子 fires at an OBSERVED air target via 流水线 D
                // (combat::air_defense), gated by observation (三.18e, rule #5), 射速 (one shot per
                // air.aa[type].interval), and 弹药 (air.aa[type].max_shots, 三.18g).
                let id = unit_id(cmd)?;
                self.require_own_unit(side, &id)?;
                self.reject_if_mounted(&id)?;
                if self.is_suppressed(&id, t) {
                    return Err(EngineError::Command("cannot fire while suppressed".into()));
                }
                let shooter = match self.state.units.get(&id) {
                    Some(u) => u.clone(),
                    None => return Err(EngineError::Command("no controllable unit".into())),
                };
                // Per-type AA weapon + 射速/弹药 (rules-as-data, air.aa keyed by unit-type).
                let Some(aa) = self
                    .rules
                    .air
                    .get("aa")
                    .and_then(|m| m.get(crate::mechanics::unit_type_key(shooter.unit_type)))
                else {
                    return Err(EngineError::Command(
                        "unit is not a 防空算子 (三.18)".into(),
                    ));
                };
                // Strict parse — a malformed air.aa block is a rules error, not a silent 0 (which
                // would masquerade as "out of shots" / "no cooldown").
                let weapon = aa
                    .get("weapon")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| EngineError::Rules("air.aa missing weapon".into()))?
                    .to_string();
                let max_shots = aa
                    .get("max_shots")
                    .and_then(|v| v.as_u64())
                    .ok_or_else(|| EngineError::Rules("air.aa missing max_shots".into()))?
                    as u32;
                let interval = aa
                    .get("interval")
                    .and_then(|v| v.as_f64())
                    .ok_or_else(|| EngineError::Rules("air.aa missing interval".into()))?;
                // 三.18g 弹药 cap.
                if *self.aa_shots.get(&id).unwrap_or(&0) >= max_shots {
                    return Err(EngineError::Command(
                        "防空算子 out of shots (三.18g)".into(),
                    ));
                }
                // 三.18 射速: one shot per interval — the weapon must be deployed and not cooling.
                if shooter.weapon_state != WeaponState::Deployed {
                    return Err(EngineError::Command(
                        "防空武器 still cooling (三.18)".into(),
                    ));
                }
                self.reject_if_busy(&id, t)?;
                let target_id = cmd
                    .get("targetUnit")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| EngineError::Command("aa_fire missing targetUnit".into()))?
                    .to_string();
                let target = match self.state.units.get(&target_id) {
                    Some(u)
                        if u.side != side
                            && u.alive
                            && u.is_on_board()
                            && crate::mechanics::can_be_targeted(u.unit_type) =>
                    {
                        u.clone()
                    }
                    _ => return Err(EngineError::Command("invalid fire target".into())),
                };
                // 三.18e observation gate (rule #5): an unobserved air target collapses into the
                // generic error so its existence cannot be probed (自带雷达 50格 / 无人机 25格 /
                // 巡飞弹 不可观察 all live in rules.observation).
                if !crate::mechanics::can_observe(&self.rules, &self.map, &shooter, &target) {
                    return Err(EngineError::Command("invalid fire target".into()));
                }
                // 三.1.3e: a 行军 防空算子 cannot fire. Placed AFTER the observation gate so probing a
                // hidden air target still yields the generic "invalid fire target" (unlike direct/
                // guided fire, AA has no Stopped gate — it may fire on the move — so March needs an
                // explicit ban here). The target is already observed, so naming 行军 leaks nothing.
                self.reject_if_marching(&id)?;
                // 三.18: AA engages AIR targets only (the target is already observed by this side, so
                // surfacing a ground-target rejection leaks nothing).
                if !crate::mechanics::is_air(target.unit_type) {
                    return Err(EngineError::Command(
                        "防空武器 only engages air targets (三.18)".into(),
                    ));
                }
                let distance = shooter.pos.distance(&target.pos);
                match combat::air_defense(
                    &self.tables,
                    &mut self.rng,
                    &weapon,
                    shooter.teams,
                    distance,
                ) {
                    None => Err(EngineError::Command(
                        "target out of AA range (三.18)".into(),
                    )),
                    Some(outcome) => {
                        // 三.18/附3.2: 歼灭 → the air target is destroyed (routes through the shared
                        // death path); 无效 → no change. The shot is spent + the weapon cools for
                        // `interval` regardless of result.
                        if outcome.destroyed {
                            self.apply_direct_fire(&target_id, Outcome::Kill, t)?;
                        }
                        *self.aa_shots.entry(id.clone()).or_insert(0) += 1;
                        // 三.18 射速: only the WEAPON cools for `interval` (like direct fire) — the
                        // rate gate is `weapon_state`, NOT a busy lock, so the 防空算子 may still
                        // move/switch mode between shots (三.18 specifies an interval, not a
                        // transition lock).
                        let cd = secs_to_ticks(interval);
                        if let Some(u) = self.state.units.get_mut(&id) {
                            u.weapon_state = WeaponState::Cooling;
                        }
                        self.schedule(t.saturating_add(cd), Event::WeaponReady { unit: id });
                        Ok(())
                    }
                }
            }
            "guide_fire" => {
                // 三.14 引导射击: a 引导算子 that observes the target lets a 重型导弹 战车 fire on it
                // without the vehicle's own 通视. 三.14b 隶属约束: a 步兵/无人战车 may guide ONLY the
                // one manned vehicle it is 隶属 to; a 无人机 may guide any 本方 missile vehicle.
                let id = unit_id(cmd)?; // the missile vehicle
                self.require_own_unit(side, &id)?;
                self.reject_if_mounted(&id)?;
                let guide = cmd
                    .get("guideId")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| EngineError::Command("guide_fire missing guideId".into()))?
                    .to_string();
                self.require_own_unit(side, &guide)?;
                self.reject_if_in_facility(&guide)?; // 三.20: a 全程隐蔽 garrison cannot observe/guide out
                self.reject_if_busy(&guide, t)?; // 三.14f: the guide is in its 75 s prep
                let weapon = cmd
                    .get("weapon")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| EngineError::Command("guide_fire missing weapon".into()))?
                    .to_string();
                let target_id = cmd
                    .get("targetUnit")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| EngineError::Command("guide_fire missing targetUnit".into()))?
                    .to_string();
                let guide_u = match self.state.units.get(&guide) {
                    Some(u) => u.clone(),
                    None => return Err(EngineError::Command("no controllable unit".into())),
                };
                if !combat::can_guide(guide_u.unit_type) {
                    return Err(EngineError::Command(
                        "this unit cannot guide (三.14a)".into(),
                    ));
                }
                // 三.13c/三.14b: a 步兵/无人战车 guides only its 隶属 vehicle, and a 无人战车 must have
                // 下车 (a still-carried UGV acts through its carrier, not on its own — 三.2/三.13c).
                if guide_u.unit_type != UnitType::Uav {
                    if guide_u.carried_by.is_some() {
                        return Err(EngineError::Command(
                            "a 被载 guide must dismount first (三.13c)".into(),
                        ));
                    }
                    // 三.13c: a 无人战车 guides only "在停止状态下"; a non-Stopped posture (掩蔽/半速/正常
                    // 待机) does not qualify. (步兵 are governed by 三.14d's not-moving test in `guide_unfit`.)
                    if guide_u.unit_type == UnitType::Ugv && guide_u.state != UnitState::Stopped {
                        return Err(EngineError::Command(
                            "a 无人战车 must be stopped to guide (三.13c)".into(),
                        ));
                    }
                    if guide_u.affiliated_to.as_deref() != Some(id.as_str()) {
                        return Err(EngineError::Command(
                            "步兵/无人战车 may only guide its 隶属 vehicle (三.14b)".into(),
                        ));
                    }
                }
                if self.is_suppressed(&guide, t) {
                    return Err(EngineError::Command("guide is suppressed (三.14d)".into()));
                }
                let vehicle_u = match self.state.units.get(&id) {
                    Some(u) => u.clone(),
                    None => return Err(EngineError::Command("no controllable unit".into())),
                };
                let target = match self.state.units.get(&target_id) {
                    Some(u)
                        if u.side != side
                            && u.alive
                            && u.is_on_board()
                            && crate::mechanics::can_be_targeted(u.unit_type) =>
                    {
                        u.clone()
                    }
                    _ => return Err(EngineError::Command("invalid fire target".into())),
                };
                match combat::guided_fire(
                    &self.tables,
                    &self.map,
                    &mut self.rng,
                    &self.rules,
                    &guide_u,
                    &vehicle_u,
                    &weapon,
                    &target,
                ) {
                    FireOutcome::Rejected(reason) => {
                        // The guide-can't-observe reason would confirm a hidden enemy exists at the
                        // id, so collapse it into the generic error (rule #5); other reasons are
                        // about the firer's own units and safe to surface.
                        let leaks = reason == "the guide does not observe the target (三.14a.i)";
                        let msg = if leaks { "invalid fire target" } else { reason };
                        Err(EngineError::Command(msg.into()))
                    }
                    FireOutcome::Resolved(outcome) => {
                        self.apply_direct_fire(&target_id, outcome, t)?;
                        // 三.14f: both 引导算子 and 被引导算子 need a 75 s prep before acting again.
                        let prep = secs_to_ticks(self.require_timing("weapon_cooldown_direct")?);
                        let ready = t.saturating_add(prep);
                        for who in [&id, &guide] {
                            if let Some(u) = self.state.units.get_mut(who) {
                                u.weapon_state = WeaponState::Cooling;
                                u.busy_until = ready;
                            }
                            self.schedule(ready, Event::WeaponReady { unit: who.clone() });
                        }
                        Ok(())
                    }
                }
            }
            "capture" => {
                // 三.5 夺控: a 地面 non-炮兵 unit standing on a 夺控点中心 takes it iff that hex and
                // its 6 neighbours hold no enemy ground unit. 三.5b/d: no wait time and it works
                // even mid-transition or 压制, so there is deliberately NO busy/state guard.
                // Adjudicated on the TRUE board (三.5a is defined over real positions); the
                // contested-rejection is kept generic — no id/count/hex — so it leaks at most the
                // single rule-mandated bit "an enemy is in the zone", never a hidden unit's
                // position (rule #5). The capturer's own-position reason is safe to surface.
                let id = unit_id(cmd)?;
                self.require_own_unit(side, &id)?;
                self.reject_if_mounted(&id)?; // 三.2: a 被载 unit acts through its vehicle, not itself
                let (utype, pos) = match self.state.units.get(&id) {
                    Some(u) => (u.unit_type, u.pos),
                    None => return Err(EngineError::Command("no controllable unit".into())),
                };
                if !crate::mechanics::can_capture(utype) {
                    return Err(EngineError::Command(
                        "air units and artillery cannot capture (三.5c)".into(),
                    ));
                }
                let Some(cp) = self.scenario.objectives.iter().find(|cp| cp.at == pos) else {
                    return Err(EngineError::Command("not on a control point".into()));
                };
                let cp_id = cp.id.clone();
                let center = cp.at;
                let radius = self.rules.capture_radius().ok_or_else(|| {
                    EngineError::Rules("missing control.capture_no_enemy_radius_hexes".into())
                })?;
                if !crate::mechanics::capture_zone_clear(&self.state, center, side, radius) {
                    return Err(EngineError::Command(
                        "control point contested by enemy ground units (三.5a)".into(),
                    ));
                }
                self.state.control.insert(cp_id, Some(side));
                Ok(())
            }
            "radar_on" => {
                // 三.19d: a 炮兵校射雷达 spins up over 75 s while 静止 + 未压制; once 开机, all friendly
                // 间瞄 is adjudicated at ≥ 格内校射 (三.19c).
                let id = unit_id(cmd)?;
                self.require_own_unit(side, &id)?;
                self.reject_if_mounted(&id)?;
                let (utype, state) = match self.state.units.get(&id) {
                    Some(u) => (u.unit_type, u.state),
                    None => return Err(EngineError::Command("no controllable unit".into())),
                };
                if utype != UnitType::RadarVehicle {
                    return Err(EngineError::Command(
                        "that unit is not a 炮兵校射雷达".into(),
                    ));
                }
                if state != UnitState::Stopped {
                    return Err(EngineError::Command(
                        "the radar must be stopped to spin up (三.19d)".into(),
                    ));
                }
                if self.is_suppressed(&id, t) {
                    return Err(EngineError::Command(
                        "the radar is suppressed and cannot spin up (三.19d)".into(),
                    ));
                }
                if self.radar_on.contains(&id) || self.radar_spinup.contains_key(&id) {
                    return Err(EngineError::Command(
                        "the radar is already on or spinning up".into(),
                    ));
                }
                let spin = secs_to_ticks(self.require_timing("radar_spinup")?);
                let done = t.saturating_add(spin);
                self.radar_gen_ctr = self.radar_gen_ctr.wrapping_add(1);
                let gen = self.radar_gen_ctr;
                self.radar_spinup.insert(id.clone(), gen);
                if let Some(u) = self.state.units.get_mut(&id) {
                    u.busy_until = done;
                }
                self.schedule(done, Event::RadarReady { vehicle: id, gen });
                Ok(())
            }
            "switch_altitude" => {
                // 三.16d: a 运输直升机 switches between adjacent altitude states over 75 s.
                let id = unit_id(cmd)?;
                self.require_own_unit(side, &id)?;
                self.reject_if_busy(&id, t)?;
                let utype = match self.state.units.get(&id) {
                    Some(u) => u.unit_type,
                    None => return Err(EngineError::Command("no controllable unit".into())),
                };
                if utype != UnitType::TransportHeli {
                    return Err(EngineError::Command(
                        "only a 运输直升机 has altitude states".into(),
                    ));
                }
                let to = cmd
                    .get("altitude")
                    .and_then(|v| v.as_str())
                    .and_then(HeliAlt::parse)
                    .ok_or_else(|| {
                        EngineError::Command(
                            "switch_altitude needs altitude high/low/very_low".into(),
                        )
                    })?;
                let cur = self.heli_alt(&id);
                if cur == to {
                    return Ok(()); // already at that altitude
                }
                if !cur.adjacent(to) {
                    return Err(EngineError::Command(
                        "can only switch to an adjacent altitude (三.16d)".into(),
                    ));
                }
                let dur = secs_to_ticks(self.require_timing("heli_altitude_switch")?);
                let done = t.saturating_add(dur);
                if let Some(u) = self.state.units.get_mut(&id) {
                    u.busy_until = done;
                }
                self.schedule(done, Event::HeliAltReady { heli: id, to });
                Ok(())
            }
            "mount" => {
                // 三.2 上车: a stopped passenger boards a stopped, co-located carrier over 75 s.
                let id = unit_id(cmd)?;
                self.require_own_unit(side, &id)?;
                let vehicle = cmd
                    .get("vehicleId")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| EngineError::Command("mount missing vehicleId".into()))?
                    .to_string();
                self.require_own_unit(side, &vehicle)?;
                self.begin_mount(&id, &vehicle, MountKind::Mount, t)?;
                Ok(())
            }
            "dismount" => {
                // 三.2 下车: a mounted passenger leaves its (stopped) carrier over 75 s.
                let id = unit_id(cmd)?;
                self.require_own_unit(side, &id)?;
                let vehicle = match self.state.units.get(&id).and_then(|u| u.carried_by.clone()) {
                    Some(v) => v,
                    None => return Err(EngineError::Command("unit is not mounted".into())),
                };
                self.begin_mount(&id, &vehicle, MountKind::Dismount, t)?;
                Ok(())
            }
            "enter_facility" => {
                // 三.20 进入工事: a stopped, co-located 算子 garrisons a 工事 over 75 s, after which it
                // is 全程隐蔽 (三.20.1b/2b/3b). COMMIT 1 supports only 人员隐蔽工事 (三.20.3); 车辆/战斗工事
                // (which need the 12-格 fire model + 战果继承) are not yet wired and are rejected.
                let id = unit_id(cmd)?;
                self.require_own_unit(side, &id)?;
                self.reject_if_mounted(&id)?; // a 被载 unit must dismount first
                self.reject_if_busy(&id, t)?;
                self.reject_if_marching(&id)?;
                if self
                    .state
                    .units
                    .get(&id)
                    .is_some_and(|u| u.inside_facility.is_some())
                {
                    return Err(EngineError::Command("unit is already inside a 工事".into()));
                }
                if self.is_suppressed(&id, t) {
                    return Err(EngineError::Command(
                        "cannot enter a 工事 while suppressed".into(),
                    ));
                }
                let facility_id = cmd
                    .get("facilityId")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| EngineError::Command("enter_facility needs facilityId".into()))?
                    .to_string();
                // rule #5: ANY id this side cannot see/use — nonexistent OR enemy-owned — collapses
                // to ONE generic error, so brute-forcing FAC{idx} ids can never reveal a hidden enemy
                // 工事's existence, kind, or owner. Only own + neutral 工事 yield specific reasons.
                let Some(fac) = self
                    .state
                    .facilities
                    .get(&facility_id)
                    .filter(|f| f.owner.is_none() || f.owner == Some(side))
                else {
                    return Err(EngineError::Command("no usable 工事 with that id".into()));
                };
                let kind = crate::mechanics::FortKind::from_facility_kind(&fac.kind)
                    .ok_or_else(|| EngineError::Command("facility is not a 工事".into()))?;
                let fac_at = fac.at;
                let fac_cap = u32::from(fac.capacity);
                let Some(unit) = self.state.units.get(&id) else {
                    return Err(EngineError::Command("no controllable unit".into()));
                };
                // 三.20c eligibility (人员→人员工事) + 三.20 co-location + stopped.
                if !crate::mechanics::fortification_admits(kind, unit.unit_type) {
                    return Err(EngineError::Command(
                        "this 算子 cannot garrison that 工事 (三.20c)".into(),
                    ));
                }
                if unit.pos != fac_at {
                    return Err(EngineError::Command(
                        "must be on the 工事's hex to enter (三.20)".into(),
                    ));
                }
                if unit.state != UnitState::Stopped {
                    return Err(EngineError::Command(
                        "must be stopped to enter a 工事 (三.20)".into(),
                    ));
                }
                // 三.20b/d capacity in 班: the entering 班数 must fit the remaining capacity.
                let occupied: u32 = self
                    .state
                    .units
                    .values()
                    .filter(|o| {
                        o.alive && o.inside_facility.as_deref() == Some(facility_id.as_str())
                    })
                    .map(|o| u32::from(o.teams))
                    .sum();
                if occupied + u32::from(unit.teams) > fac_cap {
                    return Err(EngineError::Command(
                        "工事 has no room for that many 班 (三.20d)".into(),
                    ));
                }
                let dur = secs_to_ticks(self.require_timing("fortification_transition")?);
                let done = t.saturating_add(dur);
                let gen = self.bump_facility_gen(&id);
                if let Some(u) = self.state.units.get_mut(&id) {
                    u.busy_until = done;
                }
                self.schedule(
                    done,
                    Event::FacilityEnterReady {
                        unit: id,
                        facility: facility_id,
                        gen,
                    },
                );
                Ok(())
            }
            "exit_facility" => {
                // 三.20b/f 离开工事: a garrisoned unit leaves onto the open board over 75 s.
                let id = unit_id(cmd)?;
                self.require_own_unit(side, &id)?;
                self.reject_if_busy(&id, t)?;
                let (upos, uside) = match self.state.units.get(&id) {
                    Some(u) if u.inside_facility.is_some() => (u.pos, u.side),
                    Some(_) => {
                        return Err(EngineError::Command("unit is not inside a 工事".into()))
                    }
                    None => return Err(EngineError::Command("no controllable unit".into())),
                };
                // 三.4: there must be room on the 工事's hex for the unit to emerge onto.
                if !crate::mechanics::stacking_allows_entry(
                    &self.state,
                    uside,
                    upos,
                    &id,
                    self.stacking_cap,
                ) {
                    return Err(EngineError::Command(
                        "no room to exit — the hex stack is full (三.4)".into(),
                    ));
                }
                let dur = secs_to_ticks(self.require_timing("fortification_transition")?);
                let done = t.saturating_add(dur);
                let gen = self.bump_facility_gen(&id);
                if let Some(u) = self.state.units.get_mut(&id) {
                    u.busy_until = done;
                }
                self.schedule(done, Event::FacilityExitReady { unit: id, gen });
                Ok(())
            }
            "lay_mines" => {
                // 三.21c 布雷: a 火箭布雷车, Stopped + unsuppressed, lays one 雷场 hex within range over
                // 75 s, up to max_lays_per_vehicle times. A 压制 / move during the 75 s aborts it.
                let id = unit_id(cmd)?;
                self.require_own_unit(side, &id)?;
                self.reject_if_mounted(&id)?;
                self.reject_if_busy(&id, t)?;
                self.reject_if_marching(&id)?;
                let (utype, upos, ustate) = match self.state.units.get(&id) {
                    Some(u) => (u.unit_type, u.pos, u.state),
                    None => return Err(EngineError::Command("no controllable unit".into())),
                };
                if !crate::mechanics::can_lay_mines(utype) {
                    return Err(EngineError::Command(
                        "only a 火箭布雷车 may 布雷 (三.21b)".into(),
                    ));
                }
                if self.is_suppressed(&id, t) {
                    return Err(EngineError::Command(
                        "cannot 布雷 while suppressed (三.21d)".into(),
                    ));
                }
                if ustate != UnitState::Stopped {
                    return Err(EngineError::Command(
                        "must be stopped to 布雷 (三.21d)".into(),
                    ));
                }
                let target = parse_axial(cmd.get("targetHex")).ok_or_else(|| {
                    EngineError::Command("lay_mines needs targetHex {q,r}".into())
                })?;
                if self.map.cell(&target).is_none() {
                    return Err(EngineError::Command("lay_mines target is off-map".into()));
                }
                if self.state.minefields.contains_key(&target) {
                    return Err(EngineError::Command(
                        "that hex already holds a 雷场 (三.21c)".into(),
                    ));
                }
                let range = self.rules.minefield_lay_range().unwrap_or(0);
                if i64::from(upos.distance(&target)) > range {
                    return Err(EngineError::Command(
                        "target is beyond 布雷 range (三.21c)".into(),
                    ));
                }
                let max_lays =
                    u32::try_from(self.rules.minefield_max_lays().unwrap_or(0).max(0)).unwrap_or(0);
                if self.minelayer_lays.get(&id).copied().unwrap_or(0) >= max_lays {
                    return Err(EngineError::Command(
                        "this 火箭布雷车 has used all its 布雷 (三.21c)".into(),
                    ));
                }
                let dur = secs_to_ticks(self.require_timing("minefield_lay")?);
                let done = t.saturating_add(dur);
                let gen = self.bump_minelayer_gen(&id);
                self.minelay_in_flight.insert(id.clone());
                if let Some(u) = self.state.units.get_mut(&id) {
                    u.busy_until = done;
                }
                self.schedule(
                    done,
                    Event::MineLayReady {
                        unit: id,
                        at: target,
                        gen,
                    },
                );
                Ok(())
            }
            "aggregate" => {
                // 三.17b 聚合: over 75 s, merge a same-type, co-located, own 班 (`targetUnit`) into this
                // one — combined 班数 must not exceed the cap. (Ammo averaging 三.17g is deferred: no
                // per-unit ammo model exists yet, so 聚合 operates on 班数 only.)
                let id = unit_id(cmd)?;
                let other = cmd
                    .get("targetUnit")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| EngineError::Command("aggregate needs targetUnit".into()))?
                    .to_string();
                if other == id {
                    return Err(EngineError::Command(
                        "cannot 聚合 a unit with itself".into(),
                    ));
                }
                let (ut, pos, teams) = self.aggsplit_check(side, &id, t)?;
                let (ut2, pos2, teams2) = self.aggsplit_check(side, &other, t)?;
                if ut != ut2 {
                    return Err(EngineError::Command(
                        "聚合 requires the same unit type (三.17b)".into(),
                    ));
                }
                if pos != pos2 {
                    return Err(EngineError::Command(
                        "聚合 requires both units in the same hex (三.17b)".into(),
                    ));
                }
                // 三.13d × 三.17: the consumed unit leaves the board, so it must not be a 无人战车's
                // 隶属车辆 (a master) — else its UGVs would be orphaned (pointing at a dead master). Resolve
                // the 隶属 first. (Only `other` is consumed; the surviving initiator may keep its UGVs.)
                if self
                    .state
                    .units
                    .values()
                    .any(|u| u.alive && u.affiliated_to.as_deref() == Some(other.as_str()))
                {
                    return Err(EngineError::Command(
                        "cannot 聚合 a unit that a 无人战车 is 隶属 to (三.13d/三.17)".into(),
                    ));
                }
                let cap = u8::try_from(self.rules.max_squads().unwrap_or(0).max(0)).unwrap_or(0);
                if crate::mechanics::aggregate(teams, teams2, 0, 0, cap).is_none() {
                    return Err(EngineError::Command(
                        "聚合 would exceed the 班 cap (三.17b)".into(),
                    ));
                }
                let done = t.saturating_add(secs_to_ticks(self.require_timing("aggregate_split")?));
                let gen = self.bump_aggsplit_gen(&id);
                self.aggsplit_op.insert(
                    id.clone(),
                    AggSplitInTransit {
                        gen,
                        kind: AggSplitKind::Aggregate {
                            other: other.clone(),
                        },
                    },
                );
                if let Some(u) = self.state.units.get_mut(&id) {
                    u.busy_until = done;
                }
                if let Some(u) = self.state.units.get_mut(&other) {
                    u.busy_until = done;
                }
                self.schedule(done, Event::AggSplitComplete { unit: id, gen });
                Ok(())
            }
            "split" => {
                // 三.17c 解聚: over 75 s, fission this 班 into a smaller half (kept on the parent id) plus
                // a freshly-spawned child (`{parent}#{n}`) that takes the other half. Both retain the
                // parent's posture/pos/type/side. (Ammo 三.17g deferred — split is on 班数 only.)
                let id = unit_id(cmd)?;
                let (_ut, pos, teams) = self.aggsplit_check(side, &id, t)?;
                let cap = u8::try_from(self.rules.max_squads().unwrap_or(0).max(0)).unwrap_or(0);
                if crate::mechanics::split_squads(teams, cap).is_none() {
                    return Err(EngineError::Command(
                        "解聚 needs at least 2 班 (三.17c)".into(),
                    ));
                }
                // 三.4: the spawned child becomes a new on-board unit at this hex, so there must be a
                // stacking slot for it (the "#probe" id matches nothing, so the count includes the parent).
                if !crate::mechanics::stacking_allows_entry(
                    &self.state,
                    side,
                    pos,
                    "#probe",
                    self.stacking_cap,
                ) {
                    return Err(EngineError::Command(
                        "no stacking room for the 解聚 child (三.4)".into(),
                    ));
                }
                let done = t.saturating_add(secs_to_ticks(self.require_timing("aggregate_split")?));
                let gen = self.bump_aggsplit_gen(&id);
                self.aggsplit_op.insert(
                    id.clone(),
                    AggSplitInTransit {
                        gen,
                        kind: AggSplitKind::Split,
                    },
                );
                if let Some(u) = self.state.units.get_mut(&id) {
                    u.busy_until = done;
                }
                self.schedule(done, Event::AggSplitComplete { unit: id, gen });
                Ok(())
            }
            "plan_indirect" => {
                // 三.9 间瞄计划: only 炮兵 may plan; the salvo flies 150 s, then adjudicates and
                // detonates for 300 s, with a 300 s cooldown before the next plan.
                let id = unit_id(cmd)?;
                self.require_own_unit(side, &id)?;
                self.reject_if_mounted(&id)?;
                let utype = match self.state.units.get(&id) {
                    Some(u) => u.unit_type,
                    None => return Err(EngineError::Command("no controllable unit".into())),
                };
                if utype != UnitType::Artillery {
                    return Err(EngineError::Command(
                        "间瞄 is artillery-only (三.9.1a)".into(),
                    ));
                }
                if self.is_suppressed(&id, t) {
                    return Err(EngineError::Command(
                        "cannot plan 间瞄 while suppressed".into(),
                    ));
                }
                let target = parse_axial(cmd.get("targetHex")).ok_or_else(|| {
                    EngineError::Command("plan_indirect needs targetHex {q,r}".into())
                })?;
                if self.map.cell(&target).is_none() {
                    return Err(EngineError::Command(
                        "plan_indirect target is off-map".into(),
                    ));
                }
                if self.indirect_cd.get(&id).copied().unwrap_or(0) > t {
                    return Err(EngineError::Command(
                        "artillery is on 间瞄 cooldown (三.9.1c)".into(),
                    ));
                }
                let fly = secs_to_ticks(self.require_timing("indirect_fly")?);
                let cd = secs_to_ticks(self.require_timing("indirect_cooldown")?);
                self.indirect_cd.insert(id.clone(), t.saturating_add(cd));
                self.indirect_plan_ctr = self.indirect_plan_ctr.wrapping_add(1);
                let plan_id = self.indirect_plan_ctr;
                self.indirect_plans.insert(
                    plan_id,
                    IndirectPlan {
                        artillery: id,
                        plan_hex: target,
                        impact: None,
                        base: crate::combat::IndirectBase::NoEffect,
                        gun_count: 0,
                    },
                );
                self.schedule(t.saturating_add(fly), Event::IndirectFly { plan: plan_id });
                Ok(())
            }
            "cancel_indirect" => {
                // 三.9.1d: a still-飞行 (un-adjudicated) plan may be cancelled, clearing the cooldown.
                let id = unit_id(cmd)?;
                self.require_own_unit(side, &id)?;
                let flying: Vec<u64> = self
                    .indirect_plans
                    .iter()
                    .filter(|(_, p)| p.artillery == id && p.impact.is_none())
                    .map(|(pid, _)| *pid)
                    .collect();
                if flying.is_empty() {
                    return Err(EngineError::Command(
                        "no flying 间瞄 plan to cancel (三.9.1d)".into(),
                    ));
                }
                for pid in flying {
                    self.indirect_plans.remove(&pid);
                }
                self.indirect_cd.remove(&id); // 冷却时间清零
                Ok(())
            }
            "launch_loitering" => {
                // 三.10 巡飞弹: a 发射车 launches one loaded 巡飞弹 over 75 s; a vehicle may keep only
                // one 巡飞弹 aloft (三.10d/e). `targetArea` is the loiter area for its recon/flight; the
                // deployed munition flies there via `move_to` and strikes via `strike_loitering` (三.10f).
                let id = unit_id(cmd)?; // the munition
                self.require_own_unit(side, &id)?;
                let (utype, parent) = match self.state.units.get(&id) {
                    Some(u) => (u.unit_type, u.carried_by.clone()),
                    None => return Err(EngineError::Command("no controllable unit".into())),
                };
                if utype != UnitType::LoiteringMunition {
                    return Err(EngineError::Command("that unit is not a 巡飞弹".into()));
                }
                let parent = parent.ok_or_else(|| {
                    EngineError::Command("巡飞弹 is not loaded on a carrier".into())
                })?;
                self.reject_if_marching(&parent)?; // 三.1.3e: a 行军 carrier cannot 发射巡飞弹
                self.reject_if_busy(&id, t)?;
                let target = parse_axial(cmd.get("targetArea")).ok_or_else(|| {
                    EngineError::Command("launch_loitering needs targetArea {q,r}".into())
                })?;
                if self.map.cell(&target).is_none() {
                    return Err(EngineError::Command(
                        "launch_loitering targetArea is off-map".into(),
                    ));
                }
                // 三.10d/e: at most one 巡飞弹 per carrier — counting both already-aloft munitions and
                // a sibling still inside its 75 s 发射 (busy), to close the double-launch race.
                let aloft = self.loitering_parent.values().any(|v| *v == parent);
                let pending = self.state.units.values().any(|u| {
                    u.id != id
                        && u.carried_by.as_deref() == Some(parent.as_str())
                        && u.unit_type == UnitType::LoiteringMunition
                        && u.busy_until > t
                });
                if aloft || pending {
                    return Err(EngineError::Command(
                        "the carrier already has a 巡飞弹 aloft or launching (三.10e)".into(),
                    ));
                }
                // Validate the 巡飞时长 timing here so the launch fails loudly if it is missing,
                // rather than the deploy handler silently self-destructing the munition.
                self.require_timing("loitering_endurance")?;
                let launch = secs_to_ticks(self.require_timing("loitering_launch")?);
                let done = t.saturating_add(launch);
                if let Some(u) = self.state.units.get_mut(&id) {
                    u.busy_until = done;
                }
                self.loitering_target.insert(id.clone(), target);
                self.schedule(done, Event::LoiteringLaunched { munition: id });
                Ok(())
            }
            "strike_loitering" => {
                // 三.10f: a deployed 巡飞弹 strikes a found target, resolved via the 直瞄对车 pipeline A
                // (巡飞弹 attack-level row 附1.2 + the shared vs-vehicle result table). The munition is
                // expended on the strike (one-shot); NO 停止 gate — it strikes while airborne.
                let id = unit_id(cmd)?; // the munition
                self.require_own_unit(side, &id)?;
                // Must be DEPLOYED (launched, independent, alive) — not still loaded on a carrier.
                let deployed = self.loitering_parent.contains_key(&id)
                    && self
                        .state
                        .units
                        .get(&id)
                        .is_some_and(|u| u.alive && u.carried_by.is_none());
                if !deployed {
                    return Err(EngineError::Command(
                        "only a deployed 巡飞弹 may 打击 (三.10f)".into(),
                    ));
                }
                let target_id = cmd
                    .get("targetUnit")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        EngineError::Command("strike_loitering missing targetUnit".into())
                    })?
                    .to_string();
                let weapon = combat::unit_loadout(&self.rules, UnitType::LoiteringMunition)
                    .into_iter()
                    .next()
                    .ok_or_else(|| {
                        EngineError::Rules(
                            "no 巡飞弹 loadout weapon (rules.loadout.loitering_munition)".into(),
                        )
                    })?;
                let munition = match self.state.units.get(&id) {
                    Some(u) => u.clone(),
                    None => return Err(EngineError::Command("no controllable unit".into())),
                };
                // Generic error for a missing/own/dead/off-board/un-targetable target (rule #5: don't
                // leak existence). `can_be_targeted` excludes the 天基侦察算子 (三.22b — a reduced track,
                // never 直瞄-targetable), matching fire_direct/aa_fire/guide_fire.
                let target = match self.state.units.get(&target_id) {
                    Some(u)
                        if u.side != side
                            && u.alive
                            && u.is_on_board()
                            && crate::mechanics::can_be_targeted(u.unit_type) =>
                    {
                        u.clone()
                    }
                    _ => return Err(EngineError::Command("invalid 打击 target".into())),
                };
                // 三.10b 侦察/fog gate: an unobserved target collapses into the SAME generic error so the
                // firer cannot probe a hidden enemy (rule #5). Before range etc. (the aa_fire pattern).
                if !crate::mechanics::can_observe(&self.rules, &self.map, &munition, &target) {
                    return Err(EngineError::Command("invalid 打击 target".into()));
                }
                match combat::loitering_strike(
                    &self.tables,
                    &self.map,
                    &mut self.rng,
                    &munition,
                    &weapon,
                    &target,
                ) {
                    // The target is already observed → a range/engage rejection is safe to surface.
                    FireOutcome::Rejected(reason) => Err(EngineError::Command(reason.into())),
                    FireOutcome::Resolved(outcome) => {
                        self.apply_direct_fire(&target_id, outcome, t)?;
                        // 三.10f kamikaze: the 巡飞弹 is spent on the strike — self-destruct (mirror
                        // LoiteringSelfDestruct; the queued self-destruct then no-ops on alive=false).
                        self.loitering_parent.remove(&id);
                        self.loitering_target.remove(&id);
                        if let Some(u) = self.state.units.get_mut(&id) {
                            u.alive = false;
                            u.teams = 0;
                        }
                        Ok(())
                    }
                }
            }
            other => Err(EngineError::Unimplemented(other.to_string())),
        }
    }

    /// Advance simulation by `dt` ticks, processing all events that come due.
    pub fn step(&mut self, dt: Tick) {
        let target = self.state.clock.saturating_add(dt);
        while self.queue.peek().is_some_and(|top| top.t <= target) {
            if let Some(ev) = self.queue.pop() {
                self.state.clock = ev.t;
                self.handle(ev.event);
            }
        }
        self.state.clock = target;
    }

    /// Jump directly to the next scheduled event (headless fast-forward). Returns its tick.
    pub fn advance_to_next_event(&mut self) -> Option<Tick> {
        let ev = self.queue.pop()?;
        self.state.clock = ev.t;
        let t = ev.t;
        self.handle(ev.event);
        Some(t)
    }

    /// Tick of the next scheduled event, if any. Lets time-ordered drivers (replay,
    /// decision tick) interleave external commands with internal events deterministically.
    pub fn peek_next_event_time(&self) -> Option<Tick> {
        self.queue.peek().map(|top| top.t)
    }

    /// Current logical clock in ticks.
    pub fn clock(&self) -> Tick {
        self.state.clock
    }

    /// Advance the logical clock forward to `t` without processing events. Used by
    /// time-ordered drivers (replay) after draining every event strictly before `t`;
    /// a no-op if `t` is not in the future (the clock never runs backwards).
    pub fn advance_clock_to(&mut self, t: Tick) {
        if t > self.state.clock {
            self.state.clock = t;
        }
    }

    fn handle(&mut self, event: Event) {
        match event {
            Event::TransitionComplete {
                unit,
                to,
                cover_gen,
            } => {
                // A 掩蔽 transition only lands if it wasn't interrupted (三.3b/f): its generation
                // must still be the live one in `cover_transition`. Non-cover transitions always
                // land. Consume the cover entry on a successful landing.
                if let Some(g) = cover_gen {
                    if self.cover_transition.get(&unit) != Some(&g) {
                        return; // interrupted — stale completion
                    }
                    self.cover_transition.remove(&unit);
                }
                if let Some(u) = self.state.units.get_mut(&unit) {
                    u.state = to;
                    u.busy_until = 0;
                }
            }
            Event::WeaponReady { unit } => {
                if let Some(u) = self.state.units.get_mut(&unit) {
                    u.weapon_state = WeaponState::Deployed;
                }
            }
            Event::SuppressEnd { unit } => {
                // Only lift suppression that has actually expired: a re-suppression extends
                // `suppressed_until` and schedules a later SuppressEnd, so this (possibly stale)
                // event must not clear a still-running, longer suppression early.
                let clock = self.state.clock;
                if let Some(u) = self.state.units.get_mut(&unit) {
                    if u.suppressed_until <= clock {
                        if u.state == UnitState::Suppressed {
                            u.state = UnitState::Normal;
                        }
                        u.suppressed_until = 0;
                    }
                }
            }
            Event::MoveArrive {
                unit,
                gen,
                to,
                target,
            } => {
                // Drop a step from a superseded order or for a unit that died/vanished mid-move.
                if self.move_gen.get(&unit).copied() != Some(gen) {
                    return;
                }
                let side = match self.state.units.get(&unit) {
                    Some(u) if u.alive => u.side,
                    _ => return,
                };
                // Re-validate the 三.4 stack AT arrival: another unit may have filled `to` since
                // this hop was planned (two same-tick movers), so entering now could overstack.
                // The (time, seq) heap order makes "who gets the last slot" deterministic; the
                // loser halts at its current hex instead of breaking the cap.
                if !crate::mechanics::stacking_allows_entry(
                    &self.state,
                    side,
                    to,
                    &unit,
                    self.stacking_cap,
                ) {
                    self.settle_stopped(&unit);
                    return;
                }
                // 三.1.3d, re-checked AT arrival (like stacking above): a stopped/non-marching blocker
                // may have entered `to` after this hop was scheduled, so a marching column must halt
                // before stepping onto it rather than only at planning time.
                if self
                    .state
                    .units
                    .get(&unit)
                    .is_some_and(|u| u.state == UnitState::March)
                    && !crate::mechanics::march_allows_entry(&self.state, to)
                {
                    self.settle_stopped(&unit);
                    return;
                }
                let prev = self.state.units.get(&unit).map(|u| u.pos);
                if let Some(u) = self.state.units.get_mut(&unit) {
                    u.pos = to;
                }
                // 三.1.4 活体疲劳: entering this hex while 冲锋ing accrues fatigue (一级冲锋 / 二级冲锋
                // each add `fatigue_per_hex`). The next plan_hop re-checks the mode against the new
                // 疲劳 and halts the burst once 一/二级疲劳 is reached. (Zero for any non-冲锋 posture.)
                let dfat = self
                    .state
                    .units
                    .get(&unit)
                    .map(|u| crate::mechanics::infantry_fatigue_per_hex(&self.rules, u.state))
                    .unwrap_or(0);
                if dfat != 0 {
                    if let Some(u) = self.state.units.get_mut(&unit) {
                        u.fatigue = u.fatigue.saturating_add(dfat);
                    }
                }
                // 三.15a: stamp the unit's entry order into its new hex (later = fires later).
                self.entry_seq = self.entry_seq.wrapping_add(1);
                self.same_hex_order.insert(unit.clone(), self.entry_seq);
                // 三.15f: leaving its old hex may have ended an engagement there.
                if let Some(prev) = prev {
                    if prev != to {
                        self.end_same_hex_if_resolved(prev);
                    }
                }
                // 三.2: any passengers riding this vehicle move with it.
                let riders: Vec<String> = self
                    .state
                    .units
                    .iter()
                    .filter(|(_, u)| u.carried_by.as_deref() == Some(unit.as_str()))
                    .map(|(id, _)| id.clone())
                    .collect();
                for r in riders {
                    if let Some(p) = self.state.units.get_mut(&r) {
                        p.pos = to;
                    }
                }
                // 三.9.3b: a 炮兵 that 机动 during its own 爆炸 cancels that detonation point.
                let cancelled: Vec<u64> = self
                    .indirect_plans
                    .iter()
                    .filter(|(_, p)| p.artillery == unit && p.impact.is_some())
                    .map(|(pid, _)| *pid)
                    .collect();
                for pid in cancelled {
                    self.indirect_plans.remove(&pid);
                }
                // 三.9.1b: a unit entering an active 爆炸 hex takes one 间瞄裁决.
                let booms: Vec<(combat::IndirectBase, u8)> = self
                    .indirect_plans
                    .values()
                    .filter(|p| p.impact == Some(to))
                    .map(|p| (p.base, p.gun_count))
                    .collect();
                for (base, gun_count) in booms {
                    if let Some(mods) = self.indirect_mods_for(&unit, gun_count) {
                        let outcome = combat::apply_indirect_to_target(
                            &self.tables,
                            &mut self.rng,
                            base,
                            &mods,
                        );
                        let _ = self.apply_direct_fire(&unit, outcome, self.state.clock);
                    }
                }
                // 三.21 雷场 on entry, for a live 地面单位 (air flies over). Three outcomes:
                //  • 三.21f 开辟: a 扫雷车/坦克 entering at 半速 with no friendly lane yet OPENS a 通路 for
                //    its side (single-side, 三.21h) and takes NO damage — it is clearing.
                //  • 三.21i 沿通路: a side WITH a lane here passes free — 人员 any speed, 车辆 at 半速.
                //  • 三.21e otherwise: one 附4 雷场裁决 (mines are 双方均有效, incl. the laying side), the
                //    loss applied to the enterer via the shared 8.6 path (a hit also 压制s survivors).
                let mine_act = self.state.minefields.get(&to).and_then(|m| {
                    self.state.units.get(&unit).filter(|u| u.alive).map(|u| {
                        let side_cleared = m.cleared_by.contains(&u.side);
                        let is_half = u.state == UnitState::Half;
                        let can_clear = crate::mechanics::can_clear_minefield(u.unit_type);
                        let is_ground = crate::mechanics::is_ground(u.unit_type);
                        (
                            u.side,
                            u.unit_type,
                            u.armor,
                            side_cleared,
                            is_half,
                            can_clear,
                            is_ground,
                        )
                    })
                });
                if let Some((uside, ut, armor, side_cleared, is_half, can_clear, is_ground)) =
                    mine_act
                {
                    if !is_ground {
                        // air unit — no interaction
                    } else if can_clear && is_half && !side_cleared {
                        if let Some(m) = self.state.minefields.get_mut(&to) {
                            m.cleared_by.insert(uside); // 三.21f: lane opened, no damage to the sweeper
                        }
                    } else if side_cleared && (combat::minefield_category(ut) == "人员" || is_half)
                    {
                        // 三.21i: passing along a friendly 通路 — 人员 any speed, 车辆 at 半速 — no裁决.
                    } else {
                        let loss = combat::minefield_damage(&self.tables, &mut self.rng, ut, armor);
                        let outcome = if loss > 0 {
                            Outcome::Destroyed(loss.min(u32::from(u8::MAX)) as u8)
                        } else {
                            Outcome::NoEffect
                        };
                        let _ = self.apply_direct_fire(&unit, outcome, self.state.clock);
                    }
                }
                // If the entry 间瞄/雷场 adjudication just KILLED the mover, it does nothing further on
                // this hex — a dead unit must not expose a hidden 工事 garrison (三.20.3d), join a 同格,
                // or plan another hop.
                if !self.state.units.get(&unit).is_some_and(|u| u.alive) {
                    return;
                }
                // 三.20.3d: a 人员隐蔽工事 garrison is exposed the instant an enemy enters its hex — it
                // auto-exits the 工事; the 同格交战 below then engages it, defender-first (it entered
                // the hex earlier, so its lower entry order fires first). Only 隐蔽工事 trigger this
                // (车辆/战斗工事 are revealed at 12 格 instead, a later commit).
                if let Some(mover_side) = self.state.units.get(&unit).map(|u| u.side) {
                    let exposed: Vec<String> = self
                        .state
                        .units
                        .iter()
                        .filter(|(_, u)| {
                            u.alive
                                && u.pos == to
                                && u.side != mover_side
                                && u.inside_facility.as_deref().is_some_and(|fid| {
                                    self.state.facilities.get(fid).is_some_and(|f| {
                                        crate::mechanics::FortKind::from_facility_kind(&f.kind)
                                            == Some(
                                                crate::mechanics::FortKind::PersonnelConcealment,
                                            )
                                    })
                                })
                        })
                        .map(|(id, _)| id.clone())
                        .collect();
                    for occ in exposed {
                        if let Some(u) = self.state.units.get_mut(&occ) {
                            u.inside_facility = None;
                            // 自动离开工事 frees the unit immediately for the 同格.
                            u.busy_until = 0;
                        }
                        // Invalidate any in-flight 进入/离开工事 event (gen bump) so it can't later clear
                        // an unrelated busy_until or undo a re-entry.
                        self.bump_facility_gen(&occ);
                    }
                }
                // 三.15: entering a hex that now holds both sides' 地面 units triggers 同格交战.
                self.maybe_trigger_same_hex(to, self.state.clock);
                // Plan the following hop from the freshly-entered hex (settles Stopped on arrival).
                self.plan_hop(&unit, gen, target, self.state.clock);
            }
            Event::MountComplete { passenger, gen } => {
                // Honour only if this exact transition is still live (三.2d/e interruption clears it).
                let m = match self.mount_op.get(&passenger) {
                    Some(m) if m.gen == gen => m.clone(),
                    _ => return,
                };
                self.mount_op.remove(&passenger);
                let vpos = self.state.units.get(&m.vehicle).map(|u| u.pos);
                // 三.4: if another unit filled the carrier's hex during the 75 s dismount, there may
                // no longer be room on the ground — abort the dismount and keep the passenger aboard.
                if m.kind == MountKind::Dismount {
                    let side = self.state.units.get(&passenger).map(|u| u.side);
                    if let (Some(side), Some(vp)) = (side, vpos) {
                        if !crate::mechanics::stacking_allows_entry(
                            &self.state,
                            side,
                            vp,
                            &passenger,
                            self.stacking_cap,
                        ) {
                            if let Some(u) = self.state.units.get_mut(&passenger) {
                                u.busy_until = 0;
                            }
                            if let Some(v) = self.state.units.get_mut(&m.vehicle) {
                                v.busy_until = 0;
                            }
                            return;
                        }
                    }
                }
                if let Some(u) = self.state.units.get_mut(&passenger) {
                    u.busy_until = 0;
                    // Mount → ride inside the vehicle; Dismount → step off at the vehicle's hex.
                    u.carried_by = match m.kind {
                        MountKind::Mount => Some(m.vehicle.clone()),
                        MountKind::Dismount => None,
                    };
                    if let Some(vp) = vpos {
                        u.pos = vp;
                    }
                }
                if let Some(v) = self.state.units.get_mut(&m.vehicle) {
                    v.busy_until = 0;
                }
            }
            Event::IndirectFly { plan } => {
                // 飞行结束 → 间瞄裁决 + open the 爆炸 window (三.9.3).
                self.adjudicate_indirect(plan, self.state.clock);
            }
            Event::IndirectBoomEnd { plan } => {
                // 爆炸结束 (三.9.1b): the impact hex stops adjudicating entrants.
                self.indirect_plans.remove(&plan);
            }
            Event::LoiteringLaunched { munition } => {
                // 发射完成 (三.10d): the 巡飞弹 leaves its carrier and begins its 1200 s 巡飞.
                if !self
                    .state
                    .units
                    .get(&munition)
                    .map(|u| u.alive)
                    .unwrap_or(false)
                {
                    return;
                }
                let parent = self
                    .state
                    .units
                    .get(&munition)
                    .and_then(|u| u.carried_by.clone());
                // Defensive (三.10e): if the carrier somehow already has one aloft, keep this one
                // loaded rather than overstocking the sky.
                if let Some(p) = &parent {
                    if self.loitering_parent.values().any(|v| v == p) {
                        if let Some(u) = self.state.units.get_mut(&munition) {
                            u.busy_until = 0;
                        }
                        return;
                    }
                }
                let ppos = parent
                    .as_ref()
                    .and_then(|p| self.state.units.get(p))
                    .map(|u| u.pos);
                if let Some(p) = parent {
                    self.loitering_parent.insert(munition.clone(), p);
                }
                if let Some(u) = self.state.units.get_mut(&munition) {
                    u.carried_by = None;
                    u.busy_until = 0;
                    if let Some(pp) = ppos {
                        u.pos = pp;
                    }
                }
                let endurance =
                    secs_to_ticks(self.rules.timing("loitering_endurance").unwrap_or(0.0));
                self.schedule(
                    self.state.clock.saturating_add(endurance),
                    Event::LoiteringSelfDestruct { munition },
                );
            }
            Event::LoiteringSelfDestruct { munition } => {
                // 三.10g: 巡飞时长 expired (or a stale event for an already-dead munition).
                self.loitering_parent.remove(&munition);
                self.loitering_target.remove(&munition);
                if let Some(u) = self.state.units.get_mut(&munition) {
                    if u.alive {
                        u.alive = false;
                        u.teams = 0;
                    }
                }
            }
            Event::HeliAltReady { heli, to } => {
                // 三.16d: the 运输直升机 settles at its new altitude and is free again.
                if let Some(u) = self.state.units.get_mut(&heli) {
                    u.heli_alt = to;
                    u.busy_until = 0;
                }
            }
            Event::SameHexEngage { hex, gen } => {
                // 三.15a/b: one engagement round, recurring every 25 s while both sides remain.
                self.run_same_hex_round(hex, gen, self.state.clock);
            }
            Event::RadarReady { vehicle, gen } => {
                // 三.19d: 开机 completes only if this spin-up wasn't interrupted (压制) meanwhile.
                if self.radar_spinup.get(&vehicle) == Some(&gen) {
                    self.radar_spinup.remove(&vehicle);
                    if self.state.units.get(&vehicle).is_some_and(|u| u.alive) {
                        self.radar_on.insert(vehicle.clone());
                    }
                    if let Some(u) = self.state.units.get_mut(&vehicle) {
                        u.busy_until = 0;
                    }
                }
            }
            Event::FacilityEnterReady {
                unit,
                facility,
                gen,
            } => {
                // Drop a stale entry superseded by a later facility order / forced expulsion / 三.20.3d.
                if self.facility_gen.get(&unit).copied() != Some(gen) {
                    return;
                }
                // 三.20b: the unit finishes garrisoning, but the entry is ABORTED (it stays exposed on
                // the open board) if conditions changed during the 75 s — re-checked at completion,
                // deterministically, like the stacking race at MoveArrive:
                //   • dead / already inside / no longer Stopped / 被压制 (it was knocked out of entry);
                //   • an enemy now shares the hex (rule: a garrison cannot slip into 全程隐蔽 while in
                //     contact — it must fight, not vanish);
                //   • a concurrent entrant filled the 工事 (capacity, below).
                let clock = self.state.clock;
                let suppressed = self.is_suppressed(&unit, clock);
                let (alive, free, stopped, upos, uside) = self
                    .state
                    .units
                    .get(&unit)
                    .map(|u| {
                        (
                            u.alive,
                            u.inside_facility.is_none(),
                            u.state == UnitState::Stopped,
                            u.pos,
                            u.side,
                        )
                    })
                    .unwrap_or((
                        false,
                        false,
                        false,
                        crate::hex::Axial::new(0, 0),
                        SideId::Red,
                    ));
                let enemy_in_hex = self
                    .state
                    .units
                    .values()
                    .any(|o| o.alive && o.is_on_board() && o.side != uside && o.pos == upos);
                let valid = alive && free && stopped && !suppressed && !enemy_in_hex;
                let cap = self
                    .state
                    .facilities
                    .get(&facility)
                    .map(|f| u32::from(f.capacity))
                    .unwrap_or(0);
                let teams = self
                    .state
                    .units
                    .get(&unit)
                    .map(|u| u32::from(u.teams))
                    .unwrap_or(0);
                let occupied: u32 = self
                    .state
                    .units
                    .values()
                    .filter(|o| o.alive && o.inside_facility.as_deref() == Some(facility.as_str()))
                    .map(|o| u32::from(o.teams))
                    .sum();
                if valid && occupied + teams <= cap {
                    if let Some(u) = self.state.units.get_mut(&unit) {
                        u.inside_facility = Some(facility);
                    }
                    // 三.20.3d 先裁决守方: stamp the garrison's 同格 entry order NOW (it is in the hex,
                    // just hidden), so when an enemy later enters and exposes it, the defender —
                    // having the EARLIER order — is adjudicated first in the engagement.
                    self.entry_seq = self.entry_seq.wrapping_add(1);
                    self.same_hex_order.insert(unit.clone(), self.entry_seq);
                }
                if let Some(u) = self.state.units.get_mut(&unit) {
                    u.busy_until = 0;
                }
            }
            Event::FacilityExitReady { unit, gen } => {
                // Drop a stale exit superseded by a forced expulsion / 三.20.3d / a fresh facility order
                // (so it can't clear an unrelated busy_until or undo a re-entry).
                if self.facility_gen.get(&unit).copied() != Some(gen) {
                    return;
                }
                // 三.20b/f: the garrison tries to emerge onto the 工事's hex. Re-check 三.4 stacking at
                // completion (like MoveArrive): if arrivals filled the hex during the 75 s, the exit
                // ABORTS and the unit stays inside (it must re-order). On success it keeps the hex,
                // re-stamps its 同格 entry order (latest occupant), and a 同格交战 opens if an enemy
                // shares the hex.
                let info = self.state.units.get(&unit).and_then(|u| {
                    if u.alive && u.inside_facility.is_some() {
                        Some((u.pos, u.side))
                    } else {
                        None
                    }
                });
                match info {
                    Some((pos, uside)) => {
                        let room = crate::mechanics::stacking_allows_entry(
                            &self.state,
                            uside,
                            pos,
                            &unit,
                            self.stacking_cap,
                        );
                        if let Some(u) = self.state.units.get_mut(&unit) {
                            u.busy_until = 0;
                            if room {
                                u.inside_facility = None;
                            }
                        }
                        if room {
                            self.entry_seq = self.entry_seq.wrapping_add(1);
                            self.same_hex_order.insert(unit.clone(), self.entry_seq);
                            self.maybe_trigger_same_hex(pos, self.state.clock);
                        }
                    }
                    None => {
                        // Dead, or already auto-exited by 三.20.3d — just clear the busy flag.
                        if let Some(u) = self.state.units.get_mut(&unit) {
                            u.busy_until = 0;
                        }
                    }
                }
            }
            Event::MineLayReady { unit, at, gen } => {
                // This 布雷 is resolving — it is no longer in flight (so a later 压制 won't re-abort it).
                self.minelay_in_flight.remove(&unit);
                // Drop a 布雷 superseded by a fresh 布雷 order.
                if self.minelayer_gen.get(&unit).copied() != Some(gen) {
                    return;
                }
                // If the unit was re-tasked at this exact completion tick (a move/mode order accepted
                // when busy_until == done set a LATER busy window), that order now owns the unit — this
                // lay is stale: abort WITHOUT clearing its busy_until.
                if self.state.units.get(&unit).map(|u| u.busy_until) != Some(self.state.clock) {
                    return;
                }
                // 三.21d: a 压制 / death during the 75 s aborts the lay (and consumes no 布雷 charge).
                let laid = self
                    .state
                    .units
                    .get(&unit)
                    .is_some_and(|u| u.alive && u.state == UnitState::Stopped)
                    && !self.is_suppressed(&unit, self.state.clock);
                if laid {
                    let owner = self.state.units.get(&unit).map(|u| u.side);
                    // Don't overwrite an existing 雷场 (a concurrent same-hex lay) — keep the first.
                    self.state
                        .minefields
                        .entry(at)
                        .or_insert(crate::types::RuntimeMinefield {
                            at,
                            owner,
                            cleared_by: std::collections::BTreeSet::new(),
                        });
                    *self.minelayer_lays.entry(unit.clone()).or_insert(0) += 1;
                }
                if let Some(u) = self.state.units.get_mut(&unit) {
                    u.busy_until = 0;
                }
            }
            Event::FatigueDecay { unit, gen } => {
                // 三.1.4 疲劳恢复: honour only the live decay tick. A move order (or a fresh arm) bumps
                // the 疲劳 gen, so a tick queued before the unit moved again no-ops — it did not rest the
                // full interval. Firing / 掩蔽 / 行军转换 do NOT reset recovery (they are not movement).
                if self.fatigue_gen.get(&unit).copied() != Some(gen) {
                    return;
                }
                if let Some(u) = self.state.units.get_mut(&unit) {
                    if u.fatigue > 0 {
                        u.fatigue -= 1;
                    }
                }
                // Re-arm for the next −1 while 疲劳 remains (a no-op at 0); the fresh arm supersedes this
                // tick's generation, keeping exactly one decay in flight.
                self.arm_fatigue_decay(&unit);
            }
            Event::AggSplitComplete { unit, gen } => {
                // Honour only the live transition: a 压制/同格 abort (interrupt_aggsplit) or a fresh
                // order bumps aggsplit_gen, so a superseded completion no-ops.
                let kind = match self.aggsplit_op.get(&unit) {
                    Some(o) if o.gen == gen => o.kind.clone(),
                    _ => return,
                };
                self.aggsplit_op.remove(&unit);
                let now = self.state.clock;
                // Same-tick re-task guard (cf. MineLayReady, 三.21): `reject_if_busy` admits a new order
                // at the exact completion tick (busy_until == t), and a move/mount/cover order bumps a
                // DIFFERENT gen — so a participant could be re-tasked here without bumping aggsplit_gen.
                // If a participant no longer carries THIS transition's busy window (== now), that newer
                // order owns it: abort WITHOUT mutating or clearing its state.
                let initiator_owns = self.state.units.get(&unit).map(|u| u.busy_until) == Some(now);
                let other_owns = match &kind {
                    AggSplitKind::Aggregate { other } => {
                        self.state.units.get(other).map(|u| u.busy_until) == Some(now)
                    }
                    AggSplitKind::Split => true,
                };
                if !(initiator_owns && other_owns) {
                    return;
                }
                match kind {
                    AggSplitKind::Aggregate { other } => {
                        // 二阶确定性检查 AT completion (like the facility transition): both must still be
                        // alive, Stopped, co-located, same-type, unsuppressed, and out of 同格 — else the
                        // 聚合 silently aborts (both units just freed).
                        let cap =
                            u8::try_from(self.rules.max_squads().unwrap_or(0).max(0)).unwrap_or(0);
                        let merged =
                            match (self.state.units.get(&unit), self.state.units.get(&other)) {
                                (Some(a), Some(b))
                                    if a.alive
                                        && b.alive
                                        && a.state == UnitState::Stopped
                                        && b.state == UnitState::Stopped
                                        && a.pos == b.pos
                                        && a.unit_type == b.unit_type
                                        && !self.same_hex.contains_key(&a.pos)
                                        && !self.is_suppressed(&unit, now)
                                        && !self.is_suppressed(&other, now) =>
                                {
                                    crate::mechanics::aggregate(a.teams, b.teams, 0, 0, cap)
                                        .map(|(squads, _ammo)| squads)
                                }
                                _ => None,
                            };
                        if let Some(squads) = merged {
                            // The initiator absorbs the班; the consumed unit leaves the board (alive=false,
                            // like a death — stale gen-map entries then no-op via lazy invalidation).
                            if let Some(u) = self.state.units.get_mut(&unit) {
                                u.teams = squads;
                                u.busy_until = 0;
                            }
                            if let Some(o) = self.state.units.get_mut(&other) {
                                o.teams = 0;
                                o.alive = false;
                                o.busy_until = 0;
                            }
                            self.same_hex_order.remove(&other);
                        } else {
                            // Aborted re-check: just release both busy windows.
                            if let Some(u) = self.state.units.get_mut(&unit) {
                                u.busy_until = 0;
                            }
                            if let Some(o) = self.state.units.get_mut(&other) {
                                o.busy_until = 0;
                            }
                        }
                    }
                    AggSplitKind::Split => {
                        // Re-check the parent is still eligible AND there is still a stacking slot for the
                        // child (三.4 may have filled the hex during the 75 s). Snapshot the fields needed
                        // to build the child before mutating.
                        let cap =
                            u8::try_from(self.rules.max_squads().unwrap_or(0).max(0)).unwrap_or(0);
                        let parent = self.state.units.get(&unit).filter(|u| {
                            u.alive
                                && u.state == UnitState::Stopped
                                && !self.same_hex.contains_key(&u.pos)
                        });
                        let plan = parent.and_then(|u| {
                            crate::mechanics::split_squads(u.teams, cap).map(|(keep, child)| {
                                (
                                    u.side,
                                    u.unit_type,
                                    u.armor,
                                    u.pos,
                                    u.facing,
                                    u.affiliated_to.clone(),
                                    keep,
                                    child,
                                )
                            })
                        });
                        match plan {
                            Some((uside, ut, armor, pos, facing, affil, keep, child_teams))
                                if !self.is_suppressed(&unit, now)
                                    && crate::mechanics::stacking_allows_entry(
                                        &self.state,
                                        uside,
                                        pos,
                                        "#probe",
                                        self.stacking_cap,
                                    ) =>
                            {
                                self.spawn_ctr = self.spawn_ctr.wrapping_add(1);
                                let child_id = format!("{unit}#{}", self.spawn_ctr);
                                // spawn_ctr is monotonic and `#` is reserved from scenario ids, so a
                                // collision is unreachable in practice — but guard anyway: a u64 wrap
                                // would otherwise silently overwrite a live unit. On the impossible
                                // collision, abort the 解聚 cleanly (free the parent) rather than corrupt
                                // the roster (hard rule #1: correct by construction, not by infeasibility).
                                if self.state.units.contains_key(&child_id) {
                                    if let Some(u) = self.state.units.get_mut(&unit) {
                                        u.busy_until = 0;
                                    }
                                } else {
                                    let child = RuntimeUnit {
                                        id: child_id.clone(),
                                        side: uside,
                                        unit_type: ut,
                                        armor,
                                        teams: child_teams,
                                        pos,
                                        facing,
                                        state: UnitState::Stopped,
                                        weapon_state: WeaponState::Deployed,
                                        busy_until: 0,
                                        suppressed_until: 0,
                                        alive: true,
                                        carried_by: None,
                                        affiliated_to: affil,
                                        heli_alt: HeliAlt::Low,
                                        inside_facility: None,
                                        fatigue: 0,
                                    };
                                    self.state.units.insert(child_id.clone(), child);
                                    if let Some(u) = self.state.units.get_mut(&unit) {
                                        u.teams = keep;
                                        u.busy_until = 0;
                                    }
                                    // 三.15a 先入序: re-stamp the survivor, then the child (fires later). A
                                    // fresh friendly unit cannot open a 同格 (we gated against one), but
                                    // re-eval is harmless if an enemy is somehow co-located.
                                    self.entry_seq = self.entry_seq.wrapping_add(1);
                                    self.same_hex_order.insert(unit.clone(), self.entry_seq);
                                    self.entry_seq = self.entry_seq.wrapping_add(1);
                                    self.same_hex_order.insert(child_id, self.entry_seq);
                                    self.maybe_trigger_same_hex(pos, now);
                                }
                            }
                            _ => {
                                if let Some(u) = self.state.units.get_mut(&unit) {
                                    u.busy_until = 0;
                                }
                            }
                        }
                    }
                }
            }
            Event::CaptureCheck => {
                // TODO(/add-rule 5): re-evaluate control points (occupant present + no live enemy adjacent).
            }
        }
    }

    /// Fog-of-war observation for one side. Own units in full; enemy units filtered by visibility.
    /// PLACEHOLDER visibility = distance threshold; replace with mechanics::can_observe (/add-rule 7).
    pub fn observe(&self, side: SideId) -> serde_json::Value {
        let own: Vec<_> = self
            .state
            .units_of(side)
            .map(|u| {
                let mut o = serde_json::json!({
                    "id": u.id, "type": unit_type_str(u.unit_type), "at": u.pos,
                    "teams": u.teams, "facing": u.facing, "state": format!("{:?}", u.state).to_lowercase(),
                    "weaponState": format!("{:?}", u.weapon_state).to_lowercase(),
                    "busyUntil": ticks_to_secs(u.busy_until.saturating_sub(self.state.clock).max(0)),
                });
                // `mountedIn` is string-or-absent in the schema — only include it when actually
                // carried (a serialized `null` would be rejected).
                if let Some(carrier) = &u.carried_by {
                    o["mountedIn"] = serde_json::json!(carrier);
                }
                // 三.16: a 运输直升机's own commander needs its current altitude band to decide moves
                // and 索降 (it's mutable gameplay state). Own-side only — altitude is never exposed
                // on an enemy contact, so this leaks nothing (rule #5).
                if u.unit_type == UnitType::TransportHeli {
                    o["altitude"] = serde_json::json!(match u.heli_alt {
                        HeliAlt::High => "high",
                        HeliAlt::Low => "low",
                        HeliAlt::VeryLow => "very_low",
                    });
                }
                // 三.20: a garrisoned unit's own commander sees which 工事 it is inside (own-side
                // gameplay state); a hidden occupant never appears on the enemy's contact list.
                if let Some(fac) = &u.inside_facility {
                    o["insideFacility"] = serde_json::json!(fac);
                }
                // 三.1.4: an infantry's own commander needs its 活体疲劳 level to decide whether it may
                // 冲锋 again (一级疲劳 bars 二级冲锋; 二级疲劳 bars all movement). Own-side gameplay state,
                // omitted when 0; a fatigued enemy never leaks this (rule #5).
                if u.fatigue > 0 {
                    o["fatigue"] = serde_json::json!(u.fatigue);
                }
                o
            })
            .collect();

        let enemy_side = match side {
            SideId::Red => SideId::Blue,
            SideId::Blue => SideId::Red,
        };
        // War fog (三.7, hard rule #5): an enemy unit appears only if SOME own unit can
        // actually observe it (rules.observation distance + 通视 + 掩蔽/地形 reductions);
        // the rest are omitted entirely (no god-view leak). Each shown enemy exposes only
        // id / type / position — never its teams, state, or weapon.
        let directly_seen: std::collections::BTreeSet<String> = self
            .state
            .units_of(enemy_side)
            // 三.2: a 被载 unit rides inside a vehicle and is never independently visible — only the
            // carrier shows. Excluding carried units keeps fog honest (rule #5).
            .filter(|e| e.is_on_board())
            .filter(|e| {
                // 三.20: a 被载 unit and a 隐蔽工事 garrison see nothing out; a 车辆/战斗工事 garrison DOES
                // observe out (it may fire out, 三.20.1b/2b). So an observer is any on-board own unit OR
                // a can-fire-out garrison (rule #5: a 隐蔽 garrison still reveals nothing).
                self.state.units_of(side).any(|o| {
                    (o.is_on_board() || self.garrison_can_fire_out(o))
                        && crate::mechanics::can_observe(&self.rules, &self.map, o, e)
                })
            })
            .map(|e| e.id.clone())
            .collect();
        let mut enemy: Vec<serde_json::Value> = directly_seen
            .iter()
            .filter_map(|id| self.state.units.get(id))
            .map(|e| serde_json::json!({ "id": e.id, "type": unit_type_str(e.unit_type), "at": e.pos }))
            .collect();
        // 三.22c 天基侦察: if this side has a live 天基侦察算子, it ALSO tracks the lowest-id enemy 算子
        // per hex (not 工事-hidden — that exclusion lands with the 工事 engine op). These are a REDUCED
        // track — id/type/position only, never 车班数/弹药/战损 — and are tagged `track:"space"` so the
        // commander knows they are NOT directly observed (三.22d: no 直瞄 — enforced anyway because the
        // fire path gates on can_observe, which space-tracking does not satisfy).
        for id in crate::mechanics::space_recon_reveals(&self.state, side) {
            if directly_seen.contains(&id) {
                continue; // already shown in full-fidelity (well, id/type/at) above
            }
            if let Some(e) = self.state.units.get(&id) {
                enemy.push(serde_json::json!({
                    "id": e.id, "type": unit_type_str(e.unit_type), "at": e.pos, "track": "space"
                }));
            }
        }

        // Objectives carry their CURRENT owner (from live control, not the scenario's static start),
        // and omit `owner`/`priority` when neutral/unset so the shape conforms to the schema (where
        // both are string-or-absent — a serialized `null` would be rejected).
        //
        // 夺控点 control is intentionally PUBLIC to both sides — it is the shared victory-condition
        // "scoreboard" (三.5 / 胜负), not fog-gated enemy info: a capture-based match is unplayable if
        // a side can't tell when an objective is lost and must be contested. This is NOT a rule-#5
        // leak (objective state ≠ an unobserved enemy unit's position/teams, which stay hidden above).
        let objectives: Vec<serde_json::Value> = self
            .scenario
            .objectives
            .iter()
            .map(|cp| {
                let mut o = serde_json::json!({ "id": cp.id, "at": cp.at });
                if let Some(Some(owner)) = self.state.control.get(&cp.id) {
                    o["owner"] = serde_json::json!(match owner {
                        SideId::Red => "red",
                        SideId::Blue => "blue",
                    });
                }
                if let Some(p) = &cp.priority {
                    o["priority"] = serde_json::json!(p);
                }
                o
            })
            .collect();

        // The decision-tick index (schema `tick`): clock seconds quantised by the decision step, so
        // the observation conforms to llm_tools.schema.json on its own (the harness need not patch it).
        let clock_seconds = ticks_to_secs(self.state.clock);
        let dt = self.rules.timing.decision_tick_seconds;
        let tick = if dt > 0.0 {
            // floor: the number of decision steps COMPLETED — a clock between two boundaries reports
            // the current tick, never the next (matches the harness's integer decision counter).
            (clock_seconds / dt).floor() as i64
        } else {
            0
        };
        // 三.20: surface the 工事 this side may use — its own + any unowned (neutral) ones. An enemy
        // 工事 is NOT shown (a 隐蔽工事 is concealed beyond 本格, rule #5); the occupant counts are
        // the side's own garrison info.
        let facilities: Vec<_> = self
            .state
            .facilities
            .values()
            .filter(|f| f.owner.is_none() || f.owner == Some(side))
            .map(|f| {
                // Count only THIS side's garrison (rule #5): a neutral 工事 is shown to both sides, so
                // summing every occupant would leak an enemy garrison's 班数 hiding in it.
                let occupant_squads: u32 = self
                    .state
                    .units
                    .values()
                    .filter(|u| {
                        u.alive
                            && u.side == side
                            && u.inside_facility.as_deref() == Some(f.id.as_str())
                    })
                    .map(|u| u32::from(u.teams))
                    .sum();
                serde_json::json!({
                    "id": f.id, "kind": f.kind, "at": f.at,
                    "capacity": f.capacity, "occupantSquads": occupant_squads,
                })
            })
            .collect();
        // 三.20.1a/2a: an ENEMY 车辆/战斗工事 within 12 格 + 通视 of an own unit is visible as a
        // targetable structure — its id/kind/hex only, NEVER its occupancy (rule #5). A 隐蔽工事 has
        // range 0 so it is never surfaced here (it is revealed only by entering its hex, 三.20.3d).
        let enemy_fortifications: Vec<_> = self
            .state
            .facilities
            .values()
            .filter(|f| f.owner.is_some() && f.owner != Some(side))
            .filter(|f| self.facility_observed_by(f, side))
            .map(|f| serde_json::json!({ "id": f.id, "kind": f.kind, "at": f.at }))
            .collect();
        // 三.21a 雷场观测: a 雷场 is surfaced if it is this side's own OR within `observe_range_hexes`
        // (default 10) + 通视 of an own on-board ground unit. Hex only — never the owner (rule #5); a
        // cleared 通路 is not separately surfaced (the lane is the owner's private state, 三.21h).
        let mine_range = self.rules.minefield_observe_range().unwrap_or(0);
        let minefields: Vec<_> = self
            .state
            .minefields
            .values()
            .filter(|m| {
                m.owner == Some(side)
                    || self.state.units_of(side).any(|o| {
                        o.is_on_board()
                            && crate::mechanics::is_ground(o.unit_type)
                            && i64::from(o.pos.distance(&m.at)) <= mine_range
                            && crate::mechanics::line_of_sight_alt(
                                &self.map,
                                &o.pos,
                                &m.at,
                                crate::mechanics::air_altitude_levels(&self.rules, &self.map, o),
                                0,
                            )
                    })
            })
            .map(|m| serde_json::json!({ "at": m.at }))
            .collect();
        serde_json::json!({
            "tick": tick,
            "clockSeconds": clock_seconds,
            "side": match side { SideId::Red => "red", SideId::Blue => "blue" },
            "ownUnits": own,
            "enemyUnits": enemy,
            "objectives": objectives,
            "facilities": facilities,
            "enemyFortifications": enemy_fortifications,
            "minefields": minefields,
        })
    }

    /// Snapshot the full state as JSON (for the replay viewer).
    pub fn snapshot(&self) -> serde_json::Value {
        serde_json::to_value(&self.state).unwrap_or(serde_json::Value::Null)
    }

    /// Telemetry (BALANCE ANALYSIS only): start recording every direct-fire adjudication resolved on
    /// this thread. Purely observational — it never feeds back into combat, so the sim stays
    /// byte-identical and deterministic (硬规则 #1). Off by default; pair with [`Self::drain_outcome_log`].
    ///
    /// ADMIN/OFFLINE tool, like [`Self::snapshot`] — NOT part of the agent-facing surface. Records carry
    /// no side/unit/position, only `{table, weapon, attackLevel, armor, distance, outcome}` of fires that
    /// already occurred, so they cannot leak the location of un-observed units (硬规则 #5). The buffer is
    /// thread-global, so use one engine per analysis thread; call [`Self::disable_outcome_log`] to reset.
    pub fn enable_outcome_log(&self) {
        crate::combat::outcome_log_enable();
    }

    /// Stop telemetry and discard any buffered records on this thread (explicit off/reset).
    pub fn disable_outcome_log(&self) {
        crate::combat::outcome_log_disable();
    }

    /// Drain recorded direct-fire adjudications as a JSON array of
    /// `{table, weapon, attackLevel, armor, distance, outcome}` (outcome = `destroyed:n`/`suppress`/
    /// `noeffect`/`kill`). Empty if telemetry was not enabled; leaves recording ON so a per-step drain
    /// keeps collecting.
    pub fn drain_outcome_log(&self) -> serde_json::Value {
        let recs = crate::combat::outcome_log_drain();
        let arr: Vec<serde_json::Value> = recs
            .iter()
            .map(|r| {
                let outcome = match r.outcome {
                    Outcome::Destroyed(n) => format!("destroyed:{n}"),
                    Outcome::Kill => "kill".to_string(),
                    Outcome::Suppress => "suppress".to_string(),
                    Outcome::NoEffect => "noeffect".to_string(),
                };
                serde_json::json!({
                    "table": r.table,
                    "weapon": r.weapon,
                    "attackLevel": r.attack_level,
                    "armor": r.armor,
                    "distance": r.distance,
                    "outcome": outcome,
                })
            })
            .collect();
        serde_json::Value::Array(arr)
    }

    /// One uniform die roll exposed for tests/tools that need the engine's RNG.
    pub fn roll_d6(&mut self) -> u32 {
        self.rng.d6()
    }

    pub fn pos_of(&self, unit: &str) -> Option<Axial> {
        self.state.units.get(unit).map(|u| u.pos)
    }
}

fn unit_id(cmd: &serde_json::Value) -> Result<String> {
    cmd.get("unitId")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| EngineError::Command("missing unitId".into()))
}

/// Can `t` carry passengers (三.2)? The troop carriers: 步战车 (IFV), 运输直升机, and the 皮卡
/// utility truck. (Tanks etc. do not carry in this ruleset.)
fn is_carrier(t: UnitType) -> bool {
    matches!(
        t,
        UnitType::Ifv | UnitType::TransportHeli | UnitType::Pickup
    )
}

/// Can `t` ride a vehicle as a passenger (三.2)? Only dismounted man-portable squads — 步兵 and the
/// man-portable 防空导弹班 — not other vehicles.
fn can_be_passenger(t: UnitType) -> bool {
    matches!(t, UnitType::Infantry | UnitType::AaMissileSquad)
}

/// Can `t` provide 间瞄校射 (三.9.2b)? All 本方地面单位 and 无人机 — NOT 直升机 or 巡飞弹.
fn can_spot(t: UnitType) -> bool {
    crate::mechanics::is_ground(t) || matches!(t, UnitType::Uav)
}

/// Parse an axial `{q, r}` object (schemas/llm_tools.schema.json `$defs/axial`).
fn parse_axial(v: Option<&serde_json::Value>) -> Option<Axial> {
    let v = v?;
    let q = i32::try_from(v.get("q")?.as_i64()?).ok()?;
    let r = i32::try_from(v.get("r")?.as_i64()?).ok()?;
    Some(Axial::new(q, r))
}

/// The 0..5 hex direction from `from` to an adjacent `to` (per `Axial::DIRECTIONS`), or `None` when
/// they are not one step apart in a single direction. Used for 三.1.3b 沿路走向 (road connectivity).
fn hex_dir(from: Axial, to: Axial) -> Option<u8> {
    let (dq, dr) = (to.q - from.q, to.r - from.r);
    Axial::DIRECTIONS
        .iter()
        .position(|&(q, r)| q == dq && r == dr)
        .map(|i| i as u8)
}

/// Map a `set_mode`/`move_to` `mode` string (schemas/llm_tools.schema.json vocabulary)
/// to its movement [`UnitState`]. 三.1 movement modes.
fn movement_state_from_mode(mode: &str) -> Result<UnitState> {
    Ok(match mode {
        "normal" => UnitState::Normal,
        "charge1" => UnitState::Charge1,
        "charge2" => UnitState::Charge2,
        "half" => UnitState::Half,
        "march" => UnitState::March,
        "cover" => UnitState::Cover,
        other => {
            return Err(EngineError::Command(format!(
                "unknown movement mode {other:?}"
            )))
        }
    })
}

fn unit_type_str(t: UnitType) -> &'static str {
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

/// Rule 三.7 war fog (T1.5): observe() must show only enemies some own unit can actually
/// observe (in-range AND in 通视), hide the rest, and never leak their details (hard rule #5).
#[cfg(test)]
#[test]
fn fog_of_war() {
    use crate::hex::Axial;
    use crate::types::{HexCell, ScenarioUnit, Side, Sides, Terrain};

    fn hex(q: i32, r: i32, elevation: i32) -> HexCell {
        HexCell {
            q,
            r,
            id: None,
            elevation,
            terrain: Terrain::Open,
            road: None,
        }
    }
    // A flat row r=0 (q=0..=12) for the range cases, plus an r-axis arm (q=0, r=1..=5) with a
    // blocking hill at (0,2) for the LOS case.
    let mut hexes: Vec<HexCell> = (0..=12).map(|q| hex(q, 0, 0)).collect();
    hexes.extend((1..=5).map(|r| hex(0, r, if r == 2 { 3 } else { 0 })));
    let map = Map {
        format: "openstratcore.map".into(),
        version: 1,
        name: "fog".into(),
        elevation_unit_meters: Some(10),
        hexes,
    };

    fn inf(id: &str, q: i32, r: i32) -> ScenarioUnit {
        ScenarioUnit {
            id: id.into(),
            unit_type: UnitType::Infantry,
            armor: None,
            teams: 1,
            at: Axial::new(q, r),
            facing: 0,
            state: None,
            carried_by: None,
            affiliated_to: None,
        }
    }
    let scenario = Scenario {
        format: "openstratcore.scenario".into(),
        version: 1,
        name: "fog".into(),
        map: "fog".into(),
        rules: None,
        time_limit_seconds: None,
        sides: Sides {
            red: Side {
                name: "Red".into(),
                units: vec![inf("R1", 0, 0)], // lone infantry observer; infantry sees infantry at 10
            },
            blue: Side {
                name: "Blue".into(),
                units: vec![
                    inf("B-near", 5, 0),    // dist 5 ≤ 10, flat LOS → visible
                    inf("B-far", 12, 0),    // dist 12 > 10 → hidden (out of range)
                    inf("B-blocked", 0, 5), // dist 5 but the (0,2) hill blocks LOS → hidden
                ],
            },
        },
        objectives: vec![],
        facilities: vec![],
    };
    let rules = Rules::from_json_str(include_str!("../../../config/rules.default.json")).unwrap();
    let e = Engine::new(map, scenario, rules, 1).unwrap();

    let obs = e.observe(SideId::Red);
    let enemies = obs["enemyUnits"].as_array().unwrap();
    let ids: Vec<&str> = enemies.iter().filter_map(|x| x["id"].as_str()).collect();
    assert!(
        ids.contains(&"B-near"),
        "an in-range enemy in LOS must be visible"
    );
    assert!(
        !ids.contains(&"B-far"),
        "an out-of-range enemy must be hidden"
    );
    assert!(
        !ids.contains(&"B-blocked"),
        "an enemy with no line of sight must be hidden"
    );
    // No god-view leak: enemy entries expose only id/type/position.
    for en in enemies {
        assert!(en.get("at").is_some());
        assert!(en.get("teams").is_none(), "enemy view must not leak teams");
        assert!(en.get("state").is_none(), "enemy view must not leak state");
        assert!(
            en.get("weaponState").is_none(),
            "enemy view must not leak weapon state"
        );
    }
    assert_eq!(obs["ownUnits"].as_array().unwrap().len(), 1);
}

/// 三.8 direct fire wired into the engine (T1.8): a deployed tank fires at an observable enemy,
/// the weapon goes on cooldown and recovers after 75 s; a cooling weapon and an invalid target
/// are both rejected.
#[cfg(test)]
#[test]
fn engine_fire_direct() {
    use crate::types::{HexCell, ScenarioUnit, Side, Sides, Terrain};

    let map = Map {
        format: "openstratcore.map".into(),
        version: 1,
        name: "fire".into(),
        elevation_unit_meters: Some(10),
        hexes: (0..=4)
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
    let tank = |id: &str, q: i32| ScenarioUnit {
        id: id.into(),
        unit_type: UnitType::Tank,
        armor: Some(Armor::Medium),
        teams: 4,
        at: Axial::new(q, 0),
        facing: 0,
        state: Some(UnitState::Stopped),
        carried_by: None,
        affiliated_to: None,
    };
    let scenario = Scenario {
        format: "openstratcore.scenario".into(),
        version: 1,
        name: "fire".into(),
        map: "fire".into(),
        rules: None,
        time_limit_seconds: None,
        sides: Sides {
            red: Side {
                name: "Red".into(),
                units: vec![tank("R", 0)],
            },
            blue: Side {
                name: "Blue".into(),
                units: vec![tank("B", 3)],
            },
        },
        objectives: vec![],
        facilities: vec![],
    };
    let rules = Rules::from_json_str(include_str!("../../../config/rules.default.json")).unwrap();
    let mut e = Engine::new(map, scenario, rules, 7).unwrap();

    // R fires 大号直瞄炮 at B (dist 3, observed, in range) → resolves and the weapon cools.
    let cmd = serde_json::json!({ "op": "fire_direct", "unitId": "R", "weapon": "大号直瞄炮", "targetUnit": "B" });
    e.submit(SideId::Red, &cmd, 0).unwrap();
    assert_eq!(e.state.units["R"].weapon_state, WeaponState::Cooling);
    // A second shot while cooling is rejected.
    assert!(e.submit(SideId::Red, &cmd, 100).is_err());
    // After the 75 s direct-fire cooldown the weapon is deployed again.
    e.step(7600);
    assert_eq!(e.state.units["R"].weapon_state, WeaponState::Deployed);
    // Firing at one's own unit is rejected with a generic error (no fog leak).
    let own = serde_json::json!({ "op": "fire_direct", "unitId": "R", "weapon": "大号直瞄炮", "targetUnit": "R" });
    assert!(e.submit(SideId::Red, &own, 8000).is_err());
}

/// 三.8 / rule #5: a fire command at a HIDDEN real enemy must fail with the SAME generic error
/// as a missing/own target, so the firing side cannot probe ids to map the unseen enemy.
#[cfg(test)]
#[test]
fn fire_does_not_leak_hidden_enemy() {
    use crate::types::{HexCell, ScenarioUnit, Side, Sides, Terrain};

    // A hill at q=2 blocks the line of sight from q=0 to the enemy at q=3.
    let map = Map {
        format: "openstratcore.map".into(),
        version: 1,
        name: "fog".into(),
        elevation_unit_meters: Some(10),
        hexes: (0..=3)
            .map(|q| HexCell {
                q,
                r: 0,
                id: None,
                elevation: if q == 2 { 6 } else { 0 },
                terrain: Terrain::Open,
                road: None,
            })
            .collect(),
    };
    let tank = |id: &str, q: i32, state: UnitState| ScenarioUnit {
        id: id.into(),
        unit_type: UnitType::Tank,
        armor: Some(Armor::Medium),
        teams: 4,
        at: Axial::new(q, 0),
        facing: 0,
        state: Some(state),
        carried_by: None,
        affiliated_to: None,
    };
    let rules = Rules::from_json_str(include_str!("../../../config/rules.default.json")).unwrap();
    // Fresh engine per probe (independent shots; also dodges shared borrows). The shooter's own
    // posture is a parameter so we can prove the fog gate runs BEFORE any posture/ready check.
    let make = |shooter_state: UnitState| {
        let scenario = Scenario {
            format: "openstratcore.scenario".into(),
            version: 1,
            name: "fog".into(),
            map: "fog".into(),
            rules: None,
            time_limit_seconds: None,
            sides: Sides {
                red: Side {
                    name: "Red".into(),
                    units: vec![tank("R", 0, shooter_state)],
                },
                blue: Side {
                    name: "Blue".into(),
                    units: vec![tank("B", 3, UnitState::Stopped)],
                },
            },
            objectives: vec![],
            facilities: vec![],
        };
        Engine::new(map.clone(), scenario, rules.clone(), 1).unwrap()
    };
    let fire = |target: &str| serde_json::json!({ "op": "fire_direct", "unitId": "R", "weapon": "大号直瞄炮", "targetUnit": target });
    let msg = |shooter_state, target: &str| match make(shooter_state).submit(
        SideId::Red,
        &fire(target),
        0,
    ) {
        Err(crate::EngineError::Command(m)) => m,
        other => panic!("expected a Command error, got {other:?}"),
    };
    // A hidden real enemy ("B"), a nonexistent id, and an own unit all yield the same message.
    let hidden = msg(UnitState::Stopped, "B");
    assert_eq!(
        hidden,
        msg(UnitState::Stopped, "NOPE"),
        "hidden enemy must look like a missing id"
    );
    assert_eq!(
        hidden,
        msg(UnitState::Stopped, "R"),
        "hidden enemy must look like an own-unit target"
    );
    assert_eq!(hidden, "invalid fire target");
    // Fog gate runs first: a 行军 shooter (which CANNOT fire — 三.1.3e) probing the hidden enemy
    // must still get the generic message, never the posture-specific "must be stopped" — else the
    // differing reason would confirm a real enemy exists at that id.
    assert_eq!(
        msg(UnitState::March, "B"),
        "invalid fire target",
        "posture rejection must not leak a hidden enemy's existence"
    );
}

/// 三.1 movement (T1.10): a unit walks the hex line to its target and settles Stopped; a full
/// 堆叠 (三.4) on the path halts it one hex short. Deterministic — same seed ⇒ same trace.
#[cfg(test)]
#[test]
fn move_to_traverses_and_stacking_halts() {
    use crate::types::{HexCell, ScenarioUnit, Side, Sides, Terrain};

    // Flat open corridor q = 0..=4, r = 0.
    let map = Map {
        format: "openstratcore.map".into(),
        version: 1,
        name: "road".into(),
        elevation_unit_meters: Some(10),
        hexes: (0..=4)
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
    let tank = |id: &str, q: i32| ScenarioUnit {
        id: id.into(),
        unit_type: UnitType::Tank,
        armor: None,
        teams: 2,
        at: Axial::new(q, 0),
        facing: 0,
        state: Some(UnitState::Stopped),
        carried_by: None,
        affiliated_to: None,
    };
    let rules = Rules::from_json_str(include_str!("../../../config/rules.default.json")).unwrap();
    let make = |red: Vec<ScenarioUnit>| {
        let scenario = Scenario {
            format: "openstratcore.scenario".into(),
            version: 1,
            name: "road".into(),
            map: "road".into(),
            rules: None,
            time_limit_seconds: None,
            sides: Sides {
                red: Side {
                    name: "Red".into(),
                    units: red,
                },
                blue: Side {
                    name: "Blue".into(),
                    units: vec![],
                },
            },
            objectives: vec![],
            facilities: vec![],
        };
        Engine::new(map.clone(), scenario, rules.clone(), 1).unwrap()
    };
    let drain = |e: &mut Engine| while e.advance_to_next_event().is_some() {};
    let pos_of = |e: &Engine, id: &str| e.state.units.get(id).unwrap().pos;
    let state_of = |e: &Engine, id: &str| e.state.units.get(id).unwrap().state;
    let go =
        |q, r| serde_json::json!({ "op": "move_to", "unitId": "M", "target": { "q": q, "r": r } });

    // Clear corridor: M drives (0,0) -> (3,0) and halts there, Stopped.
    let mut e = make(vec![tank("M", 0)]);
    e.submit(SideId::Red, &go(3, 0), 0).unwrap();
    drain(&mut e);
    assert_eq!(
        pos_of(&e, "M"),
        Axial::new(3, 0),
        "M should reach its target"
    );
    assert_eq!(state_of(&e, "M"), UnitState::Stopped, "and settle Stopped");

    // A full stack (4 own ground units) on (2,0) blocks the path: M halts at (1,0) (三.4a).
    let mut e = make(vec![
        tank("M", 0),
        tank("F1", 2),
        tank("F2", 2),
        tank("F3", 2),
        tank("F4", 2),
    ]);
    e.submit(SideId::Red, &go(4, 0), 0).unwrap();
    drain(&mut e);
    assert_eq!(
        pos_of(&e, "M"),
        Axial::new(1, 0),
        "M cannot enter the full hex (2,0); it halts one short"
    );
    assert_eq!(state_of(&e, "M"), UnitState::Stopped);

    // Determinism: an identical engine yields the identical final position.
    let mut e2 = make(vec![tank("M", 0)]);
    e2.submit(SideId::Red, &go(3, 0), 0).unwrap();
    drain(&mut e2);
    assert_eq!(pos_of(&e2, "M"), Axial::new(3, 0));

    // Same-tick race: (2,0) holds 3 units (one slot left); A and B both converge on it and would
    // arrive together. The 三.4 cap must still hold — only the first (by heap (time,seq) order)
    // enters; the other halts one hex short. Stacking is re-checked at arrival, not just planning.
    let go_u = |id: &str, q, r| serde_json::json!({ "op": "move_to", "unitId": id, "target": { "q": q, "r": r } });
    let mut e = make(vec![
        tank("A", 0),
        tank("B", 4),
        tank("F1", 2),
        tank("F2", 2),
        tank("F3", 2),
    ]);
    e.submit(SideId::Red, &go_u("A", 2, 0), 0).unwrap();
    e.submit(SideId::Red, &go_u("B", 2, 0), 0).unwrap();
    drain(&mut e);
    let at_hub = ["A", "B", "F1", "F2", "F3"]
        .iter()
        .filter(|id| pos_of(&e, id) == Axial::new(2, 0))
        .count();
    assert_eq!(at_hub, 4, "the 三.4 cap of 4 must never be exceeded");
    // A's order was submitted first (lower seq), so A wins the contested slot; B halts at (3,0).
    assert_eq!(pos_of(&e, "A"), Axial::new(2, 0), "A takes the last slot");
    assert_eq!(pos_of(&e, "B"), Axial::new(3, 0), "B halts one hex short");
}

/// 三.1.3 行军 engine wiring: entry gate (on-road + 车辆-only + 150 s = 武器锁定75 + 转换75), road
/// SPEED (一般公路 60 km/h ⇒ 12 s/hex, ignoring terrain), 离路禁止 (halts where the road ends), 三.1.3d
/// blocking (a stopped unit ahead halts the column), and 三.1.3e (a 行军 carrier cannot 发射巡飞弹).
#[cfg(test)]
#[test]
fn march_road_speed_route_and_blocking() {
    use crate::types::{HexCell, Road, ScenarioUnit, Side, Sides, Terrain};

    // A 一般公路 (60 km/h) corridor on q = 0..=2 connecting east(0)/west(3); q = 3,4 carry NO road.
    let road = Road {
        kind: "normal".into(),
        connects: vec![0, 3],
    };
    let map = Map {
        format: "openstratcore.map".into(),
        version: 1,
        name: "march".into(),
        elevation_unit_meters: Some(10),
        hexes: (0..=4)
            .map(|q| HexCell {
                q,
                r: 0,
                id: None,
                elevation: 0,
                terrain: Terrain::Open,
                road: if q <= 2 { Some(road.clone()) } else { None },
            })
            .collect(),
    };
    let mk = |id: &str, ut: UnitType, q: i32| ScenarioUnit {
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
    let make = |red: Vec<ScenarioUnit>| {
        let scenario = Scenario {
            format: "openstratcore.scenario".into(),
            version: 1,
            name: "march".into(),
            map: "march".into(),
            rules: None,
            time_limit_seconds: None,
            sides: Sides {
                red: Side {
                    name: "Red".into(),
                    units: red,
                },
                blue: Side {
                    name: "Blue".into(),
                    units: vec![],
                },
            },
            objectives: vec![],
            facilities: vec![],
        };
        Engine::new(map.clone(), scenario, rules.clone(), 1).unwrap()
    };
    let drain = |e: &mut Engine| while e.advance_to_next_event().is_some() {};
    let march = serde_json::json!({ "op": "set_mode", "unitId": "M", "mode": "march" });
    let go = |q: i32| serde_json::json!({ "op": "move_to", "unitId": "M", "target": { "q": q, "r": 0 } });

    // (a) 三.1.3b: a 车辆 OFF the road cannot convert to 行军.
    let mut e = make(vec![mk("M", UnitType::Tank, 4)]);
    assert!(
        e.submit(SideId::Red, &march, 0).is_err(),
        "三.1.3b: off-road tank cannot 行军"
    );
    // (b) only a 车辆 marches — 人员 on the road is refused (三.1.3 / 三.1.2f 炮兵不能机动 similarly).
    let mut e = make(vec![mk("M", UnitType::Infantry, 0)]);
    assert!(
        e.submit(SideId::Red, &march, 0).is_err(),
        "三.1.3: 人员 has no 行军 mode"
    );

    // (c) entry: the unit commits to 行军 IMMEDIATELY (三.1.3c — its weapon is locking, so no firing
    // during the window) with a 150 s = 武器锁定 75 s + 转换 75 s busy lock; it cannot move until free.
    let mut e = make(vec![mk("M", UnitType::Tank, 0)]);
    e.submit(SideId::Red, &march, 0).unwrap();
    assert_eq!(
        e.state.units["M"].state,
        UnitState::March,
        "三.1.3c: committed to 行军 at submit, not only at completion"
    );
    assert_eq!(
        e.state.units["M"].busy_until,
        secs_to_ticks(150.0),
        "三.1.3c+f: 行军 entry is a 150 s lock+transition"
    );
    assert!(
        e.submit(SideId::Red, &go(1), 0).is_err(),
        "cannot move during the 行军 entry window (busy)"
    );
    drain(&mut e);
    assert_eq!(e.state.units["M"].state, UnitState::March);

    // (d) SPEED: two hops east on the 一般公路 cost 12 s each (200 m × 3.6 / 60) — NOT the terrain
    // speed — and the unit stays 行军 (halted on the road, not Stopped).
    let t0 = e.clock();
    e.submit(SideId::Red, &go(2), t0).unwrap();
    drain(&mut e);
    assert_eq!(e.state.units["M"].pos, Axial::new(2, 0));
    assert_eq!(
        e.clock() - t0,
        secs_to_ticks(24.0),
        "三.1.3a: 行军 moves at the road's 60 km/h"
    );
    assert_eq!(
        e.state.units["M"].state,
        UnitState::March,
        "a halted marcher stays 行军 (三.1.3f exit is a separate set_mode)"
    );

    // (e) 离路禁止 (三.1.3e): ordered past the road's end, M halts where the road stops (2,0).
    e.submit(SideId::Red, &go(4), e.clock()).unwrap();
    drain(&mut e);
    assert_eq!(
        e.state.units["M"].pos,
        Axial::new(2, 0),
        "三.1.3e: 行军 cannot leave the road; it halts at the road's end"
    );

    // (f) exit 行军 via set_mode (三.1.3f, 75 s) — then a normal move ignores the march rules.
    let normal = serde_json::json!({ "op": "set_mode", "unitId": "M", "mode": "normal" });
    let t1 = e.clock();
    e.submit(SideId::Red, &normal, t1).unwrap();
    assert_eq!(
        e.state.units["M"].busy_until,
        t1.saturating_add(secs_to_ticks(75.0)),
        "三.1.3f: 停止行军 takes 75 s"
    );
    drain(&mut e);
    assert_eq!(e.state.units["M"].state, UnitState::Normal);

    // (g) 三.1.3d: a stopped unit at (2,0) blocks the column — M marching (0,0)->(4,0) halts at (1,0).
    let mut e = make(vec![mk("M", UnitType::Tank, 0), mk("X", UnitType::Tank, 2)]);
    e.submit(SideId::Red, &march, 0).unwrap();
    drain(&mut e);
    e.submit(SideId::Red, &go(4), e.clock()).unwrap();
    drain(&mut e);
    assert_eq!(
        e.state.units["M"].pos,
        Axial::new(1, 0),
        "三.1.3d: a stopped unit ahead halts the march one hex short"
    );

    // (h) 三.1.3: a scenario cannot pre-place a march-INELIGIBLE unit (here 人员) IN 行军 — Engine::new
    // rejects it rather than letting the march mechanics trust a type that can never march.
    let mut starts_marching = mk("M", UnitType::Infantry, 0);
    starts_marching.state = Some(UnitState::March);
    let bad = Scenario {
        format: "openstratcore.scenario".into(),
        version: 1,
        name: "march".into(),
        map: "march".into(),
        rules: None,
        time_limit_seconds: None,
        sides: Sides {
            red: Side {
                name: "Red".into(),
                units: vec![starts_marching],
            },
            blue: Side {
                name: "Blue".into(),
                units: vec![],
            },
        },
        objectives: vec![],
        facilities: vec![],
    };
    assert!(
        Engine::new(map.clone(), bad, rules.clone(), 1).is_err(),
        "三.1.3: a unit may not start in 行军"
    );
}

/// 三.20.3 人员隐蔽工事 engine op (W5): a 人员 garrisons a 隐蔽工事 (75 s) and is 全程隐蔽 — invisible to
/// the enemy, un-targetable, and inert (only exit_facility) — until an enemy enters the hex, which
/// 三.20.3d exposes + auto-exits it and opens a 同格交战. Eligibility (三.20c), capacity (三.20d) and
/// exit (三.20b) are gated; 车辆/战斗工事 are rejected (a later commit).
#[cfg(test)]
#[test]
fn personnel_concealment_works() {
    use crate::types::{Facility, HexCell, ScenarioUnit, Side, Sides, Terrain};

    let map = Map {
        format: "openstratcore.map".into(),
        version: 1,
        name: "fort".into(),
        elevation_unit_meters: Some(10),
        hexes: (0..=4)
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
    let su = |id: &str, ut: UnitType, q: i32, teams: u8| ScenarioUnit {
        id: id.into(),
        unit_type: ut,
        armor: None,
        teams,
        at: Axial::new(q, 0),
        facing: 0,
        state: Some(UnitState::Stopped),
        carried_by: None,
        affiliated_to: None,
    };
    let rules = Rules::from_json_str(include_str!("../../../config/rules.default.json")).unwrap();
    let make = |red: Vec<ScenarioUnit>, cap: Option<u8>| {
        let scenario = Scenario {
            format: "openstratcore.scenario".into(),
            version: 1,
            name: "fort".into(),
            map: "fort".into(),
            rules: None,
            time_limit_seconds: None,
            sides: Sides {
                red: Side {
                    name: "Red".into(),
                    units: red,
                },
                blue: Side {
                    name: "Blue".into(),
                    units: vec![su("BT", UnitType::Tank, 3, 2)],
                },
            },
            objectives: vec![],
            facilities: vec![Facility {
                kind: "works_infantry_hidden".into(),
                at: Axial::new(0, 0),
                owner: Some("red".into()),
                armor: None,
                capacity: cap,
            }],
        };
        Engine::new(map.clone(), scenario, rules.clone(), 1).unwrap()
    };
    let drain = |e: &mut Engine| while e.advance_to_next_event().is_some() {};
    let enter =
        |u: &str| serde_json::json!({ "op": "enter_facility", "unitId": u, "facilityId": "FAC0" });

    // (a) 三.20c eligibility: a 车辆 cannot garrison a 人员隐蔽工事.
    let mut e = make(vec![su("RV", UnitType::Tank, 0, 2)], None);
    assert!(
        e.submit(SideId::Red, &enter("RV"), 0).is_err(),
        "三.20c: a 车辆 cannot enter a 人员工事"
    );

    // (b) 三.20d capacity: more 班 than the 工事 holds is refused.
    let mut e = make(vec![su("RI", UnitType::Infantry, 0, 2)], Some(1));
    assert!(
        e.submit(SideId::Red, &enter("RI"), 0).is_err(),
        "三.20d: 2 班 cannot fit a capacity-1 工事"
    );

    // (c) enter (75 s) → garrisoned + 全程隐蔽: invisible to the enemy, visible (with the 工事) to its
    // own commander.
    let mut e = make(vec![su("RI", UnitType::Infantry, 0, 2)], None);
    e.submit(SideId::Red, &enter("RI"), 0).unwrap();
    assert_eq!(
        e.state.units["RI"].busy_until,
        secs_to_ticks(75.0),
        "三.20b: entry takes 75 s"
    );
    drain(&mut e);
    assert_eq!(e.state.units["RI"].inside_facility.as_deref(), Some("FAC0"));
    let blue = e.observe(SideId::Blue);
    assert!(
        blue["enemyUnits"]
            .as_array()
            .unwrap()
            .iter()
            .all(|u| u["id"] != "RI"),
        "三.20.3b: a 隐蔽 garrison is invisible to the enemy"
    );
    assert!(
        blue["facilities"].as_array().unwrap().is_empty(),
        "rule #5: the enemy's 工事 is not surfaced"
    );
    let red = e.observe(SideId::Red);
    let ri = red["ownUnits"]
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["id"] == "RI")
        .unwrap();
    assert_eq!(ri["insideFacility"], "FAC0");
    assert!(
        red["facilities"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["id"] == "FAC0"),
        "the owner sees its own 工事"
    );

    // (d) the garrison is inert — it cannot move (or fire/board); only exit is allowed.
    assert!(
        e.submit(
            SideId::Red,
            &serde_json::json!({ "op": "move_to", "unitId": "RI", "target": { "q": 1, "r": 0 } }),
            e.clock()
        )
        .is_err(),
        "三.20: a garrison cannot move"
    );

    // (e) 三.20.3c: a hidden garrison cannot be fired upon — the shot reads as the generic fog error
    // (rule #5), never a 工事-specific reason.
    let fire = serde_json::json!({ "op": "fire_direct", "unitId": "BT", "weapon": "大号直瞄炮", "targetUnit": "RI" });
    match e.submit(SideId::Blue, &fire, e.clock()) {
        Err(crate::EngineError::Command(m)) => assert_eq!(m, "invalid fire target"),
        other => panic!("expected invalid fire target, got {other:?}"),
    }

    // (f) 三.20.3d: an enemy entering the 工事 hex exposes the garrison, auto-exits it, and opens a
    // 同格交战 (the only red unit on (0,0) is the garrison, so the engagement is purely the breach).
    let mut e = make(vec![su("RI", UnitType::Infantry, 0, 2)], None);
    e.submit(SideId::Red, &enter("RI"), 0).unwrap();
    drain(&mut e);
    e.submit(
        SideId::Blue,
        &serde_json::json!({ "op": "move_to", "unitId": "BT", "target": { "q": 0, "r": 0 } }),
        e.clock(),
    )
    .unwrap();
    // Advance only until the breach lands (the enemy's arrival exposes the garrison) — stop BEFORE
    // the 同格交战 rounds resolve and tear the engagement down again.
    while e.advance_to_next_event().is_some() && e.state.units["RI"].inside_facility.is_some() {}
    assert!(
        e.state.units["RI"].inside_facility.is_none(),
        "三.20.3d: enemy entry exposes + auto-exits the garrison"
    );
    assert!(
        e.same_hex.contains_key(&Axial::new(0, 0)),
        "三.20.3d: a 同格交战 opens on the breached hex"
    );

    // (g) 三.20b 离开工事: voluntary exit (75 s) returns the unit to the open board.
    let mut e = make(vec![su("RI", UnitType::Infantry, 0, 2)], None);
    e.submit(SideId::Red, &enter("RI"), 0).unwrap();
    drain(&mut e);
    e.submit(
        SideId::Red,
        &serde_json::json!({ "op": "exit_facility", "unitId": "RI" }),
        e.clock(),
    )
    .unwrap();
    assert_eq!(
        e.state.units["RI"].busy_until,
        e.clock().saturating_add(secs_to_ticks(75.0)),
        "三.20b: exit takes 75 s"
    );
    drain(&mut e);
    assert!(e.state.units["RI"].inside_facility.is_none());

    // (h) rule #5: an enemy-owned 工事 and a NONEXISTENT id must give the SAME generic rejection, so
    // an attacker cannot probe FAC{idx} ids to discover a hidden enemy 工事's existence/kind/owner.
    let mut e = make(vec![su("RI", UnitType::Infantry, 0, 2)], None);
    let mut enemy_fac = |fid: &str| match e.submit(
        SideId::Blue, // FAC0 is red-owned; blue's BT may not garrison it
        &serde_json::json!({ "op": "enter_facility", "unitId": "BT", "facilityId": fid }),
        0,
    ) {
        Err(crate::EngineError::Command(m)) => m,
        other => panic!("expected a Command error, got {other:?}"),
    };
    assert_eq!(enemy_fac("FAC0"), "no usable 工事 with that id");
    assert_eq!(
        enemy_fac("FAC0"),
        enemy_fac("NOPE"),
        "an enemy 工事 must be indistinguishable from a nonexistent one (rule #5)"
    );
}

/// 三.20.2 人员战斗工事 engine op (W6): a 人员 garrison is 全程隐蔽 (invisible) yet the 工事 itself is a
/// targetable structure within 12 格 — the enemy fires AT the 工事 (vs its own armour, vehicle
/// pipeline), and the 人员 garrison directly inherits the result (三.20.2e). The garrison may fire/
/// observe OUT while concealed (三.20.2b). Out-of-range / non-战斗 / friendly works fire is fog-safe.
#[cfg(test)]
#[test]
fn personnel_combat_works() {
    use crate::types::{Facility, HexCell, ScenarioUnit, Side, Sides, Terrain};

    let map = Map {
        format: "openstratcore.map".into(),
        version: 1,
        name: "cw".into(),
        elevation_unit_meters: Some(10),
        hexes: (0..=14)
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
    let su = |id: &str, ut: UnitType, q: i32, teams: u8| ScenarioUnit {
        id: id.into(),
        unit_type: ut,
        armor: Some(Armor::Medium),
        teams,
        at: Axial::new(q, 0),
        facing: 0,
        state: Some(UnitState::Stopped),
        carried_by: None,
        affiliated_to: None,
    };
    let rules = Rules::from_json_str(include_str!("../../../config/rules.default.json")).unwrap();
    let make = || {
        let scenario = Scenario {
            format: "openstratcore.scenario".into(),
            version: 1,
            name: "cw".into(),
            map: "cw".into(),
            rules: None,
            time_limit_seconds: None,
            sides: Sides {
                red: Side {
                    name: "Red".into(),
                    units: vec![su("RI", UnitType::Infantry, 0, 3)],
                },
                blue: Side {
                    name: "Blue".into(),
                    units: vec![
                        su("BT", UnitType::Tank, 5, 2),
                        su("BF", UnitType::Tank, 14, 2),
                    ],
                },
            },
            objectives: vec![],
            facilities: vec![Facility {
                kind: "works_infantry_combat".into(),
                at: Axial::new(0, 0),
                owner: Some("red".into()),
                armor: Some("medium".into()),
                capacity: Some(5),
            }],
        };
        Engine::new(map.clone(), scenario, rules.clone(), 1).unwrap()
    };
    let drain = |e: &mut Engine| while e.advance_to_next_event().is_some() {};
    let at_works = |unit: &str| serde_json::json!({ "op": "fire_direct", "unitId": unit, "weapon": "大号直瞄炮", "targetFacility": "FAC0" });

    // (a) a 人员 garrisons the 战斗工事 (75 s) → 全程隐蔽.
    let mut e = make();
    e.submit(
        SideId::Red,
        &serde_json::json!({ "op": "enter_facility", "unitId": "RI", "facilityId": "FAC0" }),
        0,
    )
    .unwrap();
    drain(&mut e);
    assert_eq!(e.state.units["RI"].inside_facility.as_deref(), Some("FAC0"));

    // (a2) 三.20.2b observer-out: the 战斗工事 garrison still OBSERVES out. RI is red's only unit, yet
    // an enemy it sees (BT, within range + 通视) appears in red's fog — a 隐蔽 garrison reveals none.
    assert!(
        e.observe(SideId::Red)["enemyUnits"]
            .as_array()
            .unwrap()
            .iter()
            .any(|u| u["id"] == "BT"),
        "三.20.2b: a 战斗工事 garrison observes out (reveals an enemy it sees)"
    );

    // (b) the garrison is invisible to the enemy, but the 工事 STRUCTURE is surfaced within 12 格
    // (三.20.2a) — id/kind/hex only, never occupancy (rule #5).
    let blue = e.observe(SideId::Blue);
    assert!(
        blue["enemyUnits"]
            .as_array()
            .unwrap()
            .iter()
            .all(|u| u["id"] != "RI"),
        "三.20.2b: the garrison itself is 全程隐蔽"
    );
    let ef = blue["enemyFortifications"].as_array().unwrap();
    let fac = ef
        .iter()
        .find(|f| f["id"] == "FAC0")
        .expect("工事 surfaced at 12 格");
    assert_eq!(fac["kind"], "works_infantry_combat");
    assert!(
        fac.get("occupantSquads").is_none(),
        "no occupancy leak (rule #5)"
    );

    // (c) 三.20.2b: a 战斗工事 garrison may fire OUT — the order is NOT blocked as 工事-inert (it
    // reaches the combat layer; any rejection there is a weapon/observation reason, not 工事 inertness).
    // Fire at a nonexistent target: the chokepoint must let the order REACH combat (where it is
    // rejected as an invalid target), proving the garrison may fire out — NOT block it as 工事-inert.
    // (A nonexistent target avoids damaging BT, which the (e) inheritance loop still needs.)
    match e.submit(
        SideId::Red,
        &serde_json::json!({ "op": "fire_direct", "unitId": "RI", "weapon": "大号直瞄炮", "targetUnit": "GHOST" }),
        e.clock(),
    ) {
        Err(crate::EngineError::Command(m)) => assert!(
            !m.contains("inside a 工事"),
            "三.20.2b: a 战斗工事 garrison may fire out (reached combat), got {m:?}"
        ),
        Ok(()) => {}
        Err(other) => panic!("unexpected error: {other:?}"),
    }

    // (d) out-of-range / no-LOS works fire collapses to the generic fog error (rule #5): BF at 14 格
    // (> the 12-格 exposure range) cannot see FAC0.
    match e.submit(SideId::Blue, &at_works("BF"), e.clock()) {
        Err(crate::EngineError::Command(m)) => assert_eq!(m, "invalid fire target"),
        other => panic!("expected invalid fire target, got {other:?}"),
    }

    // (e) 三.20.2e: the enemy fires AT the 工事 (within 12 格) and the 人员 garrison inherits the
    // result — a 班 loss OR a 压制 (medium armour often deflects a kill to suppression). Check the
    // garrison's state RIGHT AFTER the (synchronous) shot, before `drain` clears the suppression;
    // bounded so a run of NoEffect rolls can't hang.
    let mut inherited = false;
    for _ in 0..40 {
        let before = e.state.units.get("RI").map(|u| u.teams).unwrap_or(0);
        let now = e.clock();
        let _ = e.submit(SideId::Blue, &at_works("BT"), now);
        let affected = match e.state.units.get("RI") {
            Some(u) => !u.alive || u.teams < before || u.suppressed_until > now,
            None => true,
        };
        if affected {
            inherited = true;
            break;
        }
        drain(&mut e); // clear the weapon cooldown (re-deploy BT for the next shot)
    }
    assert!(
        inherited,
        "三.20.2e: a 人员战斗工事 garrison inherits fire results (班 loss or 压制) aimed at the 工事"
    );
}

/// 三.20.1 车辆工事 engine op (W7): a 车辆 garrison is 全程隐蔽; the 工事 is targetable within 12 格 but
/// the vehicles inside do NOT inherit (三.20.1e) — the 工事 ABSORBS the hit, its capacity shrinking,
/// and when the garrison no longer fits it is expelled INSTANTLY + 暴露 (三.20.1f/20e). Eligibility is
/// 车辆-only (三.20c).
#[cfg(test)]
#[test]
fn vehicle_works() {
    use crate::types::{Facility, HexCell, ScenarioUnit, Side, Sides, Terrain};

    let map = Map {
        format: "openstratcore.map".into(),
        version: 1,
        name: "vw".into(),
        elevation_unit_meters: Some(10),
        hexes: (0..=14)
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
    let su = |id: &str, ut: UnitType, q: i32, teams: u8| ScenarioUnit {
        id: id.into(),
        unit_type: ut,
        armor: Some(Armor::Medium),
        teams,
        at: Axial::new(q, 0),
        facing: 0,
        state: Some(UnitState::Stopped),
        carried_by: None,
        affiliated_to: None,
    };
    let scenario = Scenario {
        format: "openstratcore.scenario".into(),
        version: 1,
        name: "vw".into(),
        map: "vw".into(),
        rules: None,
        time_limit_seconds: None,
        sides: Sides {
            red: Side {
                name: "Red".into(),
                units: vec![
                    su("RV", UnitType::Tank, 0, 2),
                    su("RP", UnitType::Infantry, 0, 1),
                ],
            },
            blue: Side {
                name: "Blue".into(),
                units: vec![su("BT", UnitType::Tank, 5, 2)],
            },
        },
        objectives: vec![],
        facilities: vec![Facility {
            kind: "works_vehicle".into(),
            at: Axial::new(0, 0),
            owner: Some("red".into()),
            armor: Some("medium".into()),
            capacity: Some(5),
        }],
    };
    let rules = Rules::from_json_str(include_str!("../../../config/rules.default.json")).unwrap();
    let mut e = Engine::new(map.clone(), scenario, rules.clone(), 1).unwrap();
    let drain = |e: &mut Engine| while e.advance_to_next_event().is_some() {};
    let enter =
        |u: &str| serde_json::json!({ "op": "enter_facility", "unitId": u, "facilityId": "FAC0" });

    // (a) 三.20c eligibility: a 人员 cannot garrison a 车辆工事; a 车辆 can.
    assert!(
        e.submit(SideId::Red, &enter("RP"), 0).is_err(),
        "三.20c: 人员 cannot enter a 车辆工事"
    );
    e.submit(SideId::Red, &enter("RV"), 0).unwrap();
    drain(&mut e);
    assert_eq!(e.state.units["RV"].inside_facility.as_deref(), Some("FAC0"));
    let cap0 = e.state.facilities["FAC0"].capacity;

    // (b) fire AT the 车辆工事 (within 12 格): the vehicle inside is NEVER damaged (三.20.1e 不继承) —
    // its 班 stay 2 — while the 工事's capacity ABSORBS the hits and shrinks, until the garrison no
    // longer fits and is expelled INSTANTLY + 暴露 (三.20.1f). Bounded so NoEffect rolls can't hang.
    let at_works = serde_json::json!({ "op": "fire_direct", "unitId": "BT", "weapon": "大号直瞄炮", "targetFacility": "FAC0" });
    let mut expelled = false;
    for _ in 0..80 {
        let _ = e.submit(SideId::Blue, &at_works, e.clock());
        // 三.20.1e: the occupant vehicle never takes a 班 loss from fire AT the 工事.
        assert_eq!(
            e.state.units["RV"].teams, 2,
            "三.20.1e: a 车辆工事 occupant does NOT inherit the fire result"
        );
        assert!(e.state.units["RV"].alive);
        if e.state.units["RV"].inside_facility.is_none() {
            expelled = true;
            break;
        }
        drain(&mut e); // clear BT's cooldown for the next shot
    }
    assert!(
        expelled,
        "三.20.1f/20e: the 车辆 garrison is expelled once the 工事's shrinking capacity no longer fits it"
    );
    assert!(
        e.state.facilities["FAC0"].capacity < cap0,
        "三.20.1c: fire shrinks the 工事's remaining capacity"
    );
    // Still alive + un-damaged on the open board (it lost the 工事's protection, not its 班).
    assert_eq!(e.state.units["RV"].teams, 2);
    assert!(e.state.units["RV"].inside_facility.is_none());
}

/// 三.20.1c/2c — a 间瞄 salvo landing on a 车辆/战斗工事's hex ALSO hits the 工事 (the structure), and
/// the result is inherited like a direct shot. Tests the `apply_indirect_at` 工事 hook directly (the
/// full 间瞄 flight has scatter/cooldown that would make an end-to-end test flaky).
#[cfg(test)]
#[test]
fn indirect_hits_works() {
    use crate::types::{Facility, HexCell, ScenarioUnit, Side, Sides, Terrain};

    let map = Map {
        format: "openstratcore.map".into(),
        version: 1,
        name: "iw".into(),
        elevation_unit_meters: Some(10),
        hexes: (0..=4)
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
    let scenario = Scenario {
        format: "openstratcore.scenario".into(),
        version: 1,
        name: "iw".into(),
        map: "iw".into(),
        rules: None,
        time_limit_seconds: None,
        sides: Sides {
            red: Side {
                name: "Red".into(),
                units: vec![ScenarioUnit {
                    id: "RI".into(),
                    unit_type: UnitType::Infantry,
                    armor: None,
                    teams: 3,
                    at: Axial::new(2, 0),
                    facing: 0,
                    state: Some(UnitState::Stopped),
                    carried_by: None,
                    affiliated_to: None,
                }],
            },
            blue: Side {
                name: "Blue".into(),
                units: vec![],
            },
        },
        objectives: vec![],
        // An un-armoured 战斗工事 at (2,0): a heavy salvo penetrates so the inheritance is unambiguous.
        facilities: vec![Facility {
            kind: "works_infantry_combat".into(),
            at: Axial::new(2, 0),
            owner: Some("red".into()),
            armor: Some("none".into()),
            capacity: Some(5),
        }],
    };
    let rules = Rules::from_json_str(include_str!("../../../config/rules.default.json")).unwrap();
    let mut e = Engine::new(map.clone(), scenario, rules.clone(), 1).unwrap();
    let drain = |e: &mut Engine| while e.advance_to_next_event().is_some() {};
    e.submit(
        SideId::Red,
        &serde_json::json!({ "op": "enter_facility", "unitId": "RI", "facilityId": "FAC0" }),
        0,
    )
    .unwrap();
    drain(&mut e);
    assert_eq!(e.state.units["RI"].inside_facility.as_deref(), Some("FAC0"));

    // A heavy 间瞄 salvo lands on the 工事's hex: the garrison (全程隐蔽, not an individual victim)
    // inherits the 工事's hit (三.20.2e). Drawn through the engine Rng — deterministic.
    let before = e.state.units["RI"].teams;
    e.apply_indirect_at(
        Axial::new(2, 0),
        crate::combat::IndirectBase::Loss(5),
        1,
        e.clock(),
    );
    let ri = e.state.units.get("RI");
    assert!(
        ri.map(|u| !u.alive || u.teams < before || u.suppressed_until > e.clock())
            .unwrap_or(true),
        "三.20.1c/2c: a 间瞄 salvo on the 工事's hex is inherited by the garrison"
    );
}

/// 三.21 雷场 engine op (W9): a 火箭布雷车 lays a 雷场 within range over 75 s (max 3, 三.21c); any 地面单位
/// entering an uncleared mined hex takes an 附4 雷场裁决 — for BOTH sides (三.21e). 通路/clearing are a
/// later commit (mines persist + every entry adjudicates here).
#[cfg(test)]
#[test]
fn minefield_lay_and_entry() {
    use crate::types::{HexCell, ScenarioUnit, Side, Sides, Terrain};

    let map = Map {
        format: "openstratcore.map".into(),
        version: 1,
        name: "mf".into(),
        elevation_unit_meters: Some(10),
        hexes: (0..=10)
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
    let su = |id: &str, ut: UnitType, q: i32| ScenarioUnit {
        id: id.into(),
        unit_type: ut,
        armor: Some(Armor::None),
        teams: 4,
        at: Axial::new(q, 0),
        facing: 0,
        state: Some(UnitState::Stopped),
        carried_by: None,
        affiliated_to: None,
    };
    let scenario = Scenario {
        format: "openstratcore.scenario".into(),
        version: 1,
        name: "mf".into(),
        map: "mf".into(),
        rules: None,
        time_limit_seconds: None,
        sides: Sides {
            red: Side {
                name: "Red".into(),
                units: vec![su("ML", UnitType::Minelayer, 0)],
            },
            blue: Side {
                name: "Blue".into(),
                units: vec![su("BT", UnitType::Tank, 2)],
            },
        },
        objectives: vec![],
        facilities: vec![],
    };
    let rules = Rules::from_json_str(include_str!("../../../config/rules.default.json")).unwrap();
    let mut e = Engine::new(map.clone(), scenario, rules.clone(), 1).unwrap();
    let drain = |e: &mut Engine| while e.advance_to_next_event().is_some() {};
    let lay = |u: &str, q: i32| serde_json::json!({ "op": "lay_mines", "unitId": u, "targetHex": { "q": q, "r": 0 } });

    // (a) 三.21b: only a 火箭布雷车 may 布雷.
    assert!(
        e.submit(SideId::Blue, &lay("BT", 3), 0).is_err(),
        "三.21b: a 坦克 cannot 布雷"
    );

    // (b) ML lays a 雷场 at (3,0) over 75 s.
    e.submit(SideId::Red, &lay("ML", 3), 0).unwrap();
    assert_eq!(e.state.units["ML"].busy_until, secs_to_ticks(75.0));
    drain(&mut e);
    assert!(
        e.state.minefields.contains_key(&Axial::new(3, 0)),
        "三.21c: the 雷场 is laid at the target hex"
    );
    // (c) 三.21a observation: the owner sees its own 雷场.
    assert!(
        e.observe(SideId::Red)["minefields"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["at"]["q"] == 3),
        "三.21a: the laying side sees its own 雷场"
    );

    // (d) 三.21e entry adjudication: a 地面单位 crossing the uncleared 雷场 takes 附4 damage. The 附4
    // table only damages on some rolls, so cross repeatedly (the mine persists) — bounded so a run of
    // 0-loss rolls can't hang.
    let mut hit = false;
    for i in 0..60 {
        let dest = if i % 2 == 0 { 4 } else { 2 }; // step across (3,0) and back
        let before = e.state.units.get("BT").map(|u| u.teams).unwrap_or(0);
        let now = e.clock();
        let _ = e.submit(
            SideId::Blue,
            &serde_json::json!({ "op": "move_to", "unitId": "BT", "target": { "q": dest, "r": 0 } }),
            now,
        );
        // step until BT reaches (3,0) and the mine adjudicates (or it dies)
        while e.advance_to_next_event().is_some() {
            let dead = e.state.units.get("BT").map(|u| !u.alive).unwrap_or(true);
            let hurt = e
                .state
                .units
                .get("BT")
                .map(|u| u.teams < before || u.suppressed_until > e.clock())
                .unwrap_or(false);
            if dead || hurt {
                hit = true;
                break;
            }
        }
        if hit {
            break;
        }
    }
    assert!(
        hit,
        "三.21e: a 地面单位 crossing an uncleared 雷场 eventually takes 附4 damage (both sides)"
    );

    // (e) 三.21c max 3 lays per 火箭布雷车: after three, the fourth is refused.
    let mut e = Engine::new(map.clone(), scenario_three(), rules.clone(), 1).unwrap();
    for q in [3, 4, 5] {
        e.submit(SideId::Red, &lay("ML", q), e.clock()).unwrap();
        drain(&mut e);
    }
    assert!(
        e.submit(SideId::Red, &lay("ML", 6), e.clock()).is_err(),
        "三.21c: a 火箭布雷车 lays at most 3 雷场"
    );

    // helper: a fresh scenario with just the red 火箭布雷车 (for the max-lays sub-case).
    fn scenario_three() -> crate::types::Scenario {
        use crate::types::{Side, Sides};
        crate::types::Scenario {
            format: "openstratcore.scenario".into(),
            version: 1,
            name: "mf".into(),
            map: "mf".into(),
            rules: None,
            time_limit_seconds: None,
            sides: Sides {
                red: Side {
                    name: "Red".into(),
                    units: vec![crate::types::ScenarioUnit {
                        id: "ML".into(),
                        unit_type: UnitType::Minelayer,
                        armor: Some(Armor::None),
                        teams: 4,
                        at: Axial::new(0, 0),
                        facing: 0,
                        state: Some(UnitState::Stopped),
                        carried_by: None,
                        affiliated_to: None,
                    }],
                },
                blue: Side {
                    name: "Blue".into(),
                    units: vec![],
                },
            },
            objectives: vec![],
            facilities: vec![],
        }
    }
}

/// 三.1 半速 mode-on-move: `move_to mode:"half"` moves at half speed (double the time) for both a
/// 人员 (infantry) and a 车辆 (the 雷场 通路 prerequisite), and the unit settles Stopped (the half-
/// speed is a per-move choice, not a persistent posture).
#[cfg(test)]
#[test]
fn half_speed_move() {
    use crate::types::{HexCell, ScenarioUnit, Side, Sides, Terrain};

    let map = Map {
        format: "openstratcore.map".into(),
        version: 1,
        name: "hs".into(),
        elevation_unit_meters: Some(10),
        hexes: (0..=4)
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
    let make = |ut: UnitType| {
        let scenario = Scenario {
            format: "openstratcore.scenario".into(),
            version: 1,
            name: "hs".into(),
            map: "hs".into(),
            rules: None,
            time_limit_seconds: None,
            sides: Sides {
                red: Side {
                    name: "Red".into(),
                    units: vec![ScenarioUnit {
                        id: "M".into(),
                        unit_type: ut,
                        armor: None,
                        teams: 2,
                        at: Axial::new(0, 0),
                        facing: 0,
                        state: Some(UnitState::Stopped),
                        carried_by: None,
                        affiliated_to: None,
                    }],
                },
                blue: Side {
                    name: "Blue".into(),
                    units: vec![],
                },
            },
            objectives: vec![],
            facilities: vec![],
        };
        Engine::new(map.clone(), scenario, rules.clone(), 1).unwrap()
    };
    let drain = |e: &mut Engine| while e.advance_to_next_event().is_some() {};
    let move_time = |ut: UnitType, half: bool| -> i64 {
        let mut e = make(ut);
        let mut go =
            serde_json::json!({ "op": "move_to", "unitId": "M", "target": { "q": 2, "r": 0 } });
        if half {
            go["mode"] = serde_json::json!("half");
        }
        let t0 = e.clock();
        e.submit(SideId::Red, &go, 0).unwrap();
        drain(&mut e);
        assert_eq!(
            e.state.units["M"].pos,
            Axial::new(2, 0),
            "M reaches its target"
        );
        assert_eq!(
            e.state.units["M"].state,
            UnitState::Stopped,
            "三.1: a 半速 move settles Stopped (per-move speed, not a posture)"
        );
        e.clock() - t0
    };
    for ut in [UnitType::Tank, UnitType::Infantry] {
        let normal = move_time(ut, false);
        let half = move_time(ut, true);
        assert!(normal > 0, "{ut:?} moves");
        assert_eq!(
            half,
            normal * 2,
            "三.1: 半速 takes 2× the normal time ({ut:?})"
        );
    }
    // 三.1: 半速 is per-move ONLY — set_mode "half" is refused (a standing Half must not exist).
    let mut e = make(UnitType::Tank);
    assert!(
        e.submit(
            SideId::Red,
            &serde_json::json!({ "op": "set_mode", "unitId": "M", "mode": "half" }),
            0
        )
        .is_err(),
        "三.1: 半速 is not a set_mode posture"
    );
}

/// 三.21f-i 雷场通路: a 扫雷车/坦克 entering a 雷场 at 半速 OPENS a single-side 通路 (no damage to it);
/// thereafter that side passes without 裁决 (人员 any speed); the ENEMY still adjudicates (the lane is
/// 单侧可见, 三.21h).
#[cfg(test)]
#[test]
fn minefield_clear_and_lane() {
    use crate::types::{Facility, HexCell, ScenarioUnit, Side, Sides, Terrain};

    let map = Map {
        format: "openstratcore.map".into(),
        version: 1,
        name: "ml".into(),
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
    let su = |id: &str, ut: UnitType, q: i32| ScenarioUnit {
        id: id.into(),
        unit_type: ut,
        armor: Some(Armor::None),
        teams: 3,
        at: Axial::new(q, 0),
        facing: 0,
        state: Some(UnitState::Stopped),
        carried_by: None,
        affiliated_to: None,
    };
    let scenario = Scenario {
        format: "openstratcore.scenario".into(),
        version: 1,
        name: "ml".into(),
        map: "ml".into(),
        rules: None,
        time_limit_seconds: None,
        sides: Sides {
            red: Side {
                name: "Red".into(),
                units: vec![
                    su("MS", UnitType::Minesweeper, 1),
                    su("RI", UnitType::Infantry, 1),
                ],
            },
            blue: Side {
                name: "Blue".into(),
                units: vec![su("BT", UnitType::Tank, 5)],
            },
        },
        objectives: vec![],
        // A neutral 雷场 at (3,0) (affects both sides, 三.21e).
        facilities: vec![Facility {
            kind: "minefield".into(),
            at: Axial::new(3, 0),
            owner: None,
            armor: None,
            capacity: None,
        }],
    };
    let rules = Rules::from_json_str(include_str!("../../../config/rules.default.json")).unwrap();
    let mut e = Engine::new(map.clone(), scenario, rules.clone(), 1).unwrap();
    let drain = |e: &mut Engine| while e.advance_to_next_event().is_some() {};
    let go = |u: &str, q: i32, half: bool| {
        let mut m =
            serde_json::json!({ "op": "move_to", "unitId": u, "target": { "q": q, "r": 0 } });
        if half {
            m["mode"] = serde_json::json!("half");
        }
        m
    };

    // (a) 三.21f: the 扫雷车 crosses (3,0) at 半速 → opens a 通路 for Red and takes NO damage.
    e.submit(SideId::Red, &go("MS", 4, true), 0).unwrap();
    drain(&mut e);
    assert_eq!(e.state.units["MS"].pos, Axial::new(4, 0));
    assert_eq!(
        e.state.units["MS"].teams, 3,
        "三.21f: the 扫雷车 clears without taking 雷场 damage"
    );
    assert!(
        e.state.minefields[&Axial::new(3, 0)]
            .cleared_by
            .contains(&SideId::Red),
        "三.21f: a 通路 is opened for the clearing side"
    );

    // (b) 三.21i: a Red 人员 then crosses the lane at ANY speed with no 裁决 (deterministic — no roll).
    e.submit(SideId::Red, &go("RI", 4, false), e.clock())
        .unwrap();
    drain(&mut e);
    assert_eq!(e.state.units["RI"].pos, Axial::new(4, 0));
    assert_eq!(
        e.state.units["RI"].teams, 3,
        "三.21i: 人员 along a friendly 通路 takes no 雷场裁决"
    );

    // (c) 三.21h single-side: the ENEMY has no lane here, so it still adjudicates. Cross repeatedly
    // (the mine persists) until BT takes 附4 damage — bounded so 0-loss rolls can't hang.
    let mut hit = false;
    for i in 0..60 {
        let dest = if i % 2 == 0 { 2 } else { 4 };
        let before = e.state.units.get("BT").map(|u| u.teams).unwrap_or(0);
        let _ = e.submit(SideId::Blue, &go("BT", dest, false), e.clock());
        while e.advance_to_next_event().is_some() {
            let bt = e.state.units.get("BT");
            if bt
                .map(|u| !u.alive || u.teams < before || u.suppressed_until > e.clock())
                .unwrap_or(true)
            {
                hit = true;
                break;
            }
        }
        if hit {
            break;
        }
    }
    assert!(
        hit,
        "三.21h: the enemy has no 通路 here, so it still takes 附4 damage (single-side lane)"
    );
}

/// 三.1.4 一二级冲锋 + 活体疲劳: 冲锋 is a per-move INFANTRY burst (2×/4× speed) that accrues 疲劳;
/// 一级疲劳 bars 二级冲锋, 二级疲劳 bars ALL movement, and 疲劳 decays −1 per 75 s while idle. set_mode
/// charge is refused (per-move only) and only 步兵 may 冲锋. Single Red 步兵 on a flat open row +
/// fixed seed → fully deterministic.
#[cfg(test)]
#[test]
fn charge_and_fatigue() {
    use crate::types::{HexCell, ScenarioUnit, Side, Sides, Terrain};

    let map = Map {
        format: "openstratcore.map".into(),
        version: 1,
        name: "cf".into(),
        elevation_unit_meters: Some(10),
        hexes: (0..=8)
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
    let make = || {
        let su = |id: &str, ut: UnitType, q: i32| ScenarioUnit {
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
        let scenario = Scenario {
            format: "openstratcore.scenario".into(),
            version: 1,
            name: "cf".into(),
            map: "cf".into(),
            rules: None,
            time_limit_seconds: None,
            sides: Sides {
                red: Side {
                    name: "Red".into(),
                    // "M" = 步兵 under test; "T" = 坦克 far away (for the 只有步兵可冲锋 check).
                    units: vec![su("M", UnitType::Infantry, 0), su("T", UnitType::Tank, 8)],
                },
                blue: Side {
                    name: "Blue".into(),
                    units: vec![],
                },
            },
            objectives: vec![],
            facilities: vec![],
        };
        Engine::new(map.clone(), scenario, rules.clone(), 1).unwrap()
    };
    let go = |unit: &str, mode: &str, q: i32| {
        let mut m =
            serde_json::json!({ "op": "move_to", "unitId": unit, "target": { "q": q, "r": 0 } });
        if mode != "normal" {
            m["mode"] = serde_json::json!(mode);
        }
        m
    };
    // Advance events until M has settled (state Stopped) — i.e. the 冲锋 burst ended. Safe-guarded by
    // the empty-queue break.
    let run_until_stopped = |e: &mut Engine| loop {
        if e.state.units["M"].state == UnitState::Stopped {
            break;
        }
        if e.advance_to_next_event().is_none() {
            break;
        }
    };

    // Per config: infantry base 144 s/hex; 一级冲锋 ×2 → 72 s/hex; 二级冲锋 ×4 → 36 s/hex; decay 75 s.
    let step_normal = secs_to_ticks(144.0);
    let step_c1 = secs_to_ticks(72.0);
    let decay = secs_to_ticks(75.0);

    // --- (a) 一级冲锋: half the time per hex, +1 疲劳/hex, halts at 二级疲劳 (fatigue 2) after 2 hexes.
    let mut e = make();
    e.submit(SideId::Red, &go("M", "charge1", 6), 0).unwrap();
    run_until_stopped(&mut e);
    assert_eq!(
        e.state.units["M"].pos,
        Axial::new(2, 0),
        "三.1.4: 一级冲锋 halts at 二级疲劳 after 2 冲锋 hexes"
    );
    assert_eq!(
        e.state.units["M"].fatigue, 2,
        "三.1.4: +1 疲劳 per 冲锋格 → 2"
    );
    let settle = e.clock();
    assert_eq!(
        settle,
        step_c1 * 2,
        "三.1.4: a 一级冲锋 hop is half the normal time (2× speed)"
    );
    assert!(
        settle < step_normal * 2,
        "一级冲锋 reaches (2,0) faster than a normal walk would"
    );
    // 二级疲劳 bars ALL movement — even a normal move_to is REFUSED (not accepted-then-halted).
    assert!(
        e.submit(SideId::Red, &go("M", "normal", 3), e.clock())
            .is_err(),
        "三.1.4: 二级疲劳禁机动 (normal move refused)"
    );
    assert!(
        e.submit(SideId::Red, &go("M", "charge1", 3), e.clock())
            .is_err(),
        "三.1.4: 二级疲劳 bars 一级冲锋 too"
    );

    // --- (b) 疲劳恢复: −1 per 75 s idle, 2 → 1 → 0, then no further decay event.
    assert!(e.advance_to_next_event().is_some());
    assert_eq!(e.clock(), settle + decay, "恢复 tick at +75 s");
    assert_eq!(e.state.units["M"].fatigue, 1, "三.1.4: 疲劳 decays 2 → 1");
    assert!(e.advance_to_next_event().is_some());
    assert_eq!(e.clock(), settle + decay * 2, "next 恢复 tick at +150 s");
    assert_eq!(e.state.units["M"].fatigue, 0, "三.1.4: 疲劳 decays 1 → 0");
    assert!(
        e.advance_to_next_event().is_none(),
        "no further 恢复 once 疲劳 reaches 0"
    );
    assert!(
        e.submit(SideId::Red, &go("M", "charge1", 3), e.clock())
            .is_ok(),
        "a fully-rested unit may 冲锋 again"
    );

    // --- (c) 二级冲锋: 4× speed; advances ONE hex then 一级疲劳 bars further 二级冲锋 (一级冲锋 still ok).
    let mut e2 = make();
    e2.submit(SideId::Red, &go("M", "charge2", 6), 0).unwrap();
    run_until_stopped(&mut e2);
    assert_eq!(
        e2.state.units["M"].pos,
        Axial::new(1, 0),
        "三.1.4: 二级冲锋 halts after 1 hex (一级疲劳 forbids continuing)"
    );
    assert_eq!(e2.state.units["M"].fatigue, 1);
    assert!(
        e2.submit(SideId::Red, &go("M", "charge2", 5), e2.clock())
            .is_err(),
        "三.1.4: 一级疲劳禁二级冲锋"
    );
    assert!(
        e2.submit(SideId::Red, &go("M", "charge1", 5), e2.clock())
            .is_ok(),
        "三.1.4: 一级疲劳 still permits 一级冲锋"
    );

    // --- (d) set_mode charge is refused — 冲锋 is a per-MOVE burst, never a standing posture.
    let mut e3 = make();
    for m in ["charge1", "charge2"] {
        assert!(
            e3.submit(
                SideId::Red,
                &serde_json::json!({ "op": "set_mode", "unitId": "M", "mode": m }),
                0
            )
            .is_err(),
            "三.1.4: {m} is per-move only, not a set_mode posture"
        );
    }

    // --- (e) only 步兵 may 冲锋: a 坦克 is refused.
    assert!(
        e3.submit(SideId::Red, &go("T", "charge1", 7), 0).is_err(),
        "三.1.4: 只有步兵可冲锋 (a 坦克 cannot)"
    );

    // --- (f) per-move speeds are not valid INITIAL postures: a unit placed in 一级冲锋 is rejected at
    // load (else a tank could start standing in Charge1, bypassing the infantry-only/per-move contract).
    {
        let bad = Scenario {
            format: "openstratcore.scenario".into(),
            version: 1,
            name: "cf".into(),
            map: "cf".into(),
            rules: None,
            time_limit_seconds: None,
            sides: Sides {
                red: Side {
                    name: "Red".into(),
                    units: vec![ScenarioUnit {
                        id: "M".into(),
                        unit_type: UnitType::Infantry,
                        armor: None,
                        teams: 2,
                        at: Axial::new(0, 0),
                        facing: 0,
                        state: Some(UnitState::Charge1),
                        carried_by: None,
                        affiliated_to: None,
                    }],
                },
                blue: Side {
                    name: "Blue".into(),
                    units: vec![],
                },
            },
            objectives: vec![],
            facilities: vec![],
        };
        assert!(
            Engine::new(map.clone(), bad, rules.clone(), 1).is_err(),
            "三.1.4: a unit cannot start in 一级冲锋 (per-move speed, not a posture)"
        );
    }

    // --- (g) move_to to the hex a unit already occupies is an accepted no-op: it does not move, and
    // 疲劳恢复 still proceeds to 0 (the no-op must not reset the recovery clock or spawn spurious work).
    let mut e4 = make();
    e4.submit(SideId::Red, &go("M", "charge1", 6), 0).unwrap();
    run_until_stopped(&mut e4);
    assert_eq!(e4.state.units["M"].fatigue, 2);
    let pos = e4.state.units["M"].pos;
    assert!(
        e4.submit(
            SideId::Red,
            &serde_json::json!({ "op": "move_to", "unitId": "M", "target": { "q": pos.q, "r": pos.r } }),
            e4.clock()
        )
        .is_ok(),
        "move_to to the current hex is an accepted no-op"
    );
    assert_eq!(
        e4.state.units["M"].pos, pos,
        "a no-op move_to does not move the unit"
    );
    assert_eq!(
        e4.state.units["M"].state,
        UnitState::Stopped,
        "a no-op move_to leaves the unit Stopped (no burst started)"
    );
    while e4.advance_to_next_event().is_some() {}
    assert_eq!(
        e4.state.units["M"].fatigue, 0,
        "三.1.4: 疲劳 still recovers to 0 after a no-op move_to"
    );
}

/// 三.16h 运输直升机整体战损: a fired-upon 运输直升机 is adjudicated AS A WHOLE — the same 毁伤/压制
/// result hits every loaded 班 (同等损失); a total kill takes the cargo with it. A GROUND 车辆 carrier
/// instead SHIELDS its 搭载 from partial loss (三.2). Drives `apply_direct_fire` directly (the shared
/// loss sink for 直瞄/间瞄/雷场) with fixed outcomes → no RNG, fully deterministic.
#[cfg(test)]
#[test]
fn transport_heli_integral_loss() {
    use crate::prob::Outcome;
    use crate::types::{HexCell, ScenarioUnit, Side, Sides, Terrain};

    let map = Map {
        format: "openstratcore.map".into(),
        version: 1,
        name: "ih".into(),
        elevation_unit_meters: Some(10),
        hexes: (0..=3)
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
    // carrier `cb` rides nobody; a 班 rides `cb` via carried_by.
    let unit = |id: &str, ut: UnitType, teams: u8, q: i32, cb: Option<&str>| ScenarioUnit {
        id: id.into(),
        unit_type: ut,
        armor: None,
        teams,
        at: Axial::new(q, 0),
        facing: 0,
        state: Some(UnitState::Stopped),
        carried_by: cb.map(|s| s.into()),
        affiliated_to: None,
    };
    let scenario = Scenario {
        format: "openstratcore.scenario".into(),
        version: 1,
        name: "ih".into(),
        map: "ih".into(),
        rules: None,
        time_limit_seconds: None,
        sides: Sides {
            red: Side {
                name: "Red".into(),
                units: vec![
                    // 运输直升机 H (teams 3) carrying 班 A,B; 步战车 V (teams 3) carrying 班 C.
                    // (V is an IFV — a valid ground carrier, 三.2 is_carrier; a 坦克 cannot carry.)
                    unit("H", UnitType::TransportHeli, 3, 0, None),
                    unit("A", UnitType::Infantry, 2, 0, Some("H")),
                    unit("B", UnitType::Infantry, 2, 0, Some("H")),
                    unit("V", UnitType::Ifv, 3, 2, None),
                    unit("C", UnitType::Infantry, 2, 2, Some("V")),
                ],
            },
            blue: Side {
                name: "Blue".into(),
                units: vec![],
            },
        },
        objectives: vec![],
        facilities: vec![],
    };
    let rules = Rules::from_json_str(include_str!("../../../config/rules.default.json")).unwrap();
    // keep clones for the adversarial (e)/(f) scenarios below (the first call moves map/rules).
    let map2 = map.clone();
    let rules2 = rules.clone();
    let mut e = Engine::new(map, scenario, rules, 1).unwrap();
    let now = e.clock();

    // (a) heli SURVIVES a partial 战损: Destroyed(1) → H loses 1, AND each loaded 班 loses 1 (同等损失),
    //     survivors of the loss being 压制 just like the heli.
    e.apply_direct_fire("H", Outcome::Destroyed(1), now)
        .unwrap();
    assert_eq!(e.state.units["H"].teams, 2, "三.16h: heli took 1");
    assert_eq!(
        e.state.units["A"].teams, 1,
        "三.16h: loaded 班 A suffers the same loss"
    );
    assert_eq!(
        e.state.units["B"].teams, 1,
        "三.16h: loaded 班 B suffers the same loss"
    );
    assert!(
        e.state.units["A"].suppressed_until > now && e.state.units["B"].suppressed_until > now,
        "三.16h: cargo surviving the integral loss is 压制 too"
    );

    // (b) a GROUND 车辆 carrier SHIELDS its 搭载 from partial loss (三.2): V takes 1, 班 C untouched.
    e.apply_direct_fire("V", Outcome::Destroyed(1), now)
        .unwrap();
    assert_eq!(e.state.units["V"].teams, 2, "V took 1");
    assert_eq!(
        e.state.units["C"].teams, 2,
        "三.2: a ground carrier shields its 搭载 from partial loss"
    );
    assert_eq!(
        e.state.units["C"].suppressed_until, 0,
        "三.2: shielded 搭载 is not 压制 by a hit the carrier absorbed"
    );

    // (c) a pure Suppress on the heli is shared with the cargo (整体接受裁决).
    let t2 = now + 1;
    e.apply_direct_fire("H", Outcome::Suppress, t2).unwrap();
    assert!(
        e.state.units["A"].suppressed_until > t2 && e.state.units["B"].suppressed_until > t2,
        "三.16h: a Suppress result is shared with the loaded 班"
    );

    // (d) a TOTAL kill of the heli takes its cargo with it (existing 承运者死亡 path, not double-applied).
    e.apply_direct_fire("H", Outcome::Destroyed(99), t2)
        .unwrap();
    assert!(!e.state.units["H"].alive, "heli destroyed");
    assert!(
        !e.state.units["A"].alive && !e.state.units["B"].alive,
        "三.16h: a destroyed heli takes its loaded 班 with it"
    );

    // (e) load-time sanity: a unit 搭载 on itself is rejected.
    let self_carry = Scenario {
        format: "openstratcore.scenario".into(),
        version: 1,
        name: "ih".into(),
        map: "ih".into(),
        rules: None,
        time_limit_seconds: None,
        sides: Sides {
            red: Side {
                name: "Red".into(),
                units: vec![unit("X", UnitType::Infantry, 2, 0, Some("X"))],
            },
            blue: Side {
                name: "Blue".into(),
                units: vec![],
            },
        },
        objectives: vec![],
        facilities: vec![],
    };
    assert!(
        Engine::new(map2.clone(), self_carry, rules2.clone(), 1).is_err(),
        "三.2: a unit cannot be 搭载 on itself"
    );

    // (f) recursion guard (Codex W13): even an adversarial 运输直升机 carrying ANOTHER 运输直升机 (a
    // non-班, only reachable via a hand-built carried_by) must NOT recurse on the 三.16h forwarding — the
    // `can_be_passenger` filter excludes it, so the call returns and the inner heli is left untouched.
    let heli_on_heli = Scenario {
        format: "openstratcore.scenario".into(),
        version: 1,
        name: "ih".into(),
        map: "ih".into(),
        rules: None,
        time_limit_seconds: None,
        sides: Sides {
            red: Side {
                name: "Red".into(),
                units: vec![
                    unit("H1", UnitType::TransportHeli, 3, 0, None),
                    unit("H2", UnitType::TransportHeli, 3, 0, Some("H1")),
                ],
            },
            blue: Side {
                name: "Blue".into(),
                units: vec![],
            },
        },
        objectives: vec![],
        facilities: vec![],
    };
    let mut e2 = Engine::new(map2, heli_on_heli, rules2, 1).unwrap();
    let t0 = e2.clock();
    e2.apply_direct_fire("H1", Outcome::Suppress, t0).unwrap(); // returns (no unbounded recursion)
    assert_eq!(
        e2.state.units["H2"].suppressed_until, 0,
        "三.16h forwarding skips a non-班 carried unit — no recursion, no spurious loss"
    );
}

/// 三.1.2g 路障: a 路障 facility hex is 不可通过 for GROUND 算子 (人员/车辆) — they halt at the hex
/// before it — while 空中算子 fly over. Single-row map, 路障 at (2,0), fixed seed → deterministic.
#[cfg(test)]
#[test]
fn roadblock_blocks_ground_not_air() {
    use crate::types::{Facility, HexCell, ScenarioUnit, Side, Sides, Terrain};

    let map = Map {
        format: "openstratcore.map".into(),
        version: 1,
        name: "rb".into(),
        elevation_unit_meters: Some(10),
        hexes: (0..=4)
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
    let make = |ut: UnitType| {
        let scenario = Scenario {
            format: "openstratcore.scenario".into(),
            version: 1,
            name: "rb".into(),
            map: "rb".into(),
            rules: None,
            time_limit_seconds: None,
            sides: Sides {
                red: Side {
                    name: "Red".into(),
                    units: vec![ScenarioUnit {
                        id: "M".into(),
                        unit_type: ut,
                        armor: None,
                        teams: 2,
                        at: Axial::new(0, 0),
                        facing: 0,
                        state: Some(UnitState::Stopped),
                        carried_by: None,
                        affiliated_to: None,
                    }],
                },
                blue: Side {
                    name: "Blue".into(),
                    units: vec![],
                },
            },
            objectives: vec![],
            // A 路障 at (2,0) blocks the lane.
            facilities: vec![Facility {
                kind: "roadblock".into(),
                at: Axial::new(2, 0),
                owner: None,
                armor: None,
                capacity: None,
            }],
        };
        Engine::new(map.clone(), scenario, rules.clone(), 1).unwrap()
    };
    let drain = |e: &mut Engine| while e.advance_to_next_event().is_some() {};
    let final_pos = |ut: UnitType| -> Axial {
        let mut e = make(ut);
        e.submit(
            SideId::Red,
            &serde_json::json!({ "op": "move_to", "unitId": "M", "target": { "q": 4, "r": 0 } }),
            0,
        )
        .unwrap();
        drain(&mut e);
        e.state.units["M"].pos
    };

    // 三.1.2g: a 车辆 (坦克) and 人员 (步兵) cannot ENTER the 路障 hex — they halt at (1,0), the hex before.
    assert_eq!(
        final_pos(UnitType::Tank),
        Axial::new(1, 0),
        "三.1.2g: 坦克 halts before the 路障 (不可通过)"
    );
    assert_eq!(
        final_pos(UnitType::Infantry),
        Axial::new(1, 0),
        "三.1.2g: 步兵 halts before the 路障 (人员 also blocked)"
    );
    // 空中算子 fly over the 路障 (only 地面算子 are blocked) — the 攻击直升机 reaches the far side.
    assert_eq!(
        final_pos(UnitType::AttackHeli),
        Axial::new(4, 0),
        "三.1.2g: 攻击直升机 flies over the 路障"
    );
}

/// 三.17 聚合解聚: 聚合 merges a same-type co-located own 班 into another over 75 s (consumed unit
/// leaves the board); 解聚 fissions a 班 into a smaller half + a deterministically-id'd spawned child.
/// Gated to ground non-炮兵; aborts on 压制 (三.17d). Same commands on a fresh engine reproduce the
/// roster (replay-deterministic child ids). Fixed seed → fully deterministic.
#[cfg(test)]
#[test]
fn aggregate_and_split() {
    use crate::prob::Outcome;
    use crate::types::{HexCell, ScenarioUnit, Side, Sides, Terrain};

    let map = Map {
        format: "openstratcore.map".into(),
        version: 1,
        name: "as".into(),
        elevation_unit_meters: Some(10),
        hexes: (0..=4)
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
    let unit = |id: &str, ut: UnitType, teams: u8, q: i32| ScenarioUnit {
        id: id.into(),
        unit_type: ut,
        armor: None,
        teams,
        at: Axial::new(q, 0),
        facing: 0,
        state: Some(UnitState::Stopped),
        carried_by: None,
        affiliated_to: None,
    };
    let scenario = Scenario {
        format: "openstratcore.scenario".into(),
        version: 1,
        name: "as".into(),
        map: "as".into(),
        rules: None,
        time_limit_seconds: None,
        sides: Sides {
            red: Side {
                name: "Red".into(),
                units: vec![
                    unit("R1", UnitType::Infantry, 2, 0),
                    unit("R2", UnitType::Infantry, 2, 0),
                    unit("R3", UnitType::Infantry, 2, 0), // a third 班 for the busy/double-consume gate
                    unit("AR", UnitType::Artillery, 2, 2), // for the 炮兵-rejected gate
                    unit("AH", UnitType::AttackHeli, 1, 3), // for the air-rejected gate
                ],
            },
            blue: Side {
                name: "Blue".into(),
                units: vec![],
            },
        },
        objectives: vec![],
        facilities: vec![],
    };
    let rules = Rules::from_json_str(include_str!("../../../config/rules.default.json")).unwrap();
    let agg = |u: &str, other: &str| serde_json::json!({ "op": "aggregate", "unitId": u, "targetUnit": other });
    let split = |u: &str| serde_json::json!({ "op": "split", "unitId": u });
    let drain = |e: &mut Engine| while e.advance_to_next_event().is_some() {};
    let tr = secs_to_ticks(75.0);

    // ---- (a) 聚合: R1(2) + R2(2) → R1(4); R2 leaves the board. Takes 75 s.
    let mut e = Engine::new(map.clone(), scenario.clone(), rules.clone(), 1).unwrap();
    e.submit(SideId::Red, &agg("R1", "R2"), 0).unwrap();
    assert_eq!(
        e.state.units["R1"].teams, 2,
        "no merge before the 75 s completes"
    );
    assert!(
        e.state.units["R2"].alive,
        "R2 still on the board mid-transition"
    );
    drain(&mut e);
    assert_eq!(e.clock(), tr, "三.17b: 聚合 takes 75 s");
    assert_eq!(e.state.units["R1"].teams, 4, "三.17b: 班数 累加 → 4");
    assert!(
        !e.state.units["R2"].alive && e.state.units["R2"].teams == 0,
        "三.17b: the consumed 班 leaves the board"
    );

    // ---- (b) 解聚: R1(4) → R1(2) + spawned child R1#1(2), both at the hex. Deterministic child id.
    e.submit(SideId::Red, &split("R1"), e.clock()).unwrap();
    drain(&mut e);
    assert_eq!(
        e.state.units["R1"].teams, 2,
        "三.17c: parent keeps the larger half (2)"
    );
    let child = e
        .state
        .units
        .get("R1#1")
        .expect("三.17c: a child R1#1 is spawned");
    assert_eq!(child.teams, 2, "三.17c: child takes the other half (2)");
    assert!(
        child.alive && child.pos == Axial::new(0, 0),
        "child spawns alive at the parent hex"
    );
    assert_eq!(child.unit_type, UnitType::Infantry);
    assert_eq!(child.side, SideId::Red);

    // ---- (c) split again → ctr advances to R1#2 (ids never collide).
    e.submit(SideId::Red, &split("R1"), e.clock()).unwrap();
    drain(&mut e);
    assert!(
        e.state.units.contains_key("R1#2"),
        "三.17c: the next child id is R1#2"
    );

    // ---- (d) replay-determinism: the SAME commands on a fresh same-seed engine reproduce the roster
    // (RNG-free + deterministic child ids).
    let mut e2 = Engine::new(map.clone(), scenario.clone(), rules.clone(), 1).unwrap();
    e2.submit(SideId::Red, &agg("R1", "R2"), 0).unwrap();
    drain(&mut e2);
    let t1 = e2.clock();
    e2.submit(SideId::Red, &split("R1"), t1).unwrap();
    drain(&mut e2);
    let t2 = e2.clock();
    e2.submit(SideId::Red, &split("R1"), t2).unwrap();
    drain(&mut e2);
    let roster = |e: &Engine| -> Vec<(String, u8, bool)> {
        let mut v: Vec<_> = e
            .state
            .units
            .iter()
            .map(|(id, u)| (id.clone(), u.teams, u.alive))
            .collect();
        v.sort();
        v
    };
    assert_eq!(
        roster(&e),
        roster(&e2),
        "三.17 replay: identical commands reproduce the identical roster (deterministic child ids)"
    );

    // ---- (e) 压制 aborts an in-flight 聚合 (三.17d). Fresh engine.
    let mut e3 = Engine::new(map.clone(), scenario.clone(), rules.clone(), 1).unwrap();
    e3.submit(SideId::Red, &agg("R1", "R2"), 0).unwrap();
    // Suppress the consumed party mid-transition → the 聚合 aborts (no merge, both survive).
    e3.apply_direct_fire("R2", Outcome::Suppress, 0).unwrap();
    drain(&mut e3);
    assert_eq!(
        e3.state.units["R1"].teams, 2,
        "三.17d: 压制 aborts the 聚合 — no merge"
    );
    assert!(
        e3.state.units["R2"].alive,
        "三.17d: the 压制d unit is not consumed"
    );

    // ---- (f) gates: 炮兵 and 空中算子 cannot 聚合/解聚 (三.17a/i); split needs ≥2 班.
    let mut e4 = Engine::new(map.clone(), scenario.clone(), rules.clone(), 1).unwrap();
    assert!(
        e4.submit(SideId::Red, &split("AR"), 0).is_err(),
        "三.17i: 炮兵 cannot 解聚"
    );
    assert!(
        e4.submit(SideId::Red, &split("AH"), 0).is_err(),
        "三.17a: 空中算子 cannot 解聚"
    );
    // split AH/AR rejected above; a 1-班 unit cannot split either — make R1 a 1-班 via two splits.
    // (R1 starts at 2 → split → 1; then a further split must fail.)
    e4.submit(SideId::Red, &split("R1"), 0).unwrap();
    drain(&mut e4);
    assert_eq!(
        e4.state.units["R1"].teams, 1,
        "R1 down to 1 班 after splitting 2"
    );
    assert!(
        e4.submit(SideId::Red, &split("R1"), e4.clock()).is_err(),
        "三.17c: a 1-班 unit cannot 解聚"
    );

    // ---- (g) double-consume guard: while R1 is busy aggregating R2, R1/R2 cannot be re-targeted
    // (reject_if_busy on the consumed party blocks a second 聚合 from consuming the same unit).
    let mut e5 = Engine::new(map.clone(), scenario.clone(), rules.clone(), 1).unwrap();
    e5.submit(SideId::Red, &agg("R1", "R2"), 0).unwrap();
    assert!(
        e5.submit(SideId::Red, &agg("R3", "R1"), 0).is_err(),
        "三.17: cannot 聚合-consume R1 while it is mid-transition"
    );
    assert!(
        e5.submit(SideId::Red, &agg("R3", "R2"), 0).is_err(),
        "三.17: cannot 聚合-consume R2 while it is being consumed"
    );

    // ---- (h) same-tick re-task guard: a `stop` order accepted at the EXACT 75 s completion tick
    // (reject_if_busy admits busy_until == t) supersedes the 聚合 — the stale completion must NOT merge.
    let mut e6 = Engine::new(map.clone(), scenario.clone(), rules.clone(), 1).unwrap();
    e6.submit(SideId::Red, &agg("R1", "R2"), 0).unwrap();
    e6.submit(
        SideId::Red,
        &serde_json::json!({ "op": "stop", "unitId": "R1" }),
        tr,
    )
    .unwrap();
    drain(&mut e6);
    assert_eq!(
        e6.state.units["R1"].teams, 2,
        "三.17: a same-tick re-task supersedes the 聚合 — no merge"
    );
    assert!(
        e6.state.units["R2"].alive,
        "三.17: R2 not consumed by the superseded 聚合"
    );
}

/// 三.13d × 三.17: a unit that a 无人战车 is 隶属 to (a master) cannot be CONSUMED by 聚合 — that would
/// orphan the UGV (pointing at a dead master). The surviving initiator may keep its UGVs. Fixed seed.
#[cfg(test)]
#[test]
fn aggregate_ugv_master_gate() {
    use crate::types::{HexCell, ScenarioUnit, Side, Sides, Terrain};

    let map = Map {
        format: "openstratcore.map".into(),
        version: 1,
        name: "um".into(),
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
    let mk = |id: &str, ut: UnitType, affil: Option<&str>| ScenarioUnit {
        id: id.into(),
        unit_type: ut,
        armor: None,
        teams: 2,
        at: Axial::new(0, 0),
        facing: 0,
        state: Some(UnitState::Stopped),
        carried_by: None,
        affiliated_to: affil.map(|s| s.into()),
    };
    // Two 步战车 (valid carriers/masters) co-located + a UGV 隶属 to V1.
    let scenario = Scenario {
        format: "openstratcore.scenario".into(),
        version: 1,
        name: "um".into(),
        map: "um".into(),
        rules: None,
        time_limit_seconds: None,
        sides: Sides {
            red: Side {
                name: "Red".into(),
                units: vec![
                    mk("V1", UnitType::Ifv, None),
                    mk("V2", UnitType::Ifv, None),
                    mk("U", UnitType::Ugv, Some("V1")),
                ],
            },
            blue: Side {
                name: "Blue".into(),
                units: vec![],
            },
        },
        objectives: vec![],
        facilities: vec![],
    };
    let rules = Rules::from_json_str(include_str!("../../../config/rules.default.json")).unwrap();
    let mut e = Engine::new(map, scenario, rules, 1).unwrap();
    let agg = |u: &str, other: &str| serde_json::json!({ "op": "aggregate", "unitId": u, "targetUnit": other });
    // Consuming V1 (a UGV's master) is rejected — would orphan U.
    assert!(
        e.submit(SideId::Red, &agg("V2", "V1"), 0).is_err(),
        "三.13d/三.17: cannot 聚合-consume a 无人战车's 隶属车辆"
    );
    // Consuming V2 (masters nobody) is fine — V1 survives and keeps U.
    assert!(
        e.submit(SideId::Red, &agg("V1", "V2"), 0).is_ok(),
        "三.17: consuming a non-master is allowed (the master survives, keeps its UGV)"
    );
}

/// 三.10 巡飞弹 飞行 + 打击: a deployed 巡飞弹 flies toward its target area via `move_to`, then strikes
/// an observed enemy via the 直瞄对车 pipeline A (附1.2 巡飞弹 row, range 2 / level 5) and is expended.
/// An out-of-recon-range (hidden) target collapses to a generic error (rule #5). Fixed seed.
#[cfg(test)]
#[test]
fn loitering_strike_and_flight() {
    use crate::types::{HexCell, ScenarioUnit, Side, Sides, Terrain};

    let map = Map {
        format: "openstratcore.map".into(),
        version: 1,
        name: "lm".into(),
        elevation_unit_meters: Some(10),
        hexes: (0..=4)
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
    let u = |id: &str, ut: UnitType, q: i32, carried: Option<&str>| ScenarioUnit {
        id: id.into(),
        unit_type: ut,
        armor: None,
        teams: if matches!(ut, UnitType::LoiteringMunition) {
            1
        } else {
            3
        },
        at: Axial::new(q, 0),
        facing: 0,
        state: Some(UnitState::Stopped),
        carried_by: carried.map(|s| s.into()),
        affiliated_to: None,
    };
    let scenario = Scenario {
        format: "openstratcore.scenario".into(),
        version: 1,
        name: "lm".into(),
        map: "lm".into(),
        rules: None,
        time_limit_seconds: None,
        sides: Sides {
            red: Side {
                name: "Red".into(),
                // 发射车 L at (0,0) with a loaded 巡飞弹 M; Blue 坦克 BT at (3,0).
                units: vec![
                    u("L", UnitType::ReconVehicle, 0, None),
                    u("M", UnitType::LoiteringMunition, 0, Some("L")),
                ],
            },
            blue: Side {
                name: "Blue".into(),
                units: vec![u("BT", UnitType::Tank, 3, None)],
            },
        },
        objectives: vec![],
        facilities: vec![],
    };
    let rules = Rules::from_json_str(include_str!("../../../config/rules.default.json")).unwrap();
    let mut e = Engine::new(map, scenario, rules, 1).unwrap();
    let mv = |q: i32| serde_json::json!({ "op": "move_to", "unitId": "M", "target": { "q": q, "r": 0 } });
    let strike = serde_json::json!({ "op": "strike_loitering", "unitId": "M", "targetUnit": "BT" });

    // (a) 发射: M is busy during the 75 s 发射, so it cannot yet be flown; then it deploys, independent.
    e.submit(
        SideId::Red,
        &serde_json::json!({ "op": "launch_loitering", "unitId": "M", "targetArea": { "q": 3, "r": 0 } }),
        0,
    )
    .unwrap();
    assert!(
        e.submit(SideId::Red, &mv(1), 0).is_err(),
        "巡飞弹 is busy mid-发射 — cannot be ordered yet"
    );
    e.advance_to_next_event(); // LoiteringLaunched (deploy) at 75 s
    assert!(
        e.state.units["M"].alive && e.state.units["M"].carried_by.is_none(),
        "三.10d: the 巡飞弹 deploys independent of its carrier"
    );

    // (b) fog: BT at dist 3 is beyond the 2-hex 侦察 — striking it collapses to a generic error (rule #5).
    assert!(
        e.submit(SideId::Red, &strike, e.clock()).is_err(),
        "三.10b/#5: an out-of-recon-range target yields the generic invalid-target error"
    );

    // (c) 飞行: fly M to (2,0) via move_to (air, 8 s/hex). Step past the two hops but NOT the 1200 s 自毁.
    e.submit(SideId::Red, &mv(2), e.clock()).unwrap();
    e.step(2000); // 20 s — both hops land (8 s, 16 s); 自毁 (+1200 s) is far off
    assert_eq!(
        e.state.units["M"].pos,
        Axial::new(2, 0),
        "三.10 飞行: a deployed 巡飞弹 flies via move_to"
    );

    // (d) 打击: BT is now within 2-hex 侦察 + strike range → the strike resolves and the 巡飞弹 is spent.
    e.submit(SideId::Red, &strike, e.clock()).unwrap();
    assert!(
        !e.state.units["M"].alive,
        "三.10f: the 巡飞弹 is consumed by its strike (one-shot)"
    );

    // (e) a spent 巡飞弹 cannot strike again.
    assert!(
        e.submit(SideId::Red, &strike, e.clock()).is_err(),
        "a spent 巡飞弹 cannot 打击 again"
    );
}

/// 间瞄 (三.9) engine wiring (T2.3): only 炮兵 may plan, the salvo adjudicates after 150 s and
/// damages BOTH sides in the impact hex (己方误伤), and the cooldown bars an immediate re-plan.
#[cfg(test)]
#[test]
fn indirect_plan_friendly_fire() {
    use crate::types::{HexCell, ScenarioUnit, Side, Sides, Terrain};

    let map = Map {
        format: "openstratcore.map".into(),
        version: 1,
        name: "arty".into(),
        elevation_unit_meters: Some(10),
        hexes: (0..=4)
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
    let u = |id: &str, ut: UnitType, q: i32| ScenarioUnit {
        id: id.into(),
        unit_type: ut,
        armor: None,
        teams: 3,
        at: Axial::new(q, 0),
        facing: 0,
        state: Some(UnitState::Stopped),
        carried_by: None,
        affiliated_to: None,
    };
    let rules = Rules::from_json_str(include_str!("../../../config/rules.default.json")).unwrap();
    let make = |seed: u64| {
        // R-Art shells (3,0) where a friendly RV and an enemy EV both stand; R-Spot at (2,0)
        // spots the target hex (目标校射, biasing toward 命中).
        let scenario = Scenario {
            format: "openstratcore.scenario".into(),
            version: 1,
            name: "arty".into(),
            map: "arty".into(),
            rules: None,
            time_limit_seconds: None,
            sides: Sides {
                red: Side {
                    name: "Red".into(),
                    units: vec![
                        u("R-Art", UnitType::Artillery, 0),
                        u("R-Spot", UnitType::Infantry, 2),
                        u("RV", UnitType::Infantry, 3),
                    ],
                },
                blue: Side {
                    name: "Blue".into(),
                    units: vec![u("EV", UnitType::Infantry, 3)],
                },
            },
            objectives: vec![],
            facilities: vec![],
        };
        Engine::new(map.clone(), scenario, rules.clone(), seed).unwrap()
    };
    let plan = serde_json::json!({ "op": "plan_indirect", "unitId": "R-Art", "targetHex": { "q": 3, "r": 0 } });
    let affected = |e: &Engine, id: &str| {
        e.state
            .units
            .get(id)
            .map(|x| !x.alive || x.teams < 3 || x.suppressed_until > e.state.clock)
            .unwrap_or(false)
    };

    // Validation: a non-artillery unit cannot plan 间瞄, and the cooldown bars an immediate re-plan.
    let mut e = make(1);
    assert!(
        e.submit(SideId::Red, &serde_json::json!({ "op": "plan_indirect", "unitId": "R-Spot", "targetHex": { "q": 3, "r": 0 } }), 0).is_err(),
        "三.9.1a: only 炮兵 may plan 间瞄"
    );
    e.submit(SideId::Red, &plan, 0).unwrap();
    assert!(
        e.submit(SideId::Red, &plan, 100).is_err(),
        "三.9.1c: the artillery is on cooldown"
    );
    // 三.9.1d: cancelling the still-flying plan clears the cooldown so a new one can be planned.
    e.submit(
        SideId::Red,
        &serde_json::json!({ "op": "cancel_indirect", "unitId": "R-Art" }),
        200,
    )
    .unwrap();
    e.submit(SideId::Red, &plan, 300)
        .expect("三.9.1d: cooldown cleared after cancel");

    // Friendly fire (三.9.3c): across seeds the salvo lands on (3,0) and damages BOTH RV and EV.
    let mut hit_friendly = false;
    let mut hit_enemy = false;
    for seed in 1..40 {
        let mut e = make(seed);
        e.submit(SideId::Red, &plan, 0).unwrap();
        e.step(16_000); // past the 150 s 飞行 (adjudication), before 压制 lifts at 300 s
        if affected(&e, "RV") {
            hit_friendly = true;
        }
        if affected(&e, "EV") {
            hit_enemy = true;
        }
        if hit_friendly && hit_enemy {
            break;
        }
    }
    assert!(
        hit_friendly,
        "三.9.3c: 间瞄 must be able to wound a friendly unit in the impact hex"
    );
    assert!(hit_enemy, "and the enemy unit sharing that hex");
}

/// 引导射击 (三.14) engine op (T2.6): a 无人机 guides a 重型导弹 战车 onto an enemy it cannot see
/// itself; both then enter the 75 s prep, and a non-无人机 guide is refused (pending the 三.13 link).
#[cfg(test)]
#[test]
fn guide_fire_op() {
    use crate::types::{HexCell, ScenarioUnit, Side, Sides, Terrain};

    let map = Map {
        format: "openstratcore.map".into(),
        version: 1,
        name: "gf".into(),
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
    let u = |id: &str, ut: UnitType, q: i32| ScenarioUnit {
        id: id.into(),
        unit_type: ut,
        armor: Some(Armor::Medium),
        teams: 3,
        at: Axial::new(q, 0),
        facing: 0,
        state: Some(UnitState::Stopped),
        carried_by: None,
        affiliated_to: None,
    };
    let rules = Rules::from_json_str(include_str!("../../../config/rules.default.json")).unwrap();
    let scenario = Scenario {
        format: "openstratcore.scenario".into(),
        version: 1,
        name: "gf".into(),
        map: "gf".into(),
        rules: None,
        time_limit_seconds: None,
        sides: Sides {
            red: Side {
                name: "Red".into(),
                units: vec![
                    u("V", UnitType::Tank, 0),     // 重型导弹 vehicle
                    u("G", UnitType::Uav, 4),      // 无人机 guide (adjacent to the target)
                    u("F", UnitType::Infantry, 0), // a non-UAV would-be guide
                ],
            },
            blue: Side {
                name: "Blue".into(),
                units: vec![u("T", UnitType::Tank, 5)],
            },
        },
        objectives: vec![],
        facilities: vec![],
    };
    let mut e = Engine::new(map, scenario, rules, 5).unwrap();
    let guide = |g: &str| serde_json::json!({ "op": "guide_fire", "unitId": "V", "guideId": g, "weapon": "重型导弹", "targetUnit": "T" });
    // 三.14b: this 步兵 has no 隶属 vehicle, so it may not guide V (the UGV-style affiliation gate).
    assert!(e.submit(SideId::Red, &guide("F"), 0).is_err());
    // The 无人机 guides V's 重型导弹 onto T (V has no LOS of its own); the command resolves.
    e.submit(SideId::Red, &guide("G"), 0).unwrap();
    // 三.14f: both are now in their 75 s prep — an immediate re-guide via the same UAV is refused.
    assert!(
        e.submit(SideId::Red, &guide("G"), 100).is_err(),
        "三.14f: the guide is in its 75 s prep"
    );
    assert_eq!(
        e.state.units.get("V").unwrap().weapon_state,
        WeaponState::Cooling,
        "三.14f: the missile vehicle is cooling"
    );
}

/// 三.18 防空射击 engine op: a 防空算子 fires 流水线 D at an OBSERVED air target, gated by 射速
/// (cooldown per interval), 弹药 (max shots, 三.18g), observation (三.18e) and air-only targeting.
#[cfg(test)]
#[test]
fn aa_fire_op() {
    use crate::types::{HexCell, ScenarioUnit, Side, Sides, Terrain};

    let map = Map {
        format: "openstratcore.map".into(),
        version: 1,
        name: "aa".into(),
        elevation_unit_meters: Some(10),
        hexes: (0..=8)
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
    let u = |id: &str, ut: UnitType, q: i32| ScenarioUnit {
        id: id.into(),
        unit_type: ut,
        armor: Some(Armor::Medium),
        teams: 4,
        at: Axial::new(q, 0),
        facing: 0,
        state: Some(UnitState::Stopped),
        carried_by: None,
        affiliated_to: None,
    };
    let rules = Rules::from_json_str(include_str!("../../../config/rules.default.json")).unwrap();
    let make = || {
        let scenario = Scenario {
            format: "openstratcore.scenario".into(),
            version: 1,
            name: "aa".into(),
            map: "aa".into(),
            rules: None,
            time_limit_seconds: None,
            sides: Sides {
                red: Side {
                    name: "Red".into(),
                    units: vec![
                        u("AA", UnitType::AaMissileVehicle, 0), // 车载防空导弹: range 50, 15s×4
                        u("RT", UnitType::Tank, 0),             // a non-防空 unit
                    ],
                },
                blue: Side {
                    name: "Blue".into(),
                    units: vec![
                        u("H", UnitType::AttackHeli, 5), // air target, observed (50) + in range
                        u("GT", UnitType::Tank, 2),      // a ground target
                    ],
                },
            },
            objectives: vec![],
            facilities: vec![],
        };
        Engine::new(map.clone(), scenario, rules.clone(), 7).unwrap()
    };
    let drain = |e: &mut Engine| while e.advance_to_next_event().is_some() {};
    let fire = |unit: &str, tgt: &str| serde_json::json!({ "op": "aa_fire", "unitId": unit, "targetUnit": tgt });

    // A shot at the observed heli resolves: one shot spent, the weapon cools for the AA interval.
    let mut e = make();
    e.submit(SideId::Red, &fire("AA", "H"), 0).unwrap();
    assert_eq!(
        e.state.units.get("AA").unwrap().weapon_state,
        WeaponState::Cooling
    );
    assert_eq!(*e.aa_shots.get("AA").unwrap(), 1);
    // 三.18 射速: cannot fire again while cooling.
    assert!(e.submit(SideId::Red, &fire("AA", "H"), 100).is_err());
    drain(&mut e); // the interval elapses → weapon ready again
    assert_eq!(
        e.state.units.get("AA").unwrap().weapon_state,
        WeaponState::Deployed
    );

    // 三.18g 弹药: with max-1 shots already spent, one more works, the next is out of shots.
    let mut e = make();
    e.aa_shots.insert("AA".into(), 3); // car: max_shots 4
    e.submit(SideId::Red, &fire("AA", "H"), 0).unwrap(); // 4th shot
    drain(&mut e);
    assert!(
        e.submit(SideId::Red, &fire("AA", "H"), e.clock()).is_err(),
        "三.18g: out of AA shots"
    );

    // 三.18: AA engages AIR only — a ground target is refused; and a non-防空 unit can't aa_fire.
    let mut e = make();
    assert!(e.submit(SideId::Red, &fire("AA", "GT"), 0).is_err()); // GT is a ground tank
    assert!(e.submit(SideId::Red, &fire("RT", "H"), 0).is_err()); // RT is not a 防空算子
}

/// 三.22 天基侦察 engine wiring: a 天基侦察算子 adds REDUCED enemy tracks to its side's observation
/// (lowest-id per hex, `track:"space"`, no teams) that are NOT directly fire-able (三.22d); the 天基
/// 算子 itself cannot be 直瞄 targeted (三.22b).
#[cfg(test)]
#[test]
fn space_recon_op() {
    use crate::types::{HexCell, ScenarioUnit, Side, Sides, Terrain};

    let map = Map {
        format: "openstratcore.map".into(),
        version: 1,
        name: "sr".into(),
        elevation_unit_meters: Some(10),
        hexes: (0..=32)
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
    let u = |id: &str, ut: UnitType, q: i32| ScenarioUnit {
        id: id.into(),
        unit_type: ut,
        armor: Some(Armor::Medium),
        teams: 3,
        at: Axial::new(q, 0),
        facing: 0,
        state: Some(UnitState::Stopped),
        carried_by: None,
        affiliated_to: None,
    };
    let rules = Rules::from_json_str(include_str!("../../../config/rules.default.json")).unwrap();
    // BT is at q30 — beyond the 25-hex vehicle observation, so only 天基 can track it.
    let make = |with_sr: bool| {
        let mut red = vec![u("RT", UnitType::Tank, 0)];
        if with_sr {
            red.push(u("SR", UnitType::SpaceRecon, 0));
        }
        let scenario = Scenario {
            format: "openstratcore.scenario".into(),
            version: 1,
            name: "sr".into(),
            map: "sr".into(),
            rules: None,
            time_limit_seconds: None,
            sides: Sides {
                red: Side {
                    name: "Red".into(),
                    units: red,
                },
                blue: Side {
                    name: "Blue".into(),
                    // BSR sits on RT's hex (q0): a 天基算子 must NOT trigger 同格交战 (三.22b).
                    units: vec![
                        u("BT", UnitType::Tank, 30),
                        u("BSR", UnitType::SpaceRecon, 0),
                    ],
                },
            },
            objectives: vec![],
            facilities: vec![],
        };
        Engine::new(map.clone(), scenario, rules.clone(), 1).unwrap()
    };

    // 三.22b: a 天基算子 co-located with an enemy (BSR on RT's hex) starts no 同格交战 (scenario scan).
    let e = make(true);
    assert!(e.same_hex.is_empty());
    let obs = e.observe(SideId::Red);
    let enemy = obs["enemyUnits"].as_array().unwrap();
    let bt = enemy
        .iter()
        .find(|u| u["id"] == "BT")
        .expect("三.22c: 天基 tracks the far enemy");
    assert_eq!(bt["track"], "space", "三.22c: a reduced, space-only track");
    assert!(bt.get("teams").is_none(), "三.22c: no 车班数 leaked");

    // 三.22b: a 天基侦察算子 cannot be 直瞄 targeted (folds into the generic invalid-target error).
    let mut e = make(true);
    let fire = |tgt: &str| serde_json::json!({ "op": "fire_direct", "unitId": "RT", "weapon": "大号直瞄炮", "targetUnit": tgt });
    assert!(e.submit(SideId::Red, &fire("BSR"), 0).is_err());
    // 三.22d: a space-only track is NOT directly observed, so it can't be 直瞄 fired upon either.
    assert!(e.submit(SideId::Red, &fire("BT"), 0).is_err());

    // Without the 天基算子, the far BT isn't tracked at all (no space reveal).
    let e = make(false);
    let obs = e.observe(SideId::Red);
    assert!(obs["enemyUnits"]
        .as_array()
        .unwrap()
        .iter()
        .all(|u| u["id"] != "BT"));
}

/// 同格交战 (三.15) engine wiring (T2.8): a unit entering a hex held by the enemy triggers an
/// immediate engagement round that damages the priority target; leaving draws a punishment shot.
#[cfg(test)]
#[test]
fn same_hex_engagement() {
    use crate::types::{HexCell, ScenarioUnit, Side, Sides, Terrain};

    let map = Map {
        format: "openstratcore.map".into(),
        version: 1,
        name: "sh".into(),
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
    let u = |id: &str, ut: UnitType, q: i32, teams: u8| ScenarioUnit {
        id: id.into(),
        unit_type: ut,
        armor: None,
        teams,
        at: Axial::new(q, 0),
        facing: 0,
        state: Some(UnitState::Stopped),
        carried_by: None,
        affiliated_to: None,
    };
    let rules = Rules::from_json_str(include_str!("../../../config/rules.default.json")).unwrap();
    // A Red tank drives into the hex held by a lone Blue infantry squad.
    let scenario = Scenario {
        format: "openstratcore.scenario".into(),
        version: 1,
        name: "sh".into(),
        map: "sh".into(),
        rules: None,
        time_limit_seconds: None,
        sides: Sides {
            red: Side {
                name: "Red".into(),
                units: vec![u("RT", UnitType::Tank, 0, 3)],
            },
            blue: Side {
                name: "Blue".into(),
                units: vec![u("BI", UnitType::Infantry, 1, 1)],
            },
        },
        objectives: vec![],
        facilities: vec![],
    };
    let mut e = Engine::new(map, scenario, rules, 4).unwrap();
    e.submit(
        SideId::Red,
        &serde_json::json!({ "op": "move_to", "unitId": "RT", "target": { "q": 1, "r": 0 } }),
        0,
    )
    .unwrap();
    e.step(3_000); // past the ~20 s one-hex drive + the immediate 同格交战 round
    let bi = e.state.units.get("BI").unwrap();
    let affected = !bi.alive || bi.teams < 1 || bi.suppressed_until > e.clock();
    assert!(
        affected,
        "三.15a/b: entering the enemy's hex immediately engages and hits the infantry"
    );
    // Hard rule #1: the recurring engagement must TERMINATE. Draining the whole event queue
    // completes (the stalemate guard bounds it even if no one dies) and the engagement ends.
    let mut guard = 0;
    while e.advance_to_next_event().is_some() {
        guard += 1;
        assert!(guard < 100_000, "engagement event loop did not terminate");
    }
    assert!(
        e.same_hex.is_empty(),
        "三.15f: the engagement ends once one side is gone (or the stalemate guard fires)"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load() -> Engine {
        let map: Map =
            serde_json::from_str(include_str!("../../../scenarios/maps/demo_valley.map.json"))
                .unwrap();
        let scenario: Scenario = serde_json::from_str(include_str!(
            "../../../scenarios/demo_skirmish.scenario.json"
        ))
        .unwrap();
        let rules =
            Rules::from_json_str(include_str!("../../../config/rules.default.json")).unwrap();
        Engine::new(map, scenario, rules, 12345).unwrap()
    }

    #[test]
    fn builds_initial_state() {
        let e = load();
        assert_eq!(e.state.units.len(), 4);
        assert!(e.state.units.contains_key("R-T1"));
        assert!(e.state.control.contains_key("CP1"));
    }

    #[test]
    fn stop_schedules_transition_deterministically() {
        let mut e = load();
        let cmd = serde_json::json!({ "op": "stop", "unitId": "R-T1" });
        e.submit(SideId::Red, &cmd, 0).unwrap();
        e.step(8000); // 80 s in cs; move_to_stop = 75 s = 7500 cs, so the transition fires
        assert_eq!(e.state.units["R-T1"].state, UnitState::Stopped);
    }

    #[test]
    fn unknown_op_errors_not_panics() {
        let mut e = load();
        let cmd = serde_json::json!({ "op": "teleport", "unitId": "R-T1" });
        assert!(e.submit(SideId::Red, &cmd, 0).is_err());
    }

    #[test]
    fn observation_reports_only_visible_enemies() {
        let e = load();
        let obs = e.observe(SideId::Red);
        // Real fog-of-war (三.7): the Demo Valley's central ridge (elev 5–6 between the two
        // sides) blocks every red↔blue line of sight at the start, so no blue unit is visible
        // — and none is leaked. (Controlled in-range / out-of-range / LOS-blocked show/hide
        // cases live in engine::fog_of_war.)
        let enemies = obs["enemyUnits"].as_array().unwrap();
        assert!(
            enemies.is_empty(),
            "the valley ridge blocks LOS to all blue at t=0"
        );
        for en in enemies {
            assert!(en.get("teams").is_none(), "enemy view must not leak teams");
            assert!(en.get("state").is_none(), "enemy view must not leak state");
        }
        assert_eq!(obs["ownUnits"].as_array().unwrap().len(), 2);
    }
}
