package mvdz

import (
	"encoding/binary"
	"maps"
	"math"
	"slices"
)

// streamTally counts the symbols of one stream per context, over the
// segments of a demo.
type streamTally map[uint16][]uint64

// add counts the values a segment codes in one stream: the symbols of a
// structural stream, the buckets of a numeric one.
func (tally streamTally) add(stream int, values []codedValue) {
	for _, coded := range values {
		counts := tally[coded.context]
		if counts == nil {
			counts = make([]uint64, streamAlphabet(stream))
			tally[coded.context] = counts
		}
		symbol := uint16(coded.value)
		if stream >= firstNumericStream {
			symbol = residualBucket(coded.value)
		}
		counts[symbol]++
	}
}

// model builds the model of a stream from its counts, FORMAT.md section 8
// rule 4. It returns the own count and what a model deflates, FORMAT.md
// section 6.2: the own contexts, the fallback table and the own tables.
func (tally streamTally) model(stream int) (int, []byte) {
	alphabet := streamAlphabet(stream)
	contexts := slices.Sorted(maps.Keys(tally))
	ownContexts := tally.ownContexts(contexts, alphabet)

	fallback := make([]uint64, alphabet)
	inflated := make([]byte, 0, 2*len(ownContexts)+(len(ownContexts)+1)*alphabet)
	for _, context := range ownContexts {
		inflated = binary.LittleEndian.AppendUint16(inflated, context)
	}
	for _, context := range contexts {
		if _, isOwn := slices.BinarySearch(ownContexts, context); !isOwn {
			addCounts(fallback, tally[context])
		}
	}
	inflated = appendQuantized(inflated, fallback)
	for _, context := range ownContexts {
		inflated = appendQuantized(inflated, tally[context])
	}
	return len(ownContexts), inflated
}

// ownContexts chooses the contexts that get a table of their own, FORMAT.md
// section 8 rule 4: those that cost fewer bits coded with their own table,
// the table included, than with the fallback table. The first pass measures
// against all contexts together, the second against the contexts the first
// left to the fallback.
func (tally streamTally) ownContexts(contexts []uint16, alphabet int) []uint16 {
	fallback := make([]uint64, alphabet)
	for _, context := range contexts {
		addCounts(fallback, tally[context])
	}
	var ownContexts []uint16
	for range 2 {
		rebuiltFallback := make([]uint64, alphabet)
		ownContexts = nil
		for _, context := range contexts {
			counts := tally[context]
			if ownTableBits(counts) < crossEntropyBits(counts, fallback) {
				ownContexts = append(ownContexts, context)
			} else {
				addCounts(rebuiltFallback, counts)
			}
		}
		fallback = rebuiltFallback
	}
	return ownContexts
}

// ownTableBits is the entropy of the counts plus the cost of storing their
// table: 16 bits and 5 bits per present symbol.
func ownTableBits(counts []uint64) float64 {
	presentCount := 0
	for _, count := range counts {
		if count > 0 {
			presentCount++
		}
	}
	return crossEntropyBits(counts, counts) + 16 + 5*float64(presentCount)
}

// crossEntropyBits is the size of the counted symbols coded with the
// frequencies of table; infinite if table lacks one of them.
func crossEntropyBits(counts []uint64, table []uint64) float64 {
	var total uint64
	for _, count := range table {
		total += count
	}
	bits := 0.0
	for symbol, count := range counts {
		if count > 0 && table[symbol] == 0 {
			return math.Inf(1)
		}
		if count > 0 {
			bits += float64(count) * math.Log2(float64(total)/float64(table[symbol]))
		}
	}
	return bits
}

// appendQuantized appends the counts quantized to the nearest step 2^(k/2)
// in ratio, as k + 1; 0 stays 0. FORMAT.md section 6.3 gives the inverse.
func appendQuantized(table []byte, counts []uint64) []byte {
	for _, count := range counts {
		quantized := uint8(0)
		if count > 0 {
			quantized = uint8(1 + math.Round(2*math.Log2(float64(count))))
		}
		table = append(table, quantized)
	}
	return table
}

func addCounts(sum []uint64, counts []uint64) {
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
