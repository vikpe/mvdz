package mvdz

// axisTrack is the 16-bit history of one axis, FORMAT.md section 5.2.
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

// predictTimeScaled extrapolates the last velocity, scaled by the time deltas
// when they differ.
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

// deltas returns the time since the player's last record and the delta
// before that, and takes time as the last, FORMAT.md section 5.2.
func (times *frameTimes) deltas(time uint32) (now, before int32) {
	now = int32(time - times.last)
	before = int32(times.last - times.previous)
	times.previous = times.last
	times.last = time
	return now, before
}

type playerTracks struct {
	origins, angles [3]axisTrack
	times           frameTimes
}

func lastValues(tracks *[3]axisTrack) [3]uint16 {
	return [3]uint16{tracks[0].last, tracks[1].last, tracks[2].last}
}

// FORMAT.md sections 5.3 and 5.4
func (walker *segmentWalker) playerOriginsAndAngles(number uint8, flags uint16) {
	tracks := &walker.playerTracks[number]
	now, before := tracks.times.deltas(walker.time)
	hasFloatCoords := walker.protocol.hasFloatCoords()
	for axis := range 3 {
		isSent := flags&(dfOrigin<<axis) != 0
		if hasFloatCoords {
			walker.floatOrigin(streamPlayerFloatOrigin+axis, axis, isSent, &walker.playerFloatOrigins[number][axis])
			continue
		}
		track := &tracks.origins[axis]
		if !isSent {
			track.hold()
			continue
		}
		context := min(residualBucket(int32(track.residual)), 19) | min(residualBucket(track.velocity()), 19)<<5
		value, residual := walker.codeU16(streamPlayerOrigin+axis, context, track.predictTimeScaled(now, before))
		track.push(value, residual)
	}
	for axis := range 3 {
		track := &tracks.angles[axis]
		isSent := flags&(dfAngles<<axis) != 0
		if !isSent {
			track.hold()
			continue
		}
		context := min(residualBucket(int32(track.residual)), 23) | min(residualBucket(track.previousVelocity()), 23)<<5
		value, residual := walker.codeU16(streamPlayerAngle+axis, context, track.predictTimeScaled(now, before))
		track.push(value, residual)
	}
}

// FORMAT.md section 5.5
func (walker *segmentWalker) entityOrigin(number uint16, axis int, isSent, isInPrevious bool) {
	if walker.protocol.hasFloatCoords() {
		walker.floatOrigin(streamEntityFloatOrigin+axis, axis, isSent, &walker.entityFloatOrigins[number][axis])
		return
	}
	track := &walker.entityOrigins[number][axis]
	if !isSent {
		track.hold()
		return
	}
	context := min(residualBucket(int32(track.residual)), 17) | min(residualBucket(track.velocity()), 17)<<5
	if isInPrevious {
		context |= 1 << 10
	}
	value, residual := walker.codeU16(streamEntityOrigin+axis, context, track.predictLinear())
	track.push(value, residual)
}

// FORMAT.md section 5.2
func (walker *segmentWalker) floatOrigin(stream int, axis int, isSent bool, track *floatTrack) {
	if !isSent {
		track.hold()
		return
	}
	prediction := track.lastBits + (track.lastBits - track.previousBits)
	context := min(residualBucket(track.residual), 63) | uint16(axis)<<6
	sourceResidual := int32(walker.peek32(0) - prediction)
	residual := walker.residual(stream, context, sourceResidual)
	valueBits := prediction + uint32(residual)
	walker.write32(valueBits)
	track.push(valueBits, residual)
}

// FORMAT.md section 5.6
func (walker *segmentWalker) usercmdGridAngle(axis int, track *axisTrack) {
	context := min(residualBucket(int32(track.residual)), 23) |
		min(residualBucket(track.velocity()), 23)<<5 |
		min(residualBucket(track.previousVelocity()), 15)<<10 |
		uint16(axis)<<14
	prediction := track.predictLinear()
	sourceValue, _ := onGrid(walker.peek32(0))
	sourceResidual := int16(sourceValue - prediction)
	residual := int16(walker.residual(streamUsercmdAngle, context, int32(sourceResidual)))
	value := prediction + uint16(residual)
	walker.write32(gridF32(value))
	track.push(value, int32(residual))
}

// kind: 1 for a player's origin, 2 for an entity's.
func (walker *segmentWalker) soundOrigin(origin [3]uint16, kind uint16, number uint8) {
	for axis := range 3 {
		context := uint16(axis) | kind<<2 | uint16(number)<<4
		walker.codeU16(streamSoundOrigin, context, origin[axis])
	}
}

// FORMAT.md section 5.6; beamOrigin is of the player the beam starts at.
func (walker *segmentWalker) tempEntityOrigins(kind uint8, isBeam bool, beamOrigin [3]uint16) {
	const point, beamStart, beamEnd = 0, 1, 2
	kindContext := uint16(kind) << 4
	last := &walker.tempEntityLast
	if !isBeam {
		for axis := range 3 {
			context := uint16(axis) | point<<2 | kindContext
			last[axis], _ = walker.codeU16(streamTempEntityOrigin, context, last[axis])
		}
		return
	}
	var start [3]uint16
	for axis := range 3 {
		context := uint16(axis) | beamStart<<2 | kindContext
		start[axis], _ = walker.codeU16(streamTempEntityOrigin, context, beamOrigin[axis])
	}
	for axis := range 3 {
		context := uint16(axis) | beamEnd<<2 | kindContext
		last[axis], _ = walker.codeU16(streamTempEntityOrigin, context, start[axis])
	}
}

// codeU16 codes a 16-bit field, FORMAT.md section 5.1. It returns the value
// and the residual to keep as state.
func (walker *segmentWalker) codeU16(stream int, context uint16, prediction uint16) (uint16, int32) {
	sourceResidual := int16(walker.peek16() - prediction)
	residual := int16(walker.residual(stream, context, int32(sourceResidual)))
	value := prediction + uint16(residual)
	walker.write16(value)
	return value, int32(residual)
}

// residual is the next residual of a numeric stream; sourceResidual is the
// one an encoder codes.
func (walker *segmentWalker) residual(stream int, context uint16, sourceResidual int32) int32 {
	if walker.err != nil {
		return 0
	}
	residual, isCoded := walker.streams.residual(stream, context, sourceResidual)
	if !isCoded {
		walker.fail("numeric stream exhausted or absent")
	}
	return residual
}
