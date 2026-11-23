package mvdz

import (
	"runtime"
	"sync"
	"sync/atomic"
)

// inParallel runs job for every index below count, on as many goroutines as
// there are CPUs.
func inParallel(count int, job func(index int)) {
	var claimed atomic.Int64
	var workers sync.WaitGroup
	for range min(count, runtime.GOMAXPROCS(0)) {
		workers.Go(func() {
			for {
				index := int(claimed.Add(1)) - 1
				if index >= count {
					return
				}
				job(index)
			}
		})
	}
	workers.Wait()
}
