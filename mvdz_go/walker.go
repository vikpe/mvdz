package mvdz

import "encoding/binary"

// segmentWalker walks a segment the same in both directions, FORMAT.md 4 and 5.
// Encoding walks a copy: the bytes ahead of the cursor are the source it proposes.
type segmentWalker struct {
	segment    []byte
	position   uint32
	payloadEnd uint32 // read when encoding only
	streams    *segmentStreams
	protocol   protocolState
	err        error // after it every primitive does nothing and every loop stops

	// FORMAT.md 4.3
	time                   uint32
	target                 uint8
	lastType               uint8
	lastTimeByte           uint8
	previousCommand        uint8
	firstCommandByType     [8]uint8
	statPreviousIndex      uint8
	statPreviousResidual   uint8
	lastHeader             uint8
	previousTempEntity     uint8
	players                [256]playerState
	usercmdPreviousPlayers [2]uint8
	usercmds               [256]usercmdState
	entities               [2048]entityState
	stats                  [32][256]uint8
	packetNumber           uint32
	tempEntityLast         [3]uint16
}

func newWalker(segment []byte, streams *segmentStreams) *segmentWalker {
	return &segmentWalker{
		segment:           segment,
		streams:           streams,
		protocol:          streams.protocol,
		statPreviousIndex: 255,
		packetNumber:      1,
	}
}

// FORMAT.md 4.4
func (walker *segmentWalker) run(tail uint32) {
	if tail > uint32(len(walker.segment)) {
		walker.fail()
		return
	}
	stop := uint32(len(walker.segment)) - tail
	for walker.err == nil && walker.position < stop {
		walker.frame()
	}
	if walker.position != stop {
		walker.fail()
	}
	walker.copyBlob(streamTail, tail)
}

func (walker *segmentWalker) frame() {
	timeByte := walker.codeSymbol(streamTime, uint16(walker.lastType)|uint16(walker.lastTimeByte)<<8)
	walker.lastTimeByte = timeByte
	walker.time += uint32(timeByte)
	frameType := walker.codeSymbol(streamType, uint16(walker.lastType)|uint16(min(timeByte, 32))<<8)
	walker.lastType = frameType
	walker.target = frameType >> 3

	switch frameType & 7 {
	case demSet:
		walker.copyBlob(streamSet, 8)
	case demMultiple:
		mask := walker.codeBlobBytes(streamMask, 4)
		walker.lengthPrefixed(frameType, mask == 0)
	case demRead, demSingle, demStats, demAll:
		walker.lengthPrefixed(frameType, false)
	default:
		walker.fail()
	}
}

func (walker *segmentWalker) lengthPrefixed(frameType uint8, isHidden bool) {
	offset := walker.position
	if walker.err == nil && uint32(len(walker.segment))-offset < 4 {
		walker.fail()
	}
	if walker.err != nil {
		return
	}
	walker.payloadEnd = offset + 4 + walker.peekUint32(0)
	walker.position += 4
	if isHidden {
		walker.hiddenPayload()
	} else {
		walker.payload(frameType)
	}
	if walker.err != nil {
		return
	}
	binary.LittleEndian.PutUint32(walker.segment[offset:], walker.position-offset-4)
}

// escape copies the rest of a payload, which the walk does not model.
func (walker *segmentWalker) escape() {
	sourceLen := uint32(len(walker.payloadRest()))
	var escapeLen uint32
	for index := range 4 {
		sourceByte := uint8(sourceLen >> (8 * index))
		escapeLen |= uint32(walker.blobByte(streamEscapeLength, sourceByte)) << (8 * index)
	}
	walker.copyBlob(streamRaw, escapeLen)
}

