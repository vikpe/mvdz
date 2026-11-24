# mvdz_go

MVDZ encoder and decoder in Go, standard library only. The encoder writes the same
bytes as the Rust one.

## API

```go
import mvdz "github.com/vikpe/mvdz/mvdz_go"

// two-pass, from a seekable input, without holding the demo
type Encoder struct {
    MaxMemory int64 // bytes, approximate; 0: no limit
}
func (encoder Encoder) Encode(output io.Writer, input io.ReadSeeker) error

// streaming: implements io.Reader
type Reader struct {
    MaxMemory int64 // bytes, approximate; 0: 64 MiB; at least 14 MiB
}
func NewReader(input io.Reader) (*Reader, error) // reads the header
func (reader *Reader) Header() Header
func (reader *Reader) Read(buffer []byte) (int, error)

type Header struct {
    Size     int64
    Duration time.Duration
    CRC32    uint32
    SHA256   [sha256.Size]byte // a claim: not checked by decoding
}

var (
    ErrNotMVDZ, ErrUnsupportedVersion, ErrCorrupt, ErrMemoryLimit error // decoding
    ErrNotMVD, ErrDemoTooLarge, ErrInternal, ErrInputChanged      error // encoding
)
```

## Example

`hello.mvd` to `hello.mvdz` and back:

```go
package main

import (
	"io"
	"log"
	"os"

	mvdz "github.com/vikpe/mvdz/mvdz_go"
)

func main() {
	// hello.mvd -> hello.mvdz
	demo, err := os.Open("hello.mvd")
	check(err)
	file, err := os.Create("hello.mvdz")
	check(err)
	check(mvdz.Encoder{}.Encode(file, demo))
	check(file.Close())

	// hello.mvdz -> hello.mvd
	file, err = os.Open("hello.mvdz")
	check(err)
	reader, err := mvdz.NewReader(file)
	check(err)
	output, err := os.Create("hello.mvd")
	check(err)
	_, err = io.Copy(output, reader)
	check(err)
	check(output.Close())
}

func check(err error) {
	if err != nil {
		log.Fatal(err)
	}
}
```

In memory: encode from a `bytes.Reader` into a `bytes.Buffer`, and decode with
`io.ReadAll` on a `Reader` over a `bytes.Reader`. `reader.Header()` gives the size,
duration and hashes before any decoding.

Errors are matched with `errors.Is`. Threads follow `GOMAXPROCS`. `Reader` delivers a
segment only after its CRC-32 is checked.

## Tests

```
gofmt -l .
go vet ./...
go test ./...
```

The tests read the vectors of `../vectors`.
