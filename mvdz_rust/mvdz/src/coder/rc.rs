//! LZMA-style binary range coder: 32-bit range, byte-wise normalization, carry through a
//! cache byte. `probability` is P(bit = 1) in 1..=4095. From the LZMA description, no code copied.

pub(crate) const PROBABILITY_BITS: u32 = 12;
const TOP: u32 = 1 << 24;

#[derive(Debug)]
pub(crate) struct RangeEncoder {
    pub(crate) decisions: u64,
    low: u64,
    range: u32,
    cache: u8,
    cache_size: u64,
    pub(crate) out: Vec<u8>,
}

impl Default for RangeEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl RangeEncoder {
    pub(crate) fn new() -> Self {
        Self {
            decisions: 0,
            low: 0,
            range: u32::MAX,
            cache: 0,
            cache_size: 1,
            out: Vec::new(),
        }
    }

    #[inline(always)]
    fn shift_low(&mut self) {
        if (self.low as u32) < 0xFF00_0000 || (self.low >> 32) != 0 {
            let carry = (self.low >> 32) as u8;
            let mut byte = self.cache;
            loop {
                self.out.push(byte.wrapping_add(carry));
                byte = 0xFF;
                self.cache_size -= 1;
                if self.cache_size == 0 {
                    break;
                }
            }
            self.cache = (self.low >> 24) as u8;
        }
        self.cache_size += 1;
        self.low = (self.low & 0x00FF_FFFF) << 8;
    }

    #[inline(always)]
    pub(crate) fn bit(&mut self, bit: u32, probability: u32) {
        self.decisions += 1;
        let bound = (self.range >> PROBABILITY_BITS) * probability;
        if bit != 0 {
            self.range = bound;
        } else {
            self.low += u64::from(bound);
            self.range -= bound;
        }
        while self.range < TOP {
            self.range <<= 8;
            self.shift_low();
        }
    }

    pub(crate) fn finish(mut self) -> Vec<u8> {
        for _ in 0..5 {
            self.shift_low();
        }
        self.out
    }
}

#[derive(Debug)]
pub(crate) struct RangeDecoder<'a> {
    pub(crate) decisions: u64,
    code: u32,
    range: u32,
    input: &'a [u8],
    pos: usize,
}

impl<'a> RangeDecoder<'a> {
    pub(crate) fn new(input: &'a [u8]) -> Self {
        let mut decoder = RangeDecoder {
            decisions: 0,
            code: 0,
            range: u32::MAX,
            input,
            pos: 0,
        };
        for _ in 0..5 {
            decoder.code = (decoder.code << 8) | u32::from(decoder.next());
        }
        decoder
    }

    #[inline(always)]
    fn next(&mut self) -> u8 {
        let byte = self.input.get(self.pos).copied().unwrap_or(0);
        self.pos += 1;
        byte
    }

    /// Including the zeros read past the end.
    pub(crate) fn pos(&self) -> usize {
        self.pos
    }

    /// Read past the end by more than the 5 flush bytes: corrupt.
    pub(crate) fn is_overrun(&self) -> bool {
        self.pos > self.input.len() + 5
    }

    #[inline(always)]
    pub(crate) fn bit(&mut self, probability: u32) -> u32 {
        self.decisions += 1;
        let bound = (self.range >> PROBABILITY_BITS) * probability;
        let bit = u32::from(self.code < bound);
        let mask = bit.wrapping_neg();
        self.code -= bound & !mask;
        self.range = (bound & mask) | (self.range.wrapping_sub(bound) & !mask);
        while self.range < TOP {
            self.range <<= 8;
            self.code = (self.code << 8) | u32::from(self.next());
        }
        bit
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Probability(pub(crate) u16);

impl Default for Probability {
    fn default() -> Self {
        Self(1 << 15)
    }
}

impl Probability {
    #[inline(always)]
    pub(crate) fn value(self) -> u32 {
        (u32::from(self.0) >> 4).clamp(1, 4095)
    }
    #[inline(always)]
    pub(crate) fn update(&mut self, bit: u32, shift: u32) {
        if bit != 0 {
            self.0 += (65535 - self.0) >> shift;
        } else {
            self.0 -= self.0 >> shift;
        }
    }
}

/// Elias-gamma binarization, an adaptive bit per (context, position).
#[derive(Debug)]
pub(crate) struct IntModel {
    probabilities: Vec<Probability>,
}

impl IntModel {
    pub(crate) fn new(context_count: usize) -> Self {
        Self {
            probabilities: vec![Probability::default(); context_count * 64],
        }
    }
    pub(crate) fn put(&mut self, encoder: &mut RangeEncoder, context: usize, value: u32) {
        let value_plus_one = u64::from(value) + 1;
        let bit_count = value_plus_one.ilog2();
        let base = context * 64;
        for position in 0..bit_count {
            let probability = &mut self.probabilities[base + position as usize];
            encoder.bit(1, probability.value());
            probability.update(1, 4);
        }
        if bit_count < 32 {
            let probability = &mut self.probabilities[base + bit_count as usize];
            encoder.bit(0, probability.value());
            probability.update(0, 4);
        }
        for position in (0..bit_count).rev() {
            let bit = ((value_plus_one >> position) & 1) as u32;
            let probability = &mut self.probabilities[base + 33 + (position as usize).min(30)];
            encoder.bit(bit, probability.value());
            probability.update(bit, 5);
        }
    }
    pub(crate) fn get(&mut self, decoder: &mut RangeDecoder, context: usize) -> u32 {
        let base = context * 64;
        let mut bit_count: usize = 0;
        while bit_count < 32 {
            let probability = &mut self.probabilities[base + bit_count];
            let bit = decoder.bit(probability.value());
            probability.update(bit, 4);
            if bit == 0 {
                break;
            }
            bit_count += 1;
        }
        let mut value_plus_one: u64 = 1;
        for position in (0..bit_count).rev() {
            let probability = &mut self.probabilities[base + 33 + position.min(30)];
            let bit = decoder.bit(probability.value());
            probability.update(bit, 5);
            value_plus_one = (value_plus_one << 1) | u64::from(bit);
        }
        (value_plus_one - 1) as u32
    }
}
