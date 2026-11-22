package mvdz

import (
	"bytes"
	"crypto/sha256"
	"encoding/binary"
	"errors"
	"fmt"
	"hash/crc32"
)

// Error categories of encoding, FORMAT.md section 7.
var (
	ErrNotMvd       = errors.New("mvdz: not an MVD demo")
	ErrDemoTooLarge = errors.New("mvdz: demo too large")
	ErrInternal     = errors.New("mvdz: internal error")
)

// Encode codes an MVD demo as an MVDZ file, FORMAT.md section 8.
func Encode(demo []byte) ([]byte, error) {
	if !startsLikeDemo(demo) {
		return nil, ErrNotMvd
	}
	return encodeAny(demo)
}

const (
	segmentTargetSize = 1 << 18
	maxSegmentCount   = 1 << 16
	firstFramesNeeded = 16
)

// encodeAny codes any bytes as an MVDZ file: Encode without the input check.
func encodeAny(demo []byte) ([]byte, error) {
	segments := cutSegments(demo)
	if len(segments) > maxSegmentCount {
		return nil, ErrDemoTooLarge
	}
	models := buildModels(segments)

	// The second pass: FORMAT.md section 8 rules 3, 5 and 9.
	var codedSegments []byte
	var isModelUsed [firstBlobStream]bool
	var protocol protocolState
	durationMs := uint64(0)
	for _, segment := range segments {
		coded, endProtocol := codeSegment(segment, protocol, models, &isModelUsed)
		protocol = endProtocol
		_, segmentDurationMs := scanFrames(segment)
		durationMs += uint64(segmentDurationMs)
		codedSegments = binary.LittleEndian.AppendUint32(codedSegments, uint32(len(coded)))
		codedSegments = binary.LittleEndian.AppendUint32(codedSegments, uint32(len(segment)))
		codedSegments = binary.LittleEndian.AppendUint32(codedSegments, segmentDurationMs)
		codedSegments = binary.LittleEndian.AppendUint32(codedSegments, crc32.ChecksumIEEE(segment))
		codedSegments = append(codedSegments, coded...)
	}

	// FORMAT.md section 2.1
	file := []byte("MVDZ")
	file = binary.LittleEndian.AppendUint32(file, formatVersion)
	file = binary.LittleEndian.AppendUint64(file, uint64(len(demo)))
	file = binary.LittleEndian.AppendUint64(file, durationMs)
	file = binary.LittleEndian.AppendUint32(file, crc32.ChecksumIEEE(demo))
	digest := sha256.Sum256(demo)
	file = append(file, digest[:]...)
	file = appendModelsBlock(file, models, &isModelUsed)
	file = append(file, codedSegments...)

	// FORMAT.md section 8 rule 7
	decoded, err := Decode(file)
	if err != nil {
		return nil, fmt.Errorf("%w: the file does not decode: %w", ErrInternal, err)
	}
	if !bytes.Equal(decoded, demo) {
		return nil, fmt.Errorf("%w: the file decodes to another demo", ErrInternal)
	}
	return file, nil
}

// codeSegment writes a segment the first way that works, FORMAT.md section 8
// rule 5: transform-coded, all tail, stored. It marks the models it uses and
// returns the protocol state at the end of the segment.
func codeSegment(segment []byte, protocol protocolState, models *fileModels, isModelUsed *[firstBlobStream]bool) ([]byte, protocolState) {
	encode := func(streams *segmentStreams) ([]byte, bool) {
		coded, isCoded := encodeStreams(streams, uint32(len(segment)), models)
		if isCoded {
			for stream, values := range streams.collected {
				isModelUsed[stream] = isModelUsed[stream] || len(values) > 0
			}
		}
		return coded, isCoded
	}
	endProtocol := protocol
	walked, walkedProtocol, isWalked := collectStreams(segment, protocol)
	if isWalked {
		endProtocol = walkedProtocol
		if coded, isCoded := encode(walked); isCoded {
			return coded, endProtocol
		}
	}
	if coded, isCoded := encode(allTailStreams(segment, protocol)); isCoded {
		return coded, endProtocol
	}
	return storedSegment(segment, protocol), endProtocol
}

