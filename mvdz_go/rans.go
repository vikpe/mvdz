package mvdz

import (
	"cmp"
	"encoding/binary"
	"fmt"
	"math/bits"
	"slices"
	"sync/atomic"
)

const (
	structuralAlphabetSize = 256
	numericAlphabetSize    = 65
	ownCountSize           = 4
	maxOwnCount            = 1 << 16
	scaleBits              = 12
	frequencyTotal         = 1 << scaleBits
	ransLowerBound         = 1 << 16
)

// streamModel is the model of one stream as the encoder codes with it,
// FORMAT.md 6.2.
type streamModel struct {
	raw     []byte
	tableOf []uint32    // by context: the index into tables; contexts past its end use the fallback
	tables  []ransTable // the fallback, then one per own context
}

// fileModels is by stream id; nil where the models block has none.
type fileModels [firstBlobStream]*streamModel

func (model *streamModel) table(context uint16) ransTable {
	if int(context) < len(model.tableOf) {
		return model.tables[model.tableOf[context]]
	}
	return model.tables[0]
}

// newTableOf takes the own contexts in ascending order.
func newTableOf(ownContexts []uint16) []uint32 {
	if len(ownContexts) == 0 {
		return nil
	}
	tableOf := make([]uint32, int(ownContexts[len(ownContexts)-1])+1)
	for index, context := range ownContexts {
		tableOf[context] = uint32(index + 1)
	}
	return tableOf
}

// ransTable is by symbol: a symbol owns the slots table[symbol] to
// table[symbol+1], FORMAT.md 6.3.
type ransTable []uint16

func newRansTables(quantizedTables []byte, alphabetSize int) []ransTable {
	tables := make([]ransTable, len(quantizedTables)/alphabetSize)
	for index := range tables {
		tables[index] = newRansTable(quantizedTables[index*alphabetSize:][:alphabetSize])
	}
	return tables
}

func newRansTable(quantized []byte) ransTable {
	weights := make([]uint64, len(quantized))
	for symbol, count := range quantized {
		weights[symbol] = quantizedWeight(uint32(count))
	}
	table := make(ransTable, len(quantized)+1)
	for symbol, frequency := range normalize(weights, frequencyTotal) {
		table[symbol+1] = table[symbol] + uint16(frequency)
	}
	return table
}

// find gives the frequency 0 for a symbol the table does not hold.
func (table ransTable) find(symbol uint8) (start uint32, frequency uint32) {
	start = uint32(table[symbol])
	return start, uint32(table[int(symbol)+1]) - start
}

// FORMAT.md 6.2
func rawModelLen(ownCount, alphabetSize int) int {
	return ownCountSize + 2*ownCount + (ownCount+1)*alphabetSize
}

// readModelHeader gives the own contexts and the quantized counts of every
// table, the fallback first; FORMAT.md 6.2.
func readModelHeader(raw []byte, alphabetSize int) (ownContexts []uint16, quantizedTables []byte, err error) {
	if len(raw) < ownCountSize {
		return nil, nil, corrupt("model")
	}
	declaredOwnCount := binary.LittleEndian.Uint32(raw)
	if declaredOwnCount > maxOwnCount || len(raw) != rawModelLen(int(declaredOwnCount), alphabetSize) {
		return nil, nil, corrupt("model")
	}
	ownCount := int(declaredOwnCount)
	ownContexts = make([]uint16, ownCount)
	for index := range ownContexts {
		ownContexts[index] = binary.LittleEndian.Uint16(raw[ownCountSize+2*index:])
		if index > 0 && ownContexts[index] <= ownContexts[index-1] {
			return nil, nil, corrupt("model")
		}
	}
	quantizedTables = raw[ownCountSize+2*ownCount:]
	for _, quantized := range quantizedTables {
		if quantized > maxQuantized {
			return nil, nil, corrupt("model")
		}
	}
	return ownContexts, quantizedTables, nil
}

// ransModels is the decoder's model by stream id; nil where the models block
// has none.
type ransModels [firstBlobStream]*decoderModel

