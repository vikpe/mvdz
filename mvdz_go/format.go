package mvdz

import (
	"crypto/sha256"
	"encoding/binary"
	"hash/crc32"
)

// Header describes the original demo.
type Header struct {
	Size       uint64
	DurationMs uint64
	CRC32      uint32
	SHA256     [sha256.Size]byte // a claim of the encoder: not checked by decoding
}

const (
	magic             = "MVDZ"
	formatVersion     = 3
	headerSize        = 60
	segmentFieldsSize = 4 + 4 + 4

	maxRawLen    = 1 << 20
	maxCodedLen  = 1 << 21
	maxModelsLen = 1 << 22
	// Of the models before they are coded, the meta model included.
	maxModelsRawLen = 1 << 22
)

// FORMAT.md 2.1
func parseHeader(block []byte) (Header, error) {
	if len(block) < len(magic) || string(block[:len(magic)]) != magic {
		return Header{}, ErrNotMVDZ
	}
	if len(block) >= 8 && binary.LittleEndian.Uint32(block[4:]) != formatVersion {
		return Header{}, ErrUnsupportedVersion
	}
	if len(block) < headerSize {
		return Header{}, corrupt("truncated header")
	}
	header := Header{
		Size:       binary.LittleEndian.Uint64(block[8:]),
		DurationMs: binary.LittleEndian.Uint64(block[16:]),
		CRC32:      binary.LittleEndian.Uint32(block[24:]),
	}
	copy(header.SHA256[:], block[28:])
	return header, nil
}

// FORMAT.md 2.1
func appendHeader(file []byte, header Header) []byte {
	file = append(file, magic...)
	file = binary.LittleEndian.AppendUint32(file, formatVersion)
	file = binary.LittleEndian.AppendUint64(file, header.Size)
	file = binary.LittleEndian.AppendUint64(file, header.DurationMs)
	file = binary.LittleEndian.AppendUint32(file, header.CRC32)
	return append(file, header.SHA256[:]...)
}

// FORMAT.md 2.2
func appendModels(file []byte, body []byte) []byte {
	file = binary.LittleEndian.AppendUint32(file, uint32(len(body)))
	return append(file, body...)
}

// FORMAT.md 2.3
func appendSegment(file []byte, raw []byte, durationMs uint32, coded []byte) []byte {
	file = binary.LittleEndian.AppendUint32(file, uint32(len(coded)))
	file = binary.LittleEndian.AppendUint32(file, uint32(len(raw)))
	file = binary.LittleEndian.AppendUint32(file, durationMs)
	file = binary.LittleEndian.AppendUint32(file, crc32.ChecksumIEEE(raw))
	return append(file, coded...)
}
