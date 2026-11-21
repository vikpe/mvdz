//! Tier F: two-pass static rANS with per-context frequency tables.
//!
//! * Symbols: bytes for structural streams (context = the walker's context,
//!   which fits in 16 bits, the table key), or the sign/bit-length bucket of
//!   a residual for numeric streams (context from [`numeric_context`]); numeric
//!   mantissa bits go raw into a separate bit stream (pcodec-style bins +
//!   offsets).
//! * Tables: the encoder counts symbols per context and gives a context its
//!   own table only where that beats the shared fallback table by more than
//!   the table's cost. Counts are sent log-quantised (two steps per octave)
//!   with an adaptive binary coder and normalised to 2^SCALE identically on
//!   both sides (integer arithmetic only).
//! * Coder: 32-bit rANS, 16-bit renormalisation, interleaved states (after
//!   Giesen, "Interleaved entropy coders", 2014; written fresh).
//!   Single-symbol tables cost nothing and are skipped.

use super::rc::{Decoder as RangeDecoder, Encoder as RangeEncoder, IntModel};
use crate::budget::{Budget, Charge, with_capacity};
use crate::error::{Error, Result};
use crate::numeric::NumericContext;
use crate::streams::{PE_OX, PI_AP, PI_OX, SND_O, STREAM_COUNT, TE_O, UC_A, is_lazy, is_numeric};
use crate::util::{ByteReader, approx_f64, par_map, put_u32};
use std::sync::Arc;

pub(crate) const SCALE: u32 = 12;
const FREQ_TOTAL: u32 = 1 << SCALE;
const STATE_MIN: u32 = 1 << 16;
const STATE_COUNT: usize = 4;

/// Steps per octave of the log-quantised counts.
const STEPS: u32 = 2;
/// 2^(k/2) in 16.16 fixed point, k = 0, 1.
const STEP_POWERS: [u64; STEPS as usize] = [65536, 92682];

/// Reconstructed weight (16.16 fixed point) of a log-quantised count.
#[inline]
fn quantized_weight(quantized: u32) -> u64 {
    if quantized == 0 {
        return 0;
    }
    let step = quantized - 1;
    let (octave, octave_step) = (step / STEPS, step % STEPS);
    let fraction = STEP_POWERS[octave_step as usize];
    if octave >= 40 {
        u64::MAX >> 8
    } else {
        fraction << octave
    }
}