// An entry of a decoderModel is entryEmpty for a table without symbols,
// entryLookup with an offset into lookupTables, or an offset into pool.
const (
	entryEmpty  = 1 << 31
	entryLookup = 1 << 30
)

// decoderModel is the model of one stream as the decoder reads symbols with
// it, FORMAT.md 6.4. A symbol's range is packed as the symbol, its frequency
// less one from bit 8 (a lone symbol's 4096 needs the 13th bit), and from
// bit 20 the slot the range starts at (pool) or the slot's offset into the
// range (lookupTables).
type decoderModel struct {
	entryOf  []uint32 // by context; contexts past its end use fallback
	fallback uint32
	// The search tables: the ranges of a table's symbols, the most frequent first.
	pool []uint32
	// frequencyTotal slots per table: the range each slot is in.
	lookupTables []uint32
}

// newDecoderModel reads a raw model. A wide table decodes with a slot lookup
// table: a search of it could take as many steps as it has symbols.
func newDecoderModel(raw []byte, alphabetSize int, memory *tableMemory) (*decoderModel, error) {
	// ponytail: Rust also gives a model's 16 most used tables a lookup table;
	// measured in Go, the search that starts at the most frequent symbol is faster.
	const searchMax = 64
	ownContexts, quantizedTables, err := readModelHeader(raw, alphabetSize)
	if err != nil {
		return nil, err
	}
	tableCount := len(ownContexts) + 1
	// present (frequency >= 1) exactly when the quantized count is non-zero
	// (normalize): the layout is known before any table is built
	presentCounts := make([]int, tableCount)
	for index := range presentCounts {
		for _, count := range quantizedTables[index*alphabetSize:][:alphabetSize] {
			if count > 0 {
				presentCounts[index]++
			}
		}
	}

	poolLen, lookupLen := 0, 0
	for _, presentCount := range presentCounts {
		if presentCount > searchMax {
			lookupLen += frequencyTotal
		} else {
			poolLen += presentCount
		}
	}
	entryOfLen := 0
	if len(ownContexts) > 0 {
		entryOfLen = int(ownContexts[len(ownContexts)-1]) + 1
	}
	if err := memory.reserve(int64(4 * (poolLen + lookupLen + entryOfLen))); err != nil {
		return nil, err
	}
	// at most maxOwnCount+1 tables: every offset is below entryLookup
	model := &decoderModel{
		entryOf:      make([]uint32, entryOfLen),
		pool:         make([]uint32, 0, poolLen),
		lookupTables: make([]uint32, 0, lookupLen),
	}
	entries := make([]uint32, tableCount)
	for index := range entries {
		table := newRansTable(quantizedTables[index*alphabetSize:][:alphabetSize])
		switch {
		case presentCounts[index] == 0:
			entries[index] = entryEmpty
		case presentCounts[index] > searchMax:
			entries[index] = model.appendLookupTable(table)
		default:
			entries[index] = model.appendSearchTable(table)
		}
	}
	model.fallback = entries[0]
	for context := range model.entryOf {
		model.entryOf[context] = model.fallback
	}
	for index, context := range ownContexts {
		model.entryOf[context] = entries[index+1]
	}
	return model, nil
}

func (model *decoderModel) appendSearchTable(table ransTable) uint32 {
	entry := uint32(len(model.pool))
	for symbol := range len(table) - 1 {
		start, end := uint32(table[symbol]), uint32(table[symbol+1])
		if end > start {
			model.pool = append(model.pool, uint32(symbol)|(end-start-1)<<8|start<<20)
		}
	}
	slices.SortFunc(model.pool[entry:], func(a, b uint32) int {
		return cmp.Or(cmp.Compare(b>>8&0xfff, a>>8&0xfff), cmp.Compare(a, b))
	})
	return entry
}

func (model *decoderModel) appendLookupTable(table ransTable) uint32 {
	entry := entryLookup | uint32(len(model.lookupTables))
	for symbol := range len(table) - 1 {
		start, end := uint32(table[symbol]), uint32(table[symbol+1])
		for slot := start; slot < end; slot++ {
			model.lookupTables = append(model.lookupTables, uint32(symbol)|(end-start-1)<<8|(slot-start)<<20)
		}
	}
	return entry
}