// FORMAT.md 4.5
func (walker *segmentWalker) payload(frameType uint8) {
	walker.previousCommand = 0
	walker.statPreviousIndex = 255
	walker.statPreviousResidual = 0
	typeIndex := frameType & 7
	previousFirstClass := commandClass(walker.firstCommandByType[typeIndex])
	walker.firstCommandByType[typeIndex] = 255 // stays for an empty payload

	for commandIndex := 0; walker.err == nil; commandIndex++ {
		if commandIndex == 1 {
			walker.firstCommandByType[typeIndex] = walker.previousCommand
		}
		var context uint16
		if class := commandClass(walker.previousCommand); class == 15 {
			context = 0x8000 | uint16(walker.previousCommand) | uint16(typeIndex)<<8 | uint16(min(commandIndex, 7))<<11
		} else {
			context = class | uint16(min(commandIndex, 15))<<4 | previousFirstClass<<8 | uint16(typeIndex)<<12
		}
		command := walker.symbol(streamCommand, context, walker.sourceCommand())
		if command == 255 {
			return
		}
		if command == 0 {
			walker.escape()
		} else {
			walker.write(command)
			walker.command(command)
		}
		walker.previousCommand = command
	}
}

// FORMAT.md 4.2: only an encoder sizes the body; a decoder reads 0 as the source length.
func (walker *segmentWalker) copyCommandBody(stream int, command uint8) {
	const lengthContexts = 0xc000
	sourceLen, _ := walker.protocol.commandBodySize(command, walker.payloadRest())
	var bodyLen uint32
	for index := range 3 {
		context := lengthContexts | uint16(index)<<8 | uint16(command)
		sourceByte := uint8(sourceLen >> (8 * index))
		bodyLen |= uint32(walker.symbol(streamCommand, context, sourceByte)) << (8 * index)
	}
	walker.copyBlob(stream, bodyLen)
}

// FORMAT.md 4.6
func (walker *segmentWalker) command(command uint8) {
	switch command {
	case svcPlayerInfo:
		walker.playerInfo()
	case svcPacketEntities, svcDeltaPacketEntities:
		walker.packetEntities(command == svcDeltaPacketEntities)
	case svcUpdateStat:
		walker.updateStat()
	case svcSound:
		walker.sound()
	case svcTempEntity:
		walker.tempEntity()
	case svcNails, svcNails2:
		walker.copyCommandBody(streamNails, command)
	case svcServerData:
		start := walker.position
		walker.copyCommandBody(streamGeneric+svcServerData, svcServerData)
		walker.protocol.observeServerData(walker.segment[start:walker.position])
	default:
		walker.copyCommandBody(streamGeneric+int(command), command)
	}
}

func (walker *segmentWalker) updateStat() {
	stats := &walker.stats[walker.target&31]
	index := walker.codeSymbol(streamStatIndex, uint16(walker.statPreviousIndex)|uint16(walker.previousCommand)<<8)
	previous := stats[index]
	value := walker.codeSymbolResidual(streamStatValue, uint16(index)|uint16(walker.statPreviousResidual)<<8, previous)
	walker.statPreviousIndex = index
	walker.statPreviousResidual = value - previous
	stats[index] = value
}

// FORMAT.md 4.7
func (walker *segmentWalker) playerInfo() {
	number := walker.codeBlobByte(streamPlayerNumber)
	player := &walker.players[number]
	flags := walker.playerInfoFlags(player.flags)
	frameContext := uint16(player.frame) | flags&(dfDead|dfGib)
	frame := walker.codeSymbolResidual(streamPlayerFrame, frameContext, player.frame)

	walker.playerOriginsAndAngles(player, flags)

	if flags&dfModel != 0 {
		walker.codeBlobByte(streamPlayerModel)
	}
	if flags&dfSkinNum != 0 {
		walker.codeBlobByte(streamPlayerSkin)
	}
	if flags&dfEffects != 0 {
		walker.codeBlobByte(streamPlayerEffects)
	}
	if flags&dfWeaponFrame != 0 {
		player.weaponFrame = walker.codeSymbolResidual(streamPlayerWeaponFrame, uint16(player.weaponFrame), player.weaponFrame)
	}
	player.flags = flags
	player.frame = frame
}

