//! Static rANS with per-context tables, integer arithmetic only so both sides agree. After
//! Giesen, "Interleaved entropy coders" (2014); written fresh.

use crate::error::{Error, Result};
use crate::fields::{put_usize, read_bytes, read_length_prefixed, read_usize};
use crate::format::MAX_MODELS_RAW_LEN;
use crate::math::{MAX_QUANTIZED, bucket, log2_fixed, normalize, quantize, quantized_weight};
use crate::memory::try_with_capacity;
use crate::parallel::par_map;
use crate::streams::{STREAM_COUNT, is_numeric, is_structural};
use std::sync::atomic::{AtomicU64, Ordering};

pub(crate) const SCALE: u32 = 12;
const FREQ_TOTAL: u32 = 1 << SCALE;
const STATE_MIN: u32 = 1 << 16;

#[derive(Debug, Clone)]
struct Table {
    quantized: Vec<u32>,
    freq: Vec<u32>,
    cumulative: Vec<u32>,
}

impl Table {
    fn from_quantized(quantized: Vec<u32>) -> Self {
        let weights: Vec<u64> = quantized
            .iter()
            .map(|&count| quantized_weight(count))
            .collect();
        let freq = normalize(&weights, FREQ_TOTAL);
        let mut cumulative = vec![0u32; freq.len() + 1];
        for symbol in 0..freq.len() {
            cumulative[symbol + 1] = cumulative[symbol] + freq[symbol];
        }
        Self {
            quantized,
            freq,
            cumulative,
        }
    }
}

/// In bits with 16 fraction bits, as [`log2_fixed`].
fn entropy_bits(counts: &[u32]) -> u64 {
    let total: u64 = counts.iter().map(|&count| u64::from(count)).sum();
    if total == 0 {
        return 0;
    }
    let log_total = log2_fixed(total);
    counts
        .iter()
        .filter(|&&count| count > 0)
        .map(|&count| u64::from(count) * (log_total - log2_fixed(u64::from(count))))
        .sum()
}

/// The cost of coding `counts` with a table built from `reference`; `None` when the
/// table lacks one of the symbols.
fn cross_bits(counts: &[u32], reference: &[u32]) -> Option<u64> {
    let total: u64 = reference.iter().map(|&count| u64::from(count)).sum();
    let log_total = log2_fixed(total.max(1));
    let mut bits = 0;
    for (symbol, &count) in counts.iter().enumerate() {
        if count > 0 {
            if reference[symbol] == 0 {
                return None;
            }
            bits += u64::from(count) * (log_total - log2_fixed(u64::from(reference[symbol])));
        }
    }
    Some(bits)
}

#[derive(Debug)]
struct Model {
    /// Own contexts, ascending: `own[i]` codes with `tables[i + 1]`.
    own: Vec<u32>,
    map: Vec<u32>,
    tables: Vec<Table>,
}

#[derive(Debug)]
struct Counts {
    alphabet_size: usize,
    own: Vec<u32>,
    own_rows: Vec<u32>,
    rest: Vec<u32>,
}

#[derive(Debug)]
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

    pub(crate) fn add(&mut self, symbols: &[u8], contexts: &[u16]) {
        let alphabet_size = self.alphabet_size;
        let max_context = contexts.iter().copied().max().unwrap_or(0) as usize;
        // dense arrays sized by the largest context: contexts are 16-bit by spec
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

    pub(crate) fn file_model(&self) -> FileModel {
        /// The best of a sweep over 387 demos; part of the canonical encoder.
        const TABLE_COST: TableCost = TableCost {
            base: 48,
            per_symbol: 6,
        };
        self.model_at(TABLE_COST)
    }

    /// The model the models block codes the other models with (FORMAT.md 2.2).
    fn meta_model(&self) -> FileModel {
        /// A table of the meta model is stored as it is, a byte per symbol.
        const TABLE_COST: TableCost = TableCost {
            base: 16 + 8 * 256,
            per_symbol: 0,
        };
        self.model_at(TABLE_COST)
    }

    fn model_at(&self, table_cost: TableCost) -> FileModel {
        let model = build_model(&select(self, table_cost));
        FileModel {
            bytes: raw_model(&model),
            model,
        }
    }
}