// tableMemory is the decoder tables of a file's models, against the reader's
// memory limit.
type tableMemory struct {
	used atomic.Int64
	max  int64
}

func (memory *tableMemory) reserve(size int64) error {
	if memory.used.Add(size) > memory.max {
		return fmt.Errorf("%w: models", ErrMemoryLimit)
	}
	return nil
}

// previousBytes is each byte of a raw model under its context: the byte before it.
func previousBytes(raw []byte) []codedValue {
	values := make([]codedValue, len(raw))
	previous := uint8(0)
	for index, symbol := range raw {
		values[index] = codedValue{uint16(previous), int32(symbol)}
		previous = symbol
	}
	return values
}

// listedModel is a model as the models block lists it.
type listedModel struct {
	stream int
	raw    []byte
}

// writeModelsBlock is the meta model as it is, then every raw model coded
// under it, FORMAT.md 2.2; rawLen is what the format limits.
func writeModelsBlock(models []listedModel) (body []byte, rawLen int, err error) {
	metaTally := streamTally{alphabetSize: structuralAlphabetSize}
	for _, listed := range models {
		metaTally.add(previousBytes(listed.raw), false)
	}
	meta := metaTally.model(metaTableCost)
	body = append(body, meta.raw...)
	rawLen = len(meta.raw)
	body = binary.LittleEndian.AppendUint32(body, uint32(len(models)))
	for _, listed := range models {
		coded, isCoded := encodeStream(meta, previousBytes(listed.raw), false)
		if !isCoded {
			return nil, 0, fmt.Errorf("%w: the meta model does not code a model", ErrInternal)
		}
		body = binary.LittleEndian.AppendUint32(body, uint32(listed.stream))
		body = binary.LittleEndian.AppendUint32(body, uint32(len(listed.raw)))
		body = binary.LittleEndian.AppendUint32(body, uint32(len(coded)))
		body = append(body, coded...)
		rawLen += len(listed.raw)
	}
	return body, rawLen, nil
}

// FORMAT.md 2.2. A file whose decoder tables take more than maxMemory is
// refused.
func readModelsBlock(body []byte, maxMemory int64) (*ransModels, error) {
	if len(body) < ownCountSize {
		return nil, corrupt("tables")
	}
	metaOwnCount := binary.LittleEndian.Uint32(body)
	if metaOwnCount > maxOwnCount {
		return nil, corrupt("tables")
	}
	fields := byteCursor{data: body}
	metaRaw := fields.readBytes(uint32(rawModelLen(int(metaOwnCount), structuralAlphabetSize)))
	if fields.isShort {
		return nil, corrupt("tables")
	}
	memory := tableMemory{max: maxMemory}
	meta, err := newDecoderModel(metaRaw, structuralAlphabetSize, &memory)
	if err != nil {
		return nil, err
	}

	type modelEntry struct {
		id, rawLen uint32
		coded      []byte
	}
	entryCount := fields.readUint32()
	if fields.isShort || entryCount == 0 || entryCount > firstBlobStream {
		return nil, corrupt("tables")
	}
	entries := make([]modelEntry, entryCount)
	nextID := uint32(0)
	rawTotal := uint64(len(metaRaw))
	for index := range entries {
		entry := &entries[index]
		entry.id = fields.readUint32()
		entry.rawLen = fields.readUint32()
		entry.coded = fields.readBytes(fields.readUint32())
		if fields.isShort || entry.id < nextID || entry.id >= firstBlobStream {
			return nil, corrupt("tables")
		}
		nextID = entry.id + 1
		rawTotal += uint64(entry.rawLen)
	}
	if !fields.isAtEnd() {
		return nil, corrupt("tables")
	}
	// before any model is decoded: no model allocates more than the format allows
	if rawTotal > maxModelsRawLen {
		return nil, corrupt("models size")
	}

	models := new(ransModels)
	errs := make([]error, len(entries))
	inParallel(len(entries), func(index int) {
		entry := entries[index]
		raw, err := decodeRawModel(entry.coded, entry.rawLen, meta)
		if err == nil {
			models[entry.id], err = newDecoderModel(raw, streamAlphabetSize(int(entry.id)), &memory)
		}
		errs[index] = err
	})
	for _, err := range errs {
		if err != nil {
			return nil, err
		}
	}
	return models, nil
}