/// Log-quantised count: the quantized count whose weight is nearest to
/// `count` in ratio.
fn quantize(count: u32) -> u32 {
    if count == 0 {
        return 0;
    }
    let weight = u64::from(count) << 16;
    let octave = count.ilog2();
    let mut step = octave * STEPS;
    for octave_step in 0..STEPS {
        if quantized_weight(1 + octave * STEPS + octave_step) <= weight {
            step = octave * STEPS + octave_step;
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

/// Normalises weights to a total of exactly `FREQ_TOTAL`, every present
/// symbol >= 1.
fn normalize(weights: &[u64]) -> Vec<u32> {
    let total: u128 = weights.iter().map(|&weight| u128::from(weight)).sum();
    let mut freqs = vec![0u32; weights.len()];
    if total == 0 {
        return freqs;
    }
    let mut sum: i64 = 0;
    for (symbol, &weight) in weights.iter().enumerate() {
        if weight > 0 {
            freqs[symbol] = ((u128::from(weight) * u128::from(FREQ_TOTAL) / total) as u32).max(1);
            sum += i64::from(freqs[symbol]);
        }
    }
    let mut diff = i64::from(FREQ_TOTAL) - sum;
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

#[derive(Clone)]
struct Table {
    quantized: Vec<u32>,
    freq: Vec<u32>,
    cumulative: Vec<u32>,
    single_symbol: Option<u16>,
}

impl Table {
    fn from_quantized(quantized: Vec<u32>) -> Self {
        let weights: Vec<u64> = quantized
            .iter()
            .map(|&count| quantized_weight(count))
            .collect();
        let freq = normalize(&weights);
        let mut cumulative = vec![0u32; freq.len() + 1];
        for symbol in 0..freq.len() {
            cumulative[symbol + 1] = cumulative[symbol] + freq[symbol];
        }
        let present: Vec<usize> = (0..freq.len()).filter(|&symbol| freq[symbol] > 0).collect();
        let single_symbol = if present.len() == 1 {
            Some(present[0] as u16)
        } else {
            None
        };
        Self {
            quantized,
            freq,
            cumulative,
            single_symbol,
        }
    }
}

fn entropy_bits(counts: &[u32]) -> f64 {
    let total: u64 = counts.iter().map(|&count| u64::from(count)).sum();
    if total == 0 {
        return 0.0;
    }
    let log_total = approx_f64(total).log2();
    counts
        .iter()
        .filter(|&&count| count > 0)
        .map(|&count| f64::from(count) * (log_total - f64::from(count).log2()))
        .sum()
}

fn cross_bits(counts: &[u32], reference: &[u32]) -> f64 {
    let total: u64 = reference.iter().map(|&count| u64::from(count)).sum();
    let log_total = approx_f64(total.max(1)).log2();
    let mut bits = 0.0;
    for (symbol, &count) in counts.iter().enumerate() {
        if count > 0 {
            if reference[symbol] == 0 {
                return f64::INFINITY;
            }
            bits = f64::mul_add(
                f64::from(count),
                log_total - f64::from(reference[symbol]).log2(),
                bits,
            );
        }
    }
    bits
}

struct Model {
    alphabet_size: usize,
    /// Own contexts, ascending: `own[i]` codes with `tables[i + 1]`.
    own: Vec<u32>,
    map: Vec<u32>,
    tables: Vec<Table>,
}

/// A stream's counts and the contexts that get their own table.
struct Counts {
    alphabet_size: usize,
    own: Vec<u32>,
    own_rows: Vec<u32>,
    rest: Vec<u32>,
}

/// Per-context symbol counts of a stream, summed over any number of symbol
/// lists (the segments of a file, for its shared model).
pub(crate) struct Tally {
    alphabet_size: usize,
    row_of: Vec<u32>,
    keys: Vec<u32>,
    rows: Vec<u32>,
    global: Vec<u32>,
}

impl Tally {
    pub(crate) fn new(alphabet_size: usize) -> Self {
        Self {
            alphabet_size,
            row_of: Vec::new(),
            keys: Vec::new(),
            rows: Vec::new(),
            global: vec![0; alphabet_size],
        }
    }

    /// Counts the symbols `symbols` of a stream, each with its context in
    /// `contexts`.
    pub(crate) fn add(&mut self, symbols: &[u8], contexts: &[u16]) {
        let alphabet_size = self.alphabet_size;
        let max_context = contexts.iter().copied().max().unwrap_or(0) as usize;
        // row_of, is_own and the model's map are dense arrays sized by the
        // largest context; contexts are 16-bit by spec (§6.12).
        if self.row_of.len() <= max_context {
            self.row_of.resize(max_context + 1, u32::MAX);
        }
        let (row_of, rows, keys, global) = (
            &mut self.row_of,
            &mut self.rows,
            &mut self.keys,
            &mut self.global,
        );
        for (&symbol, &context) in symbols.iter().zip(contexts) {
            let context = u32::from(context);
            let row = row_of[context as usize];
            let row = if row == u32::MAX {
                let row = keys.len() as u32;
                row_of[context as usize] = row;
                keys.push(context);
                rows.resize(rows.len() + alphabet_size, 0);
                row
            } else {
                row
            };
            rows[row as usize * alphabet_size + symbol as usize] += 1;
            global[symbol as usize] += 1;
        }
    }

    /// The model of these counts (a stream's in every segment of a file).
    /// A model whose header breaks the decision limit (rule M2) is made
    /// smaller until it holds: the own contexts with the fewest symbols
    /// join the fallback, an eighth of them at a time. None if even the
    /// fallback table alone breaks the limit.
    pub(crate) fn shared(&self) -> Option<Shared> {
        let mut counts = select(self);
        loop {
            let model = build_model(&counts);
            if let Some(header) = header_within_limit(&model) {
                return Some(Shared { model, header });
            }
            if counts.own.is_empty() {
                return None;
            }
            counts.fold_smallest(counts.own.len().div_ceil(8));
        }
    }
}

impl Counts {
    /// Moves the `count` own contexts with the fewest symbols (ties: the
    /// highest context first) into the fallback's counts.
    fn fold_smallest(&mut self, count: usize) {
        let alphabet_size = self.alphabet_size;
        let row = |index: usize| &self.own_rows[index * alphabet_size..(index + 1) * alphabet_size];
        let mut order: Vec<usize> = (0..self.own.len()).collect();
        order.sort_by_key(|&index| {
            (
                row(index)
                    .iter()
                    .map(|&symbol_count| u64::from(symbol_count))
                    .sum::<u64>(),
                std::cmp::Reverse(index),
            )
        });
        let mut is_dropped = vec![false; self.own.len()];
        for &index in order.iter().take(count) {
            is_dropped[index] = true;
        }
        let mut own = Vec::new();
        let mut own_rows = Vec::new();
        for index in 0..self.own.len() {
            if is_dropped[index] {
                for (rest_count, &symbol_count) in self.rest.iter_mut().zip(row(index)) {
                    *rest_count += symbol_count;
                }
            } else {
                own.push(self.own[index]);
                own_rows.extend_from_slice(row(index));
            }
        }
        self.own = own;
        self.own_rows = own_rows;
    }
}

/// The tables of a model of these counts: own contexts and the fallback's
/// counts.
fn select(tally: &Tally) -> Counts {
    const TABLE_BITS_BASE: f64 = 16.0;
    const TABLE_BITS_PER_SYM: f64 = 5.0;
    let (alphabet_size, row_of, rows, keys) =
        (tally.alphabet_size, &tally.row_of, &tally.rows, &tally.keys);
    let row =
        |context: u32| &rows[row_of[context as usize] as usize * alphabet_size..][..alphabet_size];
    // selection order is part of the output: most frequent context first
    let totals: Vec<u32> = keys
        .iter()
        .map(|&context| row(context).iter().sum())
        .collect();
    let mut order: Vec<usize> = (0..keys.len()).collect();
    order.sort_by_key(|&index| (std::cmp::Reverse(totals[index]), keys[index]));
    let keys: Vec<u32> = order.iter().map(|&index| keys[index]).collect();
    let mut own: Vec<u32> = Vec::new();
    let mut fallback = tally.global.clone();
    for pass in 0..2 {
        own.clear();
        let mut rest = vec![0u32; alphabet_size];
        for &context in &keys {
            let counts = row(context);
            let present = approx_f64(counts.iter().filter(|&&count| count > 0).count() as u64);
            let own_cost =
                TABLE_BITS_PER_SYM.mul_add(present, entropy_bits(counts) + TABLE_BITS_BASE);
            if own_cost < cross_bits(counts, &fallback) {
                own.push(context);
            } else {
                for symbol in 0..alphabet_size {
                    rest[symbol] += counts[symbol];
                }
            }
        }
        if pass == 0 {
            if rest.iter().all(|&count| count == 0) {
                break;
            }
            fallback = rest;
        }
    }
    own.sort_unstable();
    let mut is_own = vec![false; row_of.len()];
    for &context in &own {
        is_own[context as usize] = true;
    }
    let mut rest = vec![0u32; alphabet_size];
    for &context in &keys {
        if !is_own[context as usize] {
            let counts = row(context);
            for symbol in 0..alphabet_size {
                rest[symbol] += counts[symbol];
            }
        }
    }
    let mut own_rows = Vec::with_capacity(own.len() * alphabet_size);
    for &context in &own {
        own_rows.extend_from_slice(row(context));
    }
    Counts {
        alphabet_size,
        own,
        own_rows,
        rest,
    }
}

fn build_model(counts: &Counts) -> Model {
    let alphabet_size = counts.alphabet_size;
    let mut tables = vec![Table::from_quantized(
        counts.rest.iter().map(|&count| quantize(count)).collect(),
    )];
    let map_len = counts.own.last().map_or(0, |&context| context as usize + 1);
    let mut map = vec![0u32; map_len];
    for (index, &context) in counts.own.iter().enumerate() {
        tables.push(Table::from_quantized(
            counts.own_rows[index * alphabet_size..(index + 1) * alphabet_size]
                .iter()
                .map(|&count| quantize(count))
                .collect(),
        ));
        map[context as usize] = (index + 1) as u32;
    }
    Model {
        alphabet_size,
        own: counts.own.clone(),
        map,
        tables,
    }
}

// --------------------------------------------------------------- model I/O

const QUANTIZED_CONTEXTS: usize = 17 * 18;

fn write_table(
    encoder: &mut RangeEncoder,
    int_model: &mut IntModel,
    table: &[u32],
    fallback: Option<&[u32]>,
) {
    let mut previous = 0u32;
    for (symbol, &quantized) in table.iter().enumerate() {
        let fallback_context = fallback.map_or(17, |fallback| fallback[symbol].min(16)) as usize;
        int_model.put(
            encoder,
            fallback_context * 17 + previous.min(16) as usize,
            quantized,
        );
        previous = quantized;
    }
}

fn read_table(
    decoder: &mut RangeDecoder,
    int_model: &mut IntModel,
    alphabet_size: usize,
    fallback: Option<&[u32]>,
) -> Vec<u32> {
    let mut table = vec![0u32; alphabet_size];
    let mut previous = 0u32;
    for symbol in 0..alphabet_size {
        let fallback_context = fallback.map_or(17, |fallback| fallback[symbol].min(16)) as usize;
        table[symbol] = int_model
            .get(decoder, fallback_context * 17 + previous.min(16) as usize)
            .min(160);
        previous = table[symbol];
    }
    table
}

/// The serialized model header and the binary decisions it takes.
fn write_model(model: &Model) -> (Vec<u8>, u64) {
    let mut encoder = RangeEncoder::new();
    let mut int_model = IntModel::new(QUANTIZED_CONTEXTS + 8);
    int_model.put(&mut encoder, QUANTIZED_CONTEXTS, model.alphabet_size as u32);
    int_model.put(
        &mut encoder,
        QUANTIZED_CONTEXTS + 1,
        (model.tables.len() - 1) as u32,
    );
    let fallback = model.tables[0].quantized.clone();
    write_table(&mut encoder, &mut int_model, &fallback, None);
    let mut previous_context = 0u32;
    for (index, &context) in model.own.iter().enumerate() {
        int_model.put(
            &mut encoder,
            QUANTIZED_CONTEXTS + 2,
            if index == 0 {
                context
            } else {
                context - previous_context
            },
        );
        previous_context = context;
        write_table(
            &mut encoder,
            &mut int_model,
            &model.tables[index + 1].quantized,
            Some(&fallback),
        );
    }
    let decisions = encoder.decisions;
    (encoder.finish(), decisions)
}

/// Tables with at most this many symbols are decoded by searching their
/// cumulative frequencies (2 bytes per symbol); wider ones use a slot LUT.
const SEARCH_MAX: usize = 64;

// Context-map entries: an offset into `pool` (search table), or a LUT
// offset with ENTRY_LUT, or a single symbol with ENTRY_SINGLE, or empty.
const ENTRY_SINGLE: u32 = 1 << 31;
const ENTRY_LUT: u32 = 1 << 30;
const ENTRY_EMPTY: u32 = ENTRY_SINGLE | ENTRY_LUT;

/// Format limit on the binary decisions a model header may take:
/// `DECISIONS_PER_BYTE * header length + DECISIONS_BASE` (FORMAT.md, rule M2).
pub(crate) const DECISIONS_PER_BYTE: u64 = 128;
pub(crate) const DECISIONS_BASE: u64 = 1024;

/// Decoder-side model. A search table at pool offset o is
/// `[0, c1, .., cn = FREQ_TOTAL, pad..]` (16*chunks + 1 entries) followed by
/// its 16*chunks symbols, so one table is one or two cache lines.
pub(crate) struct DecoderModel {
    map: Vec<u32>,
    fallback: u32,
    chunks: usize,
    pool: Vec<u16>,
    luts: Vec<u32>,
    _charge: Option<Charge>,
}

/// Number of entries of `table[..16 * chunks]` that are <= `slot` (below
/// `FREQ_TOTAL`).
/// ponytail: about 6 % slower decode than the SSE2 search it replaced, which needs
/// `unsafe`; `std::simd` once stable.
#[inline(always)]
fn count_le(table: &[u16], chunks: usize, slot: u32) -> usize {
    let slot = slot as u16;
    // one counter per lane: the compiler vectorizes this form
    let mut counts = [0u16; 16];
    for chunk in table[..16 * chunks].as_chunks::<16>().0 {
        for lane in 0..16 {
            counts[lane] += u16::from(chunk[lane] <= slot);
        }
    }
    counts.iter().map(|&count| usize::from(count)).sum()
}

/// A model header's own tables' contexts and quantized counts of every
/// table (the fallback first), and their budget charge.
type Header = (Vec<u32>, Vec<u8>, Charge);

/// Reads a model header (FORMAT.md, "Model header").
/// `required_alphabet_size` is the alphabet the stream kind requires.
fn read_header(header: &[u8], required_alphabet_size: usize, budget: &Budget) -> Result<Header> {
    let corrupt = Error::Corrupt("model");
    let max_decisions = (header.len() as u64)
        .saturating_mul(DECISIONS_PER_BYTE)
        .saturating_add(DECISIONS_BASE);
    let mut decoder = RangeDecoder::new(header);
    let mut int_model = IntModel::new(QUANTIZED_CONTEXTS + 8);
    let alphabet_size = int_model.get(&mut decoder, QUANTIZED_CONTEXTS) as usize;
    let own_count = int_model.get(&mut decoder, QUANTIZED_CONTEXTS + 1) as usize;
    if alphabet_size != required_alphabet_size || own_count > 65535 {
        return Err(corrupt);
    }
    // the log-quantised tables (u8: quantized <= 160) until the layout is known
    let charge = budget.alloc(((own_count + 1) * alphabet_size + 4 * (own_count + 1)) as u64)?;
    let mut quantized_tables: Vec<u8> = with_capacity((own_count + 1) * alphabet_size)?;
    let mut keys: Vec<u32> = with_capacity(own_count)?;
    let fallback = read_table(&mut decoder, &mut int_model, alphabet_size, None);
    quantized_tables.extend(fallback.iter().map(|&quantized| quantized as u8));
    let mut context = 0u32;
    for index in 0..own_count {
        if decoder.decisions > max_decisions {
            return Err(corrupt);
        }
        let context_delta = int_model.get(&mut decoder, QUANTIZED_CONTEXTS + 2);
        if index > 0 && context_delta == 0 {
            return Err(corrupt);
        }
        context = context.checked_add(context_delta).ok_or(corrupt)?;
        if context > 0xffff {
            return Err(corrupt);
        }
        let table = read_table(&mut decoder, &mut int_model, alphabet_size, Some(&fallback));
        keys.push(context);
        quantized_tables.extend(table.iter().map(|&quantized| quantized as u8));
        if decoder.is_overrun() {
            return Err(corrupt);
        }
    }
    // the header is consumed exactly: the decoder reads one byte per
    // byte the encoder wrote (5 at the start, then one per renormalisation)
    if decoder.pos() != header.len() || decoder.decisions > max_decisions {
        return Err(corrupt);
    }
    budget.work(decoder.decisions)?;
    Ok((keys, quantized_tables, charge))
}

/// Reads a model header and builds its decoder model.
pub(crate) fn read_model(
    header: &[u8],
    alphabet_size: usize,
    budget: &Budget,
) -> Result<DecoderModel> {
    let (keys, quantized_tables, _charge) = read_header(header, alphabet_size, budget)?;
    // a symbol is present (frequency >= 1) exactly when its quantized count
    // is non-zero (`normalize`), so the layout is known before any table is
    // built; the tables are then built one at a time
    let present = |quantized: &[u8]| quantized.iter().filter(|&&count| count > 0).count();
    // one chunk count for every search table of the model: a fixed trip
    // count keeps the search loop free of mispredicted exits
    let chunks = quantized_tables
        .chunks(alphabet_size)
        .map(present)
        .filter(|&present_count| present_count > 1 && present_count <= SEARCH_MAX)
        .max()
        .unwrap_or(1)
        .div_ceil(16);
    let (mut pool_len, mut lut_len) = (0usize, 0usize);
    for quantized in quantized_tables.chunks(alphabet_size) {
        let present_count = present(quantized);
        if present_count > SEARCH_MAX {
            lut_len += FREQ_TOTAL as usize;
        } else if present_count > 1 {
            pool_len += 2 * 16 * chunks + 1;
        }
    }
    let map_len = keys.last().map_or(0, |&context| context as usize + 1);
    let charge = budget.alloc((pool_len * 2 + lut_len * 4 + map_len * 4) as u64)?;
    let mut model = DecoderModel {
        map: with_capacity(map_len)?,
        fallback: ENTRY_EMPTY,
        chunks,
        pool: with_capacity(pool_len)?,
        luts: with_capacity(lut_len)?,
        _charge: Some(charge),
    };
    let mut entries = with_capacity(keys.len() + 1)?;
    for quantized in quantized_tables.chunks(alphabet_size) {
        let table =
            Table::from_quantized(quantized.iter().map(|&count| u32::from(count)).collect());
        let present: Vec<usize> = (0..alphabet_size)
            .filter(|&symbol| table.freq[symbol] > 0)
            .collect();
        let entry = if present.is_empty() {
            ENTRY_EMPTY
        } else if let Some(symbol) = table.single_symbol {
            ENTRY_SINGLE | u32::from(symbol)
        } else if present.len() <= SEARCH_MAX {
            let offset = model.pool.len() as u32;
            model.pool.push(0);
            for &symbol in &present {
                model.pool.push(table.cumulative[symbol + 1] as u16);
            }
            for _ in present.len()..16 * chunks {
                model.pool.push(0x7fff);
            }
            for &symbol in &present {
                model.pool.push(symbol as u16);
            }
            for _ in present.len()..16 * chunks {
                model.pool.push(0);
            }
            offset
        } else {
            let offset = model.luts.len() as u32;
            model.luts.resize(model.luts.len() + FREQ_TOTAL as usize, 0);
            let lut = &mut model.luts[offset as usize..];
            for &symbol in &present {
                for slot in table.cumulative[symbol]..table.cumulative[symbol + 1] {
                    lut[slot as usize] = symbol as u32
                        | table.freq[symbol] << 8
                        | (slot - table.cumulative[symbol]) << 20;
                }
            }
            ENTRY_LUT | offset
        };
        entries.push(entry);
    }
    if model.pool.len() >= ENTRY_LUT as usize || model.luts.len() >= ENTRY_LUT as usize {
        return Err(Error::Limit("model size"));
    }
    model.fallback = entries[0];
    model.map.resize(map_len, model.fallback);
    for (index, &context) in keys.iter().enumerate() {
        model.map[context as usize] = entries[index + 1];
    }
    Ok(model)
}

impl DecoderModel {
    fn empty() -> Self {
        Self {
            map: Vec::new(),
            fallback: ENTRY_EMPTY,
            chunks: 1,
            pool: Vec::new(),
            luts: Vec::new(),
            _charge: None,
        }
    }
    #[inline(always)]
    fn entry(&self, context: u32) -> u32 {
        let context = context as usize;
        if context < self.map.len() {
            self.map[context]
        } else {
            self.fallback
        }
    }
    /// Decodes one symbol for map entry `entry`; `None` for an empty table.
    #[inline(always)]
    fn decode(&self, decoder: &mut RansDecoder, entry: u32) -> Option<u32> {
        if entry & ENTRY_SINGLE != 0 {
            return if entry & ENTRY_LUT != 0 {
                None
            } else {
                Some(entry & 0xffff)
            };
        }
        let state_index = decoder.state_index;
        decoder.state_index = (state_index + 1) % STATE_COUNT;
        let state = decoder.states[state_index];
        let slot = state & (FREQ_TOTAL - 1);
        let (symbol, mut state) = if entry & ENTRY_LUT == 0 {
            let width = 16 * self.chunks;
            // in range: `read_model` points every search entry at 2*width + 1 pool entries
            let table = &self.pool[entry as usize..=entry as usize + 2 * width];
            let index = count_le(&table[1..], self.chunks, slot);
            let lo = u32::from(table[index]);
            let hi = u32::from(table[index + 1]);
            // wrapping: an initial state from a hostile file may overflow
            // (the result is then garbage that the stream's checks refuse)
            (
                u32::from(table[width + 1 + index]),
                (hi - lo)
                    .wrapping_mul(state >> SCALE)
                    .wrapping_add(slot - lo),
            )
        } else {
            let lut_entry = self.luts[(entry & !ENTRY_LUT) as usize + slot as usize];
            (
                lut_entry & 0xff,
                ((lut_entry >> 8) & 0xfff)
                    .wrapping_mul(state >> SCALE)
                    .wrapping_add(lut_entry >> 20),
            )
        };
        if state < STATE_MIN {
            state = (state << 16) | u32::from(decoder.word());
        }
        decoder.states[state_index] = state;
        Some(symbol)
    }
}

// --------------------------------------------------------------- rANS core

struct RansEncoder {
    states: [u32; STATE_COUNT],
    words: Vec<u16>,
}

impl RansEncoder {
    fn new() -> Self {
        Self {
            states: [STATE_MIN; STATE_COUNT],
            words: Vec::new(),
        }
    }
    #[inline]
    fn put(&mut self, state_index: usize, freq: u32, cumulative: u32) {
        let mut state = self.states[state_index];
        let state_max = ((STATE_MIN >> SCALE) << 16) * freq;
        if state >= state_max {
            self.words.push(state as u16);
            state >>= 16;
        }
        self.states[state_index] = ((state / freq) << SCALE) + (state % freq) + cumulative;
    }
    fn finish(mut self) -> Vec<u8> {
        for state_index in (0..STATE_COUNT).rev() {
            let state = self.states[state_index];
            self.words.push(state as u16);
            self.words.push((state >> 16) as u16);
        }
        self.words.reverse();
        let mut out = Vec::with_capacity(self.words.len() * 2);
        for word in self.words {
            out.extend_from_slice(&word.to_le_bytes());
        }
        out
    }
}

struct RansDecoder<'a> {
    states: [u32; STATE_COUNT],
    input: &'a [u8],
    pos: usize,
    state_index: usize,
}

impl<'a> RansDecoder<'a> {
    fn new(input: &'a [u8]) -> Self {
        let mut decoder = RansDecoder {
            states: [0; STATE_COUNT],
            input,
            pos: 0,
            state_index: 0,
        };
        for state_index in 0..STATE_COUNT {
            let hi = u32::from(decoder.word());
            let lo = u32::from(decoder.word());
            decoder.states[state_index] = hi << 16 | lo;
        }
        decoder
    }
    #[inline(always)]
    fn word(&mut self) -> u16 {
        let word = self
            .input
            .get(self.pos..)
            .and_then(<[u8]>::first_chunk)
            .map_or(0, |bytes| u16::from_le_bytes(*bytes));
        self.pos += 2;
        word
    }
    /// The decoder of an absent stream (no symbol can be decoded).
    fn finished_empty() -> RansDecoder<'static> {
        RansDecoder {
            states: [STATE_MIN; STATE_COUNT],
            input: &[],
            pos: 0,
            state_index: 0,
        }
    }
    /// Every word read and every state back at its initial value.
    fn is_finished(&self) -> bool {
        self.pos == self.input.len() && self.states.iter().all(|&state| state == STATE_MIN)
    }
}

fn rans_encode(model: &Model, symbols: &[u8], contexts: &[u16]) -> Vec<u8> {
    let table_indices: Vec<usize> = contexts
        .iter()
        .map(|&context| model.map.get(context as usize).copied().unwrap_or(0) as usize)
        .collect();
    let mut coded = 0usize;
    let mut state_of = vec![0u8; symbols.len()];
    for (index, &table_index) in table_indices.iter().enumerate() {
        if model.tables[table_index].single_symbol.is_none() {
            state_of[index] = (coded % STATE_COUNT) as u8;
            coded += 1;
        }
    }
    let mut encoder = RansEncoder::new();
    for index in (0..symbols.len()).rev() {
        let table = &model.tables[table_indices[index]];
        if table.single_symbol.is_some() {
            continue;
        }
        let symbol = symbols[index] as usize;
        encoder.put(
            state_of[index] as usize,
            table.freq[symbol],
            table.cumulative[symbol],
        );
    }
    encoder.finish()
}

/// Appends `rans_len u32 | rans | extra`: a stream body after its model
/// header.
fn put_body(out: &mut Vec<u8>, model: &Model, symbols: &[u8], contexts: &[u16], extra: &[u8]) {
    let body = rans_encode(model, symbols, contexts);
    out.reserve(body.len() + extra.len() + 4);
    put_u32(out, body.len());
    out.extend_from_slice(&body);
    out.extend_from_slice(extra);
}

/// A back end 3 stream body: no model header.
fn pack_body(model: &Model, symbols: &[u8], contexts: &[u16], extra: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    put_body(&mut out, model, symbols, contexts, extra);
    out
}

/// The model header, or None if it breaks the decision limit (rule M2).
fn header_within_limit(model: &Model) -> Option<Vec<u8>> {
    let (header, decisions) = write_model(model);
    (decisions <= (header.len() as u64) * DECISIONS_PER_BYTE + DECISIONS_BASE).then_some(header)
}

/// A stream body: `rans_len u32 | rans | extra`.
struct Unpacked<'a> {
    rans: &'a [u8],
    extra: &'a [u8],
}

fn unpack(body: &[u8]) -> Result<Unpacked<'_>> {
    let mut reader = ByteReader::new(body);
    let rans = reader.bytes().ok_or(Error::Corrupt("rans stream"))?;
    Ok(Unpacked {
        rans,
        extra: reader.rest(),
    })
}

