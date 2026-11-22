package mvdz

import (
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"io"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
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
			decoded, decodeErr := Decode(vector.file)
			streamed, streamErr := decodeInSmallReads(vector.file)
			if !vector.isValid {
				expected := errorCategory(vector.errorMessage)
				if !errors.Is(decodeErr, expected) {
					test.Errorf("Decode: got %v, want %v (%s)", decodeErr, expected, vector.errorMessage)
				}
				if !errors.Is(streamErr, expected) {
					test.Errorf("Reader: got %v, want %v (%s)", streamErr, expected, vector.errorMessage)
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

func FuzzDecode(fuzzer *testing.F) {
	for _, vector := range readVectors(fuzzer) {
		fuzzer.Add(vector.file)
	}
	fuzzer.Fuzz(func(test *testing.T, file []byte) {
		decoded, decodeErr := Decode(file)
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
	const expectedSha256 = "987a33fe15b8a28516d14d408bb65e93c2560929d6324187be4a13aeadd2cdcf"
	if header.Size != 8435 || hex.EncodeToString(header.Sha256[:]) != expectedSha256 {
		test.Errorf("got size %d and SHA-256 %x", header.Size, header.Sha256)
	}
}

func TestNotMvdz(test *testing.T) {
	for _, file := range []string{"", "MVD", "MVDX\x02\x00\x00\x00", "\x1f\x8b\x08\x00"} {
		if _, err := Decode([]byte(file)); !errors.Is(err, ErrNotMvdz) {
			test.Errorf("%q: got %v, want %v", file, err, ErrNotMvdz)
		}
	}
}

func readVectors(test testing.TB) []testVector {
	manifest, err := os.ReadFile(filepath.Join(vectorsDirectory, "MANIFEST.tsv"))
	if err != nil {
		test.Fatal(err)
	}
	var vectors []testVector
	for _, line := range strings.Split(strings.TrimSpace(string(manifest)), "\n") {
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
		return ErrNotMvdz
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