func decodeRawModel(coded []byte, rawLen uint32, meta *decoderModel) ([]byte, error) {
	decoder, err := newStreamDecoder(meta, coded)
	if err != nil {
		return nil, err
	}
	raw := make([]byte, rawLen)
	previous := uint8(0)
	for index := range raw {
		symbol, isDecoded := decoder.symbol(uint16(previous))
		if !isDecoded {
			return nil, corrupt("model")
		}
		raw[index] = symbol
		previous = symbol
	}
	if !decoder.isDone() {
		return nil, corrupt("model")
	}
	return raw, nil
}

// streamTally counts the symbols of one stream per context, over the
// segments of a demo.
type streamTally struct {
	alphabetSize int
	rowOf        []uint32 // by context: 1 + the row of its counts; 0 for a context not counted
	rows         []uint32 // alphabetSize counts per row
}

// add counts the symbols of a structural stream, the buckets of a numeric one.
func (tally *streamTally) add(values []codedValue, isNumeric bool) {
	for _, coded := range values {
		context := int(coded.context)
		if context >= len(tally.rowOf) {
			tally.rowOf = append(tally.rowOf, make([]uint32, context+1-len(tally.rowOf))...)
		}
		row := tally.rowOf[context]
		if row == 0 {
			tally.rows = append(tally.rows, make([]uint32, tally.alphabetSize)...)
			row = uint32(len(tally.rows) / tally.alphabetSize)
			tally.rowOf[context] = row
		}
		symbol := uint16(coded.value)
		if isNumeric {
			symbol = residualBucket(coded.value)
		}
		tally.rows[int(row-1)*tally.alphabetSize+int(symbol)]++
	}
}

// contexts are the counted ones, ascending.
func (tally *streamTally) contexts() []uint16 {
	var contexts []uint16
	for context, row := range tally.rowOf {
		if row != 0 {
			contexts = append(contexts, uint16(context))
		}
	}
	return contexts
}

// counts are of a counted context.
func (tally *streamTally) counts(context uint16) []uint32 {
	row := int(tally.rowOf[context] - 1)
	return tally.rows[row*tally.alphabetSize:][:tally.alphabetSize]
}

// tableCost is of an own table in the models block, in bits, FORMAT.md 8.
type tableCost struct {
	base      uint64
	perSymbol uint64
}

var (
	// The best of a sweep over 387 demos; part of the canonical encoder.
	streamTableCost = tableCost{base: 48, perSymbol: 6}
	// A table of the meta model is stored as it is, a byte per symbol.
	metaTableCost = tableCost{base: 16 + 8*256, perSymbol: 0}
)

// FORMAT.md 8 rule 4
func (tally *streamTally) model(cost tableCost) *streamModel {
	alphabetSize := tally.alphabetSize
	contexts := tally.contexts()
	ownContexts := tally.ownContexts(contexts, cost)

	fallback := make([]uint32, alphabetSize)
	for _, context := range contexts {
		if _, isOwn := slices.BinarySearch(ownContexts, context); !isOwn {
			addCounts(fallback, tally.counts(context))
		}
	}
	raw := make([]byte, 0, rawModelLen(len(ownContexts), alphabetSize))
	raw = binary.LittleEndian.AppendUint32(raw, uint32(len(ownContexts)))
	for _, context := range ownContexts {
		raw = binary.LittleEndian.AppendUint16(raw, context)
	}
	raw = appendQuantized(raw, fallback)
	for _, context := range ownContexts {
		raw = appendQuantized(raw, tally.counts(context))
	}
	quantizedTables := raw[ownCountSize+2*len(ownContexts):]
	return &streamModel{raw, newTableOf(ownContexts), newRansTables(quantizedTables, alphabetSize)}
}