// --------------------------------------------------------------- file models

/// A model built from one stream's symbols in every segment of a file
/// ([`Tally::shared`]): its TABLES entry (FORMAT.md §2.6), used by back
/// end 3.
pub(crate) struct Shared {
    model: Model,
    /// the model header
    pub(crate) header: Vec<u8>,
}

/// The decoder models of a file, by stream id.
pub(crate) type Models = Vec<Option<Arc<DecoderModel>>>;

/// The models of a file (FORMAT.md §2.6): the models block is read once,
/// when the file is opened; that checks it and builds its models.
pub(crate) struct FileTables {
    len: u64,
    models: Models,
}

impl FileTables {
    /// Reads a models block's body: checks it and builds its models
    /// (charged to `budget`) on up to `threads` threads.
    pub(crate) fn new(body: &[u8], threads: usize, budget: &Budget) -> Result<Self> {
        Ok(Self {
            len: body.len() as u64,
            models: read_tables(body, threads, budget)?,
        })
    }

    /// The length of the body the models were read from.
    pub(crate) fn len(&self) -> u64 {
        self.len
    }

    pub(crate) fn models(&self) -> &Models {
        &self.models
    }
}

/// Reads `n u32 | n x { sid u32 | len u32 | model }`: ids strictly increasing,
/// each model a structural or numeric stream's (§8.3, M1 without a count).
/// The models do not depend on each other: they are built in parallel.
fn read_tables(body: &[u8], threads: usize, budget: &Budget) -> Result<Models> {
    let corrupt = Error::Corrupt("tables");
    let mut reader = ByteReader::new(body);
    let count = reader.u32_len().ok_or(corrupt)?;
    if count == 0 {
        return Err(corrupt);
    }
    // (stream id, alphabet, model header)
    let mut entries: Vec<(usize, usize, &[u8])> = Vec::new();
    for _ in 0..count {
        let stream = reader.u32_len().ok_or(corrupt)?;
        if entries.last().is_some_and(|previous| stream <= previous.0) {
            return Err(corrupt);
        }
        let alphabet_size = if is_lazy(stream) {
            256
        } else if is_numeric(stream) {
            NUMERIC_ALPHABET
        } else {
            return Err(corrupt);
        };
        entries.push((stream, alphabet_size, reader.bytes().ok_or(corrupt)?));
    }
    if !reader.is_done() {
        return Err(corrupt);
    }
    let built = par_map(threads, entries.len(), |index| {
        read_model(entries[index].2, entries[index].1, budget)
    });
    let mut models: Models = vec![None; STREAM_COUNT];
    for (entry, model) in entries.iter().zip(built) {
        models[entry.0] = Some(Arc::new(model?));
    }
    Ok(models)
}

