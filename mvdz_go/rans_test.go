package mvdz

import (
	"fmt"
	"testing"
)

// FORMAT.md 6.4: the widest buckets wrap, and 6.5: the mantissa bits end
// within the last byte.
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
		test.Run(fmt.Sprintf("bucket %d to %d", valueCase.bucket, valueCase.expected), func(test *testing.T) {
			singleSymbol := ransTable{symbols: []uint8{valueCase.bucket}, cumulative: []uint16{0, frequencyTotal}}
			body := append([]byte{4, 0, 0, 0, 1, 0, 0, 0}, valueCase.mantissa...)
			decoder, err := newStreamDecoder(&streamModel{tables: []ransTable{singleSymbol}}, body)
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