// ownContexts are those cheaper with a table of their own, or that the
// fallback table cannot code, FORMAT.md 8 rule 4.
func (tally *streamTally) ownContexts(contexts []uint16, cost tableCost) []uint16 {
	alphabetSize := tally.alphabetSize
	fallback := make([]uint32, alphabetSize)
	for _, context := range contexts {
		addCounts(fallback, tally.counts(context))
	}
	var ownContexts []uint16
	for range 2 {
		rebuiltFallback := make([]uint32, alphabetSize)
		ownContexts = nil
		for _, context := range contexts {
			counts := tally.counts(context)
			presentCount := uint64(0)
			for _, count := range counts {
				if count > 0 {
					presentCount++
				}
			}
			ownCost := entropyBits(counts) + (cost.base+cost.perSymbol*presentCount)<<16
			fallbackCost, isCodable := crossBits(counts, fallback)
			isCheaperAsOwn := !isCodable || ownCost < fallbackCost
			if isCheaperAsOwn {
				ownContexts = append(ownContexts, context)
			} else {
				addCounts(rebuiltFallback, counts)
			}
		}
		fallback = rebuiltFallback
	}
	return ownContexts
}

// entropyBits is the size of the counted symbols coded with their own
// frequencies, in bits with 16 fraction bits as log2Fixed.
func entropyBits(counts []uint32) uint64 {
	size, _ := crossBits(counts, counts)
	return size
}

// crossBits is the size of the counted symbols coded with a table built from
// reference; false if that table lacks one of them.
func crossBits(counts []uint32, reference []uint32) (uint64, bool) {
	var total uint64
	for _, count := range reference {
		total += uint64(count)
	}
	logTotal := log2Fixed(max(total, 1))
	size := uint64(0)
	for symbol, count := range counts {
		if count == 0 {
			continue
		}
		if reference[symbol] == 0 {
			return 0, false
		}
		size += uint64(count) * (logTotal - log2Fixed(uint64(reference[symbol])))
	}
	return size, true
}

func appendQuantized(table []byte, counts []uint32) []byte {
	for _, count := range counts {
		table = append(table, uint8(quantize(count)))
	}
	return table
}

func addCounts(sum []uint32, counts []uint32) {
	for symbol, count := range counts {
		sum[symbol] += count
	}
}

func streamAlphabetSize(stream int) int {
	if stream >= firstNumericStream {
		return numericAlphabetSize
	}
	return structuralAlphabetSize
}

// bitWriter writes mantissa bits, FORMAT.md Appendix A: least significant
// bit first, the last byte padded with zeros.
type bitWriter struct {
	data     []byte
	bitCount int
}

func (writer *bitWriter) write(value uint32, count int) {
	for index := range count {
		if writer.bitCount%8 == 0 {
			writer.data = append(writer.data, 0)
		}
		bit := uint8(value>>index) & 1
		writer.data[len(writer.data)-1] |= bit << (writer.bitCount % 8)
		writer.bitCount++
	}
}

// bitReader reads mantissa bits, FORMAT.md 6.4: least significant bit
// first, zeros past the end.
type bitReader struct {
	data     []byte
	position int
}

func (reader *bitReader) read(count int) uint32 {
	var value uint32
	for index := range count {
		byteIndex := reader.position / 8
		if byteIndex < len(reader.data) {
			bit := reader.data[byteIndex] >> (reader.position % 8) & 1
			value |= uint32(bit) << index
		}
		reader.position++
	}
	return value
}

// FORMAT.md 6.5: the bits end within the last byte.
func (reader *bitReader) isDone() bool {
	return (reader.position+7)/8 == len(reader.data)
}

// streamDecoder decodes one structural or numeric stream of a segment,
// FORMAT.md 6.1, 6.4 and 6.5.
type streamDecoder struct {
	model        *decoderModel
	rans         []byte
	wordPosition int
	state        uint32
	mantissas    bitReader
}

