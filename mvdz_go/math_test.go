package mvdz

import (
	"hash/crc32"
	"slices"
	"testing"
)

// FORMAT.md 1
func TestCRC32CheckValue(test *testing.T) {
	if checksum := crc32.ChecksumIEEE([]byte("123456789")); checksum != 0xCBF43926 {
		test.Errorf("got %#x", checksum)
	}
}

func TestResidualBucket(test *testing.T) {
	cases := []struct {
		value    int32
		expected uint16
	}{
		{0, 0}, {1, 1}, {-1, 2}, {2, 3}, {3, 3}, {-3, 4}, {4, 5}, {-128, 16}, {127, 13},
		{1<<31 - 1, 61}, {-(1<<31 - 1), 62}, {-1 << 31, 64},
	}
	for _, bucketCase := range cases {
		if bucket := residualBucket(bucketCase.value); bucket != bucketCase.expected {
			test.Errorf("residualBucket(%d) = %d, want %d", bucketCase.value, bucket, bucketCase.expected)
		}
	}
}

func TestQuantizedWeight(test *testing.T) {
	cases := []struct {
		quantized uint32
		expected  uint64
	}{
		{0, 0}, {1, 65536}, {2, 92682}, {3, 131072}, {4, 185364},
		{64, 92682 << 31}, {maxQuantized, 1 << 48},
	}
	for _, weightCase := range cases {
		if weight := quantizedWeight(weightCase.quantized); weight != weightCase.expected {
			test.Errorf("quantizedWeight(%d) = %d, want %d", weightCase.quantized, weight, weightCase.expected)
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
		{"largest weights", []uint64{1 << 48, 1 << 48}, []int{2048, 2048}},
	}
	for _, normalizeCase := range cases {
		test.Run(normalizeCase.name, func(test *testing.T) {
			frequencies := normalize(normalizeCase.weights, frequencyTotal)
			if !slices.Equal(frequencies, normalizeCase.expected) {
				test.Errorf("got %v, want %v", frequencies, normalizeCase.expected)
			}
		})
	}

	// 256 present symbols with one dominant: the floors of 1 take the sum over 4096.
	weights := make([]uint64, structuralAlphabetSize)
	for symbol := range weights {
		weights[symbol] = 1
	}
	weights[9] = 1 << 50
	frequencies := normalize(weights, frequencyTotal)
	if frequencies[9] != 4096-255 || frequencies[0] != 1 {
		test.Errorf("dominant: got %d and %d", frequencies[9], frequencies[0])
	}
}

func TestDivRound(test *testing.T) {
	cases := []struct {
		dividend, divisor, expected int32
	}{
		{7, 2, 4}, {-7, 2, -4}, {5, 3, 2}, {-5, 3, -2}, {4, 3, 1}, {0, 5, 0}, {-1 << 30, 32767, -32769},
	}
	for _, divideCase := range cases {
		if quotient := divRound(divideCase.dividend, divideCase.divisor); quotient != divideCase.expected {
			test.Errorf("divRound(%d, %d) = %d, want %d", divideCase.dividend, divideCase.divisor, quotient, divideCase.expected)
		}
	}
}

func TestGridAngles(test *testing.T) {
	// 0.0, 45/8192, 91 * 45/8192, 45.0, 90.0, 32767 * 45/8192, -180.0, -45/8192
	cases := []struct {
		grid     uint16
		expected uint32
	}{
		{0, 0}, {1, 0x3bb40000}, {91, 0x3efff000}, {8192, 0x42340000}, {16384, 0x42b40000},
		{32767, 0x4333fe98}, {0x8000, 0xc3340000}, {0xffff, 0xbbb40000},
	}
	for _, angleCase := range cases {
		if angleBits := gridToFloat32Bits(angleCase.grid); angleBits != angleCase.expected {
			test.Errorf("gridToFloat32Bits(%d) = %#x, want %#x", angleCase.grid, angleBits, angleCase.expected)
		}
	}
	for grid := range 1 << 16 {
		if found, isOnGrid := gridFromFloat32Bits(gridToFloat32Bits(uint16(grid))); !isOnGrid || int(found) != grid {
			test.Errorf("gridFromFloat32Bits(gridToFloat32Bits(%d)) = %d, %t", grid, found, isOnGrid)
		}
	}
	// -0.0, +180.0, 1.2345, one bit off 45/8192, the smallest subnormal, infinity, NaN
	offGrid := []uint32{0x80000000, 0x43340000, 0x3f9e0419, 0x3bb40001, 1, 0x7f800000, 0x7fc00000}
	for _, angleBits := range offGrid {
		if _, isOnGrid := gridFromFloat32Bits(angleBits); isOnGrid {
			test.Errorf("gridFromFloat32Bits(%#x) is on the grid", angleBits)
		}
	}
}

// The check values of FORMAT.md 8 rule 4.
func TestLog2Fixed(test *testing.T) {
	cases := []struct {
		value, expected uint64
	}{
		{1, 0}, {2, 65536}, {3, 103872}, {10, 217705}, {1000, 653117}, {4097, 786455},
		{1<<32 - 1, 2097151}, {1 << 40, 40 << 16}, {1<<62 + 12345, 4063232},
	}
	for _, logCase := range cases {
		if fixed := log2Fixed(logCase.value); fixed != logCase.expected {
			test.Errorf("log2Fixed(%d) = %d, want %d", logCase.value, fixed, logCase.expected)
		}
	}
}
