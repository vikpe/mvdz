package mvdz

import (
	"encoding/binary"
	"math"
	"math/bits"
)

// fillNumericFields runs the numeric groups, FORMAT.md section 5: they write
// the fields the walk reserved. Sounds and temp entities predict from origins,
// so they run last.
//
// Both directions run the same steps: an encoder proposes each residual from
// the value the segment holds, and the value written back is that value.
func fillNumericFields(segment []byte, records *recordLists, streams *segmentStreams) error {
	groups := numericGroups{segment: segment, streams: streams}
	for axis := range 3 {
		groups.playerOrigins(records.playerInfos, axis)
		groups.playerAngles(records.playerInfos, axis)
		groups.entityOrigins(records.packetEntities, axis)
	}
	groups.usercmdAngles(records.usercmds)
	groups.soundOrigins(records.sounds)
	groups.tempEntityOrigins(records.tempEntities)
	return groups.err
}

type numericGroups struct {
	segment []byte
	streams *segmentStreams
	err     error
}

// axisTrack is the history of one coordinate or angle, FORMAT.md section 5.2.
type axisTrack struct {
	last, previous, beforePrevious   uint16
	residual                         int32
	floatLastBits, floatPreviousBits uint32
	floatResidualBits                int32
}

type frameTimes struct {
	last, previous uint32
}

// FORMAT.md section 5.3
func (groups *numericGroups) playerOrigins(records []playerInfoRecord, axis int) {
	var tracks [256]axisTrack
	var times [256]frameTimes
	for _, record := range records {
		now, before := times[record.number].deltas(record.time)
		track := &tracks[record.number]
		isSent := record.flags&(dfOrigin<<axis) != 0
		earlierOrigins := uint32(bits.OnesCount16(record.flags & (1<<axis - 1)))
		switch {
		case record.hasFloatCoords && isSent:
			groups.floatOrigin(streamPlayerFloatOrigin+axis, record.offset+4*earlierOrigins, axis, track)
		case record.hasFloatCoords:
			track.floatPreviousBits = track.floatLastBits
			track.floatResidualBits = 0
		case isSent:
			velocity := int32(int16(track.last - track.previous))
			context := min(residualBucket(track.residual), 19) | min(residualBucket(velocity), 19)<<5
			prediction := predictTimeScaled(track.last, track.previous, now, before)
			value, residual := groups.field16(streamPlayerOrigin+axis, context, record.offset+2*earlierOrigins, prediction)
			track.residual = residual
			track.previous = track.last
			track.last = value
		default:
			track.residual = 0
			track.previous = track.last
		}
	}
}

// FORMAT.md section 5.4
func (groups *numericGroups) playerAngles(records []playerInfoRecord, axis int) {
	var tracks [256]axisTrack
	var times [256]frameTimes
	for _, record := range records {
		now, before := times[record.number].deltas(record.time)
		track := &tracks[record.number]
		value, residual := track.last, int32(0)
		isSent := record.flags&(dfAngles<<axis) != 0
		if isSent {
			originSize := uint32(2)
			if record.hasFloatCoords {
				originSize = 4
			}
			originCount := uint32(bits.OnesCount16(record.flags & 0x7))
			earlierAngles := uint32(bits.OnesCount16((record.flags >> 3) & (1<<axis - 1)))
			offset := record.offset + originSize*originCount + 2*earlierAngles
			previousVelocity := int32(int16(track.previous - track.beforePrevious))
			context := min(residualBucket(track.residual), 23) | min(residualBucket(previousVelocity), 23)<<5
			prediction := predictTimeScaled(track.last, track.previous, now, before)
			value, residual = groups.field16(streamPlayerAngle+axis, context, offset, prediction)
		}
		track.residual = residual
		track.beforePrevious = track.previous
		track.previous = track.last
		track.last = value
	}
}

