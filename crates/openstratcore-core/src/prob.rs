//! Pluggable probability providers (decision Q1-P3).
//!
//! Adjudication never hardcodes a table lookup; it goes through a [`ProbProvider`].
//! `rules.prob.providers[<table>]` selects the backend per result table:
//!   - `static`   : look up the result-table cells in the rules config (matches the ruleset).
//!   - `bayesian` : Dirichlet-Multinomial calibrated outcome distribution (offline-calibrated
//!     from ① replay outcomes and ② expert priors; see python/prob_learning).
//!   - `model`    : parametric/learned model hook (stub for now).

use crate::rng::Rng;
use crate::rules::Rules;

/// Result of one adjudication. Semantics match the ruleset's tables:
/// `Destroyed(n)` = n teams/vehicles destroyed; `Suppress`; `NoEffect`; `Kill` = annihilate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Destroyed(u8),
    Suppress,
    NoEffect,
    Kill,
}

impl Outcome {
    /// Parse a result-table cell value: integer -> Destroyed(n); "S"/"N"/"K" -> variants.
    pub fn parse(v: &serde_json::Value) -> Option<Outcome> {
        if let Some(n) = v.as_i64() {
            return Some(Outcome::Destroyed(n.max(0) as u8));
        }
        match v.as_str()? {
            "S" => Some(Outcome::Suppress),
            "N" => Some(Outcome::NoEffect),
            "K" => Some(Outcome::Kill),
            s => s.parse::<u8>().ok().map(Outcome::Destroyed),
        }
    }
}

/// Everything an adjudication needs, independent of which provider resolves it.
#[derive(Debug, Clone)]
pub struct ResolveContext {
    pub table_id: String,
    /// Effective attack level AFTER all modifiers have been applied by the caller.
    pub attack_level: i64,
}

pub trait ProbProvider {
    fn resolve(&self, ctx: &ResolveContext, rng: &mut dyn Rng) -> Outcome;
}

/// Static lookup against `rules.combat_result_tables[table]`.
pub struct StaticTable {
    /// dice count (1 or 2) and cells: attack_level(str) -> random_sum(str) -> outcome value.
    dice: u32,
    cells: serde_json::Value,
}

impl StaticTable {
    pub fn from_rules(rules: &Rules, table_id: &str) -> Option<Self> {
        let t = rules.result_table(table_id)?;
        let dice = t.get("randomDice")?.as_u64()? as u32;
        let cells = t.get("cells")?.clone();
        Some(Self { dice, cells })
    }
}

impl ProbProvider for StaticTable {
    fn resolve(&self, ctx: &ResolveContext, rng: &mut dyn Rng) -> Outcome {
        // Clamp attack level to the nearest available row at or below it.
        let row = best_row(&self.cells, ctx.attack_level);
        let roll = rng.roll_sum(self.dice);
        if let Some(level_obj) = row {
            if let Some(v) = level_obj.get(roll.to_string()) {
                if let Some(o) = Outcome::parse(v) {
                    return o;
                }
            }
        }
        Outcome::NoEffect
    }
}

/// Pick the highest attack-level row that is <= the requested level (graceful clamp).
fn best_row(cells: &serde_json::Value, level: i64) -> Option<&serde_json::Value> {
    let obj = cells.as_object()?;
    let mut best_key: Option<i64> = None;
    for k in obj.keys() {
        if let Ok(lvl) = k.parse::<i64>() {
            if lvl <= level && best_key.is_none_or(|b| lvl > b) {
                best_key = Some(lvl);
            }
        }
    }
    let key = best_key.or_else(|| {
        // fall back to the smallest available row if level is below all
        obj.keys().filter_map(|k| k.parse::<i64>().ok()).min()
    })?;
    obj.get(&key.to_string())
}

