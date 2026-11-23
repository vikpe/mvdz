package mvdz

import (
	"bytes"
	"encoding/binary"
)

const segmentTargetLen = 1 << 18

// segmentStreams are the streams of a segment: decoding reads them, encoding
// collects what the walk codes.
type segmentStreams struct {
	protocol   protocolState // at the segment start
	isEncoding bool
	decoders   [firstBlobStream]*streamDecoder // decoding: nil for a stream not in the table
	collected  [firstBlobStream][]codedValue   // encoding
	blobs      [streamCount][]byte             // decoding: the unread bytes; encoding: the bytes collected
}

// codedValue is a symbol of a structural stream or a residual of a numeric
// one, with the context it is coded under.
type codedValue struct {
	context uint16
	value   int32
}

// symbol is false if a structural stream has no next symbol. Encoding
// collects sourceSymbol.
func (streams *segmentStreams) symbol(stream int, context uint16, sourceSymbol uint8) (uint8, bool) {
	if streams.isEncoding {
		streams.collected[stream] = append(streams.collected[stream], codedValue{context, int32(sourceSymbol)})
		return sourceSymbol, true
	}
	decoder := streams.decoders[stream]
	if decoder == nil {
		return 0, false
	}
	return decoder.symbol(context)
}

// residual is false if a numeric stream has no next residual. Encoding
// collects sourceResidual.
func (streams *segmentStreams) residual(stream int, context uint16, sourceResidual int32) (int32, bool) {
	if streams.isEncoding {
		streams.collected[stream] = append(streams.collected[stream], codedValue{context, sourceResidual})
		return sourceResidual, true
	}
	decoder := streams.decoders[stream]
	if decoder == nil {
		return 0, false
	}
	return decoder.value(context)
}

// blobBytes fills segmentBytes from a blob stream when decoding, false if it
// has fewer; encoding appends segmentBytes to the stream.
func (streams *segmentStreams) blobBytes(stream int, segmentBytes []byte) bool {
	blob := streams.blobs[stream]
	if streams.isEncoding {
		streams.blobs[stream] = append(blob, segmentBytes...)
		return true
	}
	if len(segmentBytes) > len(blob) {
		return false
	}
	copy(segmentBytes, blob)
	streams.blobs[stream] = blob[len(segmentBytes):]
	return true
}

func (streams *segmentStreams) isStreamDone(stream int) bool {
	decoder := streams.decoders[stream]
	return decoder == nil || decoder.isDone()
}

func (streams *segmentStreams) hasEscape() bool {
	return len(streams.blobs[streamEscapeLength]) > 0
}

// walkSegment is false if the transform cannot represent the segment;
// else it returns the protocol state at the segment's end.
func walkSegment(segment []byte, protocol protocolState) (*segmentStreams, protocolState, bool) {
	streams := &segmentStreams{isEncoding: true, protocol: protocol}
	walked := bytes.Clone(segment)
	walker := newWalker(walked, streams)
	walker.run(uint32(len(segment) - framedLen(segment)))
	isWalked := walker.err == nil && bytes.Equal(walked, segment)
	if !isWalked {
		return nil, protocol, false
	}
	collectBlobTokens(streams)
	return streams, walker.protocol, true
}

func framedLen(segment []byte) int {
	end := 0
	for {
		length, isFrame := frameLen(segment[end:])
		if !isFrame {
			return end
		}
		end += length
	}
}

// walkTail is the fallback layout of FORMAT.md 8 rule 5: every byte in the tail.
func walkTail(segment []byte, protocol protocolState) *segmentStreams {
	streams := &segmentStreams{isEncoding: true, protocol: protocol}
	streams.blobs[streamTail] = segment
	collectBlobTokens(streams)
	return streams
}

// FORMAT.md 6.6: the blob streams in id order.
func collectBlobTokens(streams *segmentStreams) {
	var blobBytes []byte
	for _, blob := range streams.blobs {
		blobBytes = append(blobBytes, blob...)
	}
	lzCode(blobBytes, lzParse(blobBytes), streams)
}

// maxBlobTotal is of the blob streams of a segment together, FORMAT.md 7.
func maxBlobTotal(rawLen uint32) uint64 {
	return 2 * uint64(rawLen)
}

// FORMAT.md 8 rule 4
type modelBuilder struct {
	tallies [firstBlobStream]streamTally
}

func (builder *modelBuilder) add(streams *segmentStreams) {
	for stream, values := range streams.collected {
		if len(values) == 0 {
			continue
		}
		if builder.tallies[stream] == nil {
			builder.tallies[stream] = streamTally{}
		}
		builder.tallies[stream].add(values, streamAlphabetSize(stream), stream >= firstNumericStream)
	}
}

