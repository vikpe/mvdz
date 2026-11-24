package mvdz

import (
	"bytes"
	"crypto/sha256"
	"fmt"
	"hash/crc32"
	"io"
	"math"
	"runtime"
	"slices"
	"time"
)

// Encode codes an MVD demo as an MVDZ file, FORMAT.md 8.
func Encode(demo []byte) ([]byte, error) {
	return Encoder{}.encodeBytes(demo)
}

// Encoder codes MVD demos as MVDZ files. The file written depends on the demo
// only, not on the settings.
type Encoder struct {
	// MaxMemory, in bytes and approximate, lowers the segments coded at once
	// to stay within it, never below one (32 MiB). 0, the default, is no
	// limit: a segment per CPU.
	MaxMemory int64

	isInputUnchecked bool
}

// Encode reads the demo from input twice and writes the MVDZ file to output,
// without holding either. It returns ErrNotMVD for input that is not an MVD
// demo, ErrDemoTooLarge past 65536 segments, and ErrInputChanged when the
// second pass reads other bytes than the first.
func (encoder Encoder) Encode(output io.Writer, input io.ReadSeeker) error {
	header, models, plans, err := encoder.firstPass(input)
	if err != nil {
		return err
	}
	modelsBody, _, err := modelsBlock(models)
	if err != nil {
		return err
	}
	modelsBlock := appendModels(nil, modelsBody)
	decoderModels, err := readModels(bytes.NewReader(modelsBlock), math.MaxInt64)
	if err != nil {
		return fmt.Errorf("%w: the encoder's models do not read back", ErrInternal)
	}
	if _, err := output.Write(append(appendHeader(nil, header), modelsBlock...)); err != nil {
		return err
	}
	if _, err := input.Seek(0, io.SeekStart); err != nil {
		return err
	}
	return encoder.secondPass(output, input, plans, models, decoderModels)
}

func (encoder Encoder) encodeBytes(demo []byte) ([]byte, error) {
	var file bytes.Buffer
	if err := encoder.Encode(&file, bytes.NewReader(demo)); err != nil {
		return nil, err
	}
	return file.Bytes(), nil
}

// encodeAny is Encode without the input check.
func encodeAny(demo []byte) ([]byte, error) {
	return Encoder{isInputUnchecked: true}.encodeBytes(demo)
}

// segmentPlan is what the second pass needs to code one segment, and to see
// that it reads the bytes the first pass read.
type segmentPlan struct {
	len           int
	crc32         uint32
	startProtocol protocolState
	isWalked      bool
}

// FORMAT.md 8 rules 1 to 4: a batch is walked in parallel from the end
// protocol of the batch before; the few segments that start in another
// protocol are walked again.
func (encoder Encoder) firstPass(input io.Reader) (Header, *fileModels, []segmentPlan, error) {
	const maxSegmentCount = 1 << 16
	segments := segmentReader{input: input}
	var builder modelBuilder
	var plans []segmentPlan
	var protocol protocolState
	var header Header
	demoSHA256 := sha256.New()
	for {
		// the first segment alone: it sets the protocol of most demos
		batchLen := 1
		if len(plans) > 0 {
			batchLen = encoder.segmentsInFlight()
		}
		var batch [][]byte
		for len(batch) < batchLen {
			segment, err := segments.nextSegment()
			if err == io.EOF {
				break
			}
			if err != nil {
				return Header{}, nil, nil, err
			}
			batch = append(batch, segment)
		}
		isFirstBatch := len(plans) == 0
		if isFirstBatch && !encoder.isInputUnchecked {
			if len(batch) == 0 || !startsLikeMVD(batch[0]) {
				return Header{}, nil, nil, ErrNotMVD
			}
		}
		if len(batch) == 0 {
			break
		}

		walks := make([]firstPassWalk, len(batch))
		batchProtocol := protocol
		inParallel(len(batch), func(index int) {
			walks[index] = newFirstPassWalk(batch[index], batchProtocol)
		})
		walked := make([]*segmentStreams, len(batch))
		for index, segment := range batch {
			if protocol != batchProtocol {
				walks[index] = newFirstPassWalk(segment, protocol)
			}
			walked[index] = walks[index].streams
			plans = append(plans, segmentPlan{
				len:           len(segment),
				crc32:         crc32.ChecksumIEEE(segment),
				startProtocol: protocol,
				isWalked:      walks[index].isWalked,
			})
			protocol = walks[index].endProtocol
			header.Size += int64(len(segment))
			header.Duration += time.Duration(segmentDurationMs(segment)) * time.Millisecond
			header.CRC32 = crc32.Update(header.CRC32, crc32.IEEETable, segment)
			demoSHA256.Write(segment)
		}
		if len(plans) > maxSegmentCount {
			return Header{}, nil, nil, ErrDemoTooLarge
		}
		builder.add(walked)
	}
	demoSHA256.Sum(header.SHA256[:0])
	models, err := builder.models()
	return header, models, plans, err
}

