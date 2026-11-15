//! `log2` and `exp2` from +, -, * and / alone, which IEEE 754 rounds the same way on every machine, so a choice
//! made from them is the same on Linux, macOS, Windows and in WebAssembly. The standard library's `log2` and
//! `powf` call the platform's maths library, and those differ in the last bit: glibc 2.39 (x86-64) and Rust's
//! WebAssembly libm disagree on log2 of 22 of the integers 1 to 20,000. Both functions here work in
//! double-double (about 106 bits) and round once, so they are correctly rounded except within about 2^-100 of
//! a halfway point (and for exp2's subnormal results); where the platform's result is correctly rounded too,
//! the two are equal. log2, which the Flate cost model calls often, first takes a quick path through a table
//! and falls back to the slower series only when the quick answer could round either way.

/// A value as hi + lo, |lo| at most half an ulp of hi.
type Dd = (f64, f64);

const fn two_sum(a: f64, b: f64) -> Dd {
    let s = a + b;
    let v = s - a;
    (s, (a - (s - v)) + (b - v))
}

/// For |a| >= |b| (or a = 0).
const fn fast_two_sum(a: f64, b: f64) -> Dd {
    let s = a + b;
    (s, b - (s - a))
}

/// x as hi + lo, each of at most 26 significant bits (Dekker).
const fn split(x: f64) -> Dd {
    let t = 134_217_729.0 * x; // 2^27 + 1
    let hi = t - (t - x);
    (hi, x - hi)
}

/// a * b exactly, by Dekker's splitting (no fused multiply-add, which not every target has).
const fn two_prod(a: f64, b: f64) -> Dd {
    let p = a * b;
    let ((ah, al), (bh, bl)) = (split(a), split(b));
    (p, ((ah * bh - p) + ah * bl + al * bh) + al * bl)
}

const fn add(a: Dd, b: Dd) -> Dd {
    let (s, e) = two_sum(a.0, b.0);
    let (t, f) = two_sum(a.1, b.1);
    let (s, e) = fast_two_sum(s, e + t);
    fast_two_sum(s, e + f)
}

const fn mul(a: Dd, b: Dd) -> Dd {
    let (p, e) = two_prod(a.0, b.0);
    fast_two_sum(p, e + (a.0 * b.1 + a.1 * b.0))
}

const fn div(a: Dd, b: Dd) -> Dd {
    let q1 = a.0 / b.0;
    let r = add(a, mul(b, (-q1, 0.0)));
    let q2 = r.0 / b.0;
    let r = add(r, mul(b, (-q2, 0.0)));
    let q3 = r.0 / b.0;
    add(fast_two_sum(q1, q2), (q3, 0.0))
}

/// 1 / ln 2 and ln 2 to about 107 bits.
const LOG2_E: Dd = (std::f64::consts::LOG2_E, 2.0355273740931033e-17);
const LN_2: Dd = (std::f64::consts::LN_2, 2.3190468138462996e-17);

/// 1 / (2k + 1), the atanh series' factors.
const ODD: [Dd; 22] = {
    let mut t = [(0.0, 0.0); 22];
    let mut k = 0;
    while k < 22 {
        t[k] = div((1.0, 0.0), ((2 * k + 1) as f64, 0.0));
        k += 1;
    }
    t
};

/// 1 / k!, the exponential series' factors.
const FACT: [Dd; 28] = {
    let mut t = [(1.0, 0.0); 28];
    let mut k = 1;
    while k < 28 {
        t[k] = div(t[k - 1], (k as f64, 0.0));
        k += 1;
    }
    t
};

/// 2^n for n in -1022..=1023.
const fn pow2i(n: i64) -> f64 {
    f64::from_bits(((n + 1023) as u64) << 52)
}

/// log2 m for m in [sqrt(1/2), sqrt(2)], to about 106 bits: ln m = 2 atanh z = 2 (z + z^3/3 + z^5/5 + ...) with
/// z = (m - 1) / (m + 1), |z| < 0.172, so 22 terms reach 2^-106. m - 1 is exact: m is within a factor 2 of 1.
const fn log2_near_1(m: f64) -> Dd {
    let z = div((m - 1.0, 0.0), two_sum(m, 1.0));
    let z2 = mul(z, z);
    let mut s: Dd = (0.0, 0.0);
    let mut k = ODD.len();
    while k > 0 {
        k -= 1;
        s = add(mul(s, z2), ODD[k]);
    }
    mul(mul(mul(z, s), (2.0, 0.0)), LOG2_E)
}

