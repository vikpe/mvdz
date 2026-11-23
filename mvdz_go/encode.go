package mvdz

import (
	"bytes"
	"crypto/sha256"
	"fmt"
	"hash/crc32"
)

// Encode codes an MVD demo as an MVDZ file, FORMAT.md 8.
func Encode(demo []byte) ([]byte, error) {
	segments := cutSegments(demo)
	if len(segments) == 0 || !startsLikeMVD(segments[0]) {
		return nil, ErrNotMVD
	}
	return encodeSegments(demo, segments)
}

// encodeAny is Encode without the input check.
func encodeAny(demo []byte) ([]byte, error) {
	return encodeSegments(demo, cutSegments(demo))
}

func encodeSegments(demo []byte, segments [][]byte) ([]byte, error) {
	models, startProtocols, err := firstPass(segments)
	if err != nil {
		return nil, err
	}
	modelsBody, _, err := modelsBlock(models)
	if err != nil {
		return nil, err
	}
	codedSegments, durationMs := secondPass(segments, startProtocols, models)

	header := Header{
		Size:       uint64(len(demo)),
		DurationMs: durationMs,
		CRC32:      crc32.ChecksumIEEE(demo),
		SHA256:     sha256.Sum256(demo),
	}
	file := appendHeader(nil, header)
	file = appendModels(file, modelsBody)
	file = append(file, codedSegments...)

	// FORMAT.md 8 rule 7
	decoded, err := Decode(file)
	if err != nil {
		return nil, fmt.Errorf("%w: the file does not decode: %w", ErrInternal, err)
	}
	if !bytes.Equal(decoded, demo) {
		return nil, fmt.Errorf("%w: the file decodes to another demo", ErrInternal)
	}
	return file, nil
}

// FORMAT.md 8 rules 3 and 4
func firstPass(segments [][]byte) (*fileModels, []protocolState, error) {
	const maxSegmentCount = 1 << 16
	if len(segments) > maxSegmentCount {
		return nil, nil, ErrDemoTooLarge
	}
	var builder modelBuilder
	startProtocols := make([]protocolState, len(segments))
	var protocol protocolState
	for index, segment := range segments {
		startProtocols[index] = protocol
		streams, endProtocol, isWalked := walkSegment(segment, protocol)
		if isWalked {
			protocol = endProtocol
		} else {
			streams = walkTail(segment, protocol)
		}
		builder.add(streams)
	}
	models, err := builder.models()
	return models, startProtocols, err
}

// FORMAT.md 8 rules 5 and 9
func secondPass(segments [][]byte, startProtocols []protocolState, models *fileModels) ([]byte, uint64) {
	coded := make([][]byte, len(segments))
	inParallel(len(segments), func(index int) {
		coded[index] = encodeSegment(segments[index], startProtocols[index], models)
	})
	var codedSegments []byte
	durationMs := uint64(0)
	for index, segment := range segments {
		segmentMs := segmentDurationMs(segment)
		durationMs += uint64(segmentMs)
		codedSegments = appendSegment(codedSegments, segment, segmentMs, coded[index])
	}
	return codedSegments, durationMs
}

// encodeSegment writes a segment the first way that works, FORMAT.md 8
// rule 5: transform-coded, all tail, stored.
func encodeSegment(segment []byte, protocol protocolState, models *fileModels) []byte {
	if walked, _, isWalked := walkSegment(segment, protocol); isWalked {
		if coded, isCoded := encodeTransform(walked, uint32(len(segment)), models); isCoded {
			return coded
		}
	}
	if coded, isCoded := encodeTransform(walkTail(segment, protocol), uint32(len(segment)), models); isCoded {
		return coded
	}
	return storedSegment(segment, protocol)
}

// startsLikeMVD is the input check on the first segment, FORMAT.md 8 rule 1.
func startsLikeMVD(firstSegment []byte) bool {
	isGzip := len(firstSegment) >= 2 && firstSegment[0] == 0x1f && firstSegment[1] == 0x8b
	firstLen, isFrame := frameLen(firstSegment)
	if isGzip || !isFrame {
		return false
	}
	frameType := firstSegment[1] & 7
	frameHeaderSize := 6
	if frameType == demMultiple {
		frameHeaderSize = 10
	}
	hasPayload := frameType != demSet && firstLen > frameHeaderSize
	if !hasPayload {
		return false
	}
	streams, _, isWalked := walkSegment(firstSegment[:firstLen], protocolState{})
	if isWalked && !streams.hasEscape() {
		return true
	}
	return hasFrameRun(firstSegment[firstLen:])
}