// --------------------------------------------------------------- byte streams

#[inline(always)]
fn byte_context(context: u32) -> u32 {
    context & 0xffff
}

/// A structural stream coded with its model in the file, `file`.
pub(crate) fn encode_bytes(symbols: &[u8], contexts: &[u16], file: &Shared) -> Vec<u8> {
    pack_body(&file.model, symbols, contexts, &[])
}

pub(crate) struct ByteDecoder<'a> {
    model: Arc<DecoderModel>,
    decoder: RansDecoder<'a>,
    count: usize,
    used: usize,
}

impl<'a> ByteDecoder<'a> {
    pub(crate) fn empty() -> Self {
        ByteDecoder {
            model: Arc::new(DecoderModel::empty()),
            decoder: RansDecoder::finished_empty(),
            count: 0,
            used: 0,
        }
    }
    /// `file`: the stream's model in the file.
    pub(crate) fn new(body: &'a [u8], count: usize, file: &Arc<DecoderModel>) -> Result<Self> {
        let unpacked = unpack(body)?;
        if !unpacked.extra.is_empty() {
            return Err(Error::Corrupt("rans stream"));
        }
        Ok(ByteDecoder {
            model: file.clone(),
            decoder: RansDecoder::new(unpacked.rans),
            count,
            used: 0,
        })
    }
    #[inline(always)]
    pub(crate) fn get(&mut self, context: u32) -> Option<u8> {
        self.used += 1;
        let entry = self.model.entry(byte_context(context));
        self.model
            .decode(&mut self.decoder, entry)
            .map(|symbol| symbol as u8)
    }
    /// Exactly the declared count read and the coded data consumed.
    pub(crate) fn is_done(&self) -> bool {
        self.used == self.count && self.decoder.is_finished()
    }
}

