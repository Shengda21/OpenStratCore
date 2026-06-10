//! Per-unit-type special mechanics (三.10–三.13, 三.19): 巡飞弹 / 无人机 / 直升机 / 无人战车 /
//! 侦察战车. Movement and observation reuse [`crate::mechanics`]; this module holds the unit-class
//! predicates and lifetime/endurance constants. The stateful pieces (launch timers, self-destruct,
//! parent links) live in [`crate::engine`].

use crate::rules::Rules;
use crate::types::UnitType;

/// 三.10g — a 巡飞弹's 巡飞时长 in seconds before self-destruct (rules-as-data via `timing`).
pub fn loitering_endurance_seconds(rules: &Rules) -> Option<f64> {
    rules.timing("loitering_endurance")
}

/// 三.10 — a 巡飞弹 is launched by a carrier vehicle and self-destructs with it (三.10g/h).
pub fn is_loitering(t: UnitType) -> bool {
    matches!(t, UnitType::LoiteringMunition)
}

/// 三.16 运输直升机 (T3.2): 切换高度 (75 s, adjacent only), 超低空 half-speed, and 索降装载 (needs
/// 超低空 over 开阔地). Altitude is verified through behaviour (move speed / load gating).
#[cfg(test)]
#[test]
fn transport_heli() {
    use crate::engine::Engine;
    use crate::hex::Axial;
    use crate::rules::Rules as R;
    use crate::types::{
        HexCell, Map, Scenario, ScenarioUnit, Side, SideId, Sides, Terrain, UnitState,
    };

    let map = Map {
        format: "openstratcore.map".into(),
        version: 1,
        name: "th".into(),
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
    let rules = R::from_json_str(include_str!("../../../config/rules.default.json")).unwrap();
    let unit = |id: &str, ut: UnitType, q: i32| ScenarioUnit {
        id: id.into(),
        unit_type: ut,
        armor: None,
        teams: 1,
        at: Axial::new(q, 0),
        facing: 0,
        state: Some(UnitState::Stopped),
        carried_by: None,
        affiliated_to: None,
    };
    let make = || {
        let scenario = Scenario {
            format: "openstratcore.scenario".into(),
            version: 1,
            name: "th".into(),
            map: "th".into(),
            rules: None,
            time_limit_seconds: None,
            sides: Sides {
                red: Side {
                    name: "Red".into(),
                    units: vec![
                        unit("H", UnitType::TransportHeli, 0),
                        unit("I", UnitType::Infantry, 0),
                        unit("TK", UnitType::Tank, 0),
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
        Engine::new(map.clone(), scenario, rules.clone(), 1).unwrap()
    };
    let drain = |e: &mut Engine| while e.advance_to_next_event().is_some() {};
    let alt =
        |a: &str| serde_json::json!({ "op": "switch_altitude", "unitId": "H", "altitude": a });

    // (a) only a 运输直升机 has altitude states.
    let mut e = make();
    assert!(e
        .submit(
            SideId::Red,
            &serde_json::json!({ "op": "switch_altitude", "unitId": "TK", "altitude": "high" }),
            0
        )
        .is_err());
    // 三.16d: from 低空 (default) you may switch to 超低空 (adjacent), but not… well 超低空→高空 isn't.
    e.submit(SideId::Red, &alt("very_low"), 0).unwrap();
    drain(&mut e);
    // 三.16: the heli's own commander reads its current altitude band from the observation (own-side
    // gameplay state). It defaults to 低空 and now reflects the completed 超低空 switch.
    let obs = e.observe(SideId::Red);
    let h = obs["ownUnits"]
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["id"] == "H")
        .unwrap();
    assert_eq!(h["altitude"], "very_low");
    // Now at 超低空: a jump straight to 高空 is non-adjacent and refused (三.16d).
    assert!(
        e.submit(SideId::Red, &alt("high"), e.clock()).is_err(),
        "三.16d: 超低空 → 高空 is not an adjacent switch"
    );

    // (b) 三.16c: at 超低空 a one-hex flight takes twice as long as at 低空.
    let move_time = |very_low: bool| -> i64 {
        let mut e = make();
        if very_low {
            e.submit(SideId::Red, &alt("very_low"), 0).unwrap();
            drain(&mut e);
        }
        let before = e.clock();
        e.submit(
            SideId::Red,
            &serde_json::json!({ "op": "move_to", "unitId": "H", "target": { "q": 1, "r": 0 } }),
            before,
        )
        .unwrap();
        drain(&mut e);
        e.clock() - before
    };
    let low = move_time(false);
    let vlow = move_time(true);
    assert!(low > 0);
    assert_eq!(vlow, low * 2, "三.16c: 超低空 flies at half speed");

    // (c) 三.16e: 索降装载 needs the heli at 超低空; at 低空 it is refused, at 超低空 it loads.
    let mut e = make();
    let load = serde_json::json!({ "op": "mount", "unitId": "I", "vehicleId": "H" });
    assert!(
        e.submit(SideId::Red, &load, 0).is_err(),
        "三.16e: cannot load a 低空 heli"
    );
    e.submit(SideId::Red, &alt("very_low"), 0).unwrap();
    drain(&mut e);
    let now = e.clock();
    e.submit(SideId::Red, &load, now).unwrap();
    drain(&mut e);
    assert_eq!(
        e.state.units.get("I").unwrap().carried_by.as_deref(),
        Some("H"),
        "三.16e: the squad rides the 运输直升机 once it is 超低空 over 开阔地"
    );
}

/// 三.12 武装直升机 (T3.1): 10-hex personnel / 25-hex vehicle recon (over terrain from +200 m), seen
/// by the ground at 25 hexes, sees a 无人机 only adjacent, and flies at 4 s/hex over any terrain.
#[cfg(test)]
#[test]
fn attack_heli() {
    use crate::engine::Engine;
    use crate::hex::Axial;
    use crate::mechanics::{air_step_time_seconds, can_observe};
    use crate::rules::Rules as R;
    use crate::types::{
        Armor, HexCell, Map, RuntimeUnit, Scenario, ScenarioUnit, Side, SideId, Sides, Terrain,
        UnitState, WeaponState,
    };

    let rules = R::from_json_str(include_str!("../../../config/rules.default.json")).unwrap();
    let flat = Map {
        format: "openstratcore.map".into(),
        version: 1,
        name: "h".into(),
        elevation_unit_meters: Some(10),
        hexes: (0..=27)
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
    let ru = |id: &str, ut: UnitType, q: i32| RuntimeUnit {
        id: id.into(),
        side: SideId::Red,
        unit_type: ut,
        armor: Armor::Medium,
        teams: 1,
        pos: Axial::new(q, 0),
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
    let heli = ru("H", UnitType::AttackHeli, 0);
    // 三.12b: personnel at 10, vehicle at 25.
    assert!(can_observe(
        &rules,
        &flat,
        &heli,
        &ru("I", UnitType::Infantry, 10)
    ));
    assert!(!can_observe(
        &rules,
        &flat,
        &heli,
        &ru("I", UnitType::Infantry, 11)
    ));
    assert!(can_observe(
        &rules,
        &flat,
        &heli,
        &ru("V", UnitType::Tank, 25)
    ));
    assert!(!can_observe(
        &rules,
        &flat,
        &heli,
        &ru("V", UnitType::Tank, 26)
    ));
    // 三.12c: the ground sees an attack heli at 25 hexes.
    let inf = ru("G", UnitType::Infantry, 0);
    assert!(can_observe(
        &rules,
        &flat,
        &inf,
        &ru("H", UnitType::AttackHeli, 25)
    ));
    assert!(!can_observe(
        &rules,
        &flat,
        &inf,
        &ru("H", UnitType::AttackHeli, 26)
    ));
    // 三.12c: a heli sees a 无人机 only when adjacent.
    assert!(can_observe(
        &rules,
        &flat,
        &heli,
        &ru("U", UnitType::Uav, 1)
    ));
    assert!(!can_observe(
        &rules,
        &flat,
        &heli,
        &ru("U", UnitType::Uav, 2)
    ));

    // 三.12a: the heli flies at 4 s/hex.
    assert_eq!(
        air_step_time_seconds(&rules, UnitType::AttackHeli),
        Some(4.0)
    );

    // …and crosses an impassable hill that halts a ground unit.
    let hill = Map {
        format: "openstratcore.map".into(),
        version: 1,
        name: "hh".into(),
        elevation_unit_meters: Some(10),
        hexes: (0..=2)
            .map(|q| HexCell {
                q,
                r: 0,
                id: None,
                elevation: if q == 1 { 9 } else { 0 },
                terrain: Terrain::Open,
                road: None,
            })
            .collect(),
    };
    let su = |id: &str, ut: UnitType| ScenarioUnit {
        id: id.into(),
        unit_type: ut,
        armor: None,
        teams: 1,
        at: Axial::new(0, 0),
        facing: 0,
        state: Some(UnitState::Stopped),
        carried_by: None,
        affiliated_to: None,
    };
    let scenario = Scenario {
        format: "openstratcore.scenario".into(),
        version: 1,
        name: "hh".into(),
        map: "hh".into(),
        rules: None,
        time_limit_seconds: None,
        sides: Sides {
            red: Side {
                name: "Red".into(),
                units: vec![su("H", UnitType::AttackHeli)],
            },
            blue: Side {
                name: "Blue".into(),
                units: vec![],
            },
        },
        objectives: vec![],
        facilities: vec![],
    };
    let mut e = Engine::new(hill, scenario, rules, 1).unwrap();
    e.submit(
        SideId::Red,
        &serde_json::json!({ "op": "move_to", "unitId": "H", "target": { "q": 2, "r": 0 } }),
        0,
    )
    .unwrap();
    while e.advance_to_next_event().is_some() {}
    assert_eq!(
        e.state.units.get("H").unwrap().pos,
        Axial::new(2, 0),
        "三.12a: the attack heli flies over the hill"
    );
}

/// 三.19 侦察与校射 (T2.7): a 侦察型战车 reaches 50-hex ground recon, and a 炮兵校射雷达 spins up over
/// 75 s, then floors all friendly 间瞄 to 格内校射 — losing the effect on 机动 (三.19e) or 压制 (三.19f).
#[cfg(test)]
#[test]
fn recon_and_radar() {
    use crate::combat::IndirectSpotting;
    use crate::engine::Engine;
    use crate::hex::Axial;
    use crate::mechanics::can_observe;
    use crate::rules::Rules as R;
    use crate::types::{
        Armor, HexCell, Map, RuntimeUnit, Scenario, ScenarioUnit, Side, SideId, Sides, Terrain,
        UnitState, WeaponState,
    };

    let rules = R::from_json_str(include_str!("../../../config/rules.default.json")).unwrap();

    // --- 三.19b: 侦察型战车 ground recon to 50 hexes ---
    let flat = Map {
        format: "openstratcore.map".into(),
        version: 1,
        name: "r".into(),
        elevation_unit_meters: Some(10),
        hexes: (0..=52)
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
    let ru = |id: &str, ut: UnitType, q: i32| RuntimeUnit {
        id: id.into(),
        side: SideId::Red,
        unit_type: ut,
        armor: Armor::Medium,
        teams: 1,
        pos: Axial::new(q, 0),
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
    let recon = ru("R", UnitType::ReconVehicle, 0);
    assert!(
        can_observe(&rules, &flat, &recon, &ru("V", UnitType::Tank, 50)),
        "三.19b: 侦察型战车 sees a vehicle at 50 hexes"
    );
    assert!(
        !can_observe(&rules, &flat, &recon, &ru("V", UnitType::Tank, 51)),
        "三.19b: …but not at 51"
    );
    assert!(can_observe(
        &rules,
        &flat,
        &recon,
        &ru("I", UnitType::Infantry, 20)
    ));
    assert!(!can_observe(
        &rules,
        &flat,
        &recon,
        &ru("I", UnitType::Infantry, 21)
    ));

    // --- 三.16b: 运输直升机 altitude drives how far it is seen over terrain ---
    // A ridge (elevation 5 ≡ +50 m at 10 m/level) at (1,0) sits between the 侦察型战车 at (0,0) and a
    // heli at (2,0): the heli clears the crest 低空 (+200 m) / 高空 (+500 m) but is hidden 超低空
    // (+20 m, below the +50 m ridge). Locks the altitude-dependent fog fix — a 超低空 heli leaks no
    // contact it shouldn't (rule #5).
    let ridge = Map {
        format: "openstratcore.map".into(),
        version: 1,
        name: "ridge".into(),
        elevation_unit_meters: Some(10),
        hexes: (0..=2)
            .map(|q| HexCell {
                q,
                r: 0,
                id: None,
                elevation: if q == 1 { 5 } else { 0 },
                terrain: Terrain::Open,
                road: None,
            })
            .collect(),
    };
    let heli = |alt: crate::types::HeliAlt| RuntimeUnit {
        id: "H".into(),
        side: SideId::Blue,
        unit_type: UnitType::TransportHeli,
        armor: Armor::Light,
        teams: 1,
        pos: Axial::new(2, 0),
        facing: 0,
        state: UnitState::Stopped,
        weapon_state: WeaponState::Deployed,
        busy_until: 0,
        suppressed_until: 0,
        alive: true,
        carried_by: None,
        affiliated_to: None,
        heli_alt: alt,
        inside_facility: None,
        fatigue: 0,
    };
    use crate::types::HeliAlt;
    assert!(
        can_observe(&rules, &ridge, &recon, &heli(HeliAlt::Low)),
        "三.16b: a 低空 heli (+200 m) clears the +50 m ridge"
    );
    assert!(
        can_observe(&rules, &ridge, &recon, &heli(HeliAlt::High)),
        "三.16b: a 高空 heli (+500 m) clears the ridge"
    );
    assert!(
        !can_observe(&rules, &ridge, &recon, &heli(HeliAlt::VeryLow)),
        "三.16b: a 超低空 heli (+20 m) is hidden behind the +50 m ridge — no fog leak"
    );

    // --- 三.19c/d/e/f: 炮兵校射雷达 ---
    // A hill at (1,0) blocks any ground LOS from (0,0) to the plan hex (2,0): without the radar a
    // 间瞄 there is 无校射; with it 开机, it becomes 格内校射.
    let hill = Map {
        format: "openstratcore.map".into(),
        version: 1,
        name: "rad".into(),
        elevation_unit_meters: Some(10),
        hexes: (0..=2)
            .map(|q| HexCell {
                q,
                r: 0,
                id: None,
                elevation: if q == 1 { 9 } else { 0 },
                terrain: Terrain::Open,
                road: None,
            })
            .collect(),
    };
    let u = |id: &str, ut: UnitType| ScenarioUnit {
        id: id.into(),
        unit_type: ut,
        armor: None,
        teams: 1,
        at: Axial::new(0, 0),
        facing: 0,
        state: Some(UnitState::Stopped),
        carried_by: None,
        affiliated_to: None,
    };
    let scenario = Scenario {
        format: "openstratcore.scenario".into(),
        version: 1,
        name: "rad".into(),
        map: "rad".into(),
        rules: None,
        time_limit_seconds: None,
        sides: Sides {
            red: Side {
                name: "Red".into(),
                units: vec![
                    u("RAD", UnitType::RadarVehicle),
                    u("ART", UnitType::Artillery),
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
    let mut e = Engine::new(hill.clone(), scenario, rules.clone(), 1).unwrap();
    let plan = Axial::new(2, 0);
    let on = serde_json::json!({ "op": "radar_on", "unitId": "RAD" });

    // Before 开机: no ground LOS to the plan hex ⇒ 无校射.
    assert_eq!(e.spotting_level(plan, SideId::Red), IndirectSpotting::None);
    // 三.19d: spin-up takes 75 s; not yet ON before then.
    e.submit(SideId::Red, &on, 0).unwrap();
    assert_eq!(e.spotting_level(plan, SideId::Red), IndirectSpotting::None);
    e.step(8_000); // past 75 s
                   // 三.19c: now 开机 ⇒ all 间瞄 floored to 格内校射.
    assert_eq!(e.spotting_level(plan, SideId::Red), IndirectSpotting::InHex);

    // 三.19f: 压制 the radar ⇒ effect off; auto-recovers when 压制 lifts.
    let now = e.clock();
    if let Some(r) = e.state.units.get_mut("RAD") {
        r.suppressed_until = now + 5_000;
    }
    assert_eq!(e.spotting_level(plan, SideId::Red), IndirectSpotting::None);
    if let Some(r) = e.state.units.get_mut("RAD") {
        r.suppressed_until = 0;
    }
    assert_eq!(
        e.spotting_level(plan, SideId::Red),
        IndirectSpotting::InHex,
        "三.19f: radar effect auto-recovers after 压制"
    );

    // 三.19e: ordering 机动 turns the radar OFF at once.
    let go = serde_json::json!({ "op": "move_to", "unitId": "RAD", "target": { "q": 1, "r": 0 } });
    e.submit(SideId::Red, &go, e.clock()).unwrap();
    assert_eq!(
        e.spotting_level(plan, SideId::Red),
        IndirectSpotting::None,
        "三.19e: a moving radar is off"
    );
}

/// 三.11 无人机 (T2.5): 2-hex ground recon (seeing OVER terrain from +200 m), seen only when
/// adjacent, and flying at base speed across any terrain.
#[cfg(test)]
#[test]
fn uav() {
    use crate::engine::Engine;
    use crate::hex::Axial;
    use crate::mechanics::can_observe;
    use crate::rules::Rules as R;
    use crate::types::{
        Armor, HexCell, Map, RuntimeUnit, Scenario, ScenarioUnit, Side, SideId, Sides, Terrain,
        UnitState, WeaponState,
    };

    // A hill at (1,0) blocks a GROUND line of sight from (0,0) to (2,0); the air units fly over it.
    let map = Map {
        format: "openstratcore.map".into(),
        version: 1,
        name: "uav".into(),
        elevation_unit_meters: Some(10),
        hexes: (0..=3)
            .map(|q| HexCell {
                q,
                r: 0,
                id: None,
                elevation: if q == 1 { 9 } else { 0 },
                terrain: Terrain::Open,
                road: None,
            })
            .collect(),
    };
    let rules = R::from_json_str(include_str!("../../../config/rules.default.json")).unwrap();
    let ru = |id: &str, ut: UnitType, q: i32| RuntimeUnit {
        id: id.into(),
        side: SideId::Red,
        unit_type: ut,
        armor: Armor::None,
        teams: 1,
        pos: Axial::new(q, 0),
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

    // (a) 三.11b recon distance 2: the UAV sees ground at 2 hexes, not 3.
    let uav0 = ru("U", UnitType::Uav, 0);
    assert!(
        can_observe(&rules, &map, &uav0, &ru("I", UnitType::Infantry, 2)),
        "三.11b: UAV observes ground at 2 hexes"
    );
    assert!(
        !can_observe(&rules, &map, &uav0, &ru("I", UnitType::Infantry, 3)),
        "三.11b: …but not at 3 hexes"
    );

    // (b) altitude (三.11a +200 m): the UAV sees the (2,0) target OVER the (1,0) hill, while a
    // GROUND infantry at the same spot is blocked by that hill.
    let inf0 = ru("G", UnitType::Infantry, 0);
    let target2 = ru("T", UnitType::Infantry, 2);
    assert!(
        can_observe(&rules, &map, &uav0, &target2),
        "三.11a: a UAV flies over the hill and sees the target"
    );
    assert!(
        !can_observe(&rules, &map, &inf0, &target2),
        "a ground observer at the same spot is blocked by the hill"
    );

    // (c) 三.11b ground units see a UAV only when adjacent.
    let inf2 = ru("G2", UnitType::Infantry, 2);
    assert!(
        can_observe(&rules, &map, &inf2, &ru("U", UnitType::Uav, 3)),
        "三.11b: a UAV is visible adjacent"
    );
    assert!(
        !can_observe(&rules, &map, &inf2, &ru("U", UnitType::Uav, 0)),
        "三.11b: …but not at 2 hexes"
    );

    // (d) 三.11a air movement: the UAV flies across the impassable (1,0) hill that halts a ground
    // tank. (slope 9 > the 5-level off-road limit.)
    let unit = |id: &str, ut: UnitType, q: i32| ScenarioUnit {
        id: id.into(),
        unit_type: ut,
        armor: None,
        teams: 1,
        at: Axial::new(q, 0),
        facing: 0,
        state: Some(UnitState::Stopped),
        carried_by: None,
        affiliated_to: None,
    };
    let scenario = Scenario {
        format: "openstratcore.scenario".into(),
        version: 1,
        name: "uav".into(),
        map: "uav".into(),
        rules: None,
        time_limit_seconds: None,
        sides: Sides {
            red: Side {
                name: "Red".into(),
                units: vec![unit("U", UnitType::Uav, 0), unit("TK", UnitType::Tank, 0)],
            },
            blue: Side {
                name: "Blue".into(),
                units: vec![],
            },
        },
        objectives: vec![],
        facilities: vec![],
    };
    let mut e = Engine::new(map.clone(), scenario, rules.clone(), 1).unwrap();
    let go = |id: &str| serde_json::json!({ "op": "move_to", "unitId": id, "target": { "q": 2, "r": 0 } });
    e.submit(SideId::Red, &go("U"), 0).unwrap();
    e.submit(SideId::Red, &go("TK"), 0).unwrap();
    while e.advance_to_next_event().is_some() {}
    assert_eq!(
        e.state.units.get("U").unwrap().pos,
        Axial::new(2, 0),
        "三.11a: the UAV flies over the hill to its target"
    );
    assert_eq!(
        e.state.units.get("TK").unwrap().pos,
        Axial::new(0, 0),
        "the ground tank cannot climb the impassable hill"
    );
}

#[cfg(test)]
#[test]
fn loitering() {
    use crate::engine::Engine;
    use crate::hex::Axial;
    use crate::rules::Rules as R;
    use crate::types::{
        HexCell, Map, Scenario, ScenarioUnit, Side, SideId, Sides, Terrain, UnitState,
    };

    let map = Map {
        format: "openstratcore.map".into(),
        version: 1,
        name: "loi".into(),
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
    // The killer 坦克 carries 4 vehicles for a strong shot; the IFV carrier is a single unarmoured
    // vehicle so seed 1's point-blank 大号直瞄炮 reliably wrecks it.
    let unit = |id: &str, ut: UnitType, q: i32, carried: Option<&str>| ScenarioUnit {
        id: id.into(),
        unit_type: ut,
        armor: None,
        teams: if matches!(ut, UnitType::Tank) { 4 } else { 1 },
        at: Axial::new(q, 0),
        facing: 0,
        state: Some(UnitState::Stopped),
        carried_by: carried.map(|s| s.to_string()),
        affiliated_to: None,
    };
    let rules = R::from_json_str(include_str!("../../../config/rules.default.json")).unwrap();
    assert_eq!(loitering_endurance_seconds(&rules), Some(1200.0));

    // A 战车 (IFV) carries one loaded 巡飞弹 LM; a Blue tank can later wreck the carrier.
    let make = |seed: u64| {
        let scenario = Scenario {
            format: "openstratcore.scenario".into(),
            version: 1,
            name: "loi".into(),
            map: "loi".into(),
            rules: None,
            time_limit_seconds: None,
            sides: Sides {
                red: Side {
                    name: "Red".into(),
                    units: vec![
                        unit("V", UnitType::Ifv, 0, None),
                        unit("LM", UnitType::LoiteringMunition, 0, Some("V")),
                    ],
                },
                blue: Side {
                    name: "Blue".into(),
                    units: vec![unit("B", UnitType::Tank, 1, None)],
                },
            },
            objectives: vec![],
            facilities: vec![],
        };
        Engine::new(map.clone(), scenario, rules.clone(), seed).unwrap()
    };
    let launch = serde_json::json!({ "op": "launch_loitering", "unitId": "LM", "targetArea": { "q": 3, "r": 0 } });
    let carried_by = |e: &Engine, id: &str| e.state.units.get(id).unwrap().carried_by.clone();
    let alive = |e: &Engine, id: &str| e.state.units.get(id).unwrap().alive;

    // (a) launch takes 75 s, after which the 巡飞弹 deploys (no longer carried).
    let mut e = make(1);
    e.submit(SideId::Red, &launch, 0).unwrap();
    assert_eq!(carried_by(&e, "LM"), Some("V".to_string()), "still loaded");
    e.step(8_000); // past the 75 s 发射
    assert_eq!(carried_by(&e, "LM"), None, "三.10d: 巡飞弹 deployed");
    assert!(alive(&e, "LM"));

    // (b) a 战车 may only have one 巡飞弹 in flight (三.10e) — a second launch is refused (no second
    // loaded round exists here, which the op also rejects).
    assert!(
        e.submit(SideId::Red, &launch, 8_100).is_err(),
        "三.10d/e: one 巡飞弹 per vehicle at a time"
    );

    // (c) 三.10g 巡飞时长 1200 s → self-destruct.
    let mut e = make(2);
    e.submit(SideId::Red, &launch, 0).unwrap();
    e.step(8_000);
    assert!(alive(&e, "LM"));
    e.step(125_000); // 1250 s > 75 + 1200
    assert!(
        !alive(&e, "LM"),
        "三.10g: 巡飞弹 self-destructs after 1200 s"
    );

    // (d) 三.10h: a destroyed carrier takes a still-LOADED 巡飞弹 with it. B at (1,0) shells V at
    // (0,0) — adjacent, observable — and seed 1 wrecks the single-vehicle IFV.
    let kill = serde_json::json!({ "op": "fire_direct", "unitId": "B", "weapon": "大号直瞄炮", "targetUnit": "V" });
    let mut e = make(1);
    e.submit(SideId::Blue, &kill, 0).unwrap();
    assert!(!alive(&e, "V"), "the IFV carrier was wrecked");
    assert!(
        !alive(&e, "LM"),
        "三.10h: a loaded 巡飞弹 dies with its wrecked carrier"
    );

    // …and 三.10g/h: a carrier destroyed after the 巡飞弹 has DEPLOYED self-destructs it too.
    let mut e = make(1);
    e.submit(SideId::Red, &launch, 0).unwrap();
    e.step(8_000);
    assert_eq!(carried_by(&e, "LM"), None);
    e.submit(SideId::Blue, &kill, 8_100).unwrap();
    assert!(!alive(&e, "V"));
    assert!(
        !alive(&e, "LM"),
        "三.10g/h: a deployed 巡飞弹 self-destructs when its carrier is destroyed"
    );

    // (e) double-launch race (三.10e): with TWO loaded munitions on one carrier, the second launch
    // is refused while the first is still inside its 75 s 发射 (not yet aloft).
    let scenario2 = Scenario {
        format: "openstratcore.scenario".into(),
        version: 1,
        name: "loi".into(),
        map: "loi".into(),
        rules: None,
        time_limit_seconds: None,
        sides: Sides {
            red: Side {
                name: "Red".into(),
                units: vec![
                    unit("V", UnitType::Ifv, 0, None),
                    unit("LM1", UnitType::LoiteringMunition, 0, Some("V")),
                    unit("LM2", UnitType::LoiteringMunition, 0, Some("V")),
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
    let mut e = Engine::new(map.clone(), scenario2, rules.clone(), 1).unwrap();
    let launch_n = |n: &str| serde_json::json!({ "op": "launch_loitering", "unitId": n, "targetArea": { "q": 3, "r": 0 } });
    e.submit(SideId::Red, &launch_n("LM1"), 0).unwrap();
    assert!(
        e.submit(SideId::Red, &launch_n("LM2"), 10).is_err(),
        "三.10e: cannot launch a second 巡飞弹 while the first is still launching"
    );
}

/// 三.13 无人战车 (T3.3): a UGV uses the vehicle rule-set (a), is 隶属 to exactly one manned vehicle
/// (b), may guide ONLY that vehicle's 重型导弹 once dismounted & stopped (c / 三.14b), and is
/// annihilated together with that vehicle (d).
#[cfg(test)]
#[test]
fn ugv() {
    use crate::engine::Engine;
    use crate::hex::Axial;
    use crate::mechanics::{is_ground, step_time_seconds};
    use crate::prob::Outcome;
    use crate::rules::Rules as R;
    use crate::types::{
        Armor, HexCell, Map, Scenario, ScenarioUnit, Side, SideId, Sides, Terrain, UnitState,
        WeaponState,
    };

    // (a) 机动同战车: a UGV is a ground unit and steps through the *vehicle* movement helper (terrain
    // factor applies), exactly like a tank — not the terrain-independent infantry path.
    assert!(is_ground(UnitType::Ugv));
    let rules = R::from_json_str(include_str!("../../../config/rules.default.json")).unwrap();
    let ugv_open = step_time_seconds(&rules, UnitType::Ugv, Terrain::Open, 0, 0).unwrap();
    let ugv_forest = step_time_seconds(&rules, UnitType::Ugv, Terrain::Forest, 0, 0).unwrap();
    assert!(ugv_open.is_some());
    assert!(
        ugv_forest > ugv_open,
        "三.13a: the UGV pays the vehicle terrain penalty (forest slower than open)"
    );

    let map = Map {
        format: "openstratcore.map".into(),
        version: 1,
        name: "ugv".into(),
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
    // V = the UGV's 隶属 manned vehicle; W = an unrelated friendly 重型导弹 vehicle; U = the UGV
    // (dismounted, stopped, weapon ready); T = the enemy U observes (adjacent).
    let mk = |id: &str, ut: UnitType, q: i32, aff: Option<&str>| ScenarioUnit {
        id: id.into(),
        unit_type: ut,
        armor: Some(Armor::Medium),
        teams: 3,
        at: Axial::new(q, 0),
        facing: 0,
        state: Some(UnitState::Stopped),
        carried_by: None,
        affiliated_to: aff.map(|s| s.into()),
    };
    let scenario = Scenario {
        format: "openstratcore.scenario".into(),
        version: 1,
        name: "ugv".into(),
        map: "ugv".into(),
        rules: None,
        time_limit_seconds: None,
        sides: Sides {
            red: Side {
                name: "Red".into(),
                units: vec![
                    mk("V", UnitType::Tank, 0, None),
                    mk("W", UnitType::Tank, 1, None),
                    mk("U", UnitType::Ugv, 4, Some("V")),
                    // 三.14b: a 步兵 may also be 隶属 to V (to guide it) — but 三.13d is UGV-only, so FI
                    // must SURVIVE V's death. Dismounted (not aboard) so the carriage path won't take it.
                    mk("FI", UnitType::Infantry, 2, Some("V")),
                ],
            },
            blue: Side {
                name: "Blue".into(),
                units: vec![mk("T", UnitType::Tank, 5, None)],
            },
        },
        objectives: vec![],
        facilities: vec![],
    };

    // (b/三.13b) load-time validation: a 隶属 vehicle must be a same-side manned vehicle — pointing a
    // UGV at the enemy (or a missing/air/self id) is rejected by Engine::new.
    let mut bad = scenario.clone();
    bad.sides.red.units[2].affiliated_to = Some("T".into()); // U → enemy T
    assert!(
        Engine::new(map.clone(), bad, rules.clone(), 7).is_err(),
        "三.13b: a UGV cannot be 隶属 to an enemy vehicle"
    );
    // 三.13b: a UGV MUST declare a 隶属 vehicle — an unaffiliated UGV is invalid.
    let mut orphan = scenario.clone();
    orphan.sides.red.units[2].affiliated_to = None; // U with no 隶属
    assert!(
        Engine::new(map.clone(), orphan, rules.clone(), 7).is_err(),
        "三.13b: a 无人战车 must belong to a manned vehicle"
    );

    let mut e = Engine::new(map, scenario, rules, 7).unwrap();

    // 战争迷雾 (rule #5): blue observes the adjacent enemy UGV U, but the projection must NEVER carry
    // U's 隶属 link — that would reveal red's vehicle V even when V is unseen. The enemy view is a
    // strict {id,type,at} whitelist; lock it so a future serializer change cannot leak affiliation.
    let blue_view = e.observe(SideId::Blue);
    let seen_u = blue_view["enemyUnits"]
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["id"] == "U")
        .expect("blue observes the adjacent enemy UGV");
    assert!(
        seen_u.get("affiliatedTo").is_none() && seen_u.get("affiliated_to").is_none(),
        "rule #5: an enemy UGV's 隶属 vehicle must never leak through observation"
    );

    let guide = |veh: &str| {
        serde_json::json!({ "op": "guide_fire", "unitId": veh, "guideId": "U",
                            "weapon": "重型导弹", "targetUnit": "T" })
    };

    // (b/三.14b) the UGV may NOT guide W — it is 隶属 to V, not W.
    let err = e.submit(SideId::Red, &guide("W"), 0).unwrap_err();
    assert!(
        format!("{err:?}").contains("三.14b"),
        "三.14b: a UGV only guides its 隶属 vehicle, got {err:?}"
    );

    // (c) 三.13c: a 无人战车 guides only "在停止状态下" — a 掩蔽 (Cover) UGV is refused.
    e.state.units.get_mut("U").unwrap().state = UnitState::Cover;
    let err = e.submit(SideId::Red, &guide("V"), 0).unwrap_err();
    assert!(
        format!("{err:?}").contains("三.13c"),
        "三.13c: a non-stopped UGV cannot guide, got {err:?}"
    );
    e.state.units.get_mut("U").unwrap().state = UnitState::Stopped;

    // (c) the UGV guides ITS vehicle V's 重型导弹 onto T (V needs no 通视 of its own) — resolves,
    // and 三.14f puts both the guide and the firer into their 75 s prep.
    e.submit(SideId::Red, &guide("V"), 0).unwrap();
    assert_eq!(
        e.state.units.get("V").unwrap().weapon_state,
        WeaponState::Cooling,
        "三.14f: the 隶属 vehicle is cooling after the guided shot"
    );
    assert!(
        e.submit(SideId::Red, &guide("V"), 100).is_err(),
        "三.14f: the UGV is in its 75 s prep and cannot immediately re-guide"
    );

    // (d) destroying the 隶属 vehicle V annihilates the UGV with it (through the real death path).
    assert!(e.state.units.get("U").unwrap().alive);
    e.apply_direct_fire("V", Outcome::Kill, e.state.clock)
        .unwrap();
    assert!(!e.state.units.get("V").unwrap().alive);
    let u = e.state.units.get("U").unwrap();
    assert!(
        !u.alive,
        "三.13d: the UGV is annihilated with its 隶属 vehicle"
    );
    assert_eq!(u.teams, 0);
    // …but the 步兵 affiliated to V survives — 三.13d is UGV-specific, not a general 隶属 cascade.
    assert!(
        e.state.units.get("FI").unwrap().alive,
        "三.13d is UGV-only: an affiliated 步兵 is not annihilated with the vehicle"
    );
}

/// 三.22 天基侦察 (T4.3): the support 算子 cannot move / capture / be targeted (三.22b), and its
/// side-wide reveal shows only the lowest-id enemy 算子 per hex (三.22c). The fog plumbing into the
/// per-side observation + the "not direct fire" gate (三.22d) and 工事 exclusion are deferred.
#[cfg(test)]
#[test]
fn space_recon() {
    use crate::hex::Axial;
    use crate::mechanics::{can_be_targeted, can_capture, is_space_recon, space_recon_reveals};
    use crate::types::{Armor, RuntimeUnit, SideId, State, UnitState, WeaponState};
    use std::collections::BTreeMap;

    // 三.22b: the 天基侦察算子 is inert on the board — cannot 夺控 nor be a fire target; a normal unit can.
    assert!(is_space_recon(UnitType::SpaceRecon));
    assert!(!is_space_recon(UnitType::ReconVehicle));
    assert!(!can_capture(UnitType::SpaceRecon));
    assert!(!can_be_targeted(UnitType::SpaceRecon));
    assert!(can_be_targeted(UnitType::Tank));
    assert!(can_be_targeted(UnitType::Uav)); // a normal target (e.g. for AA)

    // 三.22b 不可机动: it has no base speed in config, so the movement helper refuses to step it
    // (the engine's move pathing therefore can never advance it — movement inertness is structural).
    let rules =
        crate::rules::Rules::from_json_str(include_str!("../../../config/rules.default.json"))
            .unwrap();
    assert!(crate::mechanics::step_time_seconds(
        &rules,
        UnitType::SpaceRecon,
        crate::types::Terrain::Open,
        0,
        0
    )
    .is_err());

    let ru = |id: &str, side: SideId, ut: UnitType, q: i32, carried: Option<&str>| RuntimeUnit {
        id: id.into(),
        side,
        unit_type: ut,
        armor: Armor::None,
        teams: 1,
        pos: Axial::new(q, 0),
        facing: 0,
        state: UnitState::Stopped,
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

    // Blue has two units in hex q=1 (B-T2, B-T9) and one in q=3 (B-I5); a carried B-P0 rides B-T2.
    let blue = || {
        vec![
            ru("B-T9", SideId::Blue, UnitType::Tank, 1, None),
            ru("B-T2", SideId::Blue, UnitType::Tank, 1, None),
            ru("B-P0", SideId::Blue, UnitType::Infantry, 1, Some("B-T2")),
            ru("B-I5", SideId::Blue, UnitType::Infantry, 3, None),
        ]
    };

    // 三.22c: WITHOUT a 天基侦察算子, Red reveals nothing this way.
    let s = mk(blue());
    assert!(space_recon_reveals(&s, SideId::Red).is_empty());

    // WITH a Red 天基侦察算子: one id per occupied enemy hex, the lowest — B-T2 (not B-T9) in q=1,
    // B-I5 in q=3. The carried B-P0 rides inside its carrier and is not revealed.
    let mut us = blue();
    us.push(ru("R-SR", SideId::Red, UnitType::SpaceRecon, 0, None));
    let s = mk(us);
    assert_eq!(
        space_recon_reveals(&s, SideId::Red),
        vec!["B-I5".to_string(), "B-T2".to_string()]
    );
}