// FORMAT.md 4.8
func (walker *segmentWalker) packetEntities(isDelta bool) {
	if isDelta {
		walker.lastHeader = walker.codeBlobResidual(streamEntityHeader, walker.lastHeader)
	}
	walker.packetNumber++

	previousNumber := -1
	for walker.err == nil {
		sourceWord := walker.peekUint16()
		sourceGap := 0 // a word of 0 ends the packet
		if sourceWord != 0 {
			sourceGap = (int(sourceWord&511)-previousNumber-1)&511 + 1
		}
		gap := int(walker.entityGap(uint16(sourceGap)))
		if gap == 0 {
			walker.writeUint16(0)
			break
		}
		number := (previousNumber + gap) & 511
		entity := &walker.entities[number]
		sourceBits := uint8(sourceWord>>9) ^ entity.bits
		bits := walker.symbol(streamEntityBits, uint16(entity.bits)|uint16(entity.model)<<7, sourceBits) ^ entity.bits
		word := uint16(bits)<<9 | uint16(number)
		if word == 0 {
			walker.fail()
			break
		}
		walker.writeUint16(word)
		previousNumber = number
		entity.bits = uint8(word >> 9)
		walker.entityDelta(word)
	}
}

func (walker *segmentWalker) entityGap(sourceGap uint16) uint16 {
	first := walker.blobByte(streamEntityGap, uint8(min(sourceGap, 255)))
	if first != 255 {
		return uint16(first)
	}
	low := walker.blobByte(streamEntityGap, uint8(sourceGap))
	high := walker.blobByte(streamEntityGap, uint8(sourceGap>>8))
	return uint16(low) | uint16(high)<<8
}

func (walker *segmentWalker) entityDelta(word uint16) {
	bits := word &^ 511
	number := word & 511
	if bits&uMoreBits != 0 {
		moreContext := bits>>9 | uint16(walker.entities[number].model)<<8
		bits |= uint16(walker.codeSymbol(streamEntityMore, moreContext))
	}
	var more uint16
	if bits&uFTEEvenMore != 0 && walker.protocol.fte != 0 {
		more = uint16(walker.codeSymbol(streamEntityMore, 0x8000))
		if more&uFTEYetMore != 0 {
			more |= uint16(walker.codeSymbol(streamEntityMore, 0x8001)) << 8
		}
	}
	if more&uFTEEntityDbl != 0 {
		number += 512
	}
	if more&uFTEEntityDbl2 != 0 {
		number += 1024
	}
	entity := &walker.entities[number]
	wasInPreviousPacket := entity.lastPacket == walker.packetNumber-1
	entity.lastPacket = walker.packetNumber

	if bits&uModel != 0 {
		entity.model = walker.codeBlobByte(streamEntityModel)
	} else if more&uFTEModelDbl != 0 {
		walker.codeBlobBytes(streamEntityModel, 2)
	}
	if bits&uFrame != 0 {
		entity.frame = walker.codeBlobResidual(streamEntityFrame, entity.frame)
	}
	for _, bit := range [3]uint16{uColorMap, uSkin, uEffects} {
		if bits&bit != 0 {
			walker.codeBlobByte(streamEntityMisc)
		}
	}
	for axis := range 3 {
		isOriginSent := bits&entityOriginBits[axis] != 0
		walker.entityOrigin(entity, axis, isOriginSent, wasInPreviousPacket)
		if bits&entityAngleBits[axis] != 0 {
			walker.entityAngle(entity, axis)
		}
	}

	if more&uFTETrans != 0 && walker.protocol.fte&ftePextTrans != 0 {
		walker.codeBlobByte(streamEntityMisc)
	}
	if more&uFTEScale != 0 {
		walker.codeBlobByte(streamEntityMisc)
	}
	if more&uFTEFatness != 0 {
		walker.codeBlobByte(streamEntityMisc)
	}
	if more&uFTEColorMod != 0 && walker.protocol.fte&ftePextColorMod != 0 {
		walker.codeBlobBytes(streamEntityMisc, 3)
	}
}

