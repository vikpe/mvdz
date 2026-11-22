package mvdz

import (
	"encoding/binary"
	"maps"
	"math/bits"
	"slices"
)

const (
	structuralAlphabet = 256
	numericAlphabet    = 65
	maxOwnTables       = 1 << 16
	frequencyBits      = 12
	frequencyTotal     = 1 << frequencyBits
	ransLowerBound     = 1 << 16
)

// streamModel is the model of one stream, FORMAT.md section 6.2.
type streamModel struct {
	raw      []byte           // the raw model
	contexts []uint16         // strictly increasing
	tables   []frequencyTable // the fallback, then one per context
}

// frequencyTable holds the present symbols in ascending order; symbol index
// owns the slots cumulative[index] to cumulative[index+1].
type frequencyTable struct {
	symbols    []uint8
	cumulative []uint16
}

// rawModelLen is the length of a raw model, FORMAT.md section 6.2.
func rawModelLen(ownCount int, alphabet int) int {
	return 4 + 2*ownCount + (ownCount+1)*alphabet
}

// newStreamModel builds a model from its raw bytes: the own count, the own
// contexts, the fallback table and the own tables.
func newStreamModel(raw []byte, alphabet int) (*streamModel, error) {
	if len(raw) < 4 {
		return nil, corrupt("model")
	}
	declaredOwnCount := binary.LittleEndian.Uint32(raw)
	if declaredOwnCount > maxOwnTables || len(raw) != rawModelLen(int(declaredOwnCount), alphabet) {
		return nil, corrupt("model")
	}
	ownCount := int(declaredOwnCount)
	contexts := make([]uint16, ownCount)
	for index := range contexts {
		contexts[index] = binary.LittleEndian.Uint16(raw[4+2*index:])
		if index > 0 && contexts[index] <= contexts[index-1] {
			return nil, corrupt("model")
		}
	}
	quantizedTables := raw[4+2*ownCount:]
	tables := make([]frequencyTable, ownCount+1)
	for index := range tables {
		tables[index] = newFrequencyTable(quantizedTables[index*alphabet:][:alphabet])
	}
	return &streamModel{raw, contexts, tables}, nil
}

func (model *streamModel) table(context uint16) *frequencyTable {
	index, isOwn := slices.BinarySearch(model.contexts, context)
	if !isOwn {
		return &model.tables[0]
	}
	return &model.tables[index+1]
}