/// What an own table costs in the models block, in bits (FORMAT.md 8).
#[derive(Debug, Clone, Copy)]
struct TableCost {
    base: u64,
    per_symbol: u64,
}

fn add_counts(into: &mut [u32], counts: &[u32]) {
    for (total, &count) in into.iter_mut().zip(counts) {
        *total += count;
    }
}

fn select(tally: &Tally, table_cost: TableCost) -> Counts {
    let (alphabet_size, row_of, rows, keys) =
        (tally.alphabet_size, &tally.row_of, &tally.rows, &tally.keys);
    let row =
        |context: u32| &rows[row_of[context as usize] as usize * alphabet_size..][..alphabet_size];
    let mut own: Vec<u32> = Vec::new();
    let mut fallback = tally.global.clone();
    // the contexts not taken as own, summed: the fallback of the next pass
    let mut rest = vec![0u32; alphabet_size];
    for pass in 0..2 {
        own.clear();
        for &context in keys {
            let counts = row(context);
            let present = counts.iter().filter(|&&count| count > 0).count() as u64;
            let table_bits = (table_cost.base + table_cost.per_symbol * present) << 16;
            let own_cost = entropy_bits(counts) + table_bits;
            let is_cheaper_as_own =
                cross_bits(counts, &fallback).is_none_or(|fallback_cost| own_cost < fallback_cost);
            if is_cheaper_as_own {
                own.push(context);
            } else {
                add_counts(&mut rest, counts);
            }
        }
        if pass == 0 {
            fallback = std::mem::replace(&mut rest, vec![0u32; alphabet_size]);
        }
    }
    own.sort_unstable();
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
        own: counts.own.clone(),
        map,
        tables,
    }
}

/// FORMAT.md 6.2: the count of own contexts, the own contexts (u16 each, ascending)
/// and the quantized counts of every table, the fallback first.
fn raw_model(model: &Model) -> Vec<u8> {
    let mut bytes = Vec::new();
    put_usize(&mut bytes, model.own.len());
    for &context in &model.own {
        bytes.extend_from_slice(&(context as u16).to_le_bytes());
    }
    for table in &model.tables {
        bytes.extend(table.quantized.iter().map(|&quantized| quantized as u8));
    }
    bytes
}

/// `None` for more own contexts than 16 bits can number.
fn raw_model_len(own_count: usize, alphabet_size: usize) -> Option<usize> {
    const MAX_OWN_COUNT: usize = 1 << 16;
    const OWN_COUNT_LEN: usize = 4;
    (own_count <= MAX_OWN_COUNT)
        .then_some(OWN_COUNT_LEN + 2 * own_count + (own_count + 1) * alphabet_size)
}

/// Wider tables, and hot ones (`read_model`), decode with a slot LUT instead of a search.
const SEARCH_MAX: usize = 64;

// Context-map entry: a `pool` offset (search table), or with a flag a LUT offset or empty.
const ENTRY_EMPTY: u32 = 1 << 31;
const ENTRY_LUT: u32 = 1 << 30;

/// A search table in `pool` is `[0, c1, .., cn = FREQ_TOTAL, pad..]` (16*chunks + 1
/// entries), then its 16*chunks symbols: one or two cache lines.
#[derive(Debug)]
pub(crate) struct DecoderModel {
    map: Vec<u32>,
    fallback: u32,
    chunks: usize,
    pool: Vec<u16>,
    luts: Vec<u32>,
}

/// The cold tables only; about 6 % slower than the SSE2 search it replaced, which needs
/// `unsafe`. ponytail: `std::simd` once stable.
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

/// Own contexts, quantized counts of every table (the fallback first).
type ModelHeader = (Vec<u32>, Vec<u8>);

/// FORMAT.md 6.2.
fn read_model_header(raw_model: &[u8], alphabet_size: usize) -> Result<ModelHeader> {
    let corrupt = Error::Corrupt("model");
    let mut rest = raw_model;
    let own_count = read_usize(&mut rest).ok_or(corrupt)?;
    if raw_model_len(own_count, alphabet_size) != Some(raw_model.len()) {
        return Err(corrupt);
    }
    let (contexts, quantized_tables) = rest.split_at(2 * own_count);
    let is_quantized = (quantized_tables.iter()).all(|&count| u32::from(count) <= MAX_QUANTIZED);
    if !is_quantized {
        return Err(corrupt);
    }
    let quantized_tables = quantized_tables.to_vec();
    let keys: Vec<u32> = contexts
        .as_chunks::<2>()
        .0
        .iter()
        .map(|context| u32::from(u16::from_le_bytes(*context)))
        .collect();
    let is_ascending = keys.windows(2).all(|pair| pair[0] < pair[1]);
    if !is_ascending {
        return Err(corrupt);
    }
    Ok((keys, quantized_tables))
}

