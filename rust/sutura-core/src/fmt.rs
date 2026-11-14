//! Python-compatible `%g` formatting.
//!
//! `mesh_io.write_obj` uses `%.9g` (and 3MF uses `%.7g`). Rust's formatting
//! grammar counts decimal places, not significant digits, so the conversion is
//! spelled out here and pinned by golden tests.

/// Format `value` like Python's `%g` with `sig` significant digits.
pub fn fmt_g(value: f64, sig: usize) -> String {
    assert!(sig >= 1, "fmt_g needs at least one significant digit");
    if value.is_nan() {
        return "nan".to_string();
    }
    if value.is_infinite() {
        return if value.is_sign_negative() {
            "-inf"
        } else {
            "inf"
        }
        .to_string();
    }
    let neg = value.is_sign_negative();
    let x = value.abs();
    if x == 0.0 {
        return if neg { "-0" } else { "0" }.to_string();
    }

    let exp_form = format!("{:.*e}", sig - 1, x);
    let (mantissa, exp_text) = exp_form.split_once('e').expect("exponent form");
    let exp: i32 = exp_text.parse().expect("exponent");
    let mut digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    while digits.len() > 1 && digits.ends_with('0') {
        digits.pop();
    }

    let body = if exp < -4 || exp >= sig as i32 {
        let mut out = String::new();
        out.push(digits.chars().next().unwrap());
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        out.push(if exp < 0 { '-' } else { '+' });
        let ea = exp.unsigned_abs();
        if ea < 10 {
            out.push('0');
        }
        out.push_str(&ea.to_string());
        out
    } else if exp >= 0 {
        let cut = exp as usize + 1;
        if digits.len() > cut {
            format!("{}.{}", &digits[..cut], &digits[cut..])
        } else {
            let mut out = digits;
            out.push_str(&"0".repeat(cut - out.len()));
            out
        }
    } else {
        let zeros = (-exp - 1) as usize;
        format!("0.{}{}", "0".repeat(zeros), digits)
    };

    if neg {
        format!("-{body}")
    } else {
        body
    }
}

#[cfg(test)]
mod tests {
    use super::fmt_g;

    fn check(v: f64, want: &str) {
        assert_eq!(fmt_g(v, 9), want, "value {v:?}");
    }

    #[test]
    fn matches_python_goldens() {
        check(0.0, "0");
        check(-0.0, "-0");
        check(1.0, "1");
        check(1.0 / 3.0, "0.333333333");
        check(0.1, "0.1");
        check(1e-5, "1e-05");
        check(1e-4, "0.0001");
        check(1e9, "1e+09");
        check(999_999_999.5, "1e+09");
        check(1.234_567_890_123, "1.23456789");
        check(123.456, "123.456");
        check(1e20, "1e+20");
        check(1e-20, "1e-20");
        check(2.5, "2.5");
        check(3.5, "3.5");
        check(0.000_123_456_789, "0.000123456789");
        check(100.0, "100");
        check(1000.0, "1000");
        check(123_456_789.0, "123456789");
        check(1_234_567_890.0, "1.23456789e+09");
    }

    #[test]
    fn matches_python_for_promoted_f32_values() {
        check(f64::from(0.1f32), "0.100000001");
        check(f64::from((1.0f32 / 3.0f32)), "0.333333343");
        check(f64::from(1e-5f32), "9.99999975e-06");
        check(f64::from(1e9f32), "1e+09");
        check(f64::from(-0.0f32), "-0");
        check(f64::from(123.456f32), "123.456001");
    }

    #[test]
    fn seven_digit_precision_for_3mf() {
        assert_eq!(fmt_g(0.1, 7), "0.1");
        assert_eq!(fmt_g(123.456, 7), "123.456");
        assert_eq!(fmt_g(1.234_567_89, 7), "1.234568");
    }
}