// FORMAT.md 5.5
func (walker *segmentWalker) entityOrigin(entity *entityState, axis int, isSent, wasInPreviousPacket bool) {
	if walker.protocol.hasFloatCoords() {
		walker.floatOrigin(streamEntityFloatOrigin+axis, axis, isSent, &entity.floatOrigins[axis])
		return
	}
	track := &entity.origins[axis]
	if !isSent {
		track.hold()
		return
	}
	context := min(residualBucket(int32(track.residual)), 17) | min(residualBucket(track.velocity()), 17)<<5
	if wasInPreviousPacket {
		context |= 1 << 10
	}
	value, residual := walker.codeUint16(streamEntityOrigin+axis, context, track.predictLinear())
	track.push(value, residual)
}

func (walker *segmentWalker) entityAngle(entity *entityState, axis int) {
	modelContext := uint16(entity.model) << 6
	if walker.protocol.hasFTEFloatCoords() {
		walker.codeSymbol(streamEntityAngle, 3|modelContext)
		walker.codeSymbol(streamEntityAngle, 3|modelContext|1<<14)
		return
	}
	bucket := min(byteResidualBucket(entity.angleResiduals[axis]), 15)
	previous := entity.angles[axis]
	angle := walker.codeSymbolResidual(streamEntityAngle, uint16(axis)|bucket<<2|modelContext, previous)
	entity.angleResiduals[axis] = angle - previous
	entity.angles[axis] = angle
}

// FORMAT.md 4.9
func (walker *segmentWalker) sound() {
	low := walker.codeSymbol(streamSoundChannel, uint16(walker.previousCommand))
	high := walker.codeSymbol(streamSoundChannel, 0x100|uint16(low))
	channel := uint16(low) | uint16(high)<<8
	if channel&sndVolume != 0 {
		walker.codeBlobByte(streamSoundMisc)
	}
	if channel&sndAttenuation != 0 {
		walker.codeBlobByte(streamSoundMisc)
	}
	entity := (channel >> 3) & 1023
	soundNumber := walker.codeSymbol(streamSoundNumber, (channel&7)|min(entity, 33)<<3)
	if walker.protocol.hasFloatCoords() {
		walker.copyBlob(streamSoundFloat, 12)
		return
	}
	isPlayer := 1 <= entity && entity <= 32
	if isPlayer {
		walker.soundOrigin(lastValues(&walker.players[entity-1].origins), 1, soundNumber)
	} else {
		walker.soundOrigin(lastValues(&walker.entities[entity].origins), 2, soundNumber)
	}
}

// kind: 1 for a player's origin, 2 for an entity's.
func (walker *segmentWalker) soundOrigin(origin [3]uint16, kind uint16, soundNumber uint8) {
	for axis := range 3 {
		context := uint16(axis) | kind<<2 | uint16(soundNumber)<<4
		walker.codeUint16(streamSoundOrigin, context, origin[axis])
	}
}

// FORMAT.md 4.10
func (walker *segmentWalker) tempEntity() {
	context := uint16(walker.previousTempEntity)
	if walker.previousCommand == svcTempEntity {
		context |= 1 << 8
	}
	kind := walker.codeSymbol(streamTempEntityType, context)
	walker.previousTempEntity = kind

	var beamOrigin [3]uint16 // of the player a beam starts at
	isBeam := false
	switch kind {
	case teLightning1, teLightning2, teLightning3:
		entity := walker.codeBlobBytes(streamTempEntityMisc, 2)
		isPlayer := 1 <= entity && entity <= 32
		if isPlayer {
			beamOrigin = lastValues(&walker.players[entity-1].origins)
		}
		isBeam = true
	case teGunshot, teBlood:
		walker.codeBlobByte(streamTempEntityMisc)
	}
	if walker.protocol.hasFloatCoords() {
		coordCount := uint32(3)
		if isBeam {
			coordCount = 6
		}
		walker.copyBlob(streamTempEntityFloat, coordCount*4)
		return
	}
	walker.tempEntityOrigins(kind, isBeam, beamOrigin)
}