/// The decoder tables of a file's models, against the reader's memory limit.
#[derive(Debug)]
struct TableMemory {
    used: AtomicU64,
    max: u64,
}

impl TableMemory {
    fn new(max: u64) -> Self {
        Self {
            used: AtomicU64::new(0),
            max,
        }
    }

    fn reserve(&self, len: u64) -> Result<()> {
        let used = self
            .used
            .fetch_add(len, Ordering::Relaxed)
            .saturating_add(len);
        if used > self.max {
            return Err(Error::MemoryLimit("models"));
        }
        Ok(())
    }
}

fn read_model(
    header: &[u8],
    alphabet_size: usize,
    table_memory: &TableMemory,
) -> Result<DecoderModel> {
    /// Estimated symbols coded with a table from which its slot LUT pays off.
    const LUT_MIN_USE: u64 = 1 << 14;
    let (keys, quantized_tables) = read_model_header(header, alphabet_size)?;
    // present (frequency >= 1) exactly when the quantized count is non-zero (`normalize`):
    // the layout is known before any table is built
    let present = |quantized: &[u8]| quantized.iter().filter(|&&count| count > 0).count();
    // the quantized counts are within a sqrt(2) step of the real ones: their weights sum
    // to about the symbols the table codes in the file
    let estimated_use = |quantized: &[u8]| {
        let weights = quantized
            .iter()
            .map(|&count| quantized_weight(u32::from(count)));
        weights.sum::<u64>() >> 16
    };
    let is_lut = |quantized: &[u8]| {
        present(quantized) > SEARCH_MAX || estimated_use(quantized) >= LUT_MIN_USE
    };
    // one chunk count for every search table: a fixed trip count avoids mispredicted exits
    let chunks = quantized_tables
        .chunks(alphabet_size)
        .filter(|quantized| !is_lut(quantized))
        .map(present)
        .filter(|&present_count| present_count > 0)
        .max()
        .unwrap_or(1)
        .div_ceil(16);
    let (mut pool_len, mut lut_len) = (0usize, 0usize);
    for quantized in quantized_tables.chunks(alphabet_size) {
        if is_lut(quantized) {
            lut_len += FREQ_TOTAL as usize;
        } else if present(quantized) > 0 {
            pool_len += 2 * 16 * chunks + 1;
        }
    }
    let map_len = keys.last().map_or(0, |&context| context as usize + 1);
    table_memory.reserve((pool_len * 2 + lut_len * 4 + map_len * 4) as u64)?;
    let mut model = DecoderModel {
        map: try_with_capacity(map_len)?,
        fallback: ENTRY_EMPTY,
        chunks,
        pool: try_with_capacity(pool_len)?,
        luts: try_with_capacity(lut_len)?,
    };
    let mut entries = try_with_capacity(keys.len() + 1)?;
    for quantized in quantized_tables.chunks(alphabet_size) {
        let table =
            Table::from_quantized(quantized.iter().map(|&count| u32::from(count)).collect());
        let present: Vec<usize> = (0..alphabet_size)
            .filter(|&symbol| table.freq[symbol] > 0)
            .collect();
        let entry = if present.is_empty() {
            ENTRY_EMPTY
        } else if is_lut(quantized) {
            model.push_lut(&table, &present)
        } else {
            model.push_search_table(&table, &present)
        };
        entries.push(entry);
    }
    if model.pool.len() >= ENTRY_LUT as usize || model.luts.len() >= ENTRY_LUT as usize {
        return Err(Error::Corrupt("model size"));
    }
    model.fallback = entries[0];
    model.map.resize(map_len, model.fallback);
    for (index, &context) in keys.iter().enumerate() {
        model.map[context as usize] = entries[index + 1];
    }
    Ok(model)
}