// --------------------------------------------------------------- numeric streams

pub(crate) const NUMERIC_ALPHABET: usize = 65;

/// Static-table context of a numeric value, per stream.
#[inline(always)]
pub(crate) fn numeric_context(stream: usize, context: &NumericContext) -> u32 {
    let features = &context.features;
    let clamped = |value: u8, max: u8| u32::from(value.min(max));
    match stream {
        PI_OX..=50 => clamped(features[0], 19) | clamped(features[1], 19) << 5,
        PI_AP..=53 => clamped(features[0], 23) | clamped(features[1], 23) << 5,
        PE_OX..=56 => {
            clamped(features[0], 17)
                | clamped(features[1], 17) << 5
                | u32::from(features[3] & 1) << 10
        }
        SND_O | TE_O => u32::from(features[0]) | u32::from(features[1]) << 4,
        UC_A => {
            clamped(features[0], 23)
                | clamped(features[1], 23) << 5
                | clamped(features[2], 15) << 10
                | u32::from(features[3]) << 14
        }
        _ => clamped(features[0], 63) | u32::from(features[1]) << 6,
    }
}

#[inline(always)]
fn bucket(value: i32) -> (u16, u32, u32) {
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

#[derive(Default)]
pub(crate) struct BitWriter {
    accumulator: u64,
    bit_count: u32,
    out: Vec<u8>,
}

impl BitWriter {
    #[inline]
    pub(crate) fn put(&mut self, value: u32, bit_count: u32) {
        self.accumulator |= u64::from(value) << self.bit_count;
        self.bit_count += bit_count;
        while self.bit_count >= 8 {
            self.out.push(self.accumulator as u8);
            self.accumulator >>= 8;
            self.bit_count -= 8;
        }
    }
    pub(crate) fn finish(mut self) -> Vec<u8> {
        if self.bit_count > 0 {
            self.out.push(self.accumulator as u8);
        }
        self.out
    }
}

pub(crate) struct BitReader<'a> {
    input: &'a [u8],
    pos: usize,
    accumulator: u64,
    bit_count: u32,
    /// bits handed out so far
    used: u64,
}

