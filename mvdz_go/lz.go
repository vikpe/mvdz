package mvdz

import "encoding/binary"

// The blob bytes of a segment as LZ77 tokens, FORMAT.md section 6.6: a
// literal byte, or a match that copies bytes already produced. Three streams
// code them, with the models every other stream uses.

// lzToken is a literal or a match; a literal has length 0.
type lzToken struct {
	length   int
	distance int
	literal  uint8
}

// lzParse is the canonical parse, FORMAT.md section 8 rule 10: greedy, the
// longest of at most maxChain earlier positions with the same hash, the
// nearest of equally long ones.
func lzParse(data []byte) []lzToken {
	const (
		minMatch = 4
		maxChain = 32
		hashBits = 15
		none     = -1
	)
	hash := func(position int) (int, bool) {
		if position+4 > len(data) {
			return 0, false
		}
		product := binary.LittleEndian.Uint32(data[position:]) * 0x9E3779B1
		return int(product >> (32 - hashBits)), true
	}
	// the latest position of each hash, and for each position the one before it
	latest := make([]int, 1<<hashBits)
	for slot := range latest {
		latest[slot] = none
	}
	earlier := make([]int, len(data))
	var tokens []lzToken
	for position := 0; position < len(data); {
		bestLen, bestStart := 0, 0
		candidate := none
		if slot, isHashed := hash(position); isHashed {
			candidate = latest[slot]
		}
		for steps := 0; candidate != none && steps < maxChain; steps++ {
			length := commonPrefixLen(data[candidate:], data[position:])
			if length > bestLen {
				bestLen, bestStart = length, candidate
			}
			candidate = earlier[candidate]
		}
		advance := 1
		if bestLen >= minMatch {
			tokens = append(tokens, lzToken{length: bestLen, distance: position - bestStart})
			advance = bestLen
		} else {
			tokens = append(tokens, lzToken{literal: data[position]})
		}
		for covered := position; covered < position+advance; covered++ {
			if slot, isHashed := hash(covered); isHashed {
				earlier[covered] = latest[slot]
				latest[slot] = covered
			}
		}
		position += advance
	}
	return tokens
}

func commonPrefixLen(earlier []byte, later []byte) int {
	length := 0
	for length < len(later) && earlier[length] == later[length] {
		length++
	}
	return length
}

// lzCode collects tokens as their streams code them; data is what they
// expand to.
func lzCode(data []byte, tokens []lzToken, streams *segmentStreams) {
	position := 0
	previousLen := 0
	for _, token := range tokens {
		lengthContext := lzBucketContext(previousLen)
		streams.collected[streamLzLen] = append(streams.collected[streamLzLen], codedValue{lengthContext, int32(token.length)})
		if token.length == 0 {
			previousByte := uint8(0)
			if position > 0 {
				previousByte = data[position-1]
			}
			streams.collected[streamLzLit] = append(streams.collected[streamLzLit], codedValue{uint16(previousByte), int32(token.literal)})
			position++
		} else {
			distanceContext := lzBucketContext(token.length)
			streams.collected[streamLzDist] = append(streams.collected[streamLzDist], codedValue{distanceContext, int32(token.distance)})
			position += token.length
		}
		previousLen = token.length
	}
}

// lzExpand decodes the blobTotal blob bytes of a segment from its tokens.
func lzExpand(streams *segmentStreams, blobTotal int) ([]byte, error) {
	blobBytes := make([]byte, 0, blobTotal)
	previousLen := 0
	for len(blobBytes) < blobTotal {
		codedLen, isDecoded := streams.residual(streamLzLen, lzBucketContext(previousLen), 0)
		if !isDecoded || codedLen < 0 {
			return nil, corrupt("blob tokens")
		}
		length := int(codedLen)
		if length == 0 {
			previousByte := uint8(0)
			if len(blobBytes) > 0 {
				previousByte = blobBytes[len(blobBytes)-1]
			}
			literal, isDecoded := streams.symbol(streamLzLit, uint16(previousByte), 0)
			if !isDecoded {
				return nil, corrupt("blob tokens")
			}
			blobBytes = append(blobBytes, literal)
		} else {
			codedDistance, isDecoded := streams.residual(streamLzDist, lzBucketContext(length), 0)
			distance := int(codedDistance)
			isInside := isDecoded && distance >= 1 && distance <= len(blobBytes) && length <= blobTotal-len(blobBytes)
			if !isInside {
				return nil, corrupt("blob tokens")
			}
			// byte by byte: a match may run into the bytes it produces
			for range length {
				blobBytes = append(blobBytes, blobBytes[len(blobBytes)-distance])
			}
		}
		previousLen = length
	}
	return blobBytes, nil
}

func lzBucketContext(value int) uint16 {
	const maxBucket = 63
	return min(residualBucket(int32(value)), maxBucket)
}
