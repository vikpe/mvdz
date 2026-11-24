package mvdz

import (
	"bytes"
	"crypto/sha256"
	"encoding/binary"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"math"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
	"time"
)

const vectorsDirectory = "../vectors"

type testVector struct {
	name         string
	file         []byte
	isValid      bool
	sha256       string
	size         int
	errorMessage string
}

func TestVectors(test *testing.T) {
	for _, vector := range readVectors(test) {
		test.Run(vector.name, func(test *testing.T) {
			decoded, decodeErr := decode(vector.file)
			streamed, streamErr := decodeInSmallReads(vector.file)
			if !vector.isValid {
				expected := "mvdz: " + vector.errorMessage
				category := errorCategory(vector.errorMessage)
				for decoder, err := range map[string]error{"Decode": decodeErr, "Reader": streamErr} {
					if err == nil || err.Error() != expected || !errors.Is(err, category) {
						test.Errorf("%s: got %v, want %s", decoder, err, expected)
					}
				}
				return
			}
			if decodeErr != nil {
				test.Fatalf("Decode: %v", decodeErr)
			}
			if streamErr != nil {
				test.Fatalf("Reader: %v", streamErr)
			}
			for decoder, demo := range map[string][]byte{"Decode": decoded, "Reader": streamed} {
				digest := sha256.Sum256(demo)
				if len(demo) != vector.size || hex.EncodeToString(digest[:]) != vector.sha256 {
					test.Errorf("%s: got %d bytes with SHA-256 %x, want %d bytes with %s",
						decoder, len(demo), digest, vector.size, vector.sha256)
				}
			}
		})
	}
}

// A failing file delivers the segments that passed their checks, then the
// same error on every Read.
func TestReaderErrorIsSticky(test *testing.T) {
	file, err := os.ReadFile(filepath.Join(vectorsDirectory, "hostile-segment-crc.mvdz"))
	if err != nil {
		test.Fatal(err)
	}
	reader, err := NewReader(bytes.NewReader(file))
	if err != nil {
		test.Fatal(err)
	}
	prefix, firstErr := io.ReadAll(reader)
	_, secondErr := reader.Read(make([]byte, 1))
	if len(prefix) == 0 || !errors.Is(firstErr, ErrCorrupt) || secondErr != firstErr {
		test.Errorf("got %d bytes, then %v, then %v", len(prefix), firstErr, secondErr)
	}
}

// Collecting the whole demo stops at the limit.
func TestDecodeMemoryLimit(test *testing.T) {
	file, err := os.ReadFile(filepath.Join(vectorsDirectory, "float-mvd1.normal.mvdz"))
	if err != nil {
		test.Fatal(err)
	}
	demo, err := decode(file)
	if err != nil {
		test.Fatal(err)
	}
	size := int64(len(demo))
	if _, err := decodeWithMaxMemory(file, size); err != nil {
		test.Errorf("at the limit: %v", err)
	}
	_, err = decodeWithMaxMemory(file, size-1)
	if !errors.Is(err, ErrMemoryLimit) || err.Error() != "mvdz: memory limit: demo size" {
		test.Errorf("a byte over the limit: got %v", err)
	}
}

func TestSegmentsInFlight(test *testing.T) {
	const (
		baseMemory    = 12 << 20
		segmentMemory = 2 << 20
		threads       = 4
	)
	for maxMemory, expected := range map[int64]int{
		0:                              threads,
		1:                              1,
		baseMemory + segmentMemory:     1,
		baseMemory + 3*segmentMemory:   3,
		baseMemory + 100*segmentMemory: threads,
	} {
		if count := segmentsInFlight(maxMemory, threads, baseMemory, segmentMemory); count != expected {
			test.Errorf("%d bytes: got %d, want %d", maxMemory, count, expected)
		}
	}
}

// A segment at a time gives the same demo.
func TestReaderMaxMemory(test *testing.T) {
	file, err := os.ReadFile(filepath.Join(vectorsDirectory, "duel.normal.mvdz"))
	if err != nil {
		test.Fatal(err)
	}
	demo, err := decode(file)
	if err != nil {
		test.Fatal(err)
	}
	reader, err := NewReader(bytes.NewReader(file))
	if err != nil {
		test.Fatal(err)
	}
	reader.MaxMemory = 1
	limited, err := io.ReadAll(reader)
	if err != nil || !bytes.Equal(limited, demo) {
		test.Errorf("got %d bytes and %v, want %d bytes", len(limited), err, len(demo))
	}
}

func FuzzDecode(fuzzer *testing.F) {
	for _, vector := range readVectors(fuzzer) {
		fuzzer.Add(vector.file)
	}
	fuzzer.Fuzz(func(test *testing.T, file []byte) {
		decoded, decodeErr := decode(file)
		streamed, streamErr := decodeInSmallReads(file)
		if (decodeErr == nil) != (streamErr == nil) {
			test.Fatalf("Decode: %v, Reader: %v", decodeErr, streamErr)
		}
		if decodeErr == nil && !bytes.Equal(decoded, streamed) {
			test.Fatal("Decode and Reader give different demos")
		}
	})
}

