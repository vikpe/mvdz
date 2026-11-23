package mvdz

// axisTrack is the 16-bit history of one axis, FORMAT.md 5.2.
type axisTrack struct {
	last, previous, beforePrevious uint16
	residual                       int16
}

func (track *axisTrack) velocity() int32 {
	return int32(int16(track.last - track.previous))
}

func (track *axisTrack) previousVelocity() int32 {
	return int32(int16(track.previous - track.beforePrevious))
}

func (track *axisTrack) push(value uint16, residual int32) {
	track.beforePrevious = track.previous
	track.previous = track.last
	track.last = value
	track.residual = int16(residual)
}

// hold notes a value left out of its update: it did not change.
func (track *axisTrack) hold() {
	track.push(track.last, 0)
}

func (track *axisTrack) predictLinear() uint16 {
	return track.last + uint16(track.velocity())
}

// predictTimeScaled scales the last velocity by the time deltas when they differ.
func (track *axisTrack) predictTimeScaled(now, before int32) uint16 {
	isUnscaled := now == before || before <= 0 || now < 0
	if isUnscaled {
		return track.predictLinear()
	}
	return track.last + uint16(divRound(track.velocity()*min(now, 32767), min(before, 32767)))
}

// floatTrack is the history of one float coordinate, as f32 bits.
type floatTrack struct {
	lastBits, previousBits uint32
	residual               int32
}

func (track *floatTrack) predictLinear() uint32 {
	return track.lastBits + (track.lastBits - track.previousBits)
}

func (track *floatTrack) push(valueBits uint32, residual int32) {
	track.previousBits = track.lastBits
	track.lastBits = valueBits
	track.residual = residual
}

func (track *floatTrack) hold() {
	track.push(track.lastBits, 0)
}

type frameTimes struct {
	last, previous uint32
}

// deltas takes time as the last, FORMAT.md 5.2.
func (times *frameTimes) deltas(time uint32) (now, before int32) {
	now = int32(time - times.last)
	before = int32(times.last - times.previous)
	times.previous = times.last
	times.last = time
	return now, before
}

func lastValues(tracks *[3]axisTrack) [3]uint16 {
	return [3]uint16{tracks[0].last, tracks[1].last, tracks[2].last}
}