// FORMAT.md 5.6
func (walker *segmentWalker) tempEntityOrigins(kind uint8, isBeam bool, beamOrigin [3]uint16) {
	const point, beamStart, beamEnd = 0, 1, 2
	kindContext := uint16(kind) << 4
	last := &walker.tempEntityLast
	if !isBeam {
		for axis := range 3 {
			context := uint16(axis) | point<<2 | kindContext
			last[axis], _ = walker.codeUint16(streamTempEntityOrigin, context, last[axis])
		}
		return
	}
	var start [3]uint16
	for axis := range 3 {
		context := uint16(axis) | beamStart<<2 | kindContext
		start[axis], _ = walker.codeUint16(streamTempEntityOrigin, context, beamOrigin[axis])
	}
	for axis := range 3 {
		context := uint16(axis) | beamEnd<<2 | kindContext
		last[axis], _ = walker.codeUint16(streamTempEntityOrigin, context, start[axis])
	}
}

// FORMAT.md 4.11
func (walker *segmentWalker) hiddenPayload() {
	const (
		mvdHiddenUsercmd  = 1
		hiddenUsercmdSize = 23
	)
	for walker.err == nil {
		switch walker.blobByte(streamHiddenTag, walker.sourceHiddenTag()) {
		case 0:
			walker.escape()
			continue
		case 1:
		case 2:
			return
		default:
			walker.fail()
			return
		}
		blockLen := walker.codeBlobBytes(streamHiddenLength, 4)
		blockType := walker.codeBlobBytes(streamHiddenType, 2)
		if blockType == mvdHiddenUsercmd && blockLen == hiddenUsercmdSize {
			walker.usercmd()
		} else {
			walker.copyBlob(streamHiddenMisc, blockLen)
		}
	}
}

func (walker *segmentWalker) usercmd() {
	previousPlayers := walker.usercmdPreviousPlayers
	playerContext := uint16(min(previousPlayers[0], 31)) | uint16(min(previousPlayers[1], 31))<<5
	player := walker.codeSymbol(streamUsercmd, playerContext)
	walker.usercmdPreviousPlayers = [2]uint8{player, previousPlayers[0]}

	state := &walker.usercmds[player]
	state.drop = walker.codeSymbol(streamUsercmd, 1<<12|uint16(state.drop))
	state.msec = walker.codeSymbol(streamUsercmd, 2<<12|uint16(state.msec))
	sourceGrid := uint8(0)
	for axis := range uint32(3) {
		if _, isOnGrid := gridFromFloat32Bits(walker.peekUint32(4 * axis)); isOnGrid {
			sourceGrid |= 1 << axis
		}
	}
	grid := walker.symbol(streamUsercmd, 3<<12|uint16(state.grid), sourceGrid)
	if grid > 7 {
		walker.fail()
		return
	}
	state.grid = grid

	for axis := range 3 {
		isOnGrid := grid&(1<<axis) != 0
		if isOnGrid {
			walker.usercmdGridAngle(axis, &state.angles[axis])
			continue
		}
		for byteIndex := range uint16(4) {
			walker.codeSymbol(streamUsercmd, 4<<12|uint16(axis)<<2|byteIndex)
		}
	}

	for axis := range state.moves {
		low := walker.codeSymbol(streamUsercmd, 5<<12|uint16(axis)<<8|(state.moves[axis]&0xff))
		high := walker.codeSymbol(streamUsercmd, 6<<12|uint16(axis)<<8|uint16(low))
		state.moves[axis] = uint16(low) | uint16(high)<<8
	}
	state.buttons = walker.codeSymbol(streamUsercmd, 7<<12|uint16(state.buttons))
	state.impulse = walker.codeSymbol(streamUsercmd, 8<<12|uint16(state.impulse))
}