func TestHeader(test *testing.T) {
	file, err := os.ReadFile(filepath.Join(vectorsDirectory, "float-mvd1.normal.mvdz"))
	if err != nil {
		test.Fatal(err)
	}
	reader, err := NewReader(bytes.NewReader(file))
	if err != nil {
		test.Fatal(err)
	}
	header := reader.Header()
	const expectedSHA256 = "987a33fe15b8a28516d14d408bb65e93c2560929d6324187be4a13aeadd2cdcf"
	if header.Size != 8435 || hex.EncodeToString(header.SHA256[:]) != expectedSHA256 {
		test.Errorf("got size %d and SHA-256 %x", header.Size, header.SHA256)
	}
}

// The header's milliseconds as a Duration; more than a Duration holds is the largest.
func TestHeaderDuration(test *testing.T) {
	cases := map[uint64]time.Duration{
		0:              0,
		1500:           1500 * time.Millisecond,
		math.MaxUint64: math.MaxInt64,
	}
	for durationMs, expected := range cases {
		block := appendHeader(nil, Header{})
		binary.LittleEndian.PutUint64(block[16:], durationMs)
		header, err := parseHeader(block)
		if err != nil || header.Duration != expected {
			test.Errorf("%d ms: got %v and %v, want %v", durationMs, header.Duration, err, expected)
		}
	}
}

func TestNotMVDZ(test *testing.T) {
	for _, file := range []string{"", "MVD", "MVDX\x02\x00\x00\x00", "\x1f\x8b\x08\x00"} {
		if _, err := decode([]byte(file)); !errors.Is(err, ErrNotMVDZ) {
			test.Errorf("%q: got %v, want %v", file, err, ErrNotMVDZ)
		}
	}
}

// decode is a whole file to the demo it holds, of at most the default limit.
func decode(file []byte) ([]byte, error) {
	return decodeWithMaxMemory(file, defaultMaxMemory)
}

// encode is a whole demo to the file that holds it.
func encode(demo []byte) ([]byte, error) {
	return Encoder{}.encodeBytes(demo)
}

func (encoder Encoder) encodeBytes(demo []byte) ([]byte, error) {
	var file bytes.Buffer
	if err := encoder.Encode(&file, bytes.NewReader(demo)); err != nil {
		return nil, err
	}
	return file.Bytes(), nil
}

// encodeAny skips the input check.
func encodeAny(demo []byte) ([]byte, error) {
	return Encoder{isInputUnchecked: true}.encodeBytes(demo)
}

func decodeWithMaxMemory(file []byte, maxMemory int64) ([]byte, error) {
	reader, err := NewReader(bytes.NewReader(file))
	if err != nil {
		return nil, err
	}
	reader.MaxMemory = maxMemory
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

func readVectors(test testing.TB) []testVector {
	test.Helper()
	manifest, err := os.ReadFile(filepath.Join(vectorsDirectory, "MANIFEST.tsv"))
	if err != nil {
		test.Fatal(err)
	}
	var vectors []testVector
	for line := range strings.SplitSeq(strings.TrimSpace(string(manifest)), "\n") {
		if strings.HasPrefix(line, "#") {
			continue
		}
		columns := strings.Split(line, "\t")
		if len(columns) < 3 {
			test.Fatalf("manifest line %q", line)
		}
		file, err := os.ReadFile(filepath.Join(vectorsDirectory, columns[0]))
		if err != nil {
			test.Fatal(err)
		}
		errorMessage, isHostile := strings.CutPrefix(columns[1], "error: ")
		vector := testVector{name: columns[0], file: file, isValid: !isHostile, errorMessage: errorMessage}
		if vector.isValid {
			vector.sha256 = columns[1]
			vector.size, err = strconv.Atoi(columns[2])
			if err != nil {
				test.Fatal(err)
			}
		}
		vectors = append(vectors, vector)
	}
	return vectors
}

func errorCategory(manifestMessage string) error {
	switch {
	case strings.HasPrefix(manifestMessage, "corrupt"):
		return ErrCorrupt
	case strings.HasPrefix(manifestMessage, "unsupported MVDZ version"):
		return ErrUnsupportedVersion
	case strings.HasPrefix(manifestMessage, "not an MVDZ"):
		return ErrNotMVDZ
	}
	return nil
}

func decodeInSmallReads(file []byte) ([]byte, error) {
	reader, err := NewReader(&smallReads{file})
	if err != nil {
		return nil, err
	}
	return io.ReadAll(reader)
}

// smallReads delivers a file at most 5 bytes per Read.
type smallReads struct {
	unread []byte
}

func (reads *smallReads) Read(buffer []byte) (int, error) {
	if len(reads.unread) == 0 {
		return 0, io.EOF
	}
	count := copy(buffer[:min(len(buffer), 5)], reads.unread)
	reads.unread = reads.unread[count:]
	return count, nil
}
