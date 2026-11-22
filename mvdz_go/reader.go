// Package mvdz decodes MVDZ files: losslessly compressed QuakeWorld MVD demos.
// It follows FORMAT.md, version 2 of the format.
package mvdz

import (
	"bytes"
	"encoding/binary"
	"errors"
	"fmt"
	"hash/crc32"
	"io"
)

// Error categories, FORMAT.md section 7. ErrCorrupt is wrapped with the rule
// that was broken; match with errors.Is.
var (
	ErrNotMvdz            = errors.New("mvdz: not an MVDZ file")
	ErrUnsupportedVersion = errors.New("mvdz: unsupported version")
	ErrCorrupt            = errors.New("mvdz: corrupt")
	ErrMemoryLimit        = errors.New("mvdz: memory limit")
)

// Header describes the original demo.
type Header struct {
	Size       uint64
	DurationMs uint64
	Crc32      uint32
	Sha256     [32]byte // a claim of the encoder: not checked by decoding
}

// Reader decodes an MVDZ file to the demo it holds.
type Reader struct {
	input       io.Reader
	header      Header
	models      *fileModels // nil until the models block is read
	decodedSize uint64
	demoCrc32   uint32
	segment     []byte // decoded and checked, not yet delivered
	err         error
}

// NewReader reads the file header from input.
func NewReader(input io.Reader) (*Reader, error) {
	header, err := readHeader(input)
	if err != nil {
		return nil, err
	}
	return &Reader{input: input, header: header}, nil
}

func (reader *Reader) Header() Header {
	return reader.header
}

// Read delivers the demo in order. It delivers a segment only after the
// segment passed its checks, and io.EOF only after the whole-demo checks.
func (reader *Reader) Read(buffer []byte) (int, error) {
	for len(reader.segment) == 0 {
		if reader.err != nil {
			return 0, reader.err
		}
		reader.segment, reader.err = reader.nextSegment()
	}
	count := copy(buffer, reader.segment)
	reader.segment = reader.segment[count:]
	return count, nil
}

// Decode decodes a whole MVDZ file to the demo it holds.
func Decode(file []byte) ([]byte, error) {
	reader, err := NewReader(bytes.NewReader(file))
	if err != nil {
		return nil, err
	}
	demo, err := io.ReadAll(reader)
	if err != nil {
		return nil, err
	}
	return demo, nil
}

const (
	headerSize      = 60
	formatVersion   = 2
	maxModelsLength = 1 << 24
	maxCodedLength  = 1 << 21
	maxRawLength    = 1 << 20

	// The format lets the models of a file inflate to about 350 MB in all.
	// This reader refuses more than 64 MiB with ErrMemoryLimit.
	maxModelsSize = 64 << 20
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

// nextSegment returns the next segment of the demo, or io.EOF after the last
// one if the whole-demo checks pass.
func (reader *Reader) nextSegment() ([]byte, error) {
	if reader.models == nil {
		models, err := readModels(reader.input)
		if err != nil {
			return nil, err
		}
		reader.models = models
	}
	if reader.decodedSize == reader.header.Size {
		return nil, reader.finish()
	}

	// FORMAT.md section 2.3; the checks are in the order of section 7.
	codedLength, err := readU32(reader.input)
	if err != nil {
		return nil, err
	}
	if codedLength > maxCodedLength {
		return nil, corrupt("segment size")
	}
	fields := make([]byte, 12+codedLength)
	if err := readFull(reader.input, fields); err != nil {
		return nil, err
	}
	rawLength := binary.LittleEndian.Uint32(fields[0:])
	checksum := binary.LittleEndian.Uint32(fields[8:])
	if rawLength == 0 {
		return nil, corrupt("empty segment")
	}
	if rawLength > maxRawLength {
		return nil, corrupt("segment size")
	}
	if uint64(rawLength) > reader.header.Size-reader.decodedSize {
		return nil, corrupt("segment sizes")
	}

	decoded, err := decodeSegment(fields[12:], rawLength, reader.models)
	if err != nil {
		return nil, err
	}
	if crc32.ChecksumIEEE(decoded) != checksum {
		return nil, corrupt("segment checksum")
	}
	reader.demoCrc32 = crc32.Update(reader.demoCrc32, crc32.IEEETable, decoded)
	reader.decodedSize += uint64(rawLength)
	return decoded, nil
}

// finish makes the whole-demo checks, FORMAT.md section 3.4.
func (reader *Reader) finish() error {
	var trailing [1]byte
	count, err := io.ReadFull(reader.input, trailing[:])
	if count != 0 {
		return corrupt("trailing bytes")
	}
	if err != io.EOF {
		return err
	}
	if reader.demoCrc32 != reader.header.Crc32 {
		return corrupt("checksum mismatch")
	}
	return io.EOF
}

// FORMAT.md section 2.2
func readModels(input io.Reader) (*fileModels, error) {
	length, err := readU32(input)
	if err != nil {
		return nil, err
	}
	if length > maxModelsLength {
		return nil, corrupt("models too large")
	}
	body := make([]byte, length)
	if err := readFull(input, body); err != nil {
		return nil, err
	}
	models := new(fileModels)
	if length == 0 {
		return models, nil
	}

	fields := byteCursor{data: body}
	entryCount := fields.u32()
	if entryCount == 0 {
		return nil, corrupt("tables")
	}
	nextId := uint32(0)
	modelsSize := 0
	for range entryCount {
		id := fields.u32()
		entry := fields.bytes(fields.u32())
		if fields.isShort || id < nextId || id >= firstBlobStream {
			return nil, corrupt("tables")
		}
		nextId = id + 1
		alphabet := structuralAlphabet
		if id >= firstNumericStream {
			alphabet = numericAlphabet
		}
		models[id], err = readModel(entry, alphabet, &modelsSize)
		if err != nil {
			return nil, err
		}
	}
	if !fields.isAtEnd() {
		return nil, corrupt("tables")
	}
	return models, nil
}

func readU32(input io.Reader) (uint32, error) {
	var field [4]byte
	if err := readFull(input, field[:]); err != nil {
		return 0, err
	}
	return binary.LittleEndian.Uint32(field[:]), nil
}

func readFull(input io.Reader, block []byte) error {
	_, err := io.ReadFull(input, block)
	if err == io.EOF || err == io.ErrUnexpectedEOF {
		return corrupt("incomplete file")
	}
	return err
}

func corrupt(rule string) error {
	return fmt.Errorf("%w: %s", ErrCorrupt, rule)
}