/// log2(1 + i/64) for i below 64, to about 106 bits (worked out when compiling).
const LOG2_STEP: [Dd; 64] = {
    let mut t = [(0.0, 0.0); 64];
    let mut i = 0;
    while i < 64 {
        let c = 1.0 + i as f64 / 64.0;
        t[i] = if c > std::f64::consts::SQRT_2 { add((1.0, 0.0), log2_near_1(c / 2.0)) } else { log2_near_1(c) };
        i += 1;
    }
    t
};

/// -1/2, 1/3, -1/4, ... to -1/10: ln(1 + r) = r + r^2 (the sum of these times r^k).
const LN1P_TAIL: [f64; 9] =
    [-1.0 / 2.0, 1.0 / 3.0, -1.0 / 4.0, 1.0 / 5.0, -1.0 / 6.0, 1.0 / 7.0, -1.0 / 8.0, 1.0 / 9.0, -1.0 / 10.0];

/// x = m 2^e with m in [1, 2), for a positive finite x (a subnormal one is scaled into the normal range first).
fn split_exponent(x: f64) -> (f64, i64) {
    let (x, e) = if x < f64::MIN_POSITIVE { (x * pow2i(54), -54) } else { (x, 0) };
    let bits = x.to_bits();
    (f64::from_bits((bits & ((1 << 52) - 1)) | (1023 << 52)), e + ((bits >> 52) & 0x7ff) as i64 - 1023)
}

/// log2(m 2^e) by the series alone, rounded once.
fn log2_by_series(m: f64, e: i64) -> f64 {
    let (m, e) = if m > std::f64::consts::SQRT_2 { (m / 2.0, e + 1) } else { (m, e) };
    let r = add((e as f64, 0.0), log2_near_1(m));
    r.0 + r.1
}

/// log2(x), the same bits on every machine: -inf for zero, NaN for a negative x or NaN, inf for inf.
pub(crate) fn log2(x: f64) -> f64 {
    if x.is_nan() || x < 0.0 {
        return f64::NAN;
    }
    if x == 0.0 {
        return f64::NEG_INFINITY;
    }
    if x == f64::INFINITY {
        return x;
    }
    let (m, e) = split_exponent(x);
    // Quickly: log2 x = e + log2 c + log2(1 + r), with c = 1 + i/64 from m's top 6 bits (the table) and
    // r = (m - c) / c in [0, 1/64) to about 106 bits (m - c is exact). ln(1 + r) = r + t with t = r^2 (-1/2 + r/3
    // - ...) in plain floating point, off by less than 2^-48 times t (the series stops below 2^-57 times t); the
    // double-double steps are off by less than 2^-100 times what they add. When the answer could round either
    // way inside those bounds (for 87 of the integers 1 to 2,000,000), the series alone decides.
    let i = (m.to_bits() >> 46) & 63;
    let c = f64::from_bits((1023 << 52) | (i << 46));
    let d = m - c;
    let (mut t, mut q) = (0.0, (0.0, 0.0));
    if d != 0.0 {
        let r = d / c;
        let (p, pe) = two_prod(r, c);
        let r = (r, ((d - p) - pe) / c);
        let mut tail = 0.0;
        let mut k = LN1P_TAIL.len();
        while k > 0 {
            k -= 1;
            tail = tail * r.0 + LN1P_TAIL[k];
        }
        t = r.0 * r.0 * tail;
        q = mul(add(r, (t, 0.0)), LOG2_E);
    }
    let step = LOG2_STEP[i as usize];
    let (h, l) = add(add((e as f64, 0.0), step), q);
    let err = t.abs() * pow2i(-48) + ((e as f64).abs() + step.0 + q.0.abs()) * pow2i(-100) + l.abs() * pow2i(-50);
    if h + (l + err) == h && h + (l - err) == h {
        return h;
    }
    log2_by_series(m, e)
}