// firstPassWalk is the walk of one segment in the first pass.
type firstPassWalk struct {
	streams     *segmentStreams // all tail when the segment does not walk
	endProtocol protocolState
	isWalked    bool
}

func newFirstPassWalk(segment []byte, protocol protocolState) firstPassWalk {
	streams, endProtocol, isWalked := walkSegment(segment, protocol)
	if !isWalked {
		streams = walkTail(segment, protocol)
	}
	return firstPassWalk{streams, endProtocol, isWalked}
}

// secondPass writes the segments as the first pass cut them, each once it
// decodes to its input; FORMAT.md 8 rules 5, 7 and 9.
func (encoder Encoder) secondPass(output io.Writer, input io.Reader, plans []segmentPlan, models *fileModels, decoderModels *ransModels) error {
	for batch := range slices.Chunk(plans, encoder.segmentsInFlight()) {
		segments := make([][]byte, len(batch))
		for index, plan := range batch {
			segments[index] = make([]byte, plan.len)
			if _, err := io.ReadFull(input, segments[index]); err != nil {
				return err
			}
		}
		coded := make([][]byte, len(batch))
		errs := make([]error, len(batch))
		inParallel(len(batch), func(index int) {
			coded[index], errs[index] = encodeSegment(segments[index], batch[index], models, decoderModels)
		})
		for index, segment := range segments {
			if errs[index] != nil {
				return errs[index]
			}
			block := appendSegment(nil, segment, segmentDurationMs(segment), coded[index])
			if _, err := output.Write(block); err != nil {
				return err
			}
		}
	}
	return nil
}

func (encoder Encoder) segmentsInFlight() int {
	// ponytail: measured on sample demos, not a bound; count the buffers if it must hold
	const (
		baseMemory    = 20 << 20
		segmentMemory = 12 << 20
	)
	return segmentsInFlight(encoder.MaxMemory, runtime.GOMAXPROCS(0), baseMemory, segmentMemory)
}

// encodeSegment writes a segment the first way that works, FORMAT.md 8
// rule 5: transform-coded, all tail, stored; rule 7: it decodes to segment.
func encodeSegment(segment []byte, plan segmentPlan, models *fileModels, decoderModels *ransModels) ([]byte, error) {
	isInputUnchanged := crc32.ChecksumIEEE(segment) == plan.crc32
	if !isInputUnchanged {
		return nil, ErrInputChanged
	}
	rawLen := uint32(len(segment))
	var coded []byte
	isCoded := false
	if plan.isWalked {
		if walked, _, isWalked := walkSegment(segment, plan.startProtocol); isWalked {
			coded, isCoded = encodeTransform(walked, rawLen, models)
		}
	}
	if !isCoded {
		coded, isCoded = encodeTransform(walkTail(segment, plan.startProtocol), rawLen, models)
	}
	if !isCoded {
		coded = storedSegment(segment, plan.startProtocol)
	}
	if !decodesTo(coded, segment, decoderModels) {
		return nil, fmt.Errorf("%w: the encoder's output does not decode to its input", ErrInternal)
	}
	return coded, nil
}

func decodesTo(coded []byte, segment []byte, decoderModels *ransModels) bool {
	rawLen := uint32(len(segment))
	header, err := parseTransformHeader(coded, rawLen)
	if err != nil {
		return false
	}
	decoded, err := decodeTransform(coded, header, rawLen, decoderModels)
	return err == nil && bytes.Equal(decoded, segment)
}

// startsLikeMVD is the input check on the first segment, FORMAT.md 8 rule 1.
func startsLikeMVD(firstSegment []byte) bool {
	isGzip := len(firstSegment) >= 2 && firstSegment[0] == 0x1f && firstSegment[1] == 0x8b
	firstLen, isFrame := frameLen(firstSegment)
	if isGzip || !isFrame {
		return false
	}
	frameType := firstSegment[1] & 7
	frameHeaderSize := 6
	if frameType == demMultiple {
		frameHeaderSize = 10
	}
	hasPayload := frameType != demSet && firstLen > frameHeaderSize
	if !hasPayload {
		return false
	}
	streams, _, isWalked := walkSegment(firstSegment[:firstLen], protocolState{})
	if isWalked && !streams.hasEscape() {
		return true
	}
	return frameRun(firstSegment[firstLen:]) == frameYes
}
