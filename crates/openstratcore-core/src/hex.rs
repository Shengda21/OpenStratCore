//! Axial hex coordinates (pointy-top). Deterministic; no floating point in core distance.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Axial {
    pub q: i32,
    pub r: i32,
}

impl Axial {
    pub const fn new(q: i32, r: i32) -> Self {
        Self { q, r }
    }

    /// Cube s-coordinate (q + r + s = 0).
    pub const fn s(&self) -> i32 {
        -self.q - self.r
    }

    /// Hex grid distance (number of steps), independent of elevation.
    pub fn distance(&self, other: &Axial) -> i32 {
        ((self.q - other.q).abs() + (self.r - other.r).abs() + (self.s() - other.s()).abs()) / 2
    }

    /// The six neighbor directions, indexed 0..5 (matches map.road.connects).
    pub const DIRECTIONS: [(i32, i32); 6] = [(1, 0), (1, -1), (0, -1), (-1, 0), (-1, 1), (0, 1)];

    pub fn neighbor(&self, dir: usize) -> Axial {
        let (dq, dr) = Self::DIRECTIONS[dir % 6];
        Axial::new(self.q + dq, self.r + dr)
    }

    pub fn neighbors(&self) -> [Axial; 6] {
        let mut out = [*self; 6];
        for (i, slot) in out.iter_mut().enumerate() {
            *slot = self.neighbor(i);
        }
        out
    }

    /// Hexes on the straight line between two centers (for LOS sampling). Returns endpoints inclusive.
    /// Fully **integer** and **order-independent**: `line_to(a, b)` is the exact reverse of
    /// `line_to(b, a)` (so 通视 stays symmetric), and there is no floating point — the path is
    /// bit-deterministic across platforms/opt-levels (CLAUDE.md hard rule #1).
    pub fn line_to(&self, other: &Axial) -> Vec<Axial> {
        let n = self.distance(other);
        if n == 0 {
            return vec![*self];
        }
        let n64 = i64::from(n);
        let (aq, ar, asx) = (i64::from(self.q), i64::from(self.r), i64::from(self.s()));
        let (bq, br, bsx) = (i64::from(other.q), i64::from(other.r), i64::from(other.s()));
        let mut out = Vec::with_capacity((n + 1) as usize);
        for i in 0..=n {
            let i = i64::from(i);
            // Exact cube coordinate × n (numerators); the true point is (qn/n, rn/n, sn/n).
            // qn is identical for (a,b)@i and (b,a)@(n-i), which is what makes line_to symmetric.
            let qn = aq * n64 + (bq - aq) * i;
            let rn = ar * n64 + (br - ar) * i;
            let sn = asx * n64 + (bsx - asx) * i;
            out.push(cube_round(qn, rn, sn, n64));
        }
        out
    }
}

/// Round `num / den` (den > 0) to the nearest integer, ties toward +∞ — deterministic.
fn round_div(num: i64, den: i64) -> i64 {
    (2 * num + den).div_euclid(2 * den)
}

/// Round an exact cube coordinate `(qn/den, rn/den, sn/den)` (with `qn+rn+sn == 0`) to the
/// nearest hex. The tie-break uses only the rounding residuals, which do not depend on which
/// endpoint the line was drawn from — so `line_to` stays symmetric.
fn cube_round(qn: i64, rn: i64, sn: i64, den: i64) -> Axial {
    let mut rq = round_div(qn, den);
    let mut rr = round_div(rn, den);
    let rs = round_div(sn, den);
    let dq = (qn - rq * den).abs();
    let dr = (rn - rr * den).abs();
    let ds = (sn - rs * den).abs();
    if dq > dr && dq > ds {
        rq = -rr - rs;
    } else if dr > ds {
        rr = -rq - rs;
    }
    Axial::new(rq as i32, rr as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distance_is_symmetric_and_zero_on_self() {
        let a = Axial::new(0, 0);
        let b = Axial::new(2, -1);
        assert_eq!(a.distance(&b), b.distance(&a));
        assert_eq!(a.distance(&a), 0);
        assert_eq!(a.distance(&Axial::new(1, 0)), 1);
    }

    #[test]
    fn line_endpoints_inclusive() {
        let a = Axial::new(0, 0);
        let b = Axial::new(3, 0);
        let line = a.line_to(&b);
        assert_eq!(line.first(), Some(&a));
        assert_eq!(line.last(), Some(&b));
        assert_eq!(line.len() as i32, a.distance(&b) + 1);
    }

    #[test]
    fn line_to_is_symmetric() {
        // The f64 version diverged here (step 15 was (7,8) one way, (8,7) the other), which
        // made 通视 depend on argument order. The integer version must be exactly reversible
        // for every ordered pair in a box around the origin.
        for aq in -6..=6 {
            for ar in -6..=6 {
                for bq in -6..=6 {
                    for br in -6..=6 {
                        let a = Axial::new(aq, ar);
                        let b = Axial::new(bq, br);
                        let fwd = a.line_to(&b);
                        let mut rev = b.line_to(&a);
                        rev.reverse();
                        assert_eq!(
                            fwd, rev,
                            "line_to({a:?},{b:?}) must be the reverse of line_to({b:?},{a:?})"
                        );
                    }
                }
            }
        }
        // The specific long diagonal the f64 path broke on.
        let a = Axial::new(0, 0);
        let b = Axial::new(11, 11);
        let mut rev = b.line_to(&a);
        rev.reverse();
        assert_eq!(a.line_to(&b), rev);
    }
}