/// Dirichlet-Multinomial calibrated provider (decision Q1-P3). Holds the posterior outcome
/// concentrations per attack level (`params.byLevel[level][outcome] = α`), calibrated offline from
/// ① replay outcomes and ② expert priors (python/prob_learning/calibrate.py). At resolve time it
/// samples a categorical outcome from the posterior-mean weights for the row, deterministically via
/// `rng` — so the same seed + command stream still replays identically (hard rule #1).
pub struct BayesianTable {
    by_level: serde_json::Value,
}

impl BayesianTable {
    pub fn from_params(params: serde_json::Value) -> Self {
        let by_level = params
            .get("byLevel")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        Self { by_level }
    }
}

impl ProbProvider for BayesianTable {
    fn resolve(&self, ctx: &ResolveContext, rng: &mut dyn Rng) -> Outcome {
        // Pick the calibrated row for this attack level (same clamp as the static table).
        let Some(row) = best_row(&self.by_level, ctx.attack_level).and_then(|r| r.as_object())
        else {
            return Outcome::NoEffect;
        };
        // Stable (key-sorted) list of positive-weight outcomes so the categorical draw is
        // deterministic regardless of the JSON map's backing order (hard rule #1).
        let mut items: Vec<(&String, f64)> = row
            .iter()
            .filter_map(|(k, v)| v.as_f64().map(|w| (k, w)))
            .filter(|(_, w)| *w > 0.0)
            .collect();
        items.sort_by(|a, b| a.0.cmp(b.0));
        let total: f64 = items.iter().map(|(_, w)| *w).sum();
        if items.is_empty() || total <= 0.0 {
            return Outcome::NoEffect;
        }
        // Uniform u in [0, total) via the integer Rng (next_u32_below never reaches its bound, so the
        // cumulative walk always lands inside a bucket).
        const DENOM: u32 = 1_000_000;
        let u = (f64::from(rng.next_u32_below(DENOM)) / f64::from(DENOM)) * total;
        let mut acc = 0.0;
        for (key, w) in &items {
            acc += *w;
            if u < acc {
                return Outcome::parse(&serde_json::Value::String((*key).clone()))
                    .unwrap_or(Outcome::NoEffect);
            }
        }
        // Unreachable (u < total). Fall back to the last bucket — never panic (hard rule #4).
        match items.last() {
            Some((key, _)) => Outcome::parse(&serde_json::Value::String((*key).clone()))
                .unwrap_or(Outcome::NoEffect),
            None => Outcome::NoEffect,
        }
    }
}

/// Learned parametric model hook.
pub struct ModelProvider {
    _params: serde_json::Value,
}

impl ModelProvider {
    pub fn from_params(params: serde_json::Value) -> Self {
        Self { _params: params }
    }
}

impl ProbProvider for ModelProvider {
    fn resolve(&self, _ctx: &ResolveContext, _rng: &mut dyn Rng) -> Outcome {
        // Not implemented yet. Rules are user-editable DATA (#2): a rules file selecting
        // `"kind":"model"` must NOT panic the kernel mid-adjudication (#4). Fall back to the inert
        // outcome (same as NoopProvider) until a real model backend lands — deterministic, no panic.
        Outcome::NoEffect
    }
}

/// Build the provider configured for a given table id.
pub fn build_provider(rules: &Rules, table_id: &str) -> Box<dyn ProbProvider> {
    match rules.prob.providers.get(table_id) {
        Some(spec) => match spec.kind.as_str() {
            "bayesian" => Box::new(BayesianTable::from_params(spec.params.clone())),
            "model" => Box::new(ModelProvider::from_params(spec.params.clone())),
            _ => static_or_noop(rules, table_id),
        },
        None => static_or_noop(rules, table_id),
    }
}

fn static_or_noop(rules: &Rules, table_id: &str) -> Box<dyn ProbProvider> {
    match StaticTable::from_rules(rules, table_id) {
        Some(t) => Box::new(t),
        None => Box::new(NoopProvider),
    }
}