// startsLikeDemo is the input check, FORMAT.md section 8 rule 1: the first
// frame is whole, of a type that carries a payload, and not empty; and it
// walks without an escape or 16 more whole frames follow it.
func startsLikeDemo(demo []byte) bool {
	isGzip := len(demo) >= 2 && demo[0] == 0x1f && demo[1] == 0x8b
	firstLen, isFrame := frameLen(demo)
	if isGzip || !isFrame {
		return false
	}
	frameType := demo[1] & 7
	headerSize := 6
	if frameType == demMultiple {
		headerSize = 10
	}
	hasPayload := frameType != demSet && firstLen > headerSize
	if !hasPayload {
		return false
	}
	streams, _, isWalked := collectStreams(demo[:firstLen], protocolState{})
	hasEscape := !isWalked || len(streams.blobs[streamEscapeLength]) > 0
	if !hasEscape {
		return true
	}
	rest := demo[firstLen:]
	for range firstFramesNeeded {
		length, isFrame := frameLen(rest)
		if !isFrame {
			return false
		}
		rest = rest[length:]
	}
	return true
}

// cutSegments cuts a demo into segments, FORMAT.md section 8 rule 2: after
// the first frame that takes a segment to segmentTargetSize, and every
// maxRawLen once a frame does not fit or the bytes do not frame.
func cutSegments(demo []byte) [][]byte {
	var segments [][]byte
	framedLen := 0
	for framedLen < len(demo) {
		frameLen, isFrame := frameLen(demo[framedLen:])
		isFramed := isFrame && framedLen+frameLen <= maxRawLen
		if !isFramed {
			break
		}
		framedLen += frameLen
		isCut := framedLen >= segmentTargetSize && framedLen < len(demo)
		if isCut {
			segments = append(segments, demo[:framedLen])
			demo = demo[framedLen:]
			framedLen = 0
		}
	}
	for len(demo) > maxRawLen {
		segments = append(segments, demo[:maxRawLen])
		demo = demo[maxRawLen:]
	}
	if len(demo) > 0 {
		segments = append(segments, demo)
	}
	return segments
}

// buildModels is the first pass, FORMAT.md section 8 rule 4: it counts the
// symbols of every segment as walked, or as all tail where the walk fails,
// and builds one model per stream; nil for a stream without a model.
func buildModels(segments [][]byte) *fileModels {
	var tallies [firstBlobStream]streamTally
	for stream := range tallies {
		tallies[stream] = streamTally{}
	}
	var protocol protocolState
	for _, segment := range segments {
		streams, endProtocol, isWalked := collectStreams(segment, protocol)
		if isWalked {
			protocol = endProtocol
		} else {
			streams = allTailStreams(segment, protocol)
		}
		for stream, values := range streams.collected {
			tallies[stream].add(values, streamAlphabet(stream), stream >= firstNumericStream)
		}
	}

	models := new(fileModels)
	for stream, tally := range tallies {
		if len(tally) == 0 {
			continue
		}
		alphabet := streamAlphabet(stream)
		models[stream], _ = newStreamModel(tally.rawModel(alphabet, streamTableCost), alphabet) // built here: valid
	}
	fitModelsBlock(models)
	return models
}

// fitModelsBlock drops the model with the longest raw model (ties: the
// highest stream id) until the models block is within the limits of
// FORMAT.md section 7. The models of the blob tokens stay: every fallback
// but the stored segment needs them.
func fitModelsBlock(models *fileModels) {
	for {
		_, body, rawLen := modelsBlock(models, nil)
		if len(body) <= maxModelsLen && rawLen <= maxModelsRawLen {
			return
		}
		largest := -1
		for stream, model := range models {
			isBlobTokenModel := stream == streamLzLiteral || stream == streamLzLength || stream == streamLzDistance
			if model == nil || isBlobTokenModel {
				continue
			}
			if largest < 0 || len(model.raw) >= len(models[largest].raw) {
				largest = stream
			}
		}
		if largest < 0 {
			return
		}
		models[largest] = nil
	}
}

// modelsBlock is the body of the models block listing the models of
// isListed, every model where isListed is nil, with the count of the models
// and their raw length.
func modelsBlock(models *fileModels, isListed *[firstBlobStream]bool) (count int, body []byte, rawLen int) {
	var listed []listedModel
	for stream, model := range models {
		if model != nil && (isListed == nil || isListed[stream]) {
			listed = append(listed, listedModel{stream, model.raw})
		}
	}
	if len(listed) == 0 {
		return 0, nil, 0
	}
	body, rawLen = writeModelsBlock(listed)
	return len(listed), body, rawLen
}