// Walker primitives, FORMAT.md 4. The streams are the only part that
// depends on the direction.

func (walker *segmentWalker) fail() {
	if walker.err == nil {
		walker.err = corrupt("structure")
	}
}

// peek is the source byte at offset from the cursor, 0 past the end.
func (walker *segmentWalker) peek(offset uint32) uint8 {
	index := uint64(walker.position) + uint64(offset)
	if index >= uint64(len(walker.segment)) {
		return 0
	}
	return walker.segment[index]
}

func (walker *segmentWalker) peekUint16() uint16 {
	return uint16(walker.peek(0)) | uint16(walker.peek(1))<<8
}

func (walker *segmentWalker) peekUint32(offset uint32) uint32 {
	var value uint32
	for index := range uint32(4) {
		value |= uint32(walker.peek(offset+index)) << (8 * index)
	}
	return value
}

// payloadRest is the source from the cursor to the end of the payload.
func (walker *segmentWalker) payloadRest() []byte {
	isInPayload := walker.position <= walker.payloadEnd && walker.payloadEnd <= uint32(len(walker.segment))
	if !isInPayload {
		return nil
	}
	return walker.segment[walker.position:walker.payloadEnd]
}

func (walker *segmentWalker) write(value uint8) {
	if walker.err != nil {
		return
	}
	if walker.position >= uint32(len(walker.segment)) {
		walker.fail()
		return
	}
	walker.segment[walker.position] = value
	walker.position++
}

func (walker *segmentWalker) writeUint16(value uint16) {
	walker.write(uint8(value))
	walker.write(uint8(value >> 8))
}

func (walker *segmentWalker) writeUint32(value uint32) {
	walker.writeUint16(uint16(value))
	walker.writeUint16(uint16(value >> 16))
}

func (walker *segmentWalker) symbol(stream int, context uint16, sourceSymbol uint8) uint8 {
	if walker.err != nil {
		return 255
	}
	symbol, isCoded := walker.streams.symbol(stream, context, sourceSymbol)
	if !isCoded {
		walker.fail()
		return 255
	}
	return symbol
}

func (walker *segmentWalker) residual(stream int, context uint16, sourceResidual int32) int32 {
	if walker.err != nil {
		return 0
	}
	residual, isCoded := walker.streams.residual(stream, context, sourceResidual)
	if !isCoded {
		walker.fail()
	}
	return residual
}

func (walker *segmentWalker) blobByte(stream int, sourceByte uint8) uint8 {
	if walker.err != nil {
		return 0
	}
	value := [1]byte{sourceByte}
	if !walker.streams.blobBytes(stream, value[:]) {
		walker.fail()
		return 0
	}
	return value[0]
}

func (walker *segmentWalker) codeSymbol(stream int, context uint16) uint8 {
	value := walker.symbol(stream, context, walker.peek(0))
	walker.write(value)
	return value
}

func (walker *segmentWalker) codeSymbolResidual(stream int, context uint16, prediction uint8) uint8 {
	value := walker.symbol(stream, context, walker.peek(0)-prediction) + prediction
	walker.write(value)
	return value
}

func (walker *segmentWalker) codeBlobByte(stream int) uint8 {
	value := walker.blobByte(stream, walker.peek(0))
	walker.write(value)
	return value
}

func (walker *segmentWalker) codeBlobResidual(stream int, prediction uint8) uint8 {
	value := walker.blobByte(stream, walker.peek(0)-prediction) + prediction
	walker.write(value)
	return value
}

