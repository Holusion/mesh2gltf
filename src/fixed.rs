//! Fixed-point decimal coordinates.
//!
//! Georeferenced meshes carry coordinates in the millions of metres while still resolving
//! millimetres, which is right at the edge of what an `f64` round-trips cleanly. Coordinates are
//! therefore parsed straight from their decimal text into an `i64` scaled by 1e10, so comparisons
//! and differences are exact integer arithmetic and the digits that were in the file are the
//! digits that come back out.

use serde_json::{json, Value};

/// One fixed-point unit, in metres: 1e-10 m (0.1 nm).
/// An `i64` at this scale spans roughly ±9.2e8 m, which covers any projected coordinate system.
pub const SCALE: i128 = 10_000_000_000;

/// Significant bits float32 must keep across a model's largest dimension before
/// [`BBox::far_origin`] moves the model: 1/65,536 of its size, 8 bits short of what float32 holds
/// for a model around 0. Tighter would move models that lose nothing visible.
const KEPT_BITS: i32 = 16;

/// Parse decimal and scientific notation exactly, rounding to 10 fractional digits.
/// Subtraction stays in integers until local coordinates are encoded as float32.
pub fn parse_decimal_fixed(s: &str) -> crate::Res<i64> {
    let s = s.trim();
    let invalid = || {
        format!(
            "Invalid or out-of-range coordinate: {}",
            crate::diagnostics::safe(s)
        )
    };
    let (negative, body) = if let Some(b) = s.strip_prefix('-') {
        (true, b)
    } else {
        (false, s.strip_prefix('+').unwrap_or(s))
    };
    let (mantissa, exponent) = if let Some(i) = body.find(['e', 'E']) {
        (
            &body[..i],
            body[i + 1..].parse::<i64>().map_err(|_| invalid())?,
        )
    } else {
        (body, 0)
    };
    let mut total_digits = 0usize;
    let mut leading_zeros = 0usize;
    let mut dot = false;
    let mut fraction = 0i64;
    for b in mantissa.bytes() {
        if b == b'.' && !dot {
            dot = true;
        } else if b.is_ascii_digit() {
            if total_digits == leading_zeros && b == b'0' {
                leading_zeros += 1;
            }
            total_digits += 1;
            if dot {
                fraction += 1;
            }
        } else {
            return Err(invalid().into());
        }
    }
    if total_digits == 0 {
        return Err(invalid().into());
    }
    let digit_count = total_digits - leading_zeros;
    if digit_count == 0 {
        return Ok(0);
    }
    let shift = exponent.saturating_sub(fraction).saturating_add(10);
    let kept = (digit_count as i64).saturating_add(shift);
    if kept < 0 {
        return Ok(0);
    }
    if kept > 19 {
        return Err(invalid().into());
    }
    let take = (kept as usize).min(digit_count);
    let mut digits = mantissa
        .bytes()
        .filter(|b| b.is_ascii_digit())
        .skip(leading_zeros);
    let mut value = 0i128;
    for b in digits.by_ref().take(take) {
        value = value * 10 + (b - b'0') as i128;
    }
    for _ in take..kept as usize {
        value *= 10;
    }
    if digits.next().is_some_and(|b| b >= b'5') {
        value += 1;
    }
    if negative {
        value = -value;
    }
    i64::try_from(value).map_err(|_| invalid().into())
}

/// Parse three whitespace-separated decimals into fixed-point.
pub fn parse_three_fixed(s: &str) -> crate::Res<[i64; 3]> {
    let mut it = s.split_whitespace();
    let x = it.next().ok_or("missing x")?;
    let y = it.next().ok_or("missing y")?;
    let z = it.next().ok_or("missing z")?;
    Ok([
        parse_decimal_fixed(x)?,
        parse_decimal_fixed(y)?,
        parse_decimal_fixed(z)?,
    ])
}