/// 2^y, the same bits on every machine: NaN for NaN, inf from 1024 up, 0 at -1075 and below. A result below
/// 2^-1022 (subnormal) is rounded twice, so it is within an ulp rather than correctly rounded.
pub(crate) fn exp2(y: f64) -> f64 {
    if y.is_nan() {
        return f64::NAN;
    }
    if y >= 1024.0 {
        return f64::INFINITY;
    }
    if y <= -1075.0 {
        return 0.0;
    }
    // 2^y = 2^n e^t with n = floor(y) and t = (y - n) ln 2 in [0, 0.694), so 28 terms of the series reach
    // 2^-106. y - n is exact.
    let n = y.floor();
    let t = mul((y - n, 0.0), LN_2);
    let mut s: Dd = (0.0, 0.0);
    let mut k = FACT.len();
    while k > 0 {
        k -= 1;
        s = add(mul(s, t), FACT[k]);
    }
    // Rounded once, then scaled by 2^n: exactly, or in two steps below 2^-1022, of which only the second rounds.
    let r = s.0 + s.1;
    let n = n as i64;
    if n >= -1022 {
        r * pow2i(n)
    } else {
        r * pow2i(n + 600) * pow2i(-600)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn powers_of_two_are_exact() {
        for k in -1074..=1023i64 {
            let x = if k >= -1022 { pow2i(k) } else { f64::from_bits(1 << (k + 1074)) };
            assert_eq!(log2(x), k as f64, "{k}");
            assert_eq!(exp2(k as f64), x, "{k}");
        }
    }

    #[test]
    fn the_quick_path_gives_what_the_series_gives() {
        let series = |x: f64| {
            let (m, e) = split_exponent(x);
            log2_by_series(m, e)
        };
        for i in 1..=300_000u64 {
            assert_eq!(log2(i as f64), series(i as f64), "{i}");
        }
        for n in 1..=300u64 {
            for c in 1..=n {
                let p = c as f64 / n as f64;
                assert_eq!(log2(p), series(p), "{c}/{n}");
            }
        }
        // Doubles from all over the positive range (xorshift bits), subnormal ones too.
        let mut b = 0x9e37_79b9_7f4a_7c15u64;
        for _ in 0..300_000 {
            b ^= b << 13;
            b ^= b >> 7;
            b ^= b << 17;
            let x = f64::from_bits(b >> 1);
            if x > 0.0 && x.is_finite() {
                assert_eq!(log2(x), series(x), "{x:e}");
            }
        }
    }

    #[test]
    fn correctly_rounded_where_platform_libraries_differ() {
        // Checked against 80-digit decimal arithmetic. glibc 2.39 on x86-64 gives the next double for 1621 and
        // for 11/12, Rust's WebAssembly libm for 1375 and 3393 (and for 11/12).
        assert_eq!(log2(1621.0), 10.66266837551754);
        assert_eq!(log2(1375.0), 10.425215903299383);
        assert_eq!(log2(3393.0), 11.728345714710978);
        assert_eq!(exp2(11.0 / 12.0), 1.8877486253633868);
        // The rewrite's work budget at effort 20.
        assert_eq!(exp2(1.9), 3.7321319661472296);
    }

    #[test]
    fn special_values() {
        assert!(log2(0.0) == f64::NEG_INFINITY && log2(-1.0).is_nan() && log2(f64::INFINITY) == f64::INFINITY);
        assert!(log2(f64::NAN).is_nan() && exp2(f64::NAN).is_nan());
        assert!(exp2(-1074.0) == 5e-324 && exp2(2000.0) == f64::INFINITY && exp2(-2000.0) == 0.0);
        assert!(exp2(1024.0) == f64::INFINITY && exp2(-1075.0) == 0.0 && exp2(-1074.5) == 5e-324);
    }

    /// Against glibc's log2 and exp2, which are within an ulp (other platforms' libraries don't promise it).
    #[test]
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    fn within_an_ulp_of_glibc() {
        let ulps = |a: f64, b: f64| a.to_bits().abs_diff(b.to_bits());
        for i in 1..=200_000u64 {
            assert!(ulps(log2(i as f64), (i as f64).log2()) <= 1, "{i}");
        }
        for n in 1..=300u64 {
            for c in 1..=n {
                let p = c as f64 / n as f64;
                assert!(ulps(log2(p), p.log2()) <= 1, "{c}/{n}");
            }
        }
        for d in 1..=100 {
            for k in 0..=100 {
                let y = k as f64 / d as f64;
                assert!(ulps(exp2(y), y.exp2()) <= 1 && ulps(exp2(-y), (-y).exp2()) <= 1, "{k}/{d}");
            }
        }
        for x in [f64::MIN_POSITIVE / 3.0, 5e-324, 1e-300, 1e300, f64::MAX] {
            assert!(ulps(log2(x), x.log2()) <= 1, "{x}");
        }
        for y in [1023.99, 1000.5, -1010.5, -1022.5, -1050.25, -1073.9] {
            assert!(ulps(exp2(y), y.exp2()) <= 1, "{y}");
        }
    }
}
