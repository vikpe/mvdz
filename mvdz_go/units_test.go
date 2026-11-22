package mvdz

import (
	"errors"
	"hash/crc32"
	"slices"
	"testing"
)

// FORMAT.md section 1
func TestCrc32CheckValue(test *testing.T) {
	if checksum := crc32.ChecksumIEEE([]byte("123456789")); checksum != 0xCBF43926 {
		test.Errorf("got %#x", checksum)
	}
}

func TestResidualBucket(test *testing.T) {
	buckets := map[int32]uint16{
		0: 0, 1: 1, -1: 2, 2: 3, 3: 3, -3: 4, 4: 5, -128: 16, 127: 13,
		1<<31 - 1: 61, -(1<<31 - 1): 62, -1 << 31: 64,
	}
	for value, expected := range buckets {
		if bucket := residualBucket(value); bucket != expected {
			test.Errorf("residualBucket(%d) = %d, want %d", value, bucket, expected)
		}
	}
}

func TestQuantizedWeight(test *testing.T) {
	weights := map[uint8]uint64{
		0: 0, 1: 65536, 2: 92682, 3: 131072, 4: 185364,
		80: 92682 << 39, 81: 1<<56 - 1, 255: 1<<56 - 1,
	}
	for quantized, expected := range weights {
		if weight := quantizedWeight(quantized); weight != expected {
			test.Errorf("quantizedWeight(%d) = %d, want %d", quantized, weight, expected)
		}
	}
}

func TestNormalize(test *testing.T) {
	cases := []struct {
		name     string
		weights  []uint64
		expected []int
	}{
		{"absent", []uint64{0, 0, 0}, []int{0, 0, 0}},
		{"single", []uint64{0, 7, 0}, []int{0, 4096, 0}},
		{"exact", []uint64{1, 1, 2}, []int{1024, 1024, 2048}},
		{"remainder to the first largest", []uint64{1, 1, 1}, []int{1366, 1365, 1365}},
		{"largest pays for the floors of 1", []uint64{1 << 40, 1, 1, 0}, []int{4094, 1, 1, 0}},
		{"largest weights", []uint64{1<<56 - 1, 1<<56 - 1}, []int{2048, 2048}},
	}
	for _, normalizeCase := range cases {
		frequencies := normalize(normalizeCase.weights)
		if !slices.Equal(frequencies, normalizeCase.expected) {
			test.Errorf("%s: got %v, want %v", normalizeCase.name, frequencies, normalizeCase.expected)
		}
	}

	// 256 present symbols with one dominant: the floors of 1 take the sum over 4096.
	weights := make([]uint64, structuralAlphabet)
	for symbol := range weights {
		weights[symbol] = 1
	}
	weights[9] = 1 << 50
	frequencies := normalize(weights)
	if frequencies[9] != 4096-255 || frequencies[0] != 1 {
		test.Errorf("dominant: got %d and %d", frequencies[9], frequencies[0])
	}
}

func TestDivRound(test *testing.T) {
	cases := [][3]int32{{7, 2, 4}, {-7, 2, -4}, {5, 3, 2}, {-5, 3, -2}, {4, 3, 1}, {0, 5, 0}, {-1 << 30, 32767, -32769}}
	for _, divideCase := range cases {
		if quotient := divRound(divideCase[0], divideCase[1]); quotient != divideCase[2] {
			test.Errorf("divRound(%d, %d) = %d, want %d", divideCase[0], divideCase[1], quotient, divideCase[2])
		}
	}
}

// FORMAT.md section 6.4: the widest buckets wrap, and section 6.5: the
// mantissa bits end within the last byte.
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
		singleSymbol := frequencyTable{symbols: []uint8{valueCase.bucket}, cumulative: []uint16{0, frequencyTotal}}
		body := append([]byte{4, 0, 0, 0, 1, 0, 0, 0}, valueCase.mantissa...)
		decoder, err := newStreamDecoder(&streamModel{tables: []frequencyTable{singleSymbol}}, body, 1)
		if err != nil {
			test.Fatal(err)
		}
		value, isDecoded := decoder.value(0)
		if !isDecoded || value != valueCase.expected || !decoder.isComplete() {
			test.Errorf("bucket %d: got %d (decoded %v, complete %v), want %d",
				valueCase.bucket, value, isDecoded, decoder.isComplete(), valueCase.expected)
		}
	}
}

func TestModelsSizeLimit(test *testing.T) {
	modelsSize := maxModelsSize
	emptyModel := make([]byte, 4)
	if _, err := readModel(emptyModel, structuralAlphabet, &modelsSize); !errors.Is(err, ErrMemoryLimit) {
		test.Errorf("got %v, want %v", err, ErrMemoryLimit)
	}
}
