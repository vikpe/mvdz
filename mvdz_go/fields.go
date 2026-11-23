package mvdz

import (
	"bytes"
	"encoding/binary"
)

// byteCursor reads little-endian fields from a block. A read past the end sets
// isShort and yields zero; isShort is checked once, after a group of reads.
type byteCursor struct {
	data     []byte
	position int
	isShort  bool
}

func (cursor *byteCursor) readBytes(count uint32) []byte {
	unread := cursor.data[cursor.position:]
	if cursor.isShort || uint64(count) > uint64(len(unread)) {
		cursor.isShort = true
		return nil
	}
	cursor.position += int(count)
	return unread[:count]
}

func (cursor *byteCursor) skip(count uint32) {
	cursor.readBytes(count)
}

func (cursor *byteCursor) readUint8() uint8 {
	field := cursor.readBytes(1)
	if cursor.isShort {
		return 0
	}
	return field[0]
}

func (cursor *byteCursor) readUint16() uint16 {
	field := cursor.readBytes(2)
	if cursor.isShort {
		return 0
	}
	return binary.LittleEndian.Uint16(field)
}

func (cursor *byteCursor) readUint32() uint32 {
	field := cursor.readBytes(4)
	if cursor.isShort {
		return 0
	}
	return binary.LittleEndian.Uint32(field)
}

// peekUint16 is 0 if fewer than 2 bytes are left.
func (cursor *byteCursor) peekUint16() uint16 {
	unread := cursor.data[cursor.position:]
	if cursor.isShort || len(unread) < 2 {
		return 0
	}
	return binary.LittleEndian.Uint16(unread)
}

// skipCString returns the length skipped, the NUL included.
func (cursor *byteCursor) skipCString() uint32 {
	if cursor.isShort {
		return 0
	}
	terminator := bytes.IndexByte(cursor.data[cursor.position:], 0)
	if terminator < 0 {
		cursor.isShort = true
		return 0
	}
	cursor.position += terminator + 1
	return uint32(terminator) + 1
}

func (cursor *byteCursor) isAtEnd() bool {
	return !cursor.isShort && cursor.position == len(cursor.data)
}
