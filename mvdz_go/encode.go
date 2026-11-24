package mvdz

import (
	"bytes"
	"crypto/sha256"
	"fmt"
	"hash/crc32"
	"runtime"
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
	decoderModels, err := readModels(bytes.NewReader(appendModels(nil, modelsBody)))
	if err != nil {
		return nil, fmt.Errorf("%w: the encoder's models do not read back", ErrInternal)
	}
	codedSegments, durationMs, err := secondPass(segments, startProtocols, models, decoderModels)
	if err != nil {
		return nil, err
	}

	header := Header{
		Size:       uint64(len(demo)),
		DurationMs: durationMs,
		CRC32:      crc32.ChecksumIEEE(demo),
		SHA256:     sha256.Sum256(demo),
	}
	file := appendHeader(nil, header)
	file = appendModels(file, modelsBody)
	return append(file, codedSegments...), nil
}

// FORMAT.md 8 rules 3 and 4: a batch is walked in parallel from the end
// protocol of the batch before; the few segments that start in another
// protocol are walked again.
func firstPass(segments [][]byte) (*fileModels, []protocolState, error) {
	const maxSegmentCount = 1 << 16
	if len(segments) > maxSegmentCount {
		return nil, nil, ErrDemoTooLarge
	}
	var builder modelBuilder
	startProtocols := make([]protocolState, 0, len(segments))
	var protocol protocolState
	for len(startProtocols) < len(segments) {
		// the first segment alone: it sets the protocol of most demos
		batchLen := 1
		if len(startProtocols) > 0 {
			batchLen = runtime.GOMAXPROCS(0)
		}
		batch := segments[len(startProtocols):]
		batch = batch[:min(batchLen, len(batch))]
		walked := make([]*segmentStreams, len(batch))
		endProtocols := make([]protocolState, len(batch))
		batchProtocol := protocol
		inParallel(len(batch), func(index int) {
			walked[index], endProtocols[index] = firstPassWalk(batch[index], batchProtocol)
		})
		for index, segment := range batch {
			if protocol != batchProtocol {
				walked[index], endProtocols[index] = firstPassWalk(segment, protocol)
			}
			startProtocols = append(startProtocols, protocol)
			protocol = endProtocols[index]
		}
		builder.add(walked)
	}
	models, err := builder.models()
	return models, startProtocols, err
}

// firstPassWalk is all tail when the segment does not walk; it returns the
// protocol state at the segment's end.
func firstPassWalk(segment []byte, protocol protocolState) (*segmentStreams, protocolState) {
	streams, endProtocol, isWalked := walkSegment(segment, protocol)
	if !isWalked {
		streams = walkTail(segment, protocol)
	}
	return streams, endProtocol
}

// FORMAT.md 8 rules 5 and 9
func secondPass(segments [][]byte, startProtocols []protocolState, models, decoderModels *fileModels) ([]byte, uint64, error) {
	coded := make([][]byte, len(segments))
	errs := make([]error, len(segments))
	inParallel(len(segments), func(index int) {
		coded[index], errs[index] = encodeSegment(segments[index], startProtocols[index], models, decoderModels)
	})
	var codedSegments []byte
	durationMs := uint64(0)
	for index, segment := range segments {
		if errs[index] != nil {
			return nil, 0, errs[index]
		}
		segmentMs := segmentDurationMs(segment)
		durationMs += uint64(segmentMs)
		codedSegments = appendSegment(codedSegments, segment, segmentMs, coded[index])
	}
	return codedSegments, durationMs, nil
}

// encodeSegment writes a segment the first way that works, FORMAT.md 8
// rule 5: transform-coded, all tail, stored; rule 7: it decodes to segment.
func encodeSegment(segment []byte, protocol protocolState, models, decoderModels *fileModels) ([]byte, error) {
	rawLen := uint32(len(segment))
	var coded []byte
	isCoded := false
	if walked, _, isWalked := walkSegment(segment, protocol); isWalked {
		coded, isCoded = encodeTransform(walked, rawLen, models)
	}
	if !isCoded {
		coded, isCoded = encodeTransform(walkTail(segment, protocol), rawLen, models)
	}
	if !isCoded {
		coded = storedSegment(segment, protocol)
	}
	if !decodesTo(coded, segment, decoderModels) {
		return nil, fmt.Errorf("%w: the encoder's output does not decode to its input", ErrInternal)
	}
	return coded, nil
}

func decodesTo(coded []byte, segment []byte, decoderModels *fileModels) bool {
	rawLen := uint32(len(segment))
	header, err := parseTransformHeader(coded, rawLen)
	if err != nil {
		return false
	}
	decoded, err := decodeTransform(coded, header, rawLen, decoderModels)
	return err == nil && bytes.Equal(decoded, segment)
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
