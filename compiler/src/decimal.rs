//! Fixed-point decimals for Sond.
//!
//! `Money` and `Percent` are fixed-point, NOT `f64`: money arithmetic must
//! be exact. A `Decimal` is `mantissa / 10^scale`, e.g. `3.14` is
//! `{ mantissa: 314, scale: 2 }`. Equality is numeric (`0.0 == 0.00`).

use std::cmp::Ordering;

#[derive(Debug, Clone)]
pub struct Decimal {
    pub mantissa: i128,
    pub scale: u32,
}

impl Decimal {
    /// Parses a decimal (or integer) lexeme, preserving all digits exactly.
    pub fn parse(s: &str) -> Decimal {
        match s.find('.') {
            Some(dot) => {
                let scale = (s.len() - dot - 1) as u32;
                let digits: String = s.chars().filter(|c| *c != '.').collect();
                Decimal {
                    mantissa: digits.parse().expect("valid decimal"),
                    scale,
                }
            }
            None => Decimal {
                mantissa: s.parse().expect("valid integer"),
                scale: 0,
            },
        }
    }

    /// Aligns two decimals to a common scale, returning `(a, b, scale)`.
    fn aligned(&self, other: &Decimal) -> (i128, i128, u32) {
        let scale = self.scale.max(other.scale);
        let a = self.mantissa * 10i128.pow(scale - self.scale);
        let b = other.mantissa * 10i128.pow(scale - other.scale);
        (a, b, scale)
    }

    pub fn add(&self, other: &Decimal) -> Decimal {
        let (a, b, scale) = self.aligned(other);
        Decimal {
            mantissa: a + b,
            scale,
        }
    }

    pub fn sub(&self, other: &Decimal) -> Decimal {
        let (a, b, scale) = self.aligned(other);
        Decimal {
            mantissa: a - b,
            scale,
        }
    }

    pub fn mul(&self, other: &Decimal) -> Decimal {
        Decimal {
            mantissa: self.mantissa * other.mantissa,
            scale: self.scale + other.scale,
        }
    }

    /// Rounds to `scale` decimal places, half-up (a `.5` rounds toward +∞), as
    /// the RFC's "accounting rounding" prescribes.
    pub fn round(&self, scale: u32) -> Decimal {
        if self.scale <= scale {
            return Decimal {
                mantissa: self.mantissa * 10i128.pow(scale - self.scale),
                scale,
            };
        }
        let divisor = 10i128.pow(self.scale - scale);
        let q = self.mantissa / divisor;
        let r = self.mantissa % divisor;
        // half-up toward +∞: r == +half rounds up; r == -half does not round down.
        let mantissa = if 2 * r >= divisor {
            q + 1
        } else if 2 * r < -divisor {
            q - 1
        } else {
            q
        };
        Decimal { mantissa, scale }
    }

    /// Divides, producing a result at `scale` decimal places (rounded).
    /// Note (design finding): a decimal `/` needs an explicit result scale;
    /// the `/` operator picks a guard scale and callers round afterwards.
    pub fn div(&self, other: &Decimal, scale: u32) -> Decimal {
        // Compute one extra (guard) digit, then round it away from zero.
        let guard_exp = scale as i64 + other.scale as i64 - self.scale as i64 + 1;
        let num = if guard_exp >= 0 {
            self.mantissa * 10i128.pow(guard_exp as u32)
        } else {
            self.mantissa / 10i128.pow((-guard_exp) as u32)
        };
        let q10 = num / other.mantissa;
        let last = q10 % 10;
        let mut mantissa = q10 / 10;
        if last.abs() >= 5 {
            mantissa += q10.signum();
        }
        Decimal { mantissa, scale }
    }

    fn compare(&self, other: &Decimal) -> Ordering {
        let (a, b, _) = self.aligned(other);
        a.cmp(&b)
    }
}

/// Numeric ordering and equality: `0.0 == 0.00`, `1.50 == 1.5`, `3.14 > 3.1`.
impl PartialEq for Decimal {
    fn eq(&self, other: &Self) -> bool {
        self.compare(other) == Ordering::Equal
    }
}

impl Eq for Decimal {}

impl PartialOrd for Decimal {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Decimal {
    fn cmp(&self, other: &Self) -> Ordering {
        self.compare(other)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Decimal {
        Decimal::parse(s)
    }

    #[test]
    fn parses_preserving_scale() {
        assert_eq!(d("0.00").mantissa, 0);
        assert_eq!(d("0.00").scale, 2);
        assert_eq!(
            d("3.14"),
            Decimal {
                mantissa: 314,
                scale: 2
            }
        );
        assert_eq!(
            d("100.0"),
            Decimal {
                mantissa: 1000,
                scale: 1
            }
        );
        assert_eq!(
            d("42"),
            Decimal {
                mantissa: 42,
                scale: 0
            }
        );
    }

    #[test]
    fn equality_is_numeric_not_structural() {
        // Different scales, same value.
        assert_eq!(d("0.0"), d("0.00"));
        assert_eq!(d("1.50"), d("1.5"));
        assert_ne!(d("3.14"), d("3.15"));
    }

    #[test]
    fn adds_and_subtracts_exactly() {
        assert_eq!(d("1.50").add(&d("2.25")), d("3.75"));
        assert_eq!(d("10.00").sub(&d("0.01")), d("9.99"));
        // Aligns differing scales.
        assert_eq!(d("1.5").add(&d("0.25")), d("1.75"));
    }

    #[test]
    fn multiplies_exactly() {
        // 2.00 * 3.0 = 6.000 (scales add: 2 + 1 = 3), numerically 6.
        assert_eq!(d("2.00").mul(&d("3.0")), d("6"));
        // 1000.00 * 0.21 = 210.0000
        assert_eq!(d("1000.00").mul(&d("0.21")), d("210"));
    }

    #[test]
    fn compares_across_scales() {
        assert_eq!(d("3.14").cmp(&d("3.1")), Ordering::Greater);
        assert_eq!(d("0.00").cmp(&d("0.0")), Ordering::Equal);
        assert_eq!(d("2.5").cmp(&d("10.0")), Ordering::Less);
    }

    #[test]
    fn rounds_half_up_to_scale() {
        assert_eq!(d("6.9993").round(2), d("7.00")); // fiscal case (half-up)
        assert_eq!(d("2.345").round(2), d("2.35")); // exactly half → up
        assert_eq!(d("2.344").round(2), d("2.34"));
        assert_eq!(d("2.005").round(2), d("2.01"));
        assert_eq!(d("3.14").round(2), d("3.14")); // already at scale
        assert_eq!(d("5").round(2), d("5.00")); // pad up
    }

    #[test]
    fn divides_to_scale() {
        // 21000.000 / 100.0 = 210.00
        assert_eq!(d("21000.000").div(&d("100.0"), 2), d("210.00"));
        // 10 / 3 to 4 places
        assert_eq!(d("10").div(&d("3"), 4), d("3.3333"));
        // 1 / 8 = 0.125 exactly at scale 3
        assert_eq!(d("1").div(&d("8"), 3), d("0.125"));
    }
}
