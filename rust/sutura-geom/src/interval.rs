//! Rigorous interval-arithmetic filter for the exact predicates.
//!
//! Every operation is evaluated in round-to-nearest `f64` and the result is
//! widened outward by one ulp (`next_down` / `next_up`).  Round-to-nearest
//! places the true real result within half an ulp of the computed value, so
//! the widened interval always contains it (this also holds at binade
//! boundaries and in the subnormal range).  Any non-finite endpoint (overflow,
//! `inf - inf`, `0 * inf`) makes the interval undecidable.
//!
//! A filter never decides a sign on its own guess: `sign()` returns `Some`
//! only when the whole interval lies strictly on one side of zero, otherwise
//! the caller falls back to the exact `BigRational` evaluation.  The filter
//! therefore changes speed, never results.

use num_rational::BigRational;
use num_traits::ToPrimitive;

/// A closed interval `[lo, hi]` guaranteed to contain the exact real value.
#[derive(Clone, Copy, Debug)]
pub struct Iv {
    pub lo: f64,
    pub hi: f64,
}

const UNDECIDABLE: Iv = Iv {
    lo: f64::NEG_INFINITY,
    hi: f64::INFINITY,
};

impl Iv {
    /// An exactly representable value.
    #[inline]
    pub fn exact(x: f64) -> Iv {
        Iv { lo: x, hi: x }
    }

    /// Enclosure of an exact rational (num-rational's `to_f64` is correctly
    /// rounded, so one ulp of widening on each side is sufficient).
    #[inline]
    pub fn from_rational(r: &BigRational) -> Iv {
        match r.to_f64() {
            Some(x) if x.is_finite() => Iv {
                lo: x.next_down(),
                hi: x.next_up(),
            },
            _ => UNDECIDABLE,
        }
    }

    #[inline]
    pub fn add(self, o: Iv) -> Iv {
        Iv {
            lo: (self.lo + o.lo).next_down(),
            hi: (self.hi + o.hi).next_up(),
        }
    }

    #[inline]
    pub fn sub(self, o: Iv) -> Iv {
        Iv {
            lo: (self.lo - o.hi).next_down(),
            hi: (self.hi - o.lo).next_up(),
        }
    }

    #[inline]
    pub fn mul(self, o: Iv) -> Iv {
        let a = self.lo * o.lo;
        let b = self.lo * o.hi;
        let c = self.hi * o.lo;
        let d = self.hi * o.hi;
        // f64::min/max silently drop NaN, so reject it explicitly.
        if a.is_nan() || b.is_nan() || c.is_nan() || d.is_nan() {
            return UNDECIDABLE;
        }
        Iv {
            lo: a.min(b).min(c).min(d).next_down(),
            hi: a.max(b).max(c).max(d).next_up(),
        }
    }

    /// `Some(+1 / -1)` when the sign is certain, `None` when the interval
    /// touches zero or is not finite.
    #[inline]
    pub fn sign(self) -> Option<i32> {
        if !(self.lo.is_finite() && self.hi.is_finite()) {
            return None;
        }
        if self.lo > 0.0 {
            Some(1)
        } else if self.hi < 0.0 {
            Some(-1)
        } else {
            None
        }
    }
}

#[inline]
pub fn sub3(a: &[Iv; 3], b: &[Iv; 3]) -> [Iv; 3] {
    [a[0].sub(b[0]), a[1].sub(b[1]), a[2].sub(b[2])]
}

#[inline]
pub fn add3(a: &[Iv; 3], b: &[Iv; 3]) -> [Iv; 3] {
    [a[0].add(b[0]), a[1].add(b[1]), a[2].add(b[2])]
}

#[inline]
pub fn scale3(a: &[Iv; 3], s: Iv) -> [Iv; 3] {
    [a[0].mul(s), a[1].mul(s), a[2].mul(s)]
}

#[inline]
pub fn cross3(a: &[Iv; 3], b: &[Iv; 3]) -> [Iv; 3] {
    [
        a[1].mul(b[2]).sub(a[2].mul(b[1])),
        a[2].mul(b[0]).sub(a[0].mul(b[2])),
        a[0].mul(b[1]).sub(a[1].mul(b[0])),
    ]
}

#[inline]
pub fn dot3(a: &[Iv; 3], b: &[Iv; 3]) -> Iv {
    a[0].mul(b[0]).add(a[1].mul(b[1])).add(a[2].mul(b[2]))
}

/// det of the 3x3 matrix with rows a, b, c.
#[inline]
pub fn det3(a: &[Iv; 3], b: &[Iv; 3], c: &[Iv; 3]) -> Iv {
    dot3(a, &cross3(b, c))
}

#[inline]
pub fn exact3(p: [f64; 3]) -> [Iv; 3] {
    [Iv::exact(p[0]), Iv::exact(p[1]), Iv::exact(p[2])]
}

/// det of a 4x4 matrix given by rows, via cofactor expansion on row 0.
pub fn det4(m: &[[Iv; 4]; 4]) -> Iv {
    let minor = |col: usize| -> Iv {
        let pick = |r: usize| -> [Iv; 3] {
            let mut out = [Iv::exact(0.0); 3];
            let mut k = 0;
            for (j, v) in m[r].iter().enumerate() {
                if j != col {
                    out[k] = *v;
                    k += 1;
                }
            }
            out
        };
        det3(&pick(1), &pick(2), &pick(3))
    };
    let t0 = m[0][0].mul(minor(0));
    let t1 = m[0][1].mul(minor(1));
    let t2 = m[0][2].mul(minor(2));
    let t3 = m[0][3].mul(minor(3));
    t0.sub(t1).add(t2).sub(t3)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encloses_inexact_sum() {
        // 0.1 + 0.2 is not exactly 0.3 in f64; the enclosure must straddle
        // the rounded result.
        let s = Iv::exact(0.1).add(Iv::exact(0.2));
        assert!(s.lo < 0.1 + 0.2 && s.hi > 0.1 + 0.2);
    }

    #[test]
    fn nan_and_overflow_are_undecidable() {
        let big = Iv::exact(f64::MAX);
        assert_eq!(big.mul(big).sign(), None);
        let inf = Iv { lo: f64::INFINITY, hi: f64::INFINITY };
        assert_eq!(inf.mul(Iv::exact(0.0)).sign(), None);
    }

    #[test]
    fn zero_is_undecidable() {
        assert_eq!(Iv::exact(1.0).sub(Iv::exact(1.0)).sign(), None);
    }
}
