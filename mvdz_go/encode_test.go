package mvdz

import (
	"bytes"
	"encoding/binary"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// The vectors the manifest says are a demo compressed plainly, and whether
// the demo passes the input check.
var compressedVectors = map[string]bool{
	"duel.normal.mvdz":             true,
	"duel-truncated.normal.mvdz":   true,
	"float-mvd1.normal.mvdz":       true,
	"float-fte.normal.mvdz":        true,
	"escapes.normal.mvdz":          true,
	"demoinfo-multi.normal.mvdz":   true,
	"random.normal.mvdz":           false,
	"empty.normal.mvdz":            false,
	"usercmd.normal.mvdz":          true,
	"usercmd.normal.segments.mvdz": true,
}

func TestEncodeVectors(test *testing.T) {
	for _, vector := range readVectors(test) {
		if !vector.isValid {
			continue
		}
		test.Run(vector.name, func(test *testing.T) {
			demo, err := Decode(vector.file)
			if err != nil {
				test.Fatal(err)
			}
			file, err := encodeAny(demo)
			if err != nil {
				test.Fatal(err)
			}
			decoded, err := Decode(file)
			if err != nil || !bytes.Equal(decoded, demo) {
				test.Fatalf("round trip: %v", err)
			}

			isDemo, isCompressed := compressedVectors[vector.name]
			if !isCompressed {
				return
			}
			checked, err := Encode(demo)
			if isDemo && (err != nil || !bytes.Equal(checked, file)) {
				test.Errorf("Encode: %v", err)
			}
			if !isDemo && !errors.Is(err, ErrNotMvd) {
				test.Errorf("Encode: got %v, want %v", err, ErrNotMvd)
			}
			for _, difference := range compareNormalized(file, vector.file) {
				test.Error(difference)
			}
		})
	}
}

// filePart is a part of an MVDZ file with its DEFLATE streams inflated.
type filePart struct {
	name string
	data []byte
}

// compareNormalized lists the parts in which two MVDZ files differ once their
// DEFLATE streams are inflated: the encoders of the two may deflate
// differently.
func compareNormalized(file []byte, reference []byte) []string {
	parts, err := normalizedParts(file)
	if err != nil {
		return []string{err.Error()}
	}
	referenceParts, err := normalizedParts(reference)
	if err != nil {
		return []string{"reference: " + err.Error()}
	}
	var differences []string
	for index := 0; index < max(len(parts), len(referenceParts)); index++ {
		switch {
		case index >= len(parts):
			differences = append(differences, "missing: "+referenceParts[index].name)
		case index >= len(referenceParts):
			differences = append(differences, "not in the reference: "+parts[index].name)
		case parts[index].name != referenceParts[index].name:
			differences = append(differences, fmt.Sprintf("%s where the reference has %s", parts[index].name, referenceParts[index].name))
		case !bytes.Equal(parts[index].data, referenceParts[index].data):
			differences = append(differences, fmt.Sprintf("%s differs (%d bytes, reference %d)",
				parts[index].name, len(parts[index].data), len(referenceParts[index].data)))
		}
	}
	return differences
}

func normalizedParts(file []byte) ([]filePart, error) {
	fields := byteCursor{data: file}
	parts := []filePart{{"header", fields.bytes(headerSize)}}

	models := byteCursor{data: fields.bytes(fields.u32())}
	for range models.u32() {
		id := models.u32()
		model := byteCursor{data: models.bytes(models.u32())}
		ownCount := int(model.u32())
		inflated, err := inflateExactly(model.data[model.position:], 2*ownCount+(ownCount+1)*streamAlphabet(int(id)))
		if models.isShort || model.isShort || err != nil {
			return nil, fmt.Errorf("model %d: %v", id, err)
		}
		parts = append(parts, filePart{fmt.Sprintf("model %d with %d own contexts", id, ownCount), inflated})
	}

	for segment := 0; !fields.isAtEnd(); segment++ {
		coded := byteCursor{data: fields.bytes(fields.u32() + 12)}
		parts = append(parts, filePart{fmt.Sprintf("segment %d fields (raw length, duration, CRC-32)", segment), coded.bytes(12)})
		parts = append(parts, filePart{fmt.Sprintf("segment %d transform header", segment), coded.bytes(20)})
		entryCount := binary.LittleEndian.Uint32(coded.data[coded.position-4:])
		var streamLengths [firstBlobStream]uint32
		var table []string
		blobSize := 0
		for range entryCount {
			id, count := coded.u32(), coded.u32()
			table = append(table, fmt.Sprintf("%d:%d", id, count))
			if id < firstBlobStream {
				streamLengths[id] = coded.u32()
			} else {
				blobSize += int(count)
			}
		}
		parts = append(parts, filePart{fmt.Sprintf("segment %d stream table (id:count)", segment), []byte(strings.Join(table, " "))})
		blobLength := coded.u32()
		for stream, length := range streamLengths {
			if length > 0 {
				parts = append(parts, filePart{fmt.Sprintf("segment %d stream %d body", segment, stream), coded.bytes(length)})
			}
		}
		blobBytes, err := []byte(nil), error(nil)
		if blobSize > 0 {
			blobBytes, err = inflateExactly(coded.bytes(blobLength), blobSize)
		}
		if fields.isShort || coded.isShort || err != nil {
			return nil, fmt.Errorf("segment %d: %v", segment, err)
		}
		parts = append(parts, filePart{fmt.Sprintf("segment %d blob streams", segment), blobBytes})
	}
	return parts, nil
}

// With MVDZ_TEST_DEMOS=<directory>, every *.mvd in it round-trips.
func TestEncodeDemos(test *testing.T) {
	for _, path := range testDemos(test) {
		demo, err := os.ReadFile(path)
		if err != nil {
			test.Fatal(err)
		}
		start := time.Now()
		file, err := Encode(demo)
		encodeTime := time.Since(start)
		if err != nil {
			test.Errorf("%s: %v", filepath.Base(path), err)
			continue
		}
		start = time.Now()
		decoded, err := Decode(file)
		decodeTime := time.Since(start)
		if err != nil || !bytes.Equal(decoded, demo) {
			test.Errorf("%s: round trip: %v", filepath.Base(path), err)
		}
		test.Logf("%s: %d bytes to %d (%.2f%%), encoded in %v, decoded in %v", filepath.Base(path), len(demo), len(file),
			100*float64(len(file))/float64(len(demo)), encodeTime.Round(time.Millisecond), decodeTime.Round(time.Millisecond))
	}
}

// With MVDZ_REFERENCE_CLI=<path>, the reference tool and this package read
// each other's files, and the files are equal once normalized.
func TestEncodeAgainstReference(test *testing.T) {
	cli := os.Getenv("MVDZ_REFERENCE_CLI")
	if cli == "" {
		test.Skip("MVDZ_REFERENCE_CLI is not set")
	}
	demos := map[string][]byte{}
	for _, vector := range readVectors(test) {
		if compressedVectors[vector.name] {
			demo, err := Decode(vector.file)
			if err != nil {
				test.Fatal(err)
			}
			demos[vector.name] = demo
		}
	}
	for _, path := range testDemos(test) {
		demo, err := os.ReadFile(path)
		if err != nil {
			test.Fatal(err)
		}
		demos[filepath.Base(path)] = demo
	}

	directory := test.TempDir()
	demoPath := filepath.Join(directory, "demo.mvd")
	filePath := filepath.Join(directory, "demo.mvdz")
	decodedPath := filepath.Join(directory, "decoded.mvd")
	run := func(arguments ...string) {
		if output, err := exec.Command(cli, arguments...).CombinedOutput(); err != nil {
			test.Fatalf("%s %v: %v: %s", cli, arguments, err, output)
		}
	}
	for name, demo := range demos {
		test.Run(name, func(test *testing.T) {
			file, err := Encode(demo)
			if err != nil {
				test.Fatal(err)
			}
			if err := os.WriteFile(filePath, file, 0o644); err != nil {
				test.Fatal(err)
			}
			run("decompress", "-f", filePath, decodedPath)
			decoded, err := os.ReadFile(decodedPath)
			if err != nil || !bytes.Equal(decoded, demo) {
				test.Errorf("the reference does not decode this encoder's file to the demo: %v", err)
			}

			if err := os.WriteFile(demoPath, demo, 0o644); err != nil {
				test.Fatal(err)
			}
			run("compress", "-f", demoPath, filePath)
			referenceFile, err := os.ReadFile(filePath)
			if err != nil {
				test.Fatal(err)
			}
			decoded, err = Decode(referenceFile)
			if err != nil || !bytes.Equal(decoded, demo) {
				test.Errorf("the reference's file does not decode to the demo: %v", err)
			}
			for _, difference := range compareNormalized(file, referenceFile) {
				test.Error(difference)
			}
			test.Logf("%d bytes: this encoder %d, the reference %d", len(demo), len(file), len(referenceFile))
		})
	}
}

func testDemos(test *testing.T) []string {
	directory := os.Getenv("MVDZ_TEST_DEMOS")
	if directory == "" {
		if test.Name() == "TestEncodeDemos" {
			test.Skip("MVDZ_TEST_DEMOS is not set")
		}
		return nil
	}
	paths, err := filepath.Glob(filepath.Join(directory, "*.mvd"))
	if err != nil || len(paths) == 0 {
		test.Fatalf("no demos in %s: %v", directory, err)
	}
	return paths
}
