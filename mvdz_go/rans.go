package mvdz

import (
	"encoding/binary"
	"fmt"
	"maps"
	"math/bits"
	"slices"
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

// streamModel is the model of one stream, FORMAT.md 6.2.
type streamModel struct {
	raw     []byte
	tableOf []uint32    // by context: the index into tables; contexts past its end use the fallback
	tables  []ransTable // the fallback, then one per own context
}

// fileModels is by stream id; nil where the models block has none.
type fileModels [firstBlobStream]*streamModel

// ransTable holds the present symbols in ascending order; symbol index
// owns the slots cumulative[index] to cumulative[index+1].
type ransTable struct {
	symbols    []uint8
	cumulative []uint16
}

// find is false if the table does not hold the symbol.
func (table *ransTable) find(symbol uint8) (start uint32, frequency uint32, isPresent bool) {
	index, isPresent := slices.BinarySearch(table.symbols, symbol)
	if !isPresent {
		return 0, 0, false
	}
	start = uint32(table.cumulative[index])
	return start, uint32(table.cumulative[index+1]) - start, true
}

// FORMAT.md 6.2
func rawModelLen(ownCount, alphabetSize int) int {
	return ownCountSize + 2*ownCount + (ownCount+1)*alphabetSize
}

// newStreamModel reads a raw model: the own count, the own contexts, the
// fallback table and the own tables.
func newStreamModel(raw []byte, alphabetSize int) (*streamModel, error) {
	if len(raw) < ownCountSize {
		return nil, corrupt("model")
	}
	declaredOwnCount := binary.LittleEndian.Uint32(raw)
	if declaredOwnCount > maxOwnCount || len(raw) != rawModelLen(int(declaredOwnCount), alphabetSize) {
		return nil, corrupt("model")
	}
	ownCount := int(declaredOwnCount)
	ownContexts := make([]uint16, ownCount)
	for index := range ownContexts {
		ownContexts[index] = binary.LittleEndian.Uint16(raw[ownCountSize+2*index:])
		if index > 0 && ownContexts[index] <= ownContexts[index-1] {
			return nil, corrupt("model")
		}
	}
	quantizedTables := raw[ownCountSize+2*ownCount:]
	for _, quantized := range quantizedTables {
		if quantized > maxQuantized {
			return nil, corrupt("model")
		}
	}
	return &streamModel{raw, newTableOf(ownContexts), newRansTables(quantizedTables, alphabetSize)}, nil
}

func (model *streamModel) table(context uint16) *ransTable {
	if int(context) < len(model.tableOf) {
		return &model.tables[model.tableOf[context]]
	}
	return &model.tables[0]
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

func newRansTables(quantizedTables []byte, alphabetSize int) []ransTable {
	tables := make([]ransTable, len(quantizedTables)/alphabetSize)
	for index := range tables {
		tables[index] = newRansTable(quantizedTables[index*alphabetSize:][:alphabetSize])
	}
	return tables
}

// FORMAT.md 6.3
func newRansTable(quantized []byte) ransTable {
	weights := make([]uint64, len(quantized))
	for symbol, count := range quantized {
		weights[symbol] = quantizedWeight(uint32(count))
	}
	table := ransTable{cumulative: []uint16{0}}
	start := 0
	for symbol, frequency := range normalize(weights, frequencyTotal) {
		if frequency == 0 {
			continue
		}
		start += frequency
		table.symbols = append(table.symbols, uint8(symbol))
		table.cumulative = append(table.cumulative, uint16(start))
	}
	return table
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
	metaTally := streamTally{}
	for _, listed := range models {
		metaTally.add(previousBytes(listed.raw), structuralAlphabetSize, false)
	}
	meta := metaTally.model(structuralAlphabetSize, metaTableCost)
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

// FORMAT.md 2.2
func readModelsBlock(body []byte) (*fileModels, error) {
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
	meta, err := newStreamModel(metaRaw, structuralAlphabetSize)
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

	models := new(fileModels)
	errs := make([]error, len(entries))
	inParallel(len(entries), func(index int) {
		entry := entries[index]
		raw, err := decodeRawModel(entry.coded, entry.rawLen, meta)
		if err == nil {
			models[entry.id], err = newStreamModel(raw, streamAlphabetSize(int(entry.id)))
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

func decodeRawModel(coded []byte, rawLen uint32, meta *streamModel) ([]byte, error) {
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
type streamTally map[uint16][]uint32

// add counts the symbols of a structural stream, the buckets of a numeric one.
func (tally streamTally) add(values []codedValue, alphabetSize int, isNumeric bool) {
	for _, coded := range values {
		counts := tally[coded.context]
		if counts == nil {
			counts = make([]uint32, alphabetSize)
			tally[coded.context] = counts
		}
		symbol := uint16(coded.value)
		if isNumeric {
			symbol = residualBucket(coded.value)
		}
		counts[symbol]++
	}
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
func (tally streamTally) model(alphabetSize int, cost tableCost) *streamModel {
	contexts := slices.Sorted(maps.Keys(tally))
	ownContexts := tally.ownContexts(contexts, alphabetSize, cost)

	fallback := make([]uint32, alphabetSize)
	for _, context := range contexts {
		if _, isOwn := slices.BinarySearch(ownContexts, context); !isOwn {
			addCounts(fallback, tally[context])
		}
	}
	raw := make([]byte, 0, rawModelLen(len(ownContexts), alphabetSize))
	raw = binary.LittleEndian.AppendUint32(raw, uint32(len(ownContexts)))
	for _, context := range ownContexts {
		raw = binary.LittleEndian.AppendUint16(raw, context)
	}
	raw = appendQuantized(raw, fallback)
	for _, context := range ownContexts {
		raw = appendQuantized(raw, tally[context])
	}
	quantizedTables := raw[ownCountSize+2*len(ownContexts):]
	return &streamModel{raw, newTableOf(ownContexts), newRansTables(quantizedTables, alphabetSize)}
}

// ownContexts are those cheaper with a table of their own, or that the
// fallback table cannot code, FORMAT.md 8 rule 4.
func (tally streamTally) ownContexts(contexts []uint16, alphabetSize int, cost tableCost) []uint16 {
	fallback := make([]uint32, alphabetSize)
	for _, context := range contexts {
		addCounts(fallback, tally[context])
	}
	var ownContexts []uint16
	for range 2 {
		rebuiltFallback := make([]uint32, alphabetSize)
		ownContexts = nil
		for _, context := range contexts {
			counts := tally[context]
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
	model        *streamModel
	rans         []byte
	wordPosition int
	state        uint32
	mantissas    bitReader
}

func newStreamDecoder(model *streamModel, body []byte) (*streamDecoder, error) {
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
	table := decoder.model.table(context)
	if len(table.symbols) == 0 {
		return 0, false
	}
	slot := uint16(decoder.state & (frequencyTotal - 1))
	index := 0
	for table.cumulative[index+1] <= slot {
		index++
	}
	start := uint32(table.cumulative[index])
	frequency := uint32(table.cumulative[index+1]) - start
	decoder.state = frequency*(decoder.state>>scaleBits) + uint32(slot) - start
	if decoder.state < ransLowerBound {
		decoder.state = decoder.state<<16 | decoder.word()
	}
	return table.symbols[index], true
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
		start, frequency, isPresent := table.find(symbol)
		if !isPresent {
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