// FORMAT.md section 6.3
func newFrequencyTable(quantized []byte) frequencyTable {
	weights := make([]uint64, len(quantized))
	for symbol, count := range quantized {
		weights[symbol] = quantizedWeight(uint32(count))
	}
	table := frequencyTable{cumulative: []uint16{0}}
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

// previousBytes is the context of each byte of a raw model under the meta
// model: the byte before it, 0 before the first.
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

// writeModelsBlock lays out the body of the models block, FORMAT.md section
// 2.2: the meta model as it is, then every raw model coded under it. It also
// returns the raw length the format limits.
func writeModelsBlock(models []listedModel) (body []byte, rawLen int) {
	metaTally := streamTally{}
	for _, listed := range models {
		metaTally.add(previousBytes(listed.raw), structuralAlphabet, false)
	}
	metaRaw := metaTally.rawModel(structuralAlphabet, metaTableCost)
	meta, _ := newStreamModel(metaRaw, structuralAlphabet) // built here: valid
	body = append(body, metaRaw...)
	rawLen = len(metaRaw)
	body = binary.LittleEndian.AppendUint32(body, uint32(len(models)))
	for _, listed := range models {
		coded, _ := encodeStream(meta, previousBytes(listed.raw), false) // the meta model is built from these bytes
		body = binary.LittleEndian.AppendUint32(body, uint32(listed.stream))
		body = binary.LittleEndian.AppendUint32(body, uint32(len(listed.raw)))
		body = binary.LittleEndian.AppendUint32(body, uint32(len(coded)))
		body = append(body, coded...)
		rawLen += len(listed.raw)
	}
	return body, rawLen
}

// readModelsBlock reads the body of the models block, FORMAT.md section 2.2.
func readModelsBlock(body []byte) (*fileModels, error) {
	if len(body) < 4 {
		return nil, corrupt("tables")
	}
	metaOwnCount := binary.LittleEndian.Uint32(body)
	if metaOwnCount > maxOwnTables {
		return nil, corrupt("tables")
	}
	fields := byteCursor{data: body}
	metaRaw := fields.bytes(uint32(rawModelLen(int(metaOwnCount), structuralAlphabet)))
	if fields.isShort {
		return nil, corrupt("tables")
	}
	meta, err := newStreamModel(metaRaw, structuralAlphabet)
	if err != nil {
		return nil, err
	}

	type modelEntry struct {
		id, rawLen uint32
		coded      []byte
	}
	entryCount := fields.u32()
	if fields.isShort || entryCount == 0 || entryCount > firstBlobStream {
		return nil, corrupt("tables")
	}
	entries := make([]modelEntry, entryCount)
	nextId := uint32(0)
	rawTotal := uint64(len(metaRaw))
	for index := range entries {
		entry := &entries[index]
		entry.id = fields.u32()
		entry.rawLen = fields.u32()
		entry.coded = fields.bytes(fields.u32())
		if fields.isShort || entry.id < nextId || entry.id >= firstBlobStream {
			return nil, corrupt("tables")
		}
		nextId = entry.id + 1
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
	for _, entry := range entries {
		raw, err := decodeRawModel(entry.coded, entry.rawLen, meta)
		if err != nil {
			return nil, err
		}
		models[entry.id], err = newStreamModel(raw, streamAlphabet(int(entry.id)))
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
	if !decoder.mantissas.isFinished() {
		return nil, corrupt("rans stream")
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
	if !decoder.isComplete() {
		return nil, corrupt("model")
	}
	return raw, nil
}

// streamTally counts the symbols of one stream per context, over the
// segments of a demo.
type streamTally map[uint16][]uint32

// add counts values: the symbols of a structural stream, the buckets of a
// numeric one.
func (tally streamTally) add(values []codedValue, alphabet int, isNumeric bool) {
	for _, coded := range values {
		counts := tally[coded.context]
		if counts == nil {
			counts = make([]uint32, alphabet)
			tally[coded.context] = counts
		}
		symbol := uint16(coded.value)
		if isNumeric {
			symbol = residualBucket(coded.value)
		}
		counts[symbol]++
	}
}

// tableCost is what an own table costs in the models block, in bits,
// FORMAT.md section 8.
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

// rawModel builds the raw model of a stream from its counts, FORMAT.md
// section 8 rule 4.
func (tally streamTally) rawModel(alphabet int, cost tableCost) []byte {
	contexts := slices.Sorted(maps.Keys(tally))
	ownContexts := tally.ownContexts(contexts, alphabet, cost)

	fallback := make([]uint32, alphabet)
	for _, context := range contexts {
		if _, isOwn := slices.BinarySearch(ownContexts, context); !isOwn {
			addCounts(fallback, tally[context])
		}
	}
	raw := make([]byte, 0, rawModelLen(len(ownContexts), alphabet))
	raw = binary.LittleEndian.AppendUint32(raw, uint32(len(ownContexts)))
	for _, context := range ownContexts {
		raw = binary.LittleEndian.AppendUint16(raw, context)
	}
	raw = appendQuantized(raw, fallback)
	for _, context := range ownContexts {
		raw = appendQuantized(raw, tally[context])
	}
	return raw
}

// ownContexts chooses the contexts that get a table of their own, FORMAT.md
// section 8 rule 4: those that cost fewer bits coded with their own table,
// the table included, than with the fallback table, and those the fallback
// table cannot code. The first pass measures against all contexts together,
// the second against the contexts the first left to the fallback.
func (tally streamTally) ownContexts(contexts []uint16, alphabet int, cost tableCost) []uint16 {
	fallback := make([]uint32, alphabet)
	for _, context := range contexts {
		addCounts(fallback, tally[context])
	}
	var ownContexts []uint16
	for range 2 {
		rebuiltFallback := make([]uint32, alphabet)
		ownContexts = nil
		for _, context := range contexts {
			counts := tally[context]
			presentCount := uint64(0)
			for _, count := range counts {
				if count > 0 {
					presentCount++
				}
			}
			ownBits := entropyBits(counts) + (cost.base+cost.perSymbol*presentCount)<<16
			fallbackBits, isCodable := crossBits(counts, fallback)
			if !isCodable || ownBits < fallbackBits {
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
	bits, _ := crossBits(counts, counts)
	return bits
}

// crossBits is the size of the counted symbols coded with a table built from
// reference; false if that table lacks one of them.
func crossBits(counts []uint32, reference []uint32) (uint64, bool) {
	var total uint64
	for _, count := range reference {
		total += uint64(count)
	}
	logTotal := log2Fixed(max(total, 1))
	bits := uint64(0)
	for symbol, count := range counts {
		if count == 0 {
			continue
		}
		if reference[symbol] == 0 {
			return 0, false
		}
		bits += uint64(count) * (logTotal - log2Fixed(uint64(reference[symbol])))
	}
	return bits, true
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

func streamAlphabet(stream int) int {
	if stream >= firstNumericStream {
		return numericAlphabet
	}
	return structuralAlphabet
}

// streamDecoder decodes one structural or numeric stream of a segment,
// FORMAT.md sections 6.1, 6.4 and 6.5.
type streamDecoder struct {
	model        *streamModel
	rans         []byte
	wordPosition int
	state        uint32
	mantissas    bitReader
}

func newStreamDecoder(model *streamModel, body []byte) (*streamDecoder, error) {
	fields := byteCursor{data: body}
	rans := fields.bytes(fields.u32())
	if fields.isShort {
		return nil, corrupt("stream body")
	}
	decoder := &streamDecoder{
		model:     model,
		rans:      rans,
		mantissas: bitReader{data: body[fields.position:]},
	}
	high := decoder.word()
	decoder.state = high<<16 | decoder.word()
	if decoder.state < ransLowerBound {
		return nil, corrupt("stream body")
	}
	return decoder, nil
}

// symbol decodes the next symbol; false if the context's table is empty.
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
	decoder.state = frequency*(decoder.state>>frequencyBits) + uint32(slot) - start
	if decoder.state < ransLowerBound {
		decoder.state = decoder.state<<16 | decoder.word()
	}
	return table.symbols[index], true
}

// find returns the first slot and the frequency of a symbol; false if the
// table does not hold it.
func (table *frequencyTable) find(symbol uint8) (start uint32, frequency uint32, isPresent bool) {
	index, isPresent := slices.BinarySearch(table.symbols, symbol)
	if !isPresent {
		return 0, 0, false
	}
	start = uint32(table.cumulative[index])
	return start, uint32(table.cumulative[index+1]) - start, true
}

// value decodes the next residual of a numeric stream: a bucket symbol, then
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

func (decoder *streamDecoder) isComplete() bool {
	return decoder.state == ransLowerBound &&
		decoder.wordPosition == len(decoder.rans) &&
		decoder.mantissas.isFinished()
}

func (decoder *streamDecoder) word() uint32 {
	var word uint16
	if decoder.wordPosition+2 <= len(decoder.rans) {
		word = binary.LittleEndian.Uint16(decoder.rans[decoder.wordPosition:])
	}
	decoder.wordPosition += 2
	return uint32(word)
}

// encodeStream codes the symbols or residuals of one stream to a stream
// body, FORMAT.md section 6.1 and Appendix A; false if the model does not
// hold a symbol.
func encodeStream(model *streamModel, values []codedValue, isNumeric bool) ([]byte, bool) {
	var mantissas bitWriter
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
		state = (state/frequency)<<frequencyBits + state%frequency + start
	}
	reversedWords = append(reversedWords, uint16(state), uint16(state>>16))

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