func (builder *modelBuilder) models() (*fileModels, error) {
	models := new(fileModels)
	for stream, tally := range builder.tallies {
		if len(tally) != 0 {
			models[stream] = tally.model(streamAlphabetSize(stream), streamTableCost)
		}
	}
	return models, fitModelsBlock(models)
}

// modelsBlock is the body of the models block listing every model, FORMAT.md
// 8 rule 6, and their raw length.
func modelsBlock(models *fileModels) (body []byte, rawLen int, err error) {
	var listed []listedModel
	for stream, model := range models {
		if model != nil {
			listed = append(listed, listedModel{stream, model.raw})
		}
	}
	if len(listed) == 0 {
		return nil, 0, nil
	}
	return writeModelsBlock(listed)
}

// fitModelsBlock drops the model with the longest raw model (ties: the highest
// stream id) until the block is within FORMAT.md 7. The blob tokens' stay.
func fitModelsBlock(models *fileModels) error {
	for {
		body, rawLen, err := modelsBlock(models)
		if err != nil {
			return err
		}
		if len(body) <= maxModelsLen && rawLen <= maxModelsRawLen {
			return nil
		}
		largest := -1
		for stream, model := range models {
			isBlobTokenModel := stream == streamLZLiteral || stream == streamLZLength || stream == streamLZDistance
			if model == nil || isBlobTokenModel {
				continue
			}
			if largest < 0 || len(model.raw) >= len(models[largest].raw) {
				largest = stream
			}
		}
		if largest < 0 {
			return nil
		}
		models[largest] = nil
	}
}

