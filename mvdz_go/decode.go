// Package mvdz encodes and decodes MVDZ files: losslessly compressed QuakeWorld
// MVD demos.
// It follows FORMAT.md, version 3 of the format.
package mvdz

import (
	"bytes"
	"encoding/binary"
	"fmt"
	"hash/crc32"
	"io"
	"runtime"
)

// Decode decodes a whole MVDZ file to the demo it holds. A demo of more than
// 1 GiB is refused with ErrMemoryLimit: read that one with a Reader.
func Decode(file []byte) ([]byte, error) {
	const defaultMaxMemory = 1 << 30
	return decodeWithMaxMemory(file, defaultMaxMemory)
}

// Reader decodes an MVDZ file to the demo it holds.
type Reader struct {
	input       io.Reader
	header      Header
	models      *fileModels // nil until the models block is read
	rawTotal    uint64      // of the segments read
	demoCRC32   uint32
	ready       [][]byte // decoded and checked segments, in order
	deferredErr error    // met after them
	segment     []byte   // the one being delivered
	err         error
}

// NewReader reads the file header from input.
func NewReader(input io.Reader) (*Reader, error) {
	block := make([]byte, headerSize)
	count, err := io.ReadFull(input, block)
	isShort := err == io.EOF || err == io.ErrUnexpectedEOF
	if err != nil && !isShort {
		return nil, err
	}
	header, err := parseHeader(block[:count])
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

// nextSegment returns io.EOF after the last segment if the whole-demo checks pass.
func (reader *Reader) nextSegment() ([]byte, error) {
	if reader.models == nil {
		models, err := readModels(reader.input)
		if err != nil {
			return nil, err
		}
		reader.models = models
	}
	if len(reader.ready) == 0 && reader.deferredErr == nil {
		reader.decodeBatch()
	}
	if len(reader.ready) == 0 {
		return nil, reader.deferredErr
	}
	segment := reader.ready[0]
	reader.ready = reader.ready[1:]
	return segment, nil
}

// FORMAT.md 2.2
func readModels(input io.Reader) (*fileModels, error) {
	length, err := readUint32(input)
	if err != nil {
		return nil, err
	}
	if length > maxModelsLen {
		return nil, corrupt("models too large")
	}
	body := make([]byte, length)
	if err := readFull(input, body); err != nil {
		return nil, err
	}
	if length == 0 {
		return new(fileModels), nil
	}
	return readModelsBlock(body)
}

// finish makes the whole-demo checks, FORMAT.md 3.4.
func (reader *Reader) finish() error {
	var trailing [1]byte
	count, err := io.ReadFull(reader.input, trailing[:])
	if count != 0 {
		return corrupt("trailing bytes")
	}
	if err != io.EOF {
		return err
	}
	if reader.demoCRC32 != reader.header.CRC32 {
		return corrupt("checksum mismatch")
	}
	return io.EOF
}

// codedSegment is a segment as it is read, FORMAT.md 2.3.
type codedSegment struct {
	coded  []byte
	rawLen uint32
	crc32  uint32
}

// readSegment checks the segment fields in the order of FORMAT.md 7.
func (reader *Reader) readSegment() (codedSegment, error) {
	codedLen, err := readUint32(reader.input)
	if err != nil {
		return codedSegment{}, err
	}
	if codedLen > maxCodedLen {
		return codedSegment{}, corrupt("segment size")
	}
	fields := make([]byte, segmentFieldsSize+codedLen)
	if err := readFull(reader.input, fields); err != nil {
		return codedSegment{}, err
	}
	segment := codedSegment{
		coded:  fields[segmentFieldsSize:],
		rawLen: binary.LittleEndian.Uint32(fields[0:]),
		crc32:  binary.LittleEndian.Uint32(fields[8:]),
	}
	if segment.rawLen == 0 {
		return codedSegment{}, corrupt("empty segment")
	}
	if segment.rawLen > maxRawLen {
		return codedSegment{}, corrupt("segment too large")
	}
	if uint64(segment.rawLen) > reader.header.Size-reader.rawTotal {
		return codedSegment{}, corrupt("segment sizes")
	}
	reader.rawTotal += uint64(segment.rawLen)
	return segment, nil
}

// decodeBatch decodes one segment per CPU together. The segments before one
// that fails are still delivered.
func (reader *Reader) decodeBatch() {
	var batch []codedSegment
	for len(batch) < runtime.GOMAXPROCS(0) && reader.rawTotal < reader.header.Size {
		segment, err := reader.readSegment()
		if err != nil {
			reader.deferredErr = err
			break
		}
		batch = append(batch, segment)
	}
	isDemoComplete := len(batch) == 0 && reader.deferredErr == nil
	if isDemoComplete {
		reader.deferredErr = reader.finish()
		return
	}

	decoded := make([][]byte, len(batch))
	errs := make([]error, len(batch))
	inParallel(len(batch), func(index int) {
		decoded[index], errs[index] = batch[index].decode(reader.models)
	})
	for index, segment := range decoded {
		if errs[index] != nil {
			reader.deferredErr = errs[index]
			return
		}
		reader.demoCRC32 = crc32.Update(reader.demoCRC32, crc32.IEEETable, segment)
		reader.ready = append(reader.ready, segment)
	}
}

// FORMAT.md 3.3
func (segment codedSegment) decode(models *fileModels) ([]byte, error) {
	header, err := parseTransformHeader(segment.coded, segment.rawLen)
	if err != nil {
		return nil, err
	}
	decoded, err := decodeTransform(segment.coded, header, segment.rawLen, models)
	if err != nil {
		return nil, err
	}
	if crc32.ChecksumIEEE(decoded) != segment.crc32 {
		return nil, corrupt("segment checksum")
	}
	return decoded, nil
}

func decodeWithMaxMemory(file []byte, maxMemory int64) ([]byte, error) {
	reader, err := NewReader(bytes.NewReader(file))
	if err != nil {
		return nil, err
	}
	// one byte past the limit tells a demo over it from one that fits
	demo, err := io.ReadAll(io.LimitReader(reader, maxMemory+1))
	if err != nil {
		return nil, err
	}
	if int64(len(demo)) > maxMemory {
		return nil, fmt.Errorf("%w: demo size", ErrMemoryLimit)
	}
	return demo, nil
}

func readUint32(input io.Reader) (uint32, error) {
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
