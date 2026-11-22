//! The exact arithmetic of the format (FORMAT.md): every encoder and decoder computes these
//! values bit for bit, in any language. Integers throughout; the two float functions are
//! single IEEE operations.

/// (symbol, mantissa bit count, mantissa): the symbol is 0 for zero, else
/// 2 * bitlen(|value|) - 1 + sign (0..=64).
#[inline(always)]
pub(crate) fn bucket(value: i32) -> (u16, u32, u32) {
    let magnitude = value.unsigned_abs();
    if magnitude == 0 {
        return (0, 0, 0);
    }
    let width = magnitude.bit_width();
    let symbol = 2 * width - 1 + u32::from(value < 0);
    (
        symbol as u16,
        width - 1,
        magnitude & ((1u32 << (width - 1)) - 1),
    )
}

#[inline(always)]
pub(crate) fn residual_bucket(residual: i32) -> u8 {
    bucket(residual).0 as u8
}

/// floor(2^40 / d) + 1: n * RECIPROCALS[d] >> 40 == n / d exactly for
/// 0 <= n < 2^31, 1 <= d <= 255 (the error n/2^40 < 2^-9 < 1/d).
static RECIPROCALS: [u64; 256] = {
    let mut table = [0u64; 256];
    let mut divisor = 1;
    while divisor < 256 {
        table[divisor] = (1u64 << 40) / divisor as u64 + 1;
        divisor += 1;
    }
    table
};

/// `divisor` > 0; halves round away from zero.
#[inline(always)]
pub(crate) fn div_round(dividend: i32, divisor: i32) -> i32 {
    let magnitude = dividend.unsigned_abs() + divisor.cast_unsigned() / 2;
    let quotient = if divisor < 256 {
        ((u128::from(magnitude) * u128::from(RECIPROCALS[divisor.cast_unsigned() as usize])) >> 40)
            as u32
    } else {
        magnitude / divisor.cast_unsigned()
    }
    .cast_signed();
    if dividend < 0 { -quotient } else { quotient }
}

#[inline(always)]
pub(crate) fn byte_residual_bucket(residual: u8) -> u8 {
    residual_bucket(i32::from(residual.cast_signed()))
}

/// The `grid` in -32768..=32767 whose f32 `grid * 45/8192` has exactly these bits; `None` for
/// -0.0, NaN, +180 and anything else off the grid.
#[inline]
pub(crate) fn on_grid(bits: u32) -> Option<u16> {
    // `f * 8192` is exact and the division correctly rounded, so an integer quotient is exact
    let scaled = f64::from(f32::from_bits(bits)) * 8192.0 / 45.0;
    if scaled.fract() != 0.0 || !(-32768.0..=32767.0).contains(&scaled) {
        return None;
    }
    let grid = scaled as i32 as i16;
    (grid_f32(grid.cast_unsigned()) == bits).then_some(grid.cast_unsigned())
}

/// Exact: `grid * 45/8192` as f32 bits.
#[inline]
pub(crate) fn grid_f32(grid: u16) -> u32 {
    (f32::from(grid.cast_signed()) * (45.0 / 8192.0)).to_bits()
}

const STEPS_PER_OCTAVE: u32 = 2;
/// 16.16 fixed point.
#[inline]
pub(crate) fn quantized_weight(quantized: u32) -> u64 {
    /// 2^(k/2) in 16.16 fixed point, k = 0, 1.
    const STEP_POWERS: [u64; STEPS_PER_OCTAVE as usize] = [65536, 92682];
    if quantized == 0 {
        return 0;
    }
    let step = quantized - 1;
    let (octave, octave_step) = (step / STEPS_PER_OCTAVE, step % STEPS_PER_OCTAVE);
    let fraction = STEP_POWERS[octave_step as usize];
    if octave >= 40 {
        u64::MAX >> 8
    } else {
        fraction << octave
    }
}

/// The quantized count whose weight is nearest to `count` in ratio.
pub(crate) fn quantize(count: u32) -> u32 {
    if count == 0 {
        return 0;
    }
    let weight = u64::from(count) << 16;
    let octave = count.ilog2();
    let mut step = octave * STEPS_PER_OCTAVE;
    for octave_step in 0..STEPS_PER_OCTAVE {
        if quantized_weight(1 + octave * STEPS_PER_OCTAVE + octave_step) <= weight {
            step = octave * STEPS_PER_OCTAVE + octave_step;
        }
    }
    let lo = quantized_weight(1 + step);
    let hi = quantized_weight(2 + step);
    // compare weight/lo with hi/weight  <=>  weight*weight vs lo*hi
    #[expect(
        clippy::suspicious_operation_groupings,
        reason = "weight*weight against lo*hi is meant"
    )]
    if u128::from(weight) * u128::from(weight) > u128::from(lo) * u128::from(hi) {
        2 + step
    } else {
        1 + step
    }
}

