//! The blob bytes of a segment as LZ77 tokens (FORMAT.md section 6.6): a literal byte, or a
//! match that copies bytes already produced. Three streams code them, with the models every
//! other stream uses.

use crate::error::{Error, Result};
use crate::math::residual_bucket;
use crate::memory::try_with_capacity;
use crate::numeric::NumericContext;
use crate::rans::{ByteDecoder, NumericDecoder, NumericSymbolWriter, NumericSymbols};
use crate::streams::{LZ_DIST, LZ_LEN};

/// The blob bytes of one segment as the encoder codes them.
#[derive(Debug)]
pub(crate) struct Tokens {
    pub(crate) literals: Vec<u8>,
    /// The byte before each literal, 0 before the first byte.
    pub(crate) literal_contexts: Vec<u16>,
    /// One per token, 0 for a literal.
    pub(crate) lens: NumericSymbols,
    /// One per match.
    pub(crate) dists: NumericSymbols,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Token {
    Literal(u8),
    Match { len: usize, dist: usize },
}

pub(crate) fn tokens(bytes: &[u8]) -> Tokens {
    code(bytes, &parse(bytes))
}

/// `tokens` as their streams code them; `bytes` is what they expand to.
pub(crate) fn code(bytes: &[u8], tokens: &[Token]) -> Tokens {
    let mut literals = Vec::new();
    let mut literal_contexts = Vec::new();
    let mut lens = NumericSymbolWriter::new(LZ_LEN);
    let mut dists = NumericSymbolWriter::new(LZ_DIST);
    let mut position = 0usize;
    let mut previous_len = 0;
    for &token in tokens {
        let len = match token {
            Token::Literal(byte) => {
                let before = position.checked_sub(1).and_then(|before| bytes.get(before));
                let previous_byte = before.copied().unwrap_or(0);
                literals.push(byte);
                literal_contexts.push(u16::from(previous_byte));
                position += 1;
                0
            }
            Token::Match { len, dist } => {
                dists.push(bucket_context(len), coded_value(dist));
                position += len;
                len
            }
        };
        lens.push(bucket_context(previous_len), coded_value(len));
        previous_len = len;
    }
    Tokens {
        literals,
        literal_contexts,
        lens: lens.finish(),
        dists: dists.finish(),
    }
}

/// The canonical parse (FORMAT.md section 8): greedy, the longest of at most `MAX_CHAIN`
/// earlier positions with the same hash, the nearest of equally long ones.
pub(crate) fn parse(bytes: &[u8]) -> Vec<Token> {
    const MIN_MATCH: usize = 4;
    const MAX_CHAIN: usize = 32;
    const HASH_BITS: u32 = 15;
    const NONE: u32 = u32::MAX;
    let hash = |position: usize| -> Option<usize> {
        let first_four = bytes.get(position..)?.first_chunk::<4>()?;
        let product = u32::from_le_bytes(*first_four).wrapping_mul(0x9E37_79B1);
        Some((product >> (32 - HASH_BITS)) as usize)
    };
    // the latest position of each hash, and for each position the one before it
    let mut latest = vec![NONE; 1 << HASH_BITS];
    let mut earlier = vec![NONE; bytes.len()];
    let mut tokens = Vec::new();
    let mut position = 0;
    while position < bytes.len() {
        let (mut best_len, mut best_start) = (0, 0);
        let mut candidate = hash(position).map_or(NONE, |slot| latest[slot]);
        let mut steps = 0;
        while candidate != NONE && steps < MAX_CHAIN {
            let start = candidate as usize;
            let len = common_prefix_len(&bytes[start..], &bytes[position..]);
            if len > best_len {
                (best_len, best_start) = (len, start);
            }
            candidate = earlier[start];
            steps += 1;
        }
        let advance = if best_len >= MIN_MATCH {
            tokens.push(Token::Match {
                len: best_len,
                dist: position - best_start,
            });
            best_len
        } else {
            tokens.push(Token::Literal(bytes[position]));
            1
        };
        let covered = earlier.iter_mut().enumerate().skip(position).take(advance);
        for (covered_position, earlier_position) in covered {
            if let Some(slot) = hash(covered_position) {
                *earlier_position = latest[slot];
                latest[slot] = covered_position as u32;
            }
        }
        position += advance;
    }
    tokens
}

/// The `len` blob bytes of a segment from its tokens.
pub(crate) fn expand(
    literals: &mut ByteDecoder,
    lens: &mut NumericDecoder,
    dists: &mut NumericDecoder,
    len: usize,
) -> Result<Vec<u8>> {
    let corrupt = Error::Corrupt("blob tokens");
    let mut bytes: Vec<u8> = try_with_capacity(len)?;
    let mut previous_len = 0;
    while bytes.len() < len {
        let token_len = lens.get(bucket_context(previous_len)).ok_or(corrupt)?;
        let token_len = usize::try_from(token_len).map_err(|_| corrupt)?;
        if token_len == 0 {
            let previous_byte = bytes.last().copied().unwrap_or(0);
            bytes.push(literals.get(u32::from(previous_byte)).ok_or(corrupt)?);
        } else {
            let dist = dists.get(bucket_context(token_len)).ok_or(corrupt)?;
            let dist = usize::try_from(dist).map_err(|_| corrupt)?;
            let is_inside = dist >= 1 && dist <= bytes.len() && token_len <= len - bytes.len();
            if !is_inside {
                return Err(corrupt);
            }
            // byte by byte: a match may run into the bytes it produces
            for _ in 0..token_len {
                bytes.push(bytes[bytes.len() - dist]);
            }
        }
        previous_len = token_len;
    }
    Ok(bytes)
}

fn bucket_context(value: usize) -> NumericContext {
    const MAX_BUCKET: u8 = 63;
    let bucket = residual_bucket(coded_value(value)).min(MAX_BUCKET);
    NumericContext {
        features: [bucket, 0, 0, 0],
    }
}

/// A length or distance as its stream codes it: within the blob bytes of one segment.
fn coded_value(value: usize) -> i32 {
    (value as u32).cast_signed()
}

fn common_prefix_len(earlier: &[u8], later: &[u8]) -> usize {
    earlier
        .iter()
        .zip(later)
        .take_while(|(earlier_byte, later_byte)| earlier_byte == later_byte)
        .count()
}

#[cfg(test)]
mod tests {
    use super::{Token, parse};
    use pretty_assertions::assert_eq;