func newStreamDecoder(model *decoderModel, body []byte) (*streamDecoder, error) {
	fields := byteCursor{data: body}
	rans := fields.readBytes(fields.readUint32())
	if fields.isShort {
		return nil, corrupt("rans stream")
	}
	decoder := &streamDecoder{
		model:     model,
		rans:      rans,
		mantissas: bitReader{data: body[fields.position:]},
	}
	high := decoder.word()
	decoder.state = high<<16 | decoder.word()
	if decoder.state < ransLowerBound {
		return nil, corrupt("rans stream")
	}
	return decoder, nil
}

// symbol is false if the context's table is empty.
func (decoder *streamDecoder) symbol(context uint16) (uint8, bool) {
	model := decoder.model
	entry := model.fallback
	if int(context) < len(model.entryOf) {
		entry = model.entryOf[context]
	}
	if entry == entryEmpty {
		return 0, false
	}
	slot := decoder.state & (frequencyTotal - 1)
	var symbolRange, offset uint32
	if entry&entryLookup != 0 {
		symbolRange = model.lookupTables[entry&^entryLookup+slot]
		offset = symbolRange >> 20
	} else {
		// the ranges of a table cover every slot: the search ends within it;
		// a slot below the range wraps to an offset past it
		for position := entry; ; position++ {
			symbolRange = model.pool[position]
			offset = slot - symbolRange>>20
			if offset <= symbolRange>>8&0xfff {
				break
			}
		}
	}
	state := (symbolRange>>8&0xfff+1)*(decoder.state>>scaleBits) + offset
	if state < ransLowerBound {
		state = state<<16 | decoder.word()
	}
	decoder.state = state
	return uint8(symbolRange), true
}

// value is the next residual of a numeric stream: a bucket symbol, then
// the mantissa bits below the leading one.
func (decoder *streamDecoder) value(context uint16) (int32, bool) {
	bucket, isDecoded := decoder.symbol(context)
	if !isDecoded {
		return 0, false
	}
	bitLen := (int(bucket) + 1) / 2
	if bitLen == 0 {
		return 0, true
	}
	magnitude := uint32(1)<<(bitLen-1) | decoder.mantissas.read(bitLen-1)
	if bucket%2 == 0 {
		return -int32(magnitude), true
	}
	return int32(magnitude), true
}

func (decoder *streamDecoder) isDone() bool {
	return decoder.state == ransLowerBound &&
		decoder.wordPosition == len(decoder.rans) &&
		decoder.mantissas.isDone()
}

func (decoder *streamDecoder) word() uint32 {
	var word uint16
	if decoder.wordPosition+2 <= len(decoder.rans) {
		word = binary.LittleEndian.Uint16(decoder.rans[decoder.wordPosition:])
	}
	decoder.wordPosition += 2
	return uint32(word)
}

// encodeStream is false if the model does not hold a symbol, FORMAT.md 6.1
// and Appendix A.
func encodeStream(model *streamModel, values []codedValue, isNumeric bool) ([]byte, bool) {
	state := uint32(ransLowerBound)
	var reversedWords []uint16
	for index := len(values) - 1; index >= 0; index-- {
		symbol := uint8(values[index].value)
		if isNumeric {
			symbol = uint8(residualBucket(values[index].value))
		}
		table := model.table(values[index].context)
		start, frequency := table.find(symbol)
		if frequency == 0 {
			return nil, false
		}
		if uint64(state) >= uint64(frequency)<<20 {
			reversedWords = append(reversedWords, uint16(state))
			state >>= 16
		}
		state = (state/frequency)<<scaleBits + state%frequency + start
	}
	reversedWords = append(reversedWords, uint16(state), uint16(state>>16))

	var mantissas bitWriter
	if isNumeric {
		for _, coded := range values {
			magnitude := uint32(coded.value)
			if coded.value < 0 {
				magnitude = -magnitude
			}
			mantissas.write(magnitude, max(bits.Len32(magnitude)-1, 0))
		}
	}

	body := binary.LittleEndian.AppendUint32(nil, uint32(2*len(reversedWords)))
	for _, word := range slices.Backward(reversedWords) {
		body = binary.LittleEndian.AppendUint16(body, word)
	}
	return append(body, mantissas.data...), true
}