impl<'a> BitReader<'a> {
    pub(crate) fn new(input: &'a [u8]) -> Self {
        BitReader {
            input,
            pos: 0,
            accumulator: 0,
            bit_count: 0,
            used: 0,
        }
    }
    #[inline(always)]
    fn refill(&mut self) {
        if let Some(word) = self.input.get(self.pos..).and_then(<[u8]>::first_chunk) {
            let bits = u64::from_le_bytes(*word);
            self.accumulator |= bits << self.bit_count;
            let take = (63 - self.bit_count) / 8;
            self.pos += take as usize;
            self.bit_count += take * 8;
        } else {
            while self.bit_count <= 56 {
                let byte = u64::from(self.input.get(self.pos).copied().unwrap_or(0));
                self.accumulator |= byte << self.bit_count;
                self.pos += 1;
                self.bit_count += 8;
            }
        }
    }
    /// `bit_count` bits (at most 31), least significant first; zeros past
    /// the end.
    #[inline(always)]
    pub(crate) fn get(&mut self, bit_count: u32) -> u32 {
        if self.bit_count < bit_count {
            self.refill();
        }
        let value = (self.accumulator & ((1u64 << bit_count) - 1)) as u32;
        self.accumulator >>= bit_count;
        self.bit_count -= bit_count;
        self.used += u64::from(bit_count);
        value
    }
    /// The bits read end within the last byte (at most 7 padding bits).
    pub(crate) fn is_finished(&self) -> bool {
        self.used.div_ceil(8) == self.input.len() as u64
    }
}

