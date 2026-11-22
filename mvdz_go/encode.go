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
	encodedModels, models := buildModels(segments)

	// The second pass: FORMAT.md section 8 rules 3, 5 and 9.
	var codedSegments []byte
	var isModelUsed [firstBlobStream]bool
	var protocol protocolState
	durationMs := uint64(0)
	for _, segment := range segments {
		streams, endProtocol, isWalked := collectStreams(segment, protocol)
		coded, isCoded := []byte(nil), false
		if isWalked {
			coded, isCoded = encodeStreams(streams, models)
		}
		if isCoded {
			for stream, values := range streams.collected {
				isModelUsed[stream] = isModelUsed[stream] || len(values) > 0
			}
		} else {
			allTail := &segmentStreams{protocol: protocol, tail: uint32(len(segment))}
			allTail.blobs[streamTail] = segment
			coded, _ = encodeStreams(allTail, models)
		}
		if isWalked {
			protocol = endProtocol
		}
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
	file = appendModelsBlock(file, encodedModels, &isModelUsed)
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

// startsLikeDemo is the input check, FORMAT.md section 8 rule 1: the first
// frame is whole, of a type that carries a payload, and not empty; and it
// walks without an escape or 16 more whole frames follow it.
func startsLikeDemo(demo []byte) bool {
	isGzip := len(demo) >= 2 && demo[0] == 0x1f && demo[1] == 0x8b
	firstLength, isFrame := frameLength(demo)
	if isGzip || !isFrame {
		return false
	}
	frameType := demo[1] & 7
	headerSize := 6
	if frameType == demMultiple {
		headerSize = 10
	}
	hasPayload := frameType != demSet && firstLength > headerSize
	if !hasPayload {
		return false
	}
	streams, _, isWalked := collectStreams(demo[:firstLength], protocolState{})
	hasEscape := !isWalked || len(streams.blobs[streamEscapeLength]) > 0
	if !hasEscape {
		return true
	}
	rest := demo[firstLength:]
	for range firstFramesNeeded {
		length, isFrame := frameLength(rest)
		if !isFrame {
			return false
		}
		rest = rest[length:]
	}
	return true
}

// cutSegments cuts a demo into segments, FORMAT.md section 8 rule 2: after
// the first frame that takes a segment to segmentTargetSize, and every
// maxRawLength once a frame does not fit or the bytes do not frame.
func cutSegments(demo []byte) [][]byte {
	var segments [][]byte
	framedLength := 0
	for framedLength < len(demo) {
		frameLength, isFrame := frameLength(demo[framedLength:])
		isFramed := isFrame && framedLength+frameLength <= maxRawLength
		if !isFramed {
			break
		}
		framedLength += frameLength
		isCut := framedLength >= segmentTargetSize && framedLength < len(demo)
		if isCut {
			segments = append(segments, demo[:framedLength])
			demo = demo[framedLength:]
			framedLength = 0
		}
	}
	for len(demo) > maxRawLength {
		segments = append(segments, demo[:maxRawLength])
		demo = demo[maxRawLength:]
	}
	if len(demo) > 0 {
		segments = append(segments, demo)
	}
	return segments
}

// buildModels is the first pass, FORMAT.md section 8 rule 4: it counts the
// symbols of every segment whose walk succeeds and builds one model per
// stream. It returns the models as the models block holds them and as the
// coder uses them, nil for a stream without a model.
func buildModels(segments [][]byte) (*[firstBlobStream][]byte, *fileModels) {
	var tallies [firstBlobStream]streamTally
	for stream := range tallies {
		tallies[stream] = streamTally{}
	}
	var protocol protocolState
	for _, segment := range segments {
		streams, endProtocol, isWalked := collectStreams(segment, protocol)
		if !isWalked {
			continue
		}
		protocol = endProtocol
		for stream, values := range streams.collected {
			tallies[stream].add(stream, values)
		}
	}

	encodedModels := new([firstBlobStream][]byte)
	models := new(fileModels)
	blockLength := 4
	for stream, tally := range tallies {
		if len(tally) == 0 {
			continue
		}
		ownCount, inflated := tally.model(stream)
		models[stream], _ = newStreamModel(inflated, ownCount, streamAlphabet(stream)) // fails for unordered contexts only
		encodedModels[stream] = binary.LittleEndian.AppendUint32(nil, uint32(ownCount))
		encodedModels[stream] = append(encodedModels[stream], deflate(inflated)...)
		blockLength += 8 + len(encodedModels[stream])
	}
	for blockLength > maxModelsLength {
		largest := 0
		for stream, encoded := range encodedModels {
			if len(encoded) >= len(encodedModels[largest]) {
				largest = stream
			}
		}
		blockLength -= 8 + len(encodedModels[largest])
		encodedModels[largest], models[largest] = nil, nil
	}
	return encodedModels, models
}

// collectStreams walks a segment in the encoding direction. It returns the
// streams and the protocol state at the end of the segment; false if the
// walk fails: it does not reproduce the segment, or a stream is longer than
// a stream table allows.
func collectStreams(segment []byte, protocol protocolState) (*segmentStreams, protocolState, bool) {
	framedLength, _ := scanFrames(segment)
	streams := &segmentStreams{isEncoding: true, protocol: protocol, tail: uint32(len(segment) - framedLength)}
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
	maxCount := len(segment) + maxCountSlack
	for _, values := range streams.collected {
		if len(values) > maxCount {
			return nil, protocol, false
		}
	}
	return streams, walker.protocol, true
}

// encodeStreams lays out the coded bytes of a segment, FORMAT.md section 3.1
// and section 8 rule 5; false if a stream has no model or the bytes are more
// than a segment may hold.
func encodeStreams(streams *segmentStreams, models *fileModels) ([]byte, bool) {
	var table, bodies, blobBytes []byte
	entryCount := uint32(0)
	for stream, values := range streams.collected {
		if len(values) == 0 {
			continue
		}
		if models[stream] == nil {
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
		blobBytes = append(blobBytes, blob...)
		entryCount++
	}
	var blobBody []byte
	if len(blobBytes) > 0 {
		blobBody = deflate(blobBytes)
	}

	coded := binary.LittleEndian.AppendUint32(nil, streams.protocol.fte)
	coded = binary.LittleEndian.AppendUint32(coded, streams.protocol.fte2)
	coded = binary.LittleEndian.AppendUint32(coded, streams.protocol.mvd1)
	coded = binary.LittleEndian.AppendUint32(coded, streams.tail)
	coded = binary.LittleEndian.AppendUint32(coded, entryCount)
	coded = append(coded, table...)
	coded = binary.LittleEndian.AppendUint32(coded, uint32(len(blobBody)))
	coded = append(coded, bodies...)
	coded = append(coded, blobBody...)
	return coded, len(coded) <= maxCodedLength
}

// appendModelsBlock appends the models block, FORMAT.md section 8 rule 6: the
// models some segment uses.
func appendModelsBlock(file []byte, encodedModels *[firstBlobStream][]byte, isModelUsed *[firstBlobStream]bool) []byte {
	var entries []byte
	entryCount := uint32(0)
	for stream, encoded := range encodedModels {
		if !isModelUsed[stream] {
			continue
		}
		entries = binary.LittleEndian.AppendUint32(entries, uint32(stream))
		entries = binary.LittleEndian.AppendUint32(entries, uint32(len(encoded)))
		entries = append(entries, encoded...)
		entryCount++
	}
	if entryCount == 0 {
		return binary.LittleEndian.AppendUint32(file, 0)
	}
	file = binary.LittleEndian.AppendUint32(file, uint32(4+len(entries)))
	file = binary.LittleEndian.AppendUint32(file, entryCount)
	return append(file, entries...)
}