// codeBlobBytes returns the little-endian value of the bytes.
func (walker *segmentWalker) codeBlobBytes(stream, count int) uint32 {
	var value uint32
	for index := range count {
		value |= uint32(walker.codeBlobByte(stream)) << (8 * index)
	}
	return value
}

func (walker *segmentWalker) copyBlob(stream int, count uint32) {
	if walker.err != nil {
		return
	}
	if count > uint32(len(walker.segment))-walker.position {
		walker.fail()
		return
	}
	if !walker.streams.blobBytes(stream, walker.segment[walker.position:][:count]) {
		walker.fail()
		return
	}
	walker.position += count
}

// codeUint16 returns the value and the residual to keep as state, FORMAT.md 5.1.
func (walker *segmentWalker) codeUint16(stream int, context uint16, prediction uint16) (uint16, int32) {
	sourceResidual := int16(walker.peekUint16() - prediction)
	residual := int16(walker.residual(stream, context, int32(sourceResidual)))
	value := prediction + uint16(residual)
	walker.writeUint16(value)
	return value, int32(residual)
}

func (walker *segmentWalker) codeUint32(stream int, context uint16, prediction uint32) (uint32, int32) {
	sourceResidual := int32(walker.peekUint32(0) - prediction)
	residual := walker.residual(stream, context, sourceResidual)
	value := prediction + uint32(residual)
	walker.writeUint32(value)
	return value, residual
}

// FORMAT.md 5.2
func (walker *segmentWalker) floatOrigin(stream, axis int, isSent bool, track *floatTrack) {
	if !isSent {
		track.hold()
		return
	}
	context := min(residualBucket(track.residual), 63) | uint16(axis)<<6
	valueBits, residual := walker.codeUint32(stream, context, track.predictLinear())
	track.push(valueBits, residual)
}

// sourceCommand is 255 at the payload end, 0 (an escape) before a command
// that cannot be walked, FORMAT.md 4.5.
func (walker *segmentWalker) sourceCommand() uint8 {
	rest := walker.payloadRest()
	if len(rest) == 0 {
		return 255
	}
	if _, isSized := walker.protocol.commandBodySize(rest[0], rest[1:]); !isSized {
		return 0
	}
	return rest[0]
}

// sourceHiddenTag is 2 at the payload end, 0 (an escape) before a block that
// cannot be walked, else 1, FORMAT.md 4.11.
func (walker *segmentWalker) sourceHiddenTag() uint8 {
	rest := walker.payloadRest()
	if len(rest) == 0 {
		return 2
	}
	if len(rest) < 6 {
		return 0
	}
	blockLen := binary.LittleEndian.Uint32(rest)
	isWholeBlock := blockLen != 0 && uint64(blockLen) <= uint64(len(rest)-6)
	if !isWholeBlock {
		return 0
	}
	return 1
}

// playerInfoFlags codes the flags as the bits that changed since previous.
func (walker *segmentWalker) playerInfoFlags(previous uint16) uint16 {
	sourceChanged := walker.peekUint16() ^ previous
	sourceMain := uint8(sourceChanged&0x3f) | uint8((sourceChanged>>10)&1)<<6
	if sourceChanged&^0x43f != 0 {
		sourceMain |= 0x80
	}
	mainByte := uint16(walker.symbol(streamPlayerFlags, 0x8000|(previous&0x7fff), sourceMain))
	changed := (mainByte & 0x3f) | (((mainByte >> 6) & 1) << 10)
	hasRareChange := mainByte&0x80 != 0
	if hasRareChange {
		sourceRare := uint8((sourceChanged>>6)&0xf) | uint8((sourceChanged>>11)&0xf)<<4
		sourceTop := uint8(sourceChanged >> 15)
		rareByte := uint16(walker.symbol(streamPlayerFlags, 0x100|((previous>>6)&0xff), sourceRare))
		topByte := uint16(walker.symbol(streamPlayerFlags, 0x200, sourceTop))
		changed |= ((rareByte & 0xf) << 6) | ((rareByte >> 4) << 11) | ((topByte & 1) << 15)
	}
	flags := changed ^ previous
	walker.writeUint16(flags)
	return flags
}