// FORMAT.md section 5.5
func (groups *numericGroups) entityOrigins(records []packetEntityRecord, axis int) {
	var tracks [2048]axisTrack
	for _, record := range records {
		track := &tracks[record.number&2047]
		offset := record.originOffsets[axis]
		switch {
		case offset == noOffset:
			track.previous = track.last
			track.residual = 0
			track.floatPreviousBits = track.floatLastBits
			track.floatResidualBits = 0
		case record.hasFloatCoords:
			groups.floatOrigin(streamEntityFloatOrigin+axis, offset, axis, track)
		default:
			velocity := int32(int16(track.last - track.previous))
			context := min(residualBucket(track.residual), 17) | min(residualBucket(velocity), 17)<<5
			if record.isInPrevious {
				context |= 1 << 10
			}
			value, residual := groups.field16(streamEntityOrigin+axis, context, offset, track.last+uint16(velocity))
			track.residual = residual
			track.previous = track.last
			track.last = value
		}
	}
}

// floatOrigin decodes a float coordinate as its bit pattern.
func (groups *numericGroups) floatOrigin(stream int, offset uint32, axis int, track *axisTrack) {
	prediction := track.floatLastBits + (track.floatLastBits - track.floatPreviousBits)
	context := min(residualBucket(track.floatResidualBits), 63) | uint16(axis)<<6
	sourceResidual := int32(groups.read32(offset) - prediction)
	residual := groups.residual(stream, context, sourceResidual)
	value := prediction + uint32(residual)
	groups.write32(offset, value)
	track.floatPreviousBits = track.floatLastBits
	track.floatLastBits = value
	track.floatResidualBits = residual
}

// FORMAT.md section 5.6
func (groups *numericGroups) usercmdAngles(records []usercmdRecord) {
	var tracks [256][3]axisTrack
	for _, record := range records {
		for axis := range 3 {
			isOnGrid := record.gridAxes&(1<<axis) != 0
			if !isOnGrid {
				continue
			}
			track := &tracks[record.number][axis]
			velocity := int32(int16(track.last - track.previous))
			previousVelocity := int32(int16(track.previous - track.beforePrevious))
			context := min(residualBucket(track.residual), 23) |
				min(residualBucket(velocity), 23)<<5 |
				min(residualBucket(previousVelocity), 15)<<10 |
				uint16(axis)<<14
			offset := record.pitchOffset + 4*uint32(axis)
			prediction := track.last + uint16(velocity)
			sourceValue, _ := gridValue(groups.read32(offset))
			sourceResidual := int16(sourceValue - prediction)
			residual := int16(groups.residual(streamUsercmdAngle, context, int32(sourceResidual)))
			value := prediction + uint16(residual)
			groups.write32(offset, gridAngle(value))
			track.residual = int32(residual)
			track.beforePrevious = track.previous
			track.previous = track.last
			track.last = value
		}
	}
}

func (groups *numericGroups) soundOrigins(records []soundRecord) {
	for _, record := range records {
		for axis := range uint32(3) {
			context := uint16(axis) | uint16(record.kind)<<2 | uint16(record.number)<<4
			prediction := groups.referenced(record.references[axis])
			groups.field16(streamSoundOrigin, context, record.offset+2*axis, prediction)
		}
	}
}

func (groups *numericGroups) tempEntityOrigins(records []tempEntityRecord) {
	const point, beamStart, beamEnd = 0, 1, 2
	var last [3]uint16
	for _, record := range records {
		kindContext := uint16(record.kind) << 4
		isBeam := record.kind == teLightning1 || record.kind == teLightning2 || record.kind == teLightning3
		if !isBeam {
			for axis := range uint32(3) {
				context := uint16(axis) | point<<2 | kindContext
				last[axis], _ = groups.field16(streamTempEntityOrigin, context, record.offset+2*axis, last[axis])
			}
			continue
		}
		var start [3]uint16
		for axis := range uint32(3) {
			context := uint16(axis) | beamStart<<2 | kindContext
			prediction := groups.referenced(record.references[axis])
			start[axis], _ = groups.field16(streamTempEntityOrigin, context, record.offset+2*axis, prediction)
		}
		for axis := range uint32(3) {
			context := uint16(axis) | beamEnd<<2 | kindContext
			last[axis], _ = groups.field16(streamTempEntityOrigin, context, record.offset+6+2*axis, start[axis])
		}
	}
}