// encodeTransform is false if a stream has no model or no code for one of
// its symbols, or a limit of FORMAT.md 7 does not hold; FORMAT.md 3.1.
func encodeTransform(streams *segmentStreams, rawLen uint32, models *fileModels) ([]byte, bool) {
	blobTotal := uint64(0)
	for _, blob := range streams.blobs {
		blobTotal += uint64(len(blob))
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
		if models[stream] == nil {
			return nil, false
		}
		body, isEncoded := encodeStream(models[stream], values, stream >= firstNumericStream)
		if !isEncoded {
			return nil, false
		}
		table = binary.LittleEndian.AppendUint32(table, uint32(stream))
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

	coded := appendTransformFields(nil, streams.protocol, entryCount)
	coded = append(coded, table...)
	coded = append(coded, bodies...)
	return coded, len(coded) <= maxCodedLen
}

// storedSegment is the one layout with a size bound, FORMAT.md 3.1.
func storedSegment(segment []byte, protocol protocolState) []byte {
	const (
		transformFieldsSize = 4 * 4
		noStreams           = 0
	)
	coded := make([]byte, 0, transformFieldsSize+len(segment))
	coded = appendTransformFields(coded, protocol, noStreams)
	return append(coded, segment...)
}

func appendTransformFields(coded []byte, protocol protocolState, entryCount uint32) []byte {
	coded = binary.LittleEndian.AppendUint32(coded, protocol.fte)
	coded = binary.LittleEndian.AppendUint32(coded, protocol.fte2)
	coded = binary.LittleEndian.AppendUint32(coded, protocol.mvd1)
	return binary.LittleEndian.AppendUint32(coded, entryCount)
}

// FORMAT.md 8 rule 2
func cutSegments(demo []byte) [][]byte {
	var segments [][]byte
	framesEnd := 0
	for framesEnd < len(demo) {
		length, isFrame := frameLen(demo[framesEnd:])
		frameEnd := framesEnd + length
		isWalkable := isFrame && frameEnd <= maxRawLen
		if isWalkable {
			framesEnd = frameEnd
			isCut := framesEnd >= segmentTargetLen && framesEnd < len(demo)
			if isCut {
				segments = append(segments, demo[:framesEnd])
				demo = demo[framesEnd:]
				framesEnd = 0
			}
			continue
		}
		resumeOffset := nextFrameRun(demo, framesEnd)
		for resumeOffset > maxRawLen {
			segments = append(segments, demo[:maxRawLen])
			demo = demo[maxRawLen:]
			resumeOffset -= maxRawLen
		}
		if resumeOffset == len(demo) {
			break
		}
		segments = append(segments, demo[:resumeOffset])
		demo = demo[resumeOffset:]
		framesEnd = 0
	}
	if len(demo) > 0 {
		segments = append(segments, demo)
	}
	return segments
}

// nextFrameRun is where frames follow the unframed bytes of a segment.
func nextFrameRun(segment []byte, unframedStart int) int {
	// not before the target length: a break in the frames makes no segment short
	for offset := max(unframedStart+1, segmentTargetLen); offset < len(segment); offset++ {
		if hasFrameRun(segment[offset:]) {
			return offset
		}
	}
	return len(segment)
}

// FORMAT.md 8 rule 9
func segmentDurationMs(segment []byte) uint32 {
	duration := uint32(0)
	offset := 0
	for {
		length, isFrame := frameLen(segment[offset:])
		if !isFrame {
			return duration
		}
		duration += uint32(segment[offset])
		offset += length
	}
}

type streamEntry struct {
	id  uint32
	len uint32 // the body of a structural or numeric stream, the bytes of a blob stream
}

type transformHeader struct {
	protocol   protocolState
	entries    []streamEntry // none for a stored segment
	blobTotal  uint64
	bodyOffset int
}

// FORMAT.md 3.1
func parseTransformHeader(coded []byte, rawLen uint32) (*transformHeader, error) {
	fields := byteCursor{data: coded}
	header := new(transformHeader)
	header.protocol.fte = fields.readUint32()
	header.protocol.fte2 = fields.readUint32()
	header.protocol.mvd1 = fields.readUint32()
	entryCount := fields.readUint32()
	if fields.isShort {
		return nil, corrupt("segment header")
	}
	if entryCount > streamCount {
		return nil, corrupt("stream count")
	}

	header.entries = make([]streamEntry, entryCount)
	nextID := uint32(0)
	bodyTotal := uint64(0)
	for index := range header.entries {
		entry := &header.entries[index]
		entry.id = fields.readUint32()
		if fields.isShort {
			return nil, corrupt("stream table")
		}
		if entry.id >= streamCount {
			return nil, corrupt("stream id")
		}
		entry.len = fields.readUint32()
		if fields.isShort {
			return nil, corrupt("stream table")
		}
		if entry.id < nextID {
			return nil, corrupt("stream order")
		}
		nextID = entry.id + 1
		if entry.id >= firstBlobStream {
			header.blobTotal += uint64(entry.len)
		} else {
			bodyTotal += uint64(entry.len)
		}
	}
	if header.blobTotal > maxBlobTotal(rawLen) {
		return nil, corrupt("blob streams")
	}

	header.bodyOffset = fields.position
	restLen := uint64(len(coded) - header.bodyOffset)
	isStored := entryCount == 0
	if isStored && restLen != uint64(rawLen) {
		return nil, corrupt("stored segment")
	}
	if !isStored && restLen != bodyTotal {
		return nil, corrupt("stream bodies")
	}
	return header, nil
}

// FORMAT.md 3.3
func decodeTransform(coded []byte, header *transformHeader, rawLen uint32, models *fileModels) ([]byte, error) {
	rest := coded[header.bodyOffset:]
	isStored := len(header.entries) == 0
	if isStored {
		return rest, nil
	}

	streams := &segmentStreams{protocol: header.protocol}
	for _, entry := range header.entries {
		if entry.id >= firstBlobStream {
			continue
		}
		if models[entry.id] == nil {
			return nil, corrupt("stream without a model")
		}
		decoder, err := newStreamDecoder(models[entry.id], rest[:entry.len])
		if err != nil {
			return nil, err
		}
		streams.decoders[entry.id] = decoder
		rest = rest[entry.len:]
	}

	blobBytes, err := lzExpand(streams, int(header.blobTotal))
	if err != nil {
		return nil, err
	}
	areBlobTokensDone := streams.isStreamDone(streamLZLength) && streams.isStreamDone(streamLZDistance)
	if !areBlobTokensDone {
		return nil, corrupt("blob tokens")
	}
	for _, entry := range header.entries {
		if entry.id >= firstBlobStream {
			streams.blobs[entry.id] = blobBytes[:entry.len]
			blobBytes = blobBytes[entry.len:]
		}
	}

	output := make([]byte, rawLen)
	walker := newWalker(output, streams)
	walker.run(uint32(len(streams.blobs[streamTail])))
	if walker.err != nil {
		return nil, walker.err
	}
	for _, blob := range streams.blobs {
		if len(blob) != 0 {
			return nil, corrupt("unconsumed stream data")
		}
	}
	for stream := range streams.decoders {
		if !streams.isStreamDone(stream) {
			return nil, corrupt("unconsumed stream data")
		}
	}
	return output, nil
}
