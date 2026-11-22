package mvdz

import (
	"bytes"
	"compress/flate"
	"encoding/binary"
	"io"
)

// byteCursor reads little-endian fields from a block. A read past the end sets
// isShort and yields zero; isShort is checked once, after a group of reads.
type byteCursor struct {
	data     []byte
	position int
	isShort  bool
}

func (cursor *byteCursor) bytes(count uint32) []byte {
	unread := cursor.data[cursor.position:]
	if cursor.isShort || uint64(count) > uint64(len(unread)) {
		cursor.isShort = true
		return nil
	}
	cursor.position += int(count)
	return unread[:count]
}

func (cursor *byteCursor) skip(count uint32) {
	cursor.bytes(count)
}

func (cursor *byteCursor) u8() uint8 {
	field := cursor.bytes(1)
	if cursor.isShort {
		return 0
	}
	return field[0]
}

func (cursor *byteCursor) u16() uint16 {
	field := cursor.bytes(2)
	if cursor.isShort {
		return 0
	}
	return binary.LittleEndian.Uint16(field)
}

func (cursor *byteCursor) u32() uint32 {
	field := cursor.bytes(4)
	if cursor.isShort {
		return 0
	}
	return binary.LittleEndian.Uint32(field)
}

// peek16 is the next u16 without reading it, 0 if fewer than 2 bytes are left.
func (cursor *byteCursor) peek16() uint16 {
	unread := cursor.data[cursor.position:]
	if cursor.isShort || len(unread) < 2 {
		return 0
	}
	return binary.LittleEndian.Uint16(unread)
}

// str skips a string and its NUL, and returns the length skipped.
func (cursor *byteCursor) str() uint32 {
	if cursor.isShort {
		return 0
	}
	nul := bytes.IndexByte(cursor.data[cursor.position:], 0)
	if nul < 0 {
		cursor.isShort = true
		return 0
	}
	cursor.position += nul + 1
	return uint32(nul) + 1
}

func (cursor *byteCursor) isAtEnd() bool {
	return !cursor.isShort && cursor.position == len(cursor.data)
}

// bitReader reads the mantissa bits of a numeric stream, FORMAT.md section 6.4:
// least significant bit first, zeros past the end.
type bitReader struct {
	data     []byte
	position int
}

func (reader *bitReader) read(count int) uint32 {
	var value uint32
	for index := 0; index < count; index++ {
		byteIndex := reader.position / 8
		if byteIndex < len(reader.data) {
			bit := reader.data[byteIndex] >> (reader.position % 8) & 1
			value |= uint32(bit) << index
		}
		reader.position++
	}
	return value
}

// FORMAT.md section 6.5: the bits end within the last byte.
func (reader *bitReader) isFinished() bool {
	return (reader.position+7)/8 == len(reader.data)
}

// bitWriter writes the mantissa bits of a numeric stream, FORMAT.md Appendix
// A: least significant bit first, the last byte padded with zeros.
type bitWriter struct {
	data     []byte
	bitCount int
}

// write appends the count low bits of value.
func (writer *bitWriter) write(value uint32, count int) {
	for index := range count {
		if writer.bitCount%8 == 0 {
			writer.data = append(writer.data, 0)
		}
		bit := uint8(value>>index) & 1
		writer.data[len(writer.data)-1] |= bit << (writer.bitCount % 8)
		writer.bitCount++
	}
}

// deflate codes data as one raw DEFLATE stream.
func deflate(data []byte) []byte {
	var deflated bytes.Buffer
	writer, _ := flate.NewWriter(&deflated, flate.BestCompression) // fails for an invalid level only
	writer.Write(data)                                             // a bytes.Buffer does not fail
	writer.Close()
	return deflated.Bytes()
}

// inflateExactly decodes a raw DEFLATE stream that must take all of deflated
// and decode to size bytes, FORMAT.md sections 6.2 and 6.6.
func inflateExactly(deflated []byte, size int) ([]byte, error) {
	source := bytes.NewReader(deflated)
	inflated, err := io.ReadAll(io.LimitReader(flate.NewReader(source), int64(size)+1))
	if err != nil || len(inflated) != size || source.Len() != 0 {
		return nil, corrupt("deflate data")
	}
	return inflated, nil
}