// FORMAT.md 5.3 and 5.4
func (walker *segmentWalker) playerOriginsAndAngles(player *playerState, flags uint16) {
	now, before := player.frameTimes.deltas(walker.time)
	hasFloatCoords := walker.protocol.hasFloatCoords()
	for axis := range 3 {
		isSent := flags&(dfOrigin<<axis) != 0
		if hasFloatCoords {
			walker.floatOrigin(streamPlayerFloatOrigin+axis, axis, isSent, &player.floatOrigins[axis])
			continue
		}
		track := &player.origins[axis]
		if !isSent {
			track.hold()
			continue
		}
		context := min(residualBucket(int32(track.residual)), 19) | min(residualBucket(track.velocity()), 19)<<5
		value, residual := walker.codeUint16(streamPlayerOrigin+axis, context, track.predictTimeScaled(now, before))
		track.push(value, residual)
	}
	for axis := range 3 {
		track := &player.angles[axis]
		isSent := flags&(dfAngles<<axis) != 0
		if !isSent {
			track.hold()
			continue
		}
		context := min(residualBucket(int32(track.residual)), 23) | min(residualBucket(track.previousVelocity()), 23)<<5
		value, residual := walker.codeUint16(streamPlayerAngle+axis, context, track.predictTimeScaled(now, before))
		track.push(value, residual)
	}
}

// FORMAT.md 5.6
func (walker *segmentWalker) usercmdGridAngle(axis int, track *axisTrack) {
	context := min(residualBucket(int32(track.residual)), 23) |
		min(residualBucket(track.velocity()), 23)<<5 |
		min(residualBucket(track.previousVelocity()), 15)<<10 |
		uint16(axis)<<14
	prediction := track.predictLinear()
	sourceValue, _ := gridFromFloat32Bits(walker.peekUint32(0))
	sourceResidual := int16(sourceValue - prediction)
	residual := int16(walker.residual(streamUsercmdAngle, context, int32(sourceResidual)))
	value := prediction + uint16(residual)
	walker.writeUint32(gridToFloat32Bits(value))
	track.push(value, int32(residual))
}

type playerState struct {
	flags           uint16
	frame           uint8
	weaponFrame     uint8
	origins, angles [3]axisTrack // FORMAT.md 5.3 and 5.4
	frameTimes      frameTimes
	floatOrigins    [3]floatTrack // FORMAT.md 5.2
}

type usercmdState struct {
	drop, msec, grid, buttons, impulse uint8
	moves                              [3]uint16
	angles                             [3]axisTrack // FORMAT.md 5.6
}

type entityState struct {
	angles         [3]uint8
	angleResiduals [3]uint8
	frame          uint8
	model          uint8
	bits           uint8
	lastPacket     uint32
	origins        [3]axisTrack  // FORMAT.md 5.5
	floatOrigins   [3]floatTrack // FORMAT.md 5.2
}

func commandClass(command uint8) uint16 {
	switch command {
	case 0:
		return 0
	case svcPlayerInfo:
		return 1
	case svcDeltaPacketEntities:
		return 2
	case svcPacketEntities:
		return 3
	case svcSound:
		return 4
	case svcTempEntity:
		return 5
	case svcUpdateStat:
		return 6
	case svcUpdateStatLong:
		return 7
	case svcPrint:
		return 8
	case svcSetInfo:
		return 9
	case svcUpdateFrags:
		return 10
	case svcUpdatePing:
		return 11
	case svcStuffText:
		return 12
	case svcNails, svcNails2:
		return 13
	case svcDamage:
		return 14
	}
	return 15
}
