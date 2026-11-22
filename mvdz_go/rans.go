package mvdz

import (
	"encoding/binary"
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
	contexts []uint16         // strictly increasing
	tables   []frequencyTable // the fallback, then one per context
}

// frequencyTable holds the present symbols in ascending order; symbol index
// owns the slots cumulative[index] to cumulative[index+1].
type frequencyTable struct {
	symbols    []uint8
	cumulative []uint16
}

// readModel reads one models block entry, FORMAT.md section 6.2. It adds the
// model's decoded size to modelsSize.
func readModel(entry []byte, alphabet int, modelsSize *int) (*streamModel, error) {
	fields := byteCursor{data: entry}
	declaredOwnCount := fields.u32()
	if fields.isShort || declaredOwnCount > maxOwnTables {
		return nil, corrupt("model")
	}
	ownCount := int(declaredOwnCount)
	contextsSize := 2 * ownCount
	inflatedSize := contextsSize + (ownCount+1)*alphabet
	*modelsSize += inflatedSize
	if *modelsSize > maxModelsSize {
		return nil, ErrMemoryLimit
	}
	inflated, err := inflateExactly(entry[fields.position:], inflatedSize)
	if err != nil {
		return nil, err
	}
	return newStreamModel(inflated, ownCount, alphabet)
}

// newStreamModel builds a model from its inflated bytes: the own contexts,
// the fallback table and the own tables.
func newStreamModel(inflated []byte, ownCount int, alphabet int) (*streamModel, error) {
	contextsSize := 2 * ownCount
	contexts := make([]uint16, ownCount)
	for index := range contexts {
		contexts[index] = binary.LittleEndian.Uint16(inflated[2*index:])
		if index > 0 && contexts[index] <= contexts[index-1] {
			return nil, corrupt("model contexts")
		}
	}
	tables := make([]frequencyTable, ownCount+1)
	for index := range tables {
		quantized := inflated[contextsSize+index*alphabet:][:alphabet]
		tables[index] = newFrequencyTable(quantized)
	}
	return &streamModel{contexts, tables}, nil
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
		weights[symbol] = quantizedWeight(count)
	}
	table := frequencyTable{cumulative: []uint16{0}}
	start := 0
	for symbol, frequency := range normalize(weights) {
		if frequency == 0 {
			continue
		}
		start += frequency
		table.symbols = append(table.symbols, uint8(symbol))
		table.cumulative = append(table.cumulative, uint16(start))
	}
	return table
}

// quantizedWeight is 2^((quantized-1)/2) in 16.16 fixed point.
func quantizedWeight(quantized uint8) uint64 {
	if quantized == 0 {
		return 0
	}
	stepPowers := [2]uint64{65536, 92682}
	step := quantized - 1
	octave := step / 2
	if octave >= 40 {
		return (1<<64 - 1) >> 8
	}
	return stepPowers[step%2] << octave
}

// normalize scales the weights to frequencies that sum to frequencyTotal,
// every present symbol getting at least 1.
func normalize(weights []uint64) []int {
	frequencies := make([]int, len(weights))
	var total uint64 // at most 256 weights below 2^56
	for _, weight := range weights {
		total += weight
	}
	if total == 0 {
		return frequencies
	}
	sum := 0
	for symbol, weight := range weights {
		if weight == 0 {
			continue
		}
		high, low := bits.Mul64(weight, frequencyTotal)
		share, _ := bits.Div64(high, low, total)
		frequencies[symbol] = max(int(share), 1)
		sum += frequencies[symbol]
	}
	difference := frequencyTotal - sum
	for difference != 0 {
		largest := 0
		for symbol, frequency := range frequencies {
			if frequency > frequencies[largest] {
				largest = symbol
			}
		}
		if difference > 0 {
			frequencies[largest] += difference
			break
		}
		take := min(-difference, frequencies[largest]-1, (frequencies[largest]+1)/2)
		if take <= 0 {
			break
		}
		frequencies[largest] -= take
		difference += take
	}
	return frequencies
}

// streamDecoder decodes one structural or numeric stream of a segment,
// FORMAT.md sections 6.1, 6.4 and 6.5.
type streamDecoder struct {
	model        *streamModel
	rans         []byte
	wordPosition int
	state        uint32
	remaining    uint32 // symbols of the stream's count not yet decoded
	mantissas    bitReader
}

func newStreamDecoder(model *streamModel, body []byte, count uint32) (*streamDecoder, error) {
	fields := byteCursor{data: body}
	rans := fields.bytes(fields.u32())
	if fields.isShort {
		return nil, corrupt("stream body")
	}
	decoder := &streamDecoder{
		model:     model,
		rans:      rans,
		remaining: count,
		mantissas: bitReader{data: body[fields.position:]},
	}
	high := decoder.word()
	decoder.state = high<<16 | decoder.word()
	return decoder, nil
}

// symbol decodes the next symbol; false if the stream has none left or the
// context's table is empty.
func (decoder *streamDecoder) symbol(context uint16) (uint8, bool) {
	table := decoder.model.table(context)
	if decoder.remaining == 0 || len(table.symbols) == 0 {
		return 0, false
	}
	decoder.remaining--
	if len(table.symbols) == 1 {
		return table.symbols[0], true
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
	bitLength := (int(bucket) + 1) / 2
	if bitLength == 0 {
		return 0, true
	}
	magnitude := uint32(1)<<(bitLength-1) | decoder.mantissas.read(bitLength-1)
	if bucket%2 == 0 {
		return -int32(magnitude), true
	}
	return int32(magnitude), true
}

func (decoder *streamDecoder) isComplete() bool {
	return decoder.remaining == 0 &&
		decoder.state == ransLowerBound &&
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
		isSingleSymbol := len(table.symbols) == 1
		if isSingleSymbol {
			continue
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