// collectStreams walks a segment in the encoding direction. It returns the
// streams and the protocol state at the end of the segment; false if the
// walk fails: it does not reproduce the segment.
func collectStreams(segment []byte, protocol protocolState) (*segmentStreams, protocolState, bool) {
	framedLen, _ := scanFrames(segment)
	streams := &segmentStreams{isEncoding: true, protocol: protocol, tail: uint32(len(segment) - framedLen)}
	walked := bytes.Clone(segment)
	walker := newWalker(walked, streams)
	walker.run(streams.tail)
	if walker.err != nil {
		return nil, protocol, false
	}
	if err := fillNumericFields(walked, &walker.records, streams); err != nil {
		return nil, protocol, false
	}
	if !bytes.Equal(walked, segment) {
		return nil, protocol, false
	}
	collectBlobTokens(streams)
	return streams, walker.protocol, true
}

// allTailStreams is a segment with every byte in the tail blob stream: the
// fallback layout of FORMAT.md section 8 rule 5.
func allTailStreams(segment []byte, protocol protocolState) *segmentStreams {
	streams := &segmentStreams{isEncoding: true, protocol: protocol, tail: uint32(len(segment))}
	streams.blobs[streamTail] = segment
	collectBlobTokens(streams)
	return streams
}

// collectBlobTokens parses the blob bytes, the blob streams in id order, and
// collects their tokens, FORMAT.md section 6.6.
func collectBlobTokens(streams *segmentStreams) {
	var blobBytes []byte
	for _, blob := range streams.blobs {
		blobBytes = append(blobBytes, blob...)
	}
	lzCode(blobBytes, lzParse(blobBytes), streams)
}

// encodeStreams lays out the coded bytes of a segment, FORMAT.md section 3.1
// and section 8 rule 5; false if a stream has no model or no code for one of
// its symbols, or a limit of FORMAT.md section 7 does not hold.
func encodeStreams(streams *segmentStreams, rawLen uint32, models *fileModels) ([]byte, bool) {
	maxCount := int(maxStreamCount(rawLen))
	blobTotal := uint64(0)
	for _, blob := range streams.blobs {
		blobTotal += uint64(len(blob))
		if len(blob) > maxCount {
			return nil, false
		}
	}
	if blobTotal > maxBlobTotal(rawLen) {
		return nil, false
	}

	var table, bodies []byte
	entryCount := uint32(0)
	for stream, values := range streams.collected {
		if len(values) == 0 {
			continue
		}
		if len(values) > maxCount || models[stream] == nil {
			return nil, false
		}
		body, isEncoded := encodeStream(models[stream], values, stream >= firstNumericStream)
		if !isEncoded {
			return nil, false
		}
		table = binary.LittleEndian.AppendUint32(table, uint32(stream))
		table = binary.LittleEndian.AppendUint32(table, uint32(len(values)))
		table = binary.LittleEndian.AppendUint32(table, uint32(len(body)))
		bodies = append(bodies, body...)
		entryCount++
	}
	for stream, blob := range streams.blobs {
		if len(blob) == 0 {
			continue
		}
		table = binary.LittleEndian.AppendUint32(table, uint32(stream))
		table = binary.LittleEndian.AppendUint32(table, uint32(len(blob)))
		entryCount++
	}

	coded := appendTransformFields(nil, streams.protocol, streams.tail, entryCount)
	coded = append(coded, table...)
	coded = append(coded, bodies...)
	return coded, len(coded) <= maxCodedLen
}

// storedSegment is the segment as it is, behind a transform header without
// streams, FORMAT.md section 3.1: the one layout with a size bound.
func storedSegment(segment []byte, protocol protocolState) []byte {
	const noStreams = 0
	coded := make([]byte, 0, transformFieldsLen+len(segment))
	coded = appendTransformFields(coded, protocol, uint32(len(segment)), noStreams)
	return append(coded, segment...)
}

func appendTransformFields(coded []byte, protocol protocolState, tail uint32, entryCount uint32) []byte {
	coded = binary.LittleEndian.AppendUint32(coded, protocol.fte)
	coded = binary.LittleEndian.AppendUint32(coded, protocol.fte2)
	coded = binary.LittleEndian.AppendUint32(coded, protocol.mvd1)
	coded = binary.LittleEndian.AppendUint32(coded, tail)
	return binary.LittleEndian.AppendUint32(coded, entryCount)
}

// appendModelsBlock appends the models block, FORMAT.md section 8 rule 6: the
// models some segment uses.
func appendModelsBlock(file []byte, models *fileModels, isModelUsed *[firstBlobStream]bool) []byte {
	_, body, _ := modelsBlock(models, isModelUsed)
	file = binary.LittleEndian.AppendUint32(file, uint32(len(body)))
	return append(file, body...)
}