static EMPTY_MODEL: DecoderModel = DecoderModel {
    map: Vec::new(),
    fallback: ENTRY_EMPTY,
    chunks: 1,
    pool: Vec::new(),
    luts: Vec::new(),
};

impl DecoderModel {
    /// The entry of a cold table with at most [`SEARCH_MAX`] symbols: its offset in `pool`.
    fn push_search_table(&mut self, table: &Table, present: &[usize]) -> u32 {
        let width = 16 * self.chunks;
        let offset = self.pool.len() as u32;
        self.pool.push(0);
        for &symbol in present {
            self.pool.push(table.cumulative[symbol + 1] as u16);
        }
        self.pool.resize(offset as usize + 1 + width, 0x7fff);
        for &symbol in present {
            self.pool.push(symbol as u16);
        }
        self.pool.resize(offset as usize + 1 + 2 * width, 0);
        offset
    }
    /// The entry of a wider or hot table: `ENTRY_LUT` and its offset in `luts`. Each slot holds
    /// the symbol, its frequency less one (a lone symbol's 4096 needs the 13th bit) and
    /// the slot's offset into it.
    fn push_lut(&mut self, table: &Table, present: &[usize]) -> u32 {
        let offset = self.luts.len();
        self.luts.resize(offset + FREQ_TOTAL as usize, 0);
        let lut = &mut self.luts[offset..];
        for &symbol in present {
            for slot in table.cumulative[symbol]..table.cumulative[symbol + 1] {
                lut[slot as usize] = symbol as u32
                    | (table.freq[symbol] - 1) << 8
                    | (slot - table.cumulative[symbol]) << 20;
            }
        }
        ENTRY_LUT | offset as u32
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
    /// `None` for an empty table.
    #[inline(always)]
    fn decode(&self, decoder: &mut RansDecoder, entry: u32) -> Option<u32> {
        if entry == ENTRY_EMPTY {
            return None;
        }
        let state = decoder.state;
        let slot = state & (FREQ_TOTAL - 1);
        let (symbol, mut state) = if entry & ENTRY_LUT == 0 {
            let width = 16 * self.chunks;
            // in range: `read_model` points every search entry at 2*width + 1 pool entries
            let table = &self.pool[entry as usize..=entry as usize + 2 * width];
            let index = count_le(&table[1..], self.chunks, slot);
            let lo = u32::from(table[index]);
            let hi = u32::from(table[index + 1]);
            (
                u32::from(table[width + 1 + index]),
                (hi - lo) * (state >> SCALE) + (slot - lo),
            )
        } else {
            let lut_entry = self.luts[(entry & !ENTRY_LUT) as usize + slot as usize];
            (
                lut_entry & 0xff,
                (((lut_entry >> 8) & 0xfff) + 1) * (state >> SCALE) + (lut_entry >> 20),
            )
        };
        if state < STATE_MIN {
            state = (state << 16) | u32::from(decoder.word());
        }
        decoder.state = state;
        Some(symbol)
    }
}

#[derive(Debug)]
struct RansEncoder {
    state: u32,
    words: Vec<u16>,
}

impl RansEncoder {
    fn new() -> Self {
        Self {
            state: STATE_MIN,
            words: Vec::new(),
        }
    }
    #[inline]
    fn put(&mut self, freq: u32, cumulative: u32) {
        let mut state = self.state;
        let state_max = u64::from(freq) << (16 + 16 - SCALE);
        if u64::from(state) >= state_max {
            self.words.push(state as u16);
            state >>= 16;
        }
        self.state = ((state / freq) << SCALE) + (state % freq) + cumulative;
    }
    fn finish(mut self) -> Vec<u8> {
        self.words.push(self.state as u16);
        self.words.push((self.state >> 16) as u16);
        self.words.reverse();
        let mut out = Vec::with_capacity(self.words.len() * 2);
        for word in self.words {
            out.extend_from_slice(&word.to_le_bytes());
        }
        out
    }
}

#[derive(Debug)]
struct RansDecoder<'a> {
    state: u32,
    input: &'a [u8],
    pos: usize,
}

