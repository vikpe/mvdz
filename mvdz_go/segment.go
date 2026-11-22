package mvdz

// maxStreamCount is the most a stream table entry may count, FORMAT.md
// section 3.1. Most streams hold at most a symbol per byte of the segment,
// but not all: an entity word of two bytes can take three gap symbols.
func maxStreamCount(rawLen uint32) uint32 {
	return 2*rawLen + 16
}

// maxBlobTotal is the most the blob streams of a segment may hold together,
// FORMAT.md section 7.
func maxBlobTotal(rawLen uint32) uint64 {
	return 2 * uint64(rawLen)
}

// The fields of a transform header: fte, fte2, mvd1, the tail length and the
// stream count.
const transformFieldsLen = 5 * 4

// fileModels holds the model of every structural and numeric stream id; nil
// where the models block has none.
type fileModels [firstBlobStream]*streamModel

// segmentStreams is the transform header of a segment and its streams.
// Decoding reads the streams; encoding collects what the walk and the numeric
// groups code.
type segmentStreams struct {
	protocol   protocolState // at the segment start
	tail       uint32
	stored     []byte // decoding: the bytes of a stored segment, else nil
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

// decodeSegment decodes the transform of one segment to its rawLen bytes,
// FORMAT.md section 3.3.
func decodeSegment(coded []byte, rawLen uint32, models *fileModels) ([]byte, error) {
	streams, err := readStreams(coded, rawLen, models)
	if err != nil {
		return nil, err
	}
	if streams.stored != nil {
		return streams.stored, nil
	}
	output := make([]byte, rawLen)
	walker := newWalker(output, streams)
	walker.run(streams.tail)
	if walker.err != nil {
		return nil, walker.err
	}
	for _, blob := range streams.blobs {
		if len(blob) != 0 {
			return nil, corrupt("blob stream not consumed")
		}
	}
	if err := fillNumericFields(output, &walker.records, streams); err != nil {
		return nil, err
	}
	for _, decoder := range streams.decoders {
		if decoder != nil && !decoder.isComplete() {
			return nil, corrupt("stream not complete")
		}
	}
	return output, nil
}

// readStreams reads the transform header, the stream table and the stream
// bodies, FORMAT.md section 3.1.
func readStreams(coded []byte, rawLen uint32, models *fileModels) (*segmentStreams, error) {
	fields := byteCursor{data: coded}
	streams := new(segmentStreams)
	streams.protocol.fte = fields.u32()
	streams.protocol.fte2 = fields.u32()
	streams.protocol.mvd1 = fields.u32()
	streams.tail = fields.u32()
	entryCount := fields.u32()
	if fields.isShort || streams.tail > rawLen || entryCount > streamCount {
		return nil, corrupt("transform header")
	}

	type streamEntry struct {
		id, count, length uint32
	}
	entries := make([]streamEntry, entryCount)
	var isListed [streamCount]bool
	blobSize := uint64(0)
	for index := range entries {
		entry := &entries[index]
		entry.id = fields.u32()
		entry.count = fields.u32()
		isBlob := entry.id >= firstBlobStream
		if isBlob {
			blobSize += uint64(entry.count)
		} else {
			entry.length = fields.u32()
		}
		isIdValid := entry.id < streamCount && !isListed[entry.id]
		if fields.isShort || !isIdValid || entry.count > maxStreamCount(rawLen) {
			return nil, corrupt("stream table")
		}
		isListed[entry.id] = true
	}
	if blobSize > maxBlobTotal(rawLen) {
		return nil, corrupt("blob streams")
	}
	isStored := entryCount == 0
	if isStored {
		streams.stored = fields.bytes(rawLen)
		if streams.tail != rawLen || !fields.isAtEnd() {
			return nil, corrupt("stored segment")
		}
		return streams, nil
	}

	for _, entry := range entries {
		if entry.id >= firstBlobStream {
			continue
		}
		body := fields.bytes(entry.length)
		if fields.isShort {
			return nil, corrupt("stream table")
		}
		if models[entry.id] == nil {
			return nil, corrupt("stream without a model")
		}
		decoder, err := newStreamDecoder(models[entry.id], body, entry.count)
		if err != nil {
			return nil, err
		}
		streams.decoders[entry.id] = decoder
	}
	if !fields.isAtEnd() {
		return nil, corrupt("stream table")
	}

	blobBytes, err := lzExpand(streams, int(blobSize))
	if err != nil {
		return nil, err
	}
	for _, entry := range entries {
		if entry.id >= firstBlobStream {
			streams.blobs[entry.id] = blobBytes[:entry.count]
			blobBytes = blobBytes[entry.count:]
		}
	}
	return streams, nil
}

// symbol is the next symbol of a structural stream; false if it has none.
// Encoding collects sourceSymbol.
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

// residual is the next residual of a numeric stream; false if it has none.
// Encoding collects sourceResidual.
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

// blobBytes moves bytes between a blob stream and the segment: decoding
// fills segmentBytes with the next bytes of the stream, false if it has
// fewer; encoding appends segmentBytes to the stream.
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
