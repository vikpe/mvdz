# mvdz_go

MVDZ encoder and decoder in Go, standard library only. The encoder writes the same
bytes as the Rust one.

## API

```go
import mvdz "github.com/vikpe/mvdz/mvdz_go"

// whole buffers
func Encode(demo []byte) ([]byte, error)
func Decode(file []byte) ([]byte, error)

// two-pass, from a seekable input, without holding the demo
type Encoder struct {
    MaxMemory int64 // bytes, approximate; 0: no limit
}
func (encoder Encoder) Encode(output io.Writer, input io.ReadSeeker) error

// streaming: implements io.Reader
type Reader struct {
    MaxMemory int64 // bytes, approximate; 0: no limit
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

```go
file, err := mvdz.Encode(demo)
demo2, err := mvdz.Decode(file)

err = mvdz.Encoder{}.Encode(output, input)

reader, err := mvdz.NewReader(input)
fmt.Println(reader.Header().Duration) // before decoding
_, err = io.Copy(output, reader)
```

Errors are matched with `errors.Is`. Threads follow `GOMAXPROCS`. `Reader` delivers a
segment only after its CRC-32 is checked. Whole-buffer `Decode` refuses a demo of more
than 1 GiB.

## Tests

```
gofmt -l .
go vet ./...
go test ./...
```

The tests read the vectors of `../vectors`.