impl<'a> RansDecoder<'a> {
    fn new(input: &'a [u8]) -> Self {
        let mut decoder = RansDecoder {
            state: 0,
            input,
            pos: 0,
        };
        let hi = u32::from(decoder.word());
        let lo = u32::from(decoder.word());
        decoder.state = hi << 16 | lo;
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
    fn finished_empty() -> RansDecoder<'static> {
        RansDecoder {
            state: STATE_MIN,
            input: &[],
            pos: 0,
        }
    }
    /// Every word read and the state back at its initial value.
    fn is_finished(&self) -> bool {
        self.pos == self.input.len() && self.state == STATE_MIN
    }
}

/// `None` when the model has no code for one of the symbols under its context.
fn rans_encode(model: &Model, symbols: &[u8], contexts: &[u16]) -> Option<Vec<u8>> {
    let mut encoder = RansEncoder::new();
    for (&symbol, &context) in symbols.iter().zip(contexts).rev() {
        let table_index = model.map.get(context as usize).copied().unwrap_or(0) as usize;
        let table = &model.tables[table_index];
        let freq = table.freq[symbol as usize];
        if freq == 0 {
            return None;
        }
        encoder.put(freq, table.cumulative[symbol as usize]);
    }
    Some(encoder.finish())
}

fn pack_body(model: &Model, symbols: &[u8], contexts: &[u16], extra: &[u8]) -> Option<Vec<u8>> {
    let body = rans_encode(model, symbols, contexts)?;
    let mut out = Vec::with_capacity(body.len() + extra.len() + 4);
    put_usize(&mut out, body.len());
    out.extend_from_slice(&body);
    out.extend_from_slice(extra);
    Some(out)
}

/// A stream body: `rans_len u32 | rans | extra`.
#[derive(Debug)]
struct Unpacked<'a> {
    rans: &'a [u8],
    extra: &'a [u8],
}

fn unpack(body: &[u8]) -> Result<Unpacked<'_>> {
    let mut rest = body;
    let rans = read_length_prefixed(&mut rest).ok_or(Error::Corrupt("rans stream"))?;
    Ok(Unpacked { rans, extra: rest })
}

#[derive(Debug)]
pub(crate) struct FileModel {
    model: Model,
    pub(crate) bytes: Vec<u8>,
}

/// By stream id.
pub(crate) type Models = Vec<Option<DecoderModel>>;

/// The model of `stream` in the file's models block: the stream's kind decides its coder,
/// no stream carries a model or coder id of its own.
pub(crate) fn file_model(models: Option<&Models>, stream: usize) -> Result<&DecoderModel> {
    models
        .and_then(|models| models.get(stream)?.as_ref())
        .ok_or(Error::Corrupt("stream without a model"))
}

/// The context of each byte of a raw model under the meta model: the byte before it, 0
/// before the first.
fn previous_bytes(raw_model: &[u8]) -> Vec<u16> {
    let before_first = [0u16];
    let previous = raw_model.iter().map(|&byte| u16::from(byte));
    before_first
        .into_iter()
        .chain(previous)
        .take(raw_model.len())
        .collect()
}

#[derive(Debug)]
pub(crate) struct ModelsBlock {
    pub(crate) body: Vec<u8>,
    /// Of the meta model and every model before coding: what the format limits.
    pub(crate) raw_len: usize,
}

/// FORMAT.md 2.2: the meta model as it is, then every raw model (by stream id,
/// ascending) coded under it.
pub(crate) fn write_models_block(raw_models: &[(usize, &[u8])]) -> ModelsBlock {
    let mut tally = Tally::new(256);
    for (_, raw_model) in raw_models {
        tally.add(raw_model, &previous_bytes(raw_model));
    }
    let meta = tally.meta_model();
    let mut body = meta.bytes.clone();
    let mut raw_len = meta.bytes.len();
    put_usize(&mut body, raw_models.len());
    for &(stream, raw_model) in raw_models {
        let coded = pack_body(&meta.model, raw_model, &previous_bytes(raw_model), &[])
            .expect("the meta model is built from these bytes");
        put_usize(&mut body, stream);
        put_usize(&mut body, raw_model.len());
        put_usize(&mut body, coded.len());
        body.extend_from_slice(&coded);
        raw_len += raw_model.len();
    }
    ModelsBlock { body, raw_len }
}

