package mvdz

import (
	"errors"
	"fmt"
)

// Error categories, FORMAT.md 7. ErrCorrupt is wrapped with the rule
// that was broken; match with errors.Is.
var (
	ErrNotMVDZ            = errors.New("mvdz: not an MVDZ file")
	ErrUnsupportedVersion = errors.New("mvdz: unsupported MVDZ version")
	ErrCorrupt            = errors.New("mvdz: corrupt")
	ErrMemoryLimit        = errors.New("mvdz: memory limit")
	ErrNotMVD             = errors.New("mvdz: not an MVD demo")
	ErrDemoTooLarge       = errors.New("mvdz: demo too large")
	ErrInternal           = errors.New("mvdz: internal error")
	ErrInputChanged       = errors.New("mvdz: the input changed between the passes")
)

func corrupt(rule string) error {
	return fmt.Errorf("%w: %s", ErrCorrupt, rule)
}