/// The fixed-point value of a binary float, rounded to 10 fractional digits as decimals are. The
/// float's exact value is scaled in integers, so a float64 keeps every bit it has down to 1e-10.
pub fn fixed_from_float(x: f64) -> crate::Res<i64> {
    let invalid = || format!("Invalid or out-of-range coordinate: {x}");
    let bits = x.to_bits();
    let exponent = ((bits >> 52) & 0x7ff) as i32;
    let fraction = (bits & ((1 << 52) - 1)) as i128;
    // |x| = mantissa · 2^power, with a 53-bit mantissa, so `mantissa · SCALE` fits in 87 bits.
    let (mantissa, power) = match exponent {
        0 => (fraction, -1074),
        0x7ff => return Err(invalid().into()),
        _ => (fraction | 1 << 52, exponent - 1075),
    };
    // From 2^52 up, |x| is far outside the ±9.2e8 m that fixed point spans.
    if power >= 0 {
        return Err(invalid().into());
    }
    let shift = power.unsigned_abs();
    let magnitude = if shift >= 100 {
        0
    } else {
        (mantissa * SCALE + (1 << (shift - 1))) >> shift
    };
    let value = if x.is_sign_negative() {
        -magnitude
    } else {
        magnitude
    };
    i64::try_from(value).map_err(|_| invalid().into())
}

/// Format a fixed-point value back to decimal, trailing zeros trimmed.
pub fn format_fixed(val: i128) -> String {
    if val == 0 {
        return "0".to_string();
    }
    let neg = val < 0;
    let a = if neg { -val } else { val };
    let intp = a / SCALE;
    let frac = a % SCALE;
    let sign = if neg { "-" } else { "" };
    if frac == 0 {
        format!("{}{}", sign, intp)
    } else {
        let mut frac_s = format!("{:010}", frac);
        while frac_s.ends_with('0') {
            frac_s.pop();
        }
        format!("{}{}.{}", sign, intp, frac_s)
    }
}

pub fn format_fixed_as_decimal(val: i64) -> String {
    format_fixed(val as i128)
}

/// Format a triple of fixed-point values as `x y z`.
pub fn format_triple(v: [i64; 3]) -> String {
    format!(
        "{} {} {}",
        format_fixed_as_decimal(v[0]),
        format_fixed_as_decimal(v[1]),
        format_fixed_as_decimal(v[2])
    )
}

/// An axis-aligned bounding box in fixed-point coordinates.
///
/// Empty until the first vertex is added, so an input without vertices reports no bounds rather
/// than a box collapsed on the origin.
#[derive(Clone, Copy, Default)]
pub struct BBox {
    bounds: Option<([i64; 3], [i64; 3])>,
}

impl BBox {
    pub fn new() -> Self {
        Self { bounds: None }
    }

    pub fn add(&mut self, c: [i64; 3]) {
        match &mut self.bounds {
            None => self.bounds = Some((c, c)),
            Some((min, max)) => {
                for i in 0..3 {
                    if c[i] < min[i] {
                        min[i] = c[i];
                    }
                    if c[i] > max[i] {
                        max[i] = c[i];
                    }
                }
            }
        }
    }

    /// Extent along each axis. Widened to `i128` because `max - min` can exceed `i64` even
    /// when both ends are representable.
    pub fn size(&self) -> Option<[i128; 3]> {
        let (min, max) = self.bounds?;
        Some([
            max[0] as i128 - min[0] as i128,
            max[1] as i128 - min[1] as i128,
            max[2] as i128 - min[2] as i128,
        ])
    }

    /// Midpoint of the box, the origin `--center` subtracts.
    /// Computed in `i128` so the intermediate sum cannot overflow; the result always lies inside
    /// the box and so is representable.
    pub fn center(&self) -> Option<[i64; 3]> {
        let (min, max) = self.bounds?;
        let mid = |a: i64, b: i64| ((a as i128 + b as i128) / 2) as i64;
        Some([
            mid(min[0], max[0]),
            mid(min[1], max[1]),
            mid(min[2], max[2]),
        ])
    }