/// `meta | n u32 | n x { id u32 | raw_len u32 | len u32 | stream }`, ids strictly increasing.
/// The models are independent, so they are built in parallel.
///
/// # Errors
///
/// [`Error::MemoryLimit`] when their decoder tables take more than `max_memory` bytes.
pub(crate) fn read_models_block(body: &[u8], threads: usize, max_memory: u64) -> Result<Models> {
    let corrupt = Error::Corrupt("tables");
    let table_memory = TableMemory::new(max_memory);
    let mut rest = body;
    let meta_own_count = read_usize(&mut &*rest).ok_or(corrupt)?;
    let meta_len = raw_model_len(meta_own_count, 256).ok_or(corrupt)?;
    let meta = read_model(
        read_bytes(&mut rest, meta_len).ok_or(corrupt)?,
        256,
        &table_memory,
    )?;
    let count = read_usize(&mut rest).ok_or(corrupt)?;
    if count == 0 {
        return Err(corrupt);
    }
    // (stream, alphabet size, raw length, coded)
    let mut entries: Vec<(usize, usize, usize, &[u8])> = Vec::new();
    let mut raw_total = meta_len as u64;
    for _ in 0..count {
        let stream = read_usize(&mut rest).ok_or(corrupt)?;
        if entries.last().is_some_and(|previous| stream <= previous.0) {
            return Err(corrupt);
        }
        let alphabet_size = if is_structural(stream) {
            256
        } else if is_numeric(stream) {
            NUMERIC_ALPHABET
        } else {
            return Err(corrupt);
        };
        let raw_len = read_usize(&mut rest).ok_or(corrupt)?;
        raw_total += raw_len as u64;
        let coded = read_length_prefixed(&mut rest).ok_or(corrupt)?;
        entries.push((stream, alphabet_size, raw_len, coded));
    }
    if !rest.is_empty() {
        return Err(corrupt);
    }
    // before any model is decoded: no model allocates more than the format allows
    if raw_total > MAX_MODELS_RAW_LEN {
        return Err(Error::Corrupt("models size"));
    }
    let built = par_map(threads, &entries, |&(_, alphabet_size, raw_len, coded)| {
        let raw_model = decode_raw_model(coded, raw_len, &meta)?;
        read_model(&raw_model, alphabet_size, &table_memory)
    });
    let mut models: Models = Vec::from_fn(STREAM_COUNT, |_| None);
    for (entry, model) in entries.iter().zip(built) {
        models[entry.0] = Some(model?);
    }
    Ok(models)
}

fn decode_raw_model(coded: &[u8], raw_len: usize, meta: &DecoderModel) -> Result<Vec<u8>> {
    let corrupt = Error::Corrupt("model");
    let mut decoder = StreamDecoder::new(coded, meta)?;
    let mut raw_model = try_with_capacity(raw_len)?;
    let mut previous_byte = 0;
    for _ in 0..raw_len {
        previous_byte = decoder.symbol(u32::from(previous_byte)).ok_or(corrupt)?;
        raw_model.push(previous_byte);
    }
    if !decoder.is_done() {
        return Err(corrupt);
    }
    Ok(raw_model)
}

pub(crate) const NUMERIC_ALPHABET: usize = 65;

#[derive(Debug, Default)]
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

#[derive(Debug)]
pub(crate) struct BitReader<'a> {
    input: &'a [u8],
    pos: usize,
    accumulator: u64,
    bit_count: u32,
    bits_used: u64,
}