/// The values of a numeric stream as they are coded: the bucket of each
/// value (its symbol), its context, and the mantissa bits of all of them.
pub(crate) struct NumericSymbols {
    pub(crate) symbols: Vec<u8>,
    pub(crate) contexts: Vec<u16>,
    pub(crate) extra: Vec<u8>,
}

/// Collects the values of numeric stream `stream` as [`NumericSymbols`].
pub(crate) struct NumericSymbolWriter {
    stream: usize,
    symbols: Vec<u8>,
    contexts: Vec<u16>,
    bits: BitWriter,
}

impl NumericSymbolWriter {
    pub(crate) fn new(stream: usize) -> Self {
        Self {
            stream,
            symbols: Vec::new(),
            contexts: Vec::new(),
            bits: BitWriter::default(),
        }
    }
    #[inline(always)]
    pub(crate) fn push(&mut self, context: NumericContext, value: i32) {
        let (symbol, mantissa_bits, mantissa) = bucket(value);
        let key = numeric_context(self.stream, &context);
        debug_assert!(
            key <= 0xffff,
            "stream {}: context {key:#x} over 16 bits",
            self.stream
        );
        self.symbols.push(symbol as u8);
        self.contexts.push(key as u16);
        self.bits.put(mantissa, mantissa_bits);
    }
    pub(crate) fn finish(self) -> NumericSymbols {
        NumericSymbols {
            symbols: self.symbols,
            contexts: self.contexts,
            extra: self.bits.finish(),
        }
    }
}

/// A numeric stream coded with its model in the file, `file`.
pub(crate) fn encode_num(num: &NumericSymbols, file: &Shared) -> Vec<u8> {
    pack_body(&file.model, &num.symbols, &num.contexts, &num.extra)
}

pub(crate) struct NumericDecoder<'a> {
    stream: usize,
    model: Arc<DecoderModel>,
    decoder: RansDecoder<'a>,
    bits: BitReader<'a>,
    count: usize,
    used: usize,
}

