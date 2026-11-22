package mvdz

import (
	"math"
	"math/bits"
)

// The exact arithmetic of the format, FORMAT.md: every encoder and decoder
// computes these values bit for bit, in any language. Integers throughout;
// the two float functions are single IEEE operations.

// residualBucket is q(v) of FORMAT.md section 1: 0 for zero, else twice the
// bit length of the magnitude, less one, plus one for a negative value.
func residualBucket(value int32) uint16 {
	if value == 0 {
		return 0
	}
	magnitude := uint32(value)
	sign := 0
	if value < 0 {
		magnitude = -magnitude
		sign = 1
	}
	return uint16(2*bits.Len32(magnitude) - 1 + sign)
}

// divRound divides by a positive divisor; halves round away from zero.
func divRound(dividend, divisor int32) int32 {
	magnitude := uint32(dividend)
	if dividend < 0 {
		magnitude = -magnitude
	}
	quotient := int32((magnitude + uint32(divisor)/2) / uint32(divisor))
	if dividend < 0 {
		return -quotient
	}
	return quotient
}

// onGrid is the grid value of a usercmd angle, FORMAT.md section 4.11; false
// for -0.0, NaN, +180 and anything else off the 360/65536 grid.
func onGrid(angleBits uint32) (uint16, bool) {
	// f * 8192 is exact and the division correctly rounded, so an integer quotient is exact
	scaled := float64(math.Float32frombits(angleBits)) * 8192 / 45
	isOnGrid := scaled == math.Trunc(scaled) && scaled >= -32768 && scaled <= 32767
	if !isOnGrid {
		return 0, false
	}
	grid := uint16(int16(scaled))
	return grid, gridF32(grid) == angleBits
}

// gridF32 is the float bits of grid * 45/8192, exact, FORMAT.md section 5.6.
func gridF32(grid uint16) uint32 {
	return math.Float32bits(float32(int16(grid)) * (45.0 / 8192.0))
}

const stepsPerOctave = 2

// quantizedWeight is 2^((quantized-1)/2) in 16.16 fixed point, FORMAT.md
// section 6.3.
func quantizedWeight(quantized uint32) uint64 {
	if quantized == 0 {
		return 0
	}
	stepPowers := [stepsPerOctave]uint64{65536, 92682}
	step := quantized - 1
	octave := step / stepsPerOctave
	if octave >= 40 {
		return math.MaxUint64 >> 8
	}
	return stepPowers[step%stepsPerOctave] << octave
}

// quantize is the quantized count whose weight is nearest to count in ratio,
// FORMAT.md section 8 rule 4.
func quantize(count uint32) uint32 {
	if count == 0 {
		return 0
	}
	weight := uint64(count) << 16
	octave := uint32(bits.Len32(count) - 1)
	step := octave * stepsPerOctave
	for octaveStep := range uint32(stepsPerOctave) {
		if quantizedWeight(1+octave*stepsPerOctave+octaveStep) <= weight {
			step = octave*stepsPerOctave + octaveStep
		}
	}
	// weight/low against high/weight, as weight*weight against low*high
	squareHigh, squareLow := bits.Mul64(weight, weight)
	productHigh, productLow := bits.Mul64(quantizedWeight(1+step), quantizedWeight(2+step))
	isNearerToHigh := squareHigh > productHigh || (squareHigh == productHigh && squareLow > productLow)
	if isNearerToHigh {
		return 2 + step
	}
	return 1 + step
}

// normalize scales the weights to frequencies that sum to total, every
// present symbol getting at least 1, FORMAT.md section 6.3.
func normalize(weights []uint64, total int) []int {
	frequencies := make([]int, len(weights))
	var weightTotal uint64 // at most 256 weights below 2^56
	for _, weight := range weights {
		weightTotal += weight
	}
	if weightTotal == 0 {
		return frequencies
	}
	sum := 0
	for symbol, weight := range weights {
		if weight == 0 {
			continue
		}
		high, low := bits.Mul64(weight, uint64(total))
		share, _ := bits.Div64(high, low, weightTotal)
		frequencies[symbol] = max(int(share), 1)
		sum += frequencies[symbol]
	}
	difference := total - sum
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

// log2Fixed is log2 of value (at least 1) with 16 fraction bits, rounded
// down, FORMAT.md section 8 rule 4.
func log2Fixed(value uint64) uint64 {
	const fractionBits = 16
	const one = 1 << 30
	integer := bits.Len64(value) - 1
	// in [one, 2 * one)
	mantissa := value << (30 - min(integer, 30)) >> (max(integer, 30) - 30)
	fraction := uint64(0)
	for range fractionBits {
		mantissa = mantissa * mantissa >> 30
		fraction <<= 1
		if mantissa >= 2*one {
			mantissa >>= 1
			fraction |= 1
		}
	}
	return uint64(integer)<<fractionBits | fraction
}