impl<'a> BitReader<'a> {
    pub(crate) fn new(input: &'a [u8]) -> Self {
        BitReader {
            input,
            pos: 0,
            accumulator: 0,
            bit_count: 0,
            bits_used: 0,
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
    /// At most 31 bits, least significant first; zeros past the end.
    #[inline(always)]
    pub(crate) fn get(&mut self, bit_count: u32) -> u32 {
        if self.bit_count < bit_count {
            self.refill();
        }
        let value = (self.accumulator & ((1u64 << bit_count) - 1)) as u32;
        self.accumulator >>= bit_count;
        self.bit_count -= bit_count;
        self.bits_used += u64::from(bit_count);
        value
    }
    /// At most 7 padding bits.
    pub(crate) fn is_finished(&self) -> bool {
        self.bits_used.div_ceil(8) == self.input.len() as u64
    }
}

/// What one stream codes. `symbols`: the bytes of a structural stream, the bucket of each
/// value of a numeric one; `extra`: the mantissa bits of the values.
#[derive(Debug)]
pub(crate) struct StreamSymbols {
    pub(crate) symbols: Vec<u8>,
    pub(crate) contexts: Vec<u16>,
    pub(crate) extra: Vec<u8>,
}

#[derive(Debug, Default)]
pub(crate) struct StreamWriter {
    symbols: Vec<u8>,
    contexts: Vec<u16>,
    bits: BitWriter,
}

impl StreamWriter {
    #[inline(always)]
    pub(crate) fn push_symbol(&mut self, context: u32, symbol: u8) {
        debug_assert!(context <= 0xffff, "context {context:#x} over 16 bits");
        self.symbols.push(symbol);
        self.contexts.push(context as u16);
    }
    #[inline(always)]
    pub(crate) fn push_value(&mut self, context: u32, value: i32) {
        let (symbol, mantissa_bits, mantissa) = bucket(value);
        self.push_symbol(context, symbol as u8);
        self.bits.put(mantissa, mantissa_bits);
    }
    pub(crate) fn finish(self) -> StreamSymbols {
        StreamSymbols {
            symbols: self.symbols,
            contexts: self.contexts,
            extra: self.bits.finish(),
        }
    }
}

/// `None` when the model has no code for one of the symbols under its context.
pub(crate) fn encode_stream(stream: &StreamSymbols, file_model: &FileModel) -> Option<Vec<u8>> {
    pack_body(
        &file_model.model,
        &stream.symbols,
        &stream.contexts,
        &stream.extra,
    )
}

/// One stream body under its model.
#[derive(Debug)]
pub(crate) struct StreamDecoder<'a> {
    model: &'a DecoderModel,
    decoder: RansDecoder<'a>,
    bits: BitReader<'a>,
}

impl<'a> StreamDecoder<'a> {
    /// Of a stream the segment does not code.
    pub(crate) fn empty() -> Self {
        StreamDecoder {
            model: &EMPTY_MODEL,
            decoder: RansDecoder::finished_empty(),
            bits: BitReader::new(&[]),
        }
    }
    pub(crate) fn new(body: &'a [u8], file_model: &'a DecoderModel) -> Result<Self> {
        let unpacked = unpack(body)?;
        let decoder = RansDecoder::new(unpacked.rans);
        if decoder.state < STATE_MIN {
            return Err(Error::Corrupt("rans stream"));
        }
        Ok(StreamDecoder {
            model: file_model,
            decoder,
            bits: BitReader::new(unpacked.extra),
        })
    }
    #[inline(always)]
    pub(crate) fn symbol(&mut self, context: u32) -> Option<u8> {
        let entry = self.model.entry(context & 0xffff);
        (self.model.decode(&mut self.decoder, entry)).map(|symbol| symbol as u8)
    }
    #[inline(always)]
    pub(crate) fn value(&mut self, context: u32) -> Option<i32> {
        let entry = self.model.entry(context);
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
    /// A structural stream has no mantissa bits: its `extra` is empty.
    pub(crate) fn is_done(&self) -> bool {
        self.decoder.is_finished() && self.bits.is_finished()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context as _;
    use pretty_assertions::assert_eq;

    fn models(
        symbols: &[u8],
        contexts: &[u16],
        alphabet_size: usize,
    ) -> anyhow::Result<(FileModel, DecoderModel)> {
        let mut tally = Tally::new(alphabet_size);
        tally.add(symbols, contexts);
        let file_model = tally.file_model();
        let model = read_model(
            &file_model.bytes,
            alphabet_size,
            &TableMemory::new(u64::MAX),
        )?;
        Ok((file_model, model))
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
        let stream = StreamSymbols {
            symbols,
            contexts,
            extra: Vec::new(),
        };
        let (symbols, contexts) = (&stream.symbols, &stream.contexts);
        let (file_model, model) = models(symbols, contexts, 256)?;
        let body = encode_stream(&stream, &file_model).context("every symbol has a code")?;
        let mut decoder = StreamDecoder::new(&body, &model)?;
        for (index, &symbol) in symbols.iter().enumerate() {
            assert_eq!(decoder.symbol(u32::from(contexts[index])), Some(symbol));
        }
        assert!(decoder.is_done());
        // the model of a byte stream is refused as a numeric stream's
        assert!(
            read_model(
                &file_model.bytes,
                NUMERIC_ALPHABET,
                &TableMemory::new(u64::MAX)
            )
            .is_err()
        );
        // a body cut short is refused; a symbol short does not finish
        assert!(StreamDecoder::new(&body[..body.len() - 2], &model).is_err());
        let mut decoder = StreamDecoder::new(&body, &model)?;
        for &context in &contexts[..symbols.len() - 1] {
            decoder.symbol(u32::from(context));
        }
        assert!(!decoder.is_done());
        Ok(())
    }

    #[test]
    fn quantized_count_over_the_maximum_is_refused() {
        let table_memory = TableMemory::new(u64::MAX);
        let no_own_contexts = 0u32.to_le_bytes();
        let model_of = |quantized: u8| [&no_own_contexts[..], &[quantized; 256]].concat();
        assert!(read_model(&model_of(MAX_QUANTIZED as u8), 256, &table_memory).is_ok());
        assert!(read_model(&model_of(MAX_QUANTIZED as u8 + 1), 256, &table_memory).is_err());
        assert_eq!(quantize(u32::MAX), MAX_QUANTIZED);
    }

    #[test]
    fn hot_tables_take_a_lut_within_the_reservation() -> anyhow::Result<()> {
        let no_own_contexts = 0u32.to_le_bytes();
        let mut quantized = [0u8; NUMERIC_ALPHABET];
        quantized[3] = 1;
        quantized[4] = 1;
        let cold = [&no_own_contexts[..], &quantized].concat();
        let model = read_model(&cold, NUMERIC_ALPHABET, &TableMemory::new(u64::MAX))?;
        assert_eq!((model.pool.len(), model.luts.len()), (2 * 16 + 1, 0));

        quantized[3] = MAX_QUANTIZED as u8;
        let hot = [&no_own_contexts[..], &quantized].concat();
        let model = read_model(&hot, NUMERIC_ALPHABET, &TableMemory::new(u64::MAX))?;
        assert_eq!(
            (model.pool.len(), model.luts.len()),
            (0, FREQ_TOTAL as usize)
        );
        let reserved = (model.pool.len() * 2 + model.luts.len() * 4 + model.map.len() * 4) as u64;
        assert!(read_model(&hot, NUMERIC_ALPHABET, &TableMemory::new(reserved)).is_ok());
        assert_eq!(
            read_model(&hot, NUMERIC_ALPHABET, &TableMemory::new(reserved - 1)).err(),
            Some(Error::MemoryLimit("models"))
        );
        Ok(())
    }

    /// Context 0 always codes one value: a lone symbol of frequency 4096 in a hot table.
    #[test]
    fn hot_narrow_and_lone_symbol_tables_round_trip() -> anyhow::Result<()> {
        let values: Vec<(u32, i32)> = (0..40_000i32)
            .map(|index| ((index % 2).cast_unsigned(), (index % 2) * (index % 3 - 1)))
            .collect();
        let mut writer = StreamWriter::default();
        for (context, value) in &values {
            writer.push_value(*context, *value);
        }
        let stream = writer.finish();
        let (file_model, model) = models(&stream.symbols, &stream.contexts, NUMERIC_ALPHABET)?;
        assert_eq!(model.luts.len(), 2 * FREQ_TOTAL as usize);
        let body = encode_stream(&stream, &file_model).context("every symbol has a code")?;
        let mut decoder = StreamDecoder::new(&body, &model)?;
        for &(context, value) in &values {
            assert_eq!(decoder.value(context), Some(value));
        }
        assert!(decoder.is_done());
        Ok(())
    }

    #[test]
    fn numeric_round_trip() -> anyhow::Result<()> {
        let values: Vec<(u32, i32)> = (0..3000i32)
            .map(|index| ((index % 5).cast_unsigned(), (index * 37) % 2001 - 1000))
            .collect();
        let mut writer = StreamWriter::default();
        for (context, value) in &values {
            writer.push_value(*context, *value);
        }
        let stream = writer.finish();
        let (file_model, model) = models(&stream.symbols, &stream.contexts, NUMERIC_ALPHABET)?;
        let body = encode_stream(&stream, &file_model).context("every symbol has a code")?;
        let mut decoder = StreamDecoder::new(&body, &model)?;
        for &(context, value) in &values {
            assert_eq!(decoder.value(context), Some(value));
        }
        assert!(decoder.is_done());
        Ok(())
    }
}