struct NoopProvider;
impl ProbProvider for NoopProvider {
    fn resolve(&self, _ctx: &ResolveContext, _rng: &mut dyn Rng) -> Outcome {
        Outcome::NoEffect
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::PcgRng;

    fn rules() -> Rules {
        Rules::from_json_str(include_str!("../../../config/rules.default.json")).unwrap()
    }

    #[test]
    fn static_table_resolves_deterministically() {
        let r = rules();
        let p = build_provider(&r, "direct_vs_vehicle");
        let ctx = ResolveContext {
            table_id: "direct_vs_vehicle".into(),
            attack_level: 10,
        };
        let mut a = PcgRng::from_seed(1);
        let mut b = PcgRng::from_seed(1);
        // Same seed -> identical outcomes across runs.
        for _ in 0..200 {
            assert_eq!(p.resolve(&ctx, &mut a), p.resolve(&ctx, &mut b));
        }
    }

    #[test]
    fn bayesian_table_resolves_deterministically() {
        // config/rules.calibrated.json switches the result tables from `static` to `bayesian`
        // (produced by python/prob_learning/calibrate.py / the /calibrate-prob skill). The bayesian
        // provider must resolve WITHOUT panicking, deterministically (same seed → same stream), and
        // only to outcomes present in the calibrated row — i.e. `make test` stays green after the
        // static→bayesian switch (decision Q1-P3 closure). ALL three result tables are now calibrated
        // (prior-seeded from their static distributions, strength 4).
        let r =
            Rules::from_json_str(include_str!("../../../config/rules.calibrated.json")).unwrap();
        assert_eq!(r.prob.providers["direct_vs_personnel"].kind, "bayesian");
        assert_eq!(r.prob.providers["direct_vs_vehicle"].kind, "bayesian");
        assert_eq!(r.prob.providers["small_arms_vs_vehicle"].kind, "bayesian");

        let p = build_provider(&r, "direct_vs_personnel");
        let ctx = ResolveContext {
            table_id: "direct_vs_personnel".into(),
            attack_level: 5, // calibrated row {"1": .., "S": ..}
        };
        let mut a = PcgRng::from_seed(2);
        let mut b = PcgRng::from_seed(2);
        let (mut destroyed, mut suppress) = (false, false);
        for _ in 0..400 {
            let oa = p.resolve(&ctx, &mut a);
            assert_eq!(oa, p.resolve(&ctx, &mut b)); // same seed ⇒ identical (hard rule #1)
            match oa {
                Outcome::Destroyed(1) => destroyed = true,
                Outcome::Suppress => suppress = true,
                other => panic!("calibrated row yields only 1/S, got {other:?}"),
            }
        }
        // The calibrated categorical (≈73% 毁伤 / 27% 压制 at level 5) genuinely varies.
        assert!(
            destroyed && suppress,
            "bayesian draw must sample both outcomes"
        );
    }

    #[test]
    fn outcome_parse() {
        assert_eq!(
            Outcome::parse(&serde_json::json!(2)),
            Some(Outcome::Destroyed(2))
        );
        assert_eq!(
            Outcome::parse(&serde_json::json!("S")),
            Some(Outcome::Suppress)
        );
        assert_eq!(Outcome::parse(&serde_json::json!("K")), Some(Outcome::Kill));
    }

    #[test]
    fn model_provider_does_not_panic() {
        // Rules are user-editable data (#2): a table set to "kind":"model" selects the stub
        // ModelProvider, which must resolve WITHOUT panicking (#4) — it falls back to NoEffect.
        let p = ModelProvider::from_params(serde_json::json!({}));
        let ctx = ResolveContext {
            table_id: "direct_vs_vehicle".into(),
            attack_level: 10,
        };
        let mut rng = PcgRng::from_seed(1);
        assert_eq!(p.resolve(&ctx, &mut rng), Outcome::NoEffect);
    }
}