impl<'a> NumericDecoder<'a> {
    pub(crate) fn empty() -> Self {
        NumericDecoder {
            stream: 0,
            model: Arc::new(DecoderModel::empty()),
            decoder: RansDecoder::finished_empty(),
            bits: BitReader::new(&[]),
            count: 0,
            used: 0,
        }
    }
    /// `file`: the stream's model in the file.
    pub(crate) fn new(
        stream: usize,
        body: &'a [u8],
        count: usize,
        file: &Arc<DecoderModel>,
    ) -> Result<Self> {
        let unpacked = unpack(body)?;
        Ok(NumericDecoder {
            stream,
            model: file.clone(),
            decoder: RansDecoder::new(unpacked.rans),
            bits: BitReader::new(unpacked.extra),
            count,
            used: 0,
        })
    }
    #[inline(always)]
    pub(crate) fn get(&mut self, context: NumericContext) -> Option<i32> {
        self.used += 1;
        let entry = self.model.entry(numeric_context(self.stream, &context));
        let symbol = self.model.decode(&mut self.decoder, entry)?;
        if symbol > 64 {
            return None;
        }
        // branchless: symbol 0 reads no bits and gives 0
        let width = symbol.div_ceil(2);
        let bit_count = width.saturating_sub(1);
        let lead = u32::from(width > 0) << bit_count;
        let magnitude = (lead | self.bits.get(bit_count)).cast_signed();
        Some(if symbol & 1 == 0 {
            magnitude.wrapping_neg()
        } else {
            magnitude
        })
    }
    /// Exactly the declared count read and every byte consumed.
    pub(crate) fn is_done(&self) -> bool {
        self.used == self.count && self.decoder.is_finished() && self.bits.is_finished()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context as _;
    use pretty_assertions::assert_eq;

    /// The shared model of `symbols` and its decoder model.
    fn models(
        symbols: &[u8],
        contexts: &[u16],
        alphabet_size: usize,
    ) -> anyhow::Result<(Shared, Arc<DecoderModel>)> {
        let mut tally = Tally::new(alphabet_size);
        tally.add(symbols, contexts);
        let shared = tally.shared().context("a model")?;
        let model = read_model(&shared.header, alphabet_size, &Budget::unlimited())?;
        Ok((shared, Arc::new(model)))
    }

    #[test]
    fn count_le_counts_entries_up_to_slot() {
        let table: Vec<u16> = (0..64u16).map(|entry| entry * 70).collect();
        for chunks in 1..=4 {
            for slot in [0, 1, 69, 70, 71, 2000, 4095] {
                let expected = table[..16 * chunks]
                    .iter()
                    .filter(|&&entry| u32::from(entry) <= slot)
                    .count();
                assert_eq!(count_le(&table, chunks, slot), expected, "{chunks} {slot}");
            }
        }
    }

    #[test]
    fn byte_round_trip_and_limits() -> anyhow::Result<()> {
        // three contexts with different distributions: own tables
        let contexts: Vec<u16> = (0..5000u16).map(|index| index % 3).collect();
        let symbols: Vec<u8> = (0..5000u32)
            .map(|index| {
                [
                    (index * 7 % 13) as u8,
                    100 + (index % 5) as u8,
                    200 + (index % 2) as u8,
                ][(index % 3) as usize]
            })
            .collect();
        let (shared, model) = models(&symbols, &contexts, 256)?;
        let body = encode_bytes(&symbols, &contexts, &shared);
        let mut decoder = ByteDecoder::new(&body, symbols.len(), &model)?;
        for (index, &symbol) in symbols.iter().enumerate() {
            assert_eq!(decoder.get(u32::from(contexts[index])), Some(symbol));
        }
        assert!(decoder.is_done());
        // the model of a byte stream is refused as a numeric stream's
        assert!(read_model(&shared.header, NUMERIC_ALPHABET, &Budget::unlimited()).is_err());
        // a body cut short is refused; a symbol short does not finish
        assert!(ByteDecoder::new(&body[..body.len() - 2], symbols.len(), &model).is_err());
        let mut decoder = ByteDecoder::new(&body, symbols.len(), &model)?;
        for index in 0..symbols.len() - 1 {
            decoder.get(u32::from(contexts[index]));
        }
        assert!(!decoder.is_done());
        Ok(())
    }

    /// A model whose header would break the decision limit (thousands of
    /// own tables of one symbol each cost almost no bytes) is made smaller
    /// until it holds, and still codes the stream.
    #[test]
    fn model_over_the_decision_limit_shrinks() -> anyhow::Result<()> {
        let count = 60_000u32;
        let contexts: Vec<u16> = (0..count).map(|index| (index % 3000) as u16).collect();
        let symbols: Vec<u8> = contexts
            .iter()
            .map(|&context| (context % 4) as u8)
            .collect();
        let mut tally = Tally::new(256);
        tally.add(&symbols, &contexts);
        let full = build_model(&select(&tally));
        assert!(
            header_within_limit(&full).is_none(),
            "the full model breaks rule M2"
        );
        let (shared, model) = models(&symbols, &contexts, 256)?;
        assert!(shared.model.own.len() < full.own.len());
        let body = encode_bytes(&symbols, &contexts, &shared);
        let mut decoder = ByteDecoder::new(&body, symbols.len(), &model)?;
        for (index, &symbol) in symbols.iter().enumerate() {
            assert_eq!(decoder.get(u32::from(contexts[index])), Some(symbol));
        }
        assert!(decoder.is_done());
        Ok(())
    }

    #[test]
    fn numeric_round_trip() -> anyhow::Result<()> {
        let values: Vec<(NumericContext, i32)> = (0..3000i32)
            .map(|index| {
                (
                    NumericContext {
                        features: [(index % 5).cast_unsigned() as u8, 0, 0, 0, 0, 0],
                    },
                    (index * 37) % 2001 - 1000,
                )
            })
            .collect();
        let mut writer = NumericSymbolWriter::new(PI_OX);
        for (context, value) in &values {
            writer.push(*context, *value);
        }
        let num = writer.finish();
        let (shared, model) = models(&num.symbols, &num.contexts, NUMERIC_ALPHABET)?;
        let body = encode_num(&num, &shared);
        let mut decoder = NumericDecoder::new(PI_OX, &body, values.len(), &model)?;
        for &(context, value) in &values {
            assert_eq!(decoder.get(context), Some(value));
        }
        assert!(decoder.is_done());
        Ok(())
    }
}
