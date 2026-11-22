package mvdz

import (
	"bytes"
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"testing"
	"time"
)

// The vectors the manifest says are a demo compressed plainly, and whether
// the demo passes the input check. An encoder reproduces them byte for byte.
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
	"commands.normal.mvdz":         true,
	"protocol-change.normal.mvdz":  true,
	"two-matches.normal.mvdz":      true,
	"oversized-frame.normal.mvdz":  true,
	"entity-words.normal.mvdz":     true,
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
			if !bytes.Equal(file, vector.file) {
				test.Errorf("%d bytes, not the %d of the vector", len(file), len(vector.file))
			}
		})
	}
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
// each other's files, and write the same file for the same demo.
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
			if !bytes.Equal(file, referenceFile) {
				test.Errorf("this encoder's file differs from the reference's")
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

// Any bytes round-trip through the encoder without its input check, and
// Encode either refuses them or writes the same file.
func FuzzEncode(fuzzer *testing.F) {
	for _, vector := range readVectors(fuzzer) {
		if !vector.isValid {
			continue
		}
		demo, err := Decode(vector.file)
		if err != nil {
			fuzzer.Fatal(err)
		}
		for _, prefixLen := range []int{0, 64, 1000, 20000} {
			fuzzer.Add(demo[:min(prefixLen, len(demo))])
		}
	}
	fuzzer.Fuzz(func(test *testing.T, demo []byte) {
		file, err := encodeAny(demo)
		if err != nil {
			test.Fatal(err)
		}
		decoded, err := Decode(file)
		if err != nil || !bytes.Equal(decoded, demo) {
			test.Fatalf("round trip: %v", err)
		}
		checked, err := Encode(demo)
		isSame := err == nil && bytes.Equal(checked, file)
		if !isSame && !errors.Is(err, ErrNotMvd) {
			test.Fatalf("Encode: %v", err)
		}
	})
}
