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
	ErrNotMVD             = errors.New("mvdz: not an MVD demo")
	ErrDemoTooLarge       = errors.New("mvdz: demo too large")
	ErrInternal           = errors.New("mvdz: internal error")
)

func corrupt(rule string) error {
	return fmt.Errorf("%w: %s", ErrCorrupt, rule)
}