    fn replay(tokens: &[Token]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for &token in tokens {
            match token {
                Token::Literal(byte) => bytes.push(byte),
                Token::Match { len, dist } => {
                    for _ in 0..len {
                        bytes.push(bytes[bytes.len() - dist]);
                    }
                }
            }
        }
        bytes
    }

    #[test]
    fn parse_is_greedy_and_takes_the_nearest_of_equal_matches() {
        let literals =
            |text: &[u8]| -> Vec<Token> { text.iter().map(|&byte| Token::Literal(byte)).collect() };
        // "abcd" occurs twice before the last one: the nearer start wins
        let mut expected = literals(b"abcdX");
        expected.push(Token::Match { len: 4, dist: 5 });
        expected.push(Token::Literal(b'Y'));
        expected.push(Token::Match { len: 4, dist: 5 });
        assert_eq!(parse(b"abcdXabcdYabcd"), expected);
        // a match may overlap the bytes it produces
        let mut expected = literals(b"a");
        expected.push(Token::Match { len: 9, dist: 1 });
        assert_eq!(parse(b"aaaaaaaaaa"), expected);
        // three equal bytes are under the shortest match
        assert_eq!(parse(b"abcXabc"), literals(b"abcXabc"));
        assert_eq!(parse(b""), Vec::new());
    }

    #[test]
    fn tokens_replay_to_the_input() {
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        let mut noise = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let text = b"\\name\\player\\team\\red\\skin\\base\n".repeat(40);
        let zeros = vec![0u8; 5000];
        let random: Vec<u8> = (0..5000).map(|_| noise() as u8).collect();
        let mixed: Vec<u8> = (0..20_000)
            .map(|index| {
                if noise() % 7 == 0 {
                    noise() as u8
                } else {
                    text[index % text.len()]
                }
            })
            .collect();
        for input in [text, zeros, random, mixed] {
            assert!(replay(&parse(&input)) == input);
        }
    }
}