    /// The origin positions must be stored relative to, so that float32 keeps their detail, or
    /// `None` when they are close enough to 0 as they are.
    ///
    /// float32 resolves 24 significant bits. A model away from 0 compared with its size, typically
    /// georeferenced, spends some of them on its distance to 0. The model is moved once fewer than
    /// [`KEPT_BITS`] would remain across its largest dimension. A model a few kilometres across
    /// with local coordinates is never moved; a building in projected coordinates always is.
    ///
    /// The origin is the box center rounded to the largest power of ten no larger than the model,
    /// which costs at most one bit and keeps the origin a short decimal. A JSON reader, which parses
    /// numbers as `f64`, then gets the origin back intact as a node translation.
    pub fn far_origin(&self) -> Option<[i64; 3]> {
        let (min, max) = self.bounds?;
        let size = self.size()?.into_iter().max()?;
        let reach = (0..3)
            .map(|i| (min[i] as i128).abs().max((max[i] as i128).abs()))
            .max()?;
        if reach == 0 {
            return None;
        }
        let metres = |v: i128| v as f64 / SCALE as f64;
        // Spacing between consecutive float32 values at `reach`.
        let spacing = 2f64.powi(metres(reach).log2().floor() as i32 - 23);
        if spacing * 2f64.powi(KEPT_BITS) <= metres(size) {
            return None;
        }
        let mut step = 1i128;
        while step * 10 <= size {
            step *= 10;
        }
        let center = self.center()?;
        Some(center.map(|c| {
            let rounded = (c as i128 + step / 2).div_euclid(step) * step;
            i64::try_from(rounded).unwrap_or(c)
        }))
    }

    /// `(min, max)` as floating-point source coordinates, for transforming the box.
    pub fn as_f64(&self) -> Option<([f64; 3], [f64; 3])> {
        let (min, max) = self.bounds?;
        let f = |c: [i64; 3]| c.map(|v| v as f64 / SCALE as f64);
        Some((f(min), f(max)))
    }

    /// `min → max`, for human-readable output.
    pub fn range_str(&self) -> Option<String> {
        let (min, max) = self.bounds?;
        Some(format!("{} → {}", format_triple(min), format_triple(max)))
    }

    /// `x × y × z`, for human-readable output.
    pub fn size_str(&self) -> Option<String> {
        let s = self.size()?;
        Some(format!(
            "{} × {} × {}",
            format_fixed(s[0]),
            format_fixed(s[1]),
            format_fixed(s[2])
        ))
    }