/// Frequencies summing to exactly `total`, every present symbol at least 1.
pub(crate) fn normalize(weights: &[u64], total: u32) -> Vec<u32> {
    let weight_total: u128 = weights.iter().map(|&weight| u128::from(weight)).sum();
    let mut freqs = vec![0u32; weights.len()];
    if weight_total == 0 {
        return freqs;
    }
    let mut sum: i64 = 0;
    for (symbol, &weight) in weights.iter().enumerate() {
        if weight > 0 {
            freqs[symbol] = ((u128::from(weight) * u128::from(total) / weight_total) as u32).max(1);
            sum += i64::from(freqs[symbol]);
        }
    }
    let mut diff = i64::from(total) - sum;
    while diff != 0 {
        // adjust the largest entry (first one on ties)
        let mut largest = 0;
        for symbol in 0..freqs.len() {
            if freqs[symbol] > freqs[largest] {
                largest = symbol;
            }
        }
        if diff > 0 {
            freqs[largest] += diff.cast_unsigned() as u32;
            diff = 0;
        } else {
            let take = (-diff)
                .min(i64::from(freqs[largest]) - 1)
                .min((i64::from(freqs[largest]) + 1) / 2);
            if take <= 0 {
                break;
            }
            freqs[largest] -= take.cast_unsigned() as u32;
            diff += take;
        }
    }
    freqs
}

/// log2 of `value` (at least 1) with 16 fraction bits, rounded down: integer arithmetic,
/// so every encoder selects the same tables (FORMAT.md section 8).
pub(crate) fn log2_fixed(value: u64) -> u64 {
    const FRACTION_BITS: u32 = 16;
    const ONE: u64 = 1 << 30;
    let integer = value.ilog2();
    // in [ONE, 2 * ONE)
    let mut mantissa = if integer >= 30 {
        value >> (integer - 30)
    } else {
        value << (30 - integer)
    };
    let mut fraction = 0;
    for _ in 0..FRACTION_BITS {
        mantissa = (mantissa * mantissa) >> 30;
        fraction <<= 1;
        if mantissa >= 2 * ONE {
            mantissa >>= 1;
            fraction |= 1;
        }
    }
    u64::from(integer) << FRACTION_BITS | fraction
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn log2_fixed_check_values() {
        for (value, expected) in [
            (1, 0),
            (2, 65_536),
            (3, 103_872),
            (10, 217_705),
            (1000, 653_117),
            (4097, 786_455),
            (u64::from(u32::MAX), 2_097_151),
            (1 << 40, 40 << 16),
            ((1 << 62) + 12_345, 4_063_232),
        ] {
            assert_eq!(log2_fixed(value), expected, "log2_fixed({value})");
        }
        let is_monotone = (1..100_000u64).all(|value| log2_fixed(value) <= log2_fixed(value + 1));
        assert!(is_monotone);
    }

    fn div_round_ref(dividend: i64, divisor: i64) -> i64 {
        if (dividend < 0) == (divisor < 0) {
            (dividend + divisor / 2) / divisor
        } else {
            -((-dividend + divisor / 2) / divisor)
        }
    }
    #[test]
    fn div_round_matches_reference() {
        let mut rng_state = 12345u64;
        for divisor in 1..600i32 {
            for _ in 0..2000 {
                rng_state ^= rng_state << 13;
                rng_state ^= rng_state >> 7;
                rng_state ^= rng_state << 17;
                let dividend =
                    (rng_state % (2 * 32768 * 32767) as u64).cast_signed() - 32768 * 32767;
                assert_eq!(
                    i64::from(div_round(dividend as i32, divisor)),
                    div_round_ref(dividend, i64::from(divisor)),
                    "{dividend} / {divisor}"
                );
            }
            for dividend in [
                -32768 * 32767,
                32767 * 32767,
                0,
                1,
                -1,
                divisor / 2,
                -(divisor / 2),
                divisor,
                -divisor,
            ] {
                assert_eq!(
                    i64::from(div_round(dividend, divisor)),
                    div_round_ref(i64::from(dividend), i64::from(divisor)),
                    "{dividend} / {divisor}"
                );
            }
        }
    }
}
