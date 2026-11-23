//! The blob bytes of a segment as LZ77 tokens (FORMAT.md 6.6): a literal byte, or a
//! match that copies bytes already produced. Three streams code them, with the models every
//! other stream uses.

use crate::error::{Error, Result};
use crate::math::clamped_bucket;
use crate::memory::try_with_capacity;
use crate::rans::{StreamDecoder, StreamSymbols, StreamWriter};
use crate::streams::{LZ_DISTANCE, LZ_LENGTH, LZ_LITERAL};

/// The blob bytes of one segment as the encoder codes them.
#[derive(Debug)]
pub(crate) struct Tokens {
    /// Each under the byte before it, 0 before the first byte.
    pub(crate) literals: StreamSymbols,
    /// One per token, 0 for a literal.
    pub(crate) lengths: StreamSymbols,
    /// One per match.
    pub(crate) distances: StreamSymbols,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Token {
    Literal(u8),
    Match { len: usize, distance: usize },
}

pub(crate) fn tokens(bytes: &[u8]) -> Tokens {
    code(bytes, &parse(bytes))
}

/// `tokens` as their streams code them; `bytes` is what they expand to.
pub(crate) fn code(bytes: &[u8], tokens: &[Token]) -> Tokens {
    let mut literals = StreamWriter::default();
    let mut lengths = StreamWriter::default();
    let mut distances = StreamWriter::default();
    let mut position = 0usize;
    let mut previous_len = 0;
    for &token in tokens {
        let len = match token {
            Token::Literal(byte) => {
                let before = position.checked_sub(1).and_then(|before| bytes.get(before));
                let previous_byte = before.copied().unwrap_or(0);
                literals.push_symbol(u32::from(previous_byte), byte);
                position += 1;
                0
            }
            Token::Match { len, distance } => {
                distances.push_value(bucket_context(len), coded_value(distance));
                position += len;
                len
            }
        };
        lengths.push_value(bucket_context(previous_len), coded_value(len));
        previous_len = len;
    }
    Tokens {
        literals: literals.finish(),
        lengths: lengths.finish(),
        distances: distances.finish(),
    }
}

/// The canonical parse (FORMAT.md 8): greedy, the longest of at most `MAX_CHAIN`
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
                distance: position - best_start,
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

/// The `len` blob bytes of a segment from its tokens; `decoders` by stream id.
pub(crate) fn expand(decoders: &mut [StreamDecoder], len: usize) -> Result<Vec<u8>> {
    let corrupt = Error::Corrupt("blob tokens");
    let mut bytes: Vec<u8> = try_with_capacity(len)?;
    let mut previous_len = 0;
    while bytes.len() < len {
        let token_len = decoders[LZ_LENGTH]
            .value(bucket_context(previous_len))
            .ok_or(corrupt)?;
        let token_len = usize::try_from(token_len).map_err(|_| corrupt)?;
        if token_len == 0 {
            let previous_byte = bytes.last().copied().unwrap_or(0);
            let literal = decoders[LZ_LITERAL].symbol(u32::from(previous_byte));
            bytes.push(literal.ok_or(corrupt)?);
        } else {
            let distance = decoders[LZ_DISTANCE]
                .value(bucket_context(token_len))
                .ok_or(corrupt)?;
            let distance = usize::try_from(distance).map_err(|_| corrupt)?;
            let is_inside =
                distance >= 1 && distance <= bytes.len() && token_len <= len - bytes.len();
            if !is_inside {
                return Err(corrupt);
            }
            // byte by byte: a match may run into the bytes it produces
            for _ in 0..token_len {
                bytes.push(bytes[bytes.len() - distance]);
            }
        }
        previous_len = token_len;
    }
    Ok(bytes)
}

fn bucket_context(value: usize) -> u32 {
    const MAX_BUCKET: u8 = 63;
    clamped_bucket(coded_value(value), MAX_BUCKET)
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
    use crate::tests::common::Rng;
    use pretty_assertions::assert_eq;

    fn replay(tokens: &[Token]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for &token in tokens {
            match token {
                Token::Literal(byte) => bytes.push(byte),
                Token::Match { len, distance } => {
                    for _ in 0..len {
                        bytes.push(bytes[bytes.len() - distance]);
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
        expected.push(Token::Match {
            len: 4,
            distance: 5,
        });
        expected.push(Token::Literal(b'Y'));
        expected.push(Token::Match {
            len: 4,
            distance: 5,
        });
        assert_eq!(parse(b"abcdXabcdYabcd"), expected);
        // a match may overlap the bytes it produces
        let mut expected = literals(b"a");
        expected.push(Token::Match {
            len: 9,
            distance: 1,
        });
        assert_eq!(parse(b"aaaaaaaaaa"), expected);
        // three equal bytes are under the shortest match
        assert_eq!(parse(b"abcXabc"), literals(b"abcXabc"));
        assert_eq!(parse(b""), Vec::new());
    }

    #[test]
    fn tokens_replay_to_the_input() {
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        let mut noise = || rng.next();
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