// referenced is the origin coordinate a sound or beam is predicted from.
func (groups *numericGroups) referenced(offset uint32) uint16 {
	if offset == noOffset {
		return 0
	}
	return groups.read16(offset)
}

// field16 codes a 16-bit field, FORMAT.md section 5.1. It returns the value
// and the residual to keep as state.
func (groups *numericGroups) field16(stream int, context uint16, offset uint32, prediction uint16) (uint16, int32) {
	sourceResidual := int16(groups.read16(offset) - prediction)
	residual := int16(groups.residual(stream, context, int32(sourceResidual)))
	value := prediction + uint16(residual)
	if uint64(offset)+2 <= uint64(len(groups.segment)) {
		binary.LittleEndian.PutUint16(groups.segment[offset:], value)
	}
	return value, int32(residual)
}

func (groups *numericGroups) read16(offset uint32) uint16 {
	if uint64(offset)+2 > uint64(len(groups.segment)) {
		return 0
	}
	return binary.LittleEndian.Uint16(groups.segment[offset:])
}

func (groups *numericGroups) read32(offset uint32) uint32 {
	if uint64(offset)+4 > uint64(len(groups.segment)) {
		return 0
	}
	return binary.LittleEndian.Uint32(groups.segment[offset:])
}

func (groups *numericGroups) write32(offset uint32, value uint32) {
	if uint64(offset)+4 <= uint64(len(groups.segment)) {
		binary.LittleEndian.PutUint32(groups.segment[offset:], value)
	}
}

// residual is the next residual of a numeric stream; sourceResidual is the
// one an encoder codes.
func (groups *numericGroups) residual(stream int, context uint16, sourceResidual int32) int32 {
	if groups.err != nil {
		return 0
	}
	residual, isCoded := groups.streams.residual(stream, context, sourceResidual)
	if !isCoded {
		groups.err = corrupt("numeric stream exhausted or absent")
	}
	return residual
}

// gridAngle is the float bits of a usercmd angle on the 360/65536 grid,
// FORMAT.md section 5.6.
func gridAngle(value uint16) uint32 {
	return math.Float32bits(float32(int16(value)) * 45 / 8192)
}

// gridValue is the grid value of a usercmd angle, FORMAT.md section 4.11;
// false if the angle is not on the grid.
func gridValue(angleBits uint32) (uint16, bool) {
	scaled := float64(math.Float32frombits(angleBits)) * 8192 / 45
	isInRange := scaled >= -32768 && scaled <= 32767
	if !isInRange {
		return 0, false
	}
	value := uint16(int16(math.Round(scaled)))
	return value, gridAngle(value) == angleBits
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

// predictTimeScaled extrapolates the last velocity, scaled by the time deltas
// when they differ.
func predictTimeScaled(last, previous uint16, now, before int32) uint16 {
	velocity := int32(int16(last - previous))
	isUnscaled := now == before || before <= 0 || now < 0
	if isUnscaled {
		return last + uint16(velocity)
	}
	return last + uint16(divRound(velocity*min(now, 32767), min(before, 32767)))
}

// divRound divides by a positive divisor; halves round away from zero.
func divRound(dividend, divisor int32) int32 {
	magnitude := uint32(dividend)
	if dividend < 0 {
		magnitude = -magnitude
	}
	quotient := int32((magnitude + uint32(divisor)/2) / uint32(divisor))
	if dividend < 0 {
		return -quotient
	}
	return quotient
}

// residualBucket is q(v) of FORMAT.md section 1.
func residualBucket(value int32) uint16 {
	if value == 0 {
		return 0
	}
	magnitude := uint32(value)
	sign := 0
	if value < 0 {
		magnitude = -magnitude
		sign = 1
	}
	return uint16(2*bits.Len32(magnitude) - 1 + sign)
}