    /// The bounds as JSON. Coordinates are decimal *strings*: a georeferenced value carries more
    /// significant digits than a JSON number survives in most parsers, and losing them here would
    /// defeat the point of the fixed-point pipeline.
    pub fn record(self) -> Option<Value> {
        let (min, max) = self.bounds?;
        Some(json!({
            "min": min.map(format_fixed_as_decimal),
            "max": max.map(format_fixed_as_decimal),
            "size": self.size()?.map(format_fixed),
            "center": self.center()?.map(format_fixed_as_decimal),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_formats_round_trip() {
        for s in [
            "0",
            "1",
            "-1",
            "1000000.0000000001",
            "-0.5",
            "651234.5678901234",
        ] {
            assert_eq!(format_fixed_as_decimal(parse_decimal_fixed(s).unwrap()), {
                // round-trip drops the redundant ".0" but keeps every significant digit
                let t = if s.contains('.') {
                    s.trim_end_matches('0').trim_end_matches('.')
                } else {
                    s
                };
                t.to_string()
            });
        }
    }

    #[test]
    fn rounds_the_eleventh_fractional_digit() {
        assert_eq!(parse_decimal_fixed("0.00000000005").unwrap(), 1);
        assert_eq!(parse_decimal_fixed("0.00000000004").unwrap(), 0);
        assert_eq!(
            parse_decimal_fixed("-0.99999999995").unwrap(),
            -SCALE as i64
        );
    }

    #[test]
    fn floats_convert_by_their_exact_value() {
        // 0.1f32 is 0.100000001490116119384765625.
        assert_eq!(fixed_from_float(0.1f32 as f64).unwrap(), 1_000_000_015);
        assert_eq!(fixed_from_float(-2.5).unwrap(), -25_000_000_000);
        // A georeferenced float64 keeps its last bits: 2^-30 m steps at this magnitude.
        let x = 6_543_210.0 + 2f64.powi(-30);
        assert_eq!(fixed_from_float(x).unwrap(), 65_432_100_000_000_009);
        assert_eq!(fixed_from_float(f64::MIN_POSITIVE).unwrap(), 0);
        assert_eq!(fixed_from_float(-0.).unwrap(), 0);
        assert!(fixed_from_float(1e9).is_err());
        assert!(fixed_from_float(f64::NAN).is_err());
        assert!(fixed_from_float(f64::INFINITY).is_err());
    }

    #[test]
    fn rejects_out_of_range_coordinates() {
        assert!(parse_decimal_fixed("1000000000").is_err());
        assert!(parse_decimal_fixed("99999999999999999999999999").is_err());
    }

    #[test]
    fn rejects_non_numeric_input() {
        assert!(parse_decimal_fixed("nan").is_err());
        assert!(parse_decimal_fixed("1a2").is_err());
        assert!(parse_decimal_fixed("").is_err());
    }

    #[test]
    fn handles_edge_shaped_decimals() {
        assert_eq!(
            format_fixed_as_decimal(parse_decimal_fixed(".5").unwrap()),
            "0.5"
        );
        assert_eq!(
            format_fixed_as_decimal(parse_decimal_fixed("1.").unwrap()),
            "1"
        );
        assert_eq!(
            format_fixed_as_decimal(parse_decimal_fixed("+2.25").unwrap()),
            "2.25"
        );
        assert_eq!(
            format_fixed_as_decimal(parse_decimal_fixed("-0").unwrap()),
            "0"
        );
        // digits past the tenth are dropped (the eleventh only rounds), and the formatter
        // trims the trailing zero
        assert_eq!(
            format_fixed_as_decimal(parse_decimal_fixed("0.123456789012345").unwrap()),
            "0.123456789"
        );
        assert_eq!(
            format_fixed_as_decimal(parse_decimal_fixed("0.123456789062345").unwrap()),
            "0.1234567891"
        );
    }

    #[test]
    fn bbox_is_empty_until_a_point_is_added() {
        let b = BBox::new();
        assert!(b.center().is_none());
        assert!(b.range_str().is_none());
        assert!(b.record().is_none());
    }

    #[test]
    fn bbox_tracks_extent_and_centre() {
        let mut b = BBox::new();
        b.add(parse_three_fixed("-1 0 2.5").unwrap());
        b.add(parse_three_fixed("3 0 -2.5").unwrap());
        b.add(parse_three_fixed("1 0 0").unwrap());

        assert_eq!(b.range_str().unwrap(), "-1 0 -2.5 → 3 0 2.5");
        assert_eq!(format_triple(b.center().unwrap()), "1 0 0");
        assert_eq!(b.size_str().unwrap(), "4 × 0 × 5");
    }

    fn far_origin(points: &[&str]) -> Option<String> {
        let mut b = BBox::new();
        for p in points {
            b.add(parse_three_fixed(p).unwrap());
        }
        b.far_origin().map(format_triple)
    }

    #[test]
    fn local_coordinates_stay_in_place() {
        // A 5 km site in local coordinates, even one not around 0.
        assert_eq!(far_origin(&["0 0 0", "5000 4000 120"]), None);
        assert_eq!(far_origin(&["2000 3000 0", "5000 4000 120"]), None);
        // A 100 m building on a site grid a few kilometres wide.
        assert_eq!(far_origin(&["5000 5000 0", "5100 5100 30"]), None);
        // A 2 m object a hundred metres away.
        assert_eq!(far_origin(&["100 0 0", "102 1 1"]), None);
        // Nothing to place.
        assert_eq!(far_origin(&[]), None);
        assert_eq!(far_origin(&["0 0 0"]), None);
    }

    #[test]
    fn georeferenced_coordinates_move_to_a_round_origin() {
        // A 10 m building in Lambert-93: the center rounded to 10 m.
        assert_eq!(
            far_origin(&["651234.56 6862001.2 120", "651244.5 6862011.1 130"]),
            Some("651240 6862010 130".into())
        );
        // A 3 km terrain in UTM: rounded to 1 km.
        assert_eq!(
            far_origin(&["500300 4999700 0", "503300 5002700 800"]),
            Some("502000 5001000 0".into())
        );
        // A 2 cm object far away: rounded to 1 cm, negative coordinates rounding to nearest.
        assert_eq!(
            far_origin(&["-651234.567 0 0", "-651234.547 0.02 0.02"]),
            Some("-651234.56 0.01 0.01".into())
        );
        // A single point away from 0 has no size to lose.
        assert_eq!(far_origin(&["5 5 5"]), Some("5 5 5".into()));
    }

    #[test]
    fn bbox_centre_is_exact_for_large_coordinates() {
        // The whole reason for fixed point: an f64 midpoint of these two loses the last digits.
        let mut b = BBox::new();
        b.add(parse_three_fixed("651000.0000000001 0 0").unwrap());
        b.add(parse_three_fixed("651000.0000000003 0 0").unwrap());
        assert_eq!(format_triple(b.center().unwrap()), "651000.0000000002 0 0");
    }
}
