package mvdz

import (
	"encoding/binary"
	"hash/crc32"
	"io"
)

// Header describes the original demo.
type Header struct {
	Size       uint64
	DurationMs uint64
	Crc32      uint32
	Sha256     [32]byte // a claim of the encoder: not checked by decoding
}

const (
	headerSize    = 60
	formatVersion = 3
	maxModelsLen  = 1 << 24
	// Of the models before they are coded, the meta model included.
	maxModelsRawLen = 1 << 24
	maxCodedLen     = 1 << 21
	maxRawLen       = 1 << 20
)

// FORMAT.md section 2.1
func readHeader(input io.Reader) (Header, error) {
	var fields [headerSize]byte
	count, err := io.ReadFull(input, fields[:])
	isTruncated := err == io.EOF || err == io.ErrUnexpectedEOF
	if err != nil && !isTruncated {
		return Header{}, err
	}
	if count < 4 || string(fields[:4]) != "MVDZ" {
		return Header{}, ErrNotMvdz
	}
	if count >= 8 && binary.LittleEndian.Uint32(fields[4:]) != formatVersion {
		return Header{}, ErrUnsupportedVersion
	}
	if isTruncated {
		return Header{}, corrupt("incomplete file")
	}
	header := Header{
		Size:       binary.LittleEndian.Uint64(fields[8:]),
		DurationMs: binary.LittleEndian.Uint64(fields[16:]),
		Crc32:      binary.LittleEndian.Uint32(fields[24:]),
	}
	copy(header.Sha256[:], fields[28:])
	return header, nil
}

// appendHeader writes the file header, FORMAT.md section 2.1.
func appendHeader(file []byte, header Header) []byte {
	file = append(file, "MVDZ"...)
	file = binary.LittleEndian.AppendUint32(file, formatVersion)
	file = binary.LittleEndian.AppendUint64(file, header.Size)
	file = binary.LittleEndian.AppendUint64(file, header.DurationMs)
	file = binary.LittleEndian.AppendUint32(file, header.Crc32)
	return append(file, header.Sha256[:]...)
}

// appendSegment writes a segment block, FORMAT.md section 2.3.
func appendSegment(file []byte, raw []byte, durationMs uint32, coded []byte) []byte {
	file = binary.LittleEndian.AppendUint32(file, uint32(len(coded)))
	file = binary.LittleEndian.AppendUint32(file, uint32(len(raw)))
	file = binary.LittleEndian.AppendUint32(file, durationMs)
	file = binary.LittleEndian.AppendUint32(file, crc32.ChecksumIEEE(raw))
	return append(file, coded...)
}
