package mvdz

import (
	"encoding/hex"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// vectors/lz-tokens.tsv: inputs with the tokens of the canonical parse.
func TestLZTokens(test *testing.T) {
	file, err := os.ReadFile(filepath.Join(vectorsDirectory, "lz-tokens.tsv"))
	if err != nil {
		test.Fatal(err)
	}
	inputCount := 0
	for line := range strings.SplitSeq(strings.TrimSpace(string(file)), "\n") {
		if strings.HasPrefix(line, "#") {
			continue
		}
		inputHex, expected, _ := strings.Cut(line, "\t")
		input, err := hex.DecodeString(inputHex)
		if err != nil {
			test.Fatal(err)
		}
		var tokens []string
		for _, token := range lzParse(input) {
			if token.length == 0 {
				tokens = append(tokens, fmt.Sprintf("L%02x", token.literal))
			} else {
				tokens = append(tokens, fmt.Sprintf("M%d,%d", token.length, token.distance))
			}
		}
		if parsed := strings.Join(tokens, " "); parsed != expected {
			test.Errorf("%s: got %s, want %s", inputHex, parsed, expected)
		}
		inputCount++
	}
	if inputCount == 0 {
		test.Error("no input in the file")
	}
}
