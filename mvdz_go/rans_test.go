package mvdz

import (
	"errors"
	"fmt"
	"math"
	"testing"
)

// FORMAT.md 6.4: the widest buckets wrap, and 6.5: the mantissa bits end
// within the last byte. A lone symbol has the frequency 4096 in a search
// table and in a lookup table.
func TestNumericValue(test *testing.T) {
	cases := []struct {
		bucket   uint8
		mantissa []byte
		expected int32
	}{
		{0, nil, 0},
		{1, nil, 1},
		{2, nil, -1},
		{5, []byte{0b10}, 6},
		{6, []byte{0b01}, -5},
		{63, []byte{0xff, 0xff, 0xff, 0x7f}, -1},
		{63, []byte{0, 0, 0, 0}, -1 << 31},
		{64, []byte{0, 0, 0, 0}, -1 << 31},
		{64, []byte{0xff, 0xff, 0xff, 0x7f}, 1},
	}
	for _, valueCase := range cases {
		singleSymbol := make(ransTable, numericAlphabetSize+1)
		for symbol := int(valueCase.bucket) + 1; symbol < len(singleSymbol); symbol++ {
			singleSymbol[symbol] = frequencyTotal
		}
		var search, lookup decoderModel
		search.fallback = search.appendSearchTable(singleSymbol)
		lookup.fallback = lookup.appendLookupTable(singleSymbol)
		for name, model := range map[string]*decoderModel{"search": &search, "lookup": &lookup} {
			test.Run(fmt.Sprintf("%s: bucket %d to %d", name, valueCase.bucket, valueCase.expected), func(test *testing.T) {
				body := append([]byte{4, 0, 0, 0, 1, 0, 0, 0}, valueCase.mantissa...)
				decoder, err := newStreamDecoder(model, body)
				if err != nil {
					test.Fatal(err)
				}
				value, isDecoded := decoder.value(0)
				if !isDecoded || value != valueCase.expected || !decoder.isDone() {
					test.Errorf("got %d (decoded %v, done %v)", value, isDecoded, decoder.isDone())
				}
			})
		}
	}
}

// A wide table takes a lookup table, within what the model reserves.
func TestDecoderModelTables(test *testing.T) {
	rawModel := func(presentCount int) []byte {
		raw := make([]byte, ownCountSize+numericAlphabetSize)
		for symbol := range presentCount {
			raw[ownCountSize+symbol] = 1
		}
		return raw
	}
	unlimited := func() *tableMemory { return &tableMemory{max: math.MaxInt64} }

	narrow, err := newDecoderModel(rawModel(64), numericAlphabetSize, unlimited())
	if err != nil {
		test.Fatal(err)
	}
	if len(narrow.pool) != 64 || len(narrow.lookupTables) != 0 {
		test.Errorf("narrow: pool %d, lookup tables %d", len(narrow.pool), len(narrow.lookupTables))
	}

	wide, err := newDecoderModel(rawModel(65), numericAlphabetSize, unlimited())
	if err != nil {
		test.Fatal(err)
	}
	if len(wide.pool) != 0 || len(wide.lookupTables) != frequencyTotal {
		test.Errorf("wide: pool %d, lookup tables %d", len(wide.pool), len(wide.lookupTables))
	}
	reserved := int64(4 * len(wide.lookupTables))
	if _, err := newDecoderModel(rawModel(65), numericAlphabetSize, &tableMemory{max: reserved}); err != nil {
		test.Errorf("at the reservation: %v", err)
	}
	_, err = newDecoderModel(rawModel(65), numericAlphabetSize, &tableMemory{max: reserved - 1})
	if !errors.Is(err, ErrMemoryLimit) || err.Error() != "mvdz: memory limit: models" {
		test.Errorf("a byte under the reservation: got %v", err)
	}
}

// Context 0 always codes one value, a lone symbol of frequency 4096, in a
// search table; the structural stream has a wide table, read by lookup.
func TestStreamRoundTrip(test *testing.T) {
	numeric := make([]codedValue, 40_000)
	structural := make([]codedValue, 40_000)
	for index := range numeric {
		context := index % 2
		numeric[index] = codedValue{uint16(context), int32(context * (index%3 - 1))}
		structural[index] = codedValue{uint16(context), int32(index * 7 % 256)}
	}
	cases := []struct {
		name            string
		values          []codedValue
		alphabetSize    int
		isNumeric       bool
		hasLookupTables bool
	}{
		{name: "numeric", values: numeric, alphabetSize: numericAlphabetSize, isNumeric: true},
		{name: "structural", values: structural, alphabetSize: structuralAlphabetSize, hasLookupTables: true},
	}
	for _, streamCase := range cases {
		test.Run(streamCase.name, func(test *testing.T) {
			tally := streamTally{alphabetSize: streamCase.alphabetSize}
			tally.add(streamCase.values, streamCase.isNumeric)
			model := tally.model(streamTableCost)
			decoderModel, err := newDecoderModel(model.raw, streamCase.alphabetSize, &tableMemory{max: math.MaxInt64})
			if err != nil {
				test.Fatal(err)
			}
			if hasLookupTables := len(decoderModel.lookupTables) > 0; hasLookupTables != streamCase.hasLookupTables {
				test.Errorf("lookup tables: %d slots", len(decoderModel.lookupTables))
			}
			body, isEncoded := encodeStream(model, streamCase.values, streamCase.isNumeric)
			if !isEncoded {
				test.Fatal("a symbol has no code")
			}
			decoder, err := newStreamDecoder(decoderModel, body)
			if err != nil {
				test.Fatal(err)
			}
			for index, coded := range streamCase.values {
				value, isDecoded := int32(0), false
				if streamCase.isNumeric {
					value, isDecoded = decoder.value(coded.context)
				} else {
					var symbol uint8
					symbol, isDecoded = decoder.symbol(coded.context)
					value = int32(symbol)
				}
				if !isDecoded || value != coded.value {
					test.Fatalf("value %d: got %d (decoded %v), want %d", index, value, isDecoded, coded.value)
				}
			}
			if !decoder.isDone() {
				test.Error("the stream is not done")
			}
		})
	}
}
