package mvdz

import "encoding/binary"

const (
	hiddenUsercmd     = 1
	hiddenUsercmdSize = 23
)

// segmentWalker walks a segment field by field, FORMAT.md sections 4 and 5.
// After the first error every primitive does nothing and every loop stops.
//
// The walk is the same in both directions. Decoding writes the segment from
// the streams. Encoding walks a copy of the segment: what the walker reads
// ahead of its cursor (peek, payloadRest) is the source, every symbol it asks
// for is the one it proposes from the source, and it writes back the bytes it
// reads. Decoding reads zeros ahead and ignores the proposals.
type segmentWalker struct {
	segment    []byte
	position   uint32
	payloadEnd uint32 // read when encoding only
	streams    *segmentStreams
	protocol   protocolState
	err        error

	// FORMAT.md section 4.3
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

type playerState struct {
	flags           uint16
	frame           uint8
	weaponFrame     uint8
	origins, angles [3]axisTrack // FORMAT.md sections 5.3 and 5.4
	times           frameTimes
	floatOrigins    [3]floatTrack // FORMAT.md section 5.2
}

type usercmdState struct {
	drop, msec, grid, buttons, impulse uint8
	moves                              [3]uint16
	angles                             [3]axisTrack // FORMAT.md section 5.6
}

type entityState struct {
	angles         [3]uint8
	angleResiduals [3]uint8
	frame          uint8
	model          uint8
	bits           uint8
	lastPacket     uint32
	origins        [3]axisTrack  // FORMAT.md section 5.5
	floatOrigins   [3]floatTrack // FORMAT.md section 5.2
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

// FORMAT.md section 4.4
func (walker *segmentWalker) run(tail uint32) {
	if tail > uint32(len(walker.segment)) {
		walker.fail("tail longer than the segment")
		return
	}
	stop := uint32(len(walker.segment)) - tail
	for walker.err == nil && walker.position < stop {
		walker.frame()
	}
	if walker.position != stop {
		walker.fail("frames run into the tail")
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
		walker.fail("frame type")
	}
}

func (walker *segmentWalker) lengthPrefixed(frameType uint8, isHidden bool) {
	sourceLen := walker.peek32(0)
	lengthOffset := walker.position
	if walker.err == nil && uint32(len(walker.segment))-lengthOffset < 4 {
		walker.fail("segment overflow")
	}
	if walker.err != nil {
		return
	}
	walker.position += 4
	walker.payloadEnd = walker.position + sourceLen
	if isHidden {
		walker.hiddenPayload()
	} else {
		walker.payload(frameType)
	}
	if walker.err != nil {
		return
	}
	binary.LittleEndian.PutUint32(walker.segment[lengthOffset:], walker.position-lengthOffset-4)
}

// FORMAT.md section 4.5
func (walker *segmentWalker) payload(frameType uint8) {
	walker.previousCommand = 0
	walker.statPreviousIndex = 255
	walker.statPreviousResidual = 0
	typeIndex := frameType & 7
	firstClass := commandClass(walker.firstCommandByType[typeIndex])
	walker.firstCommandByType[typeIndex] = 255 // stays for an empty payload

	for index := 0; walker.err == nil; index++ {
		if index == 1 {
			walker.firstCommandByType[typeIndex] = walker.previousCommand
		}
		class := commandClass(walker.previousCommand)
		context := class | uint16(min(index, 15))<<4 | firstClass<<8 | uint16(typeIndex)<<12
		if class == 15 {
			context = 0x8000 | uint16(walker.previousCommand) | uint16(typeIndex)<<8 | uint16(min(index, 7))<<11
		}
		command := walker.symbol(streamCommand, context, walker.sourceCommand())
		if command == 255 {
			return
		}
		if command == 0 {
			walker.escape()
			return
		}
		walker.write(command)
		walker.command(command)
		walker.previousCommand = command
	}
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

// escape copies the rest of a payload, which the walk does not model.
func (walker *segmentWalker) escape() {
	sourceLen := uint32(len(walker.payloadRest()))
	var length uint32
	for index := range 4 {
		sourceByte := uint8(sourceLen >> (8 * index))
		length |= uint32(walker.blobByte(streamEscapeLength, sourceByte)) << (8 * index)
	}
	walker.copyBlob(streamRaw, length)
}

// FORMAT.md section 4.6
func (walker *segmentWalker) command(command uint8) {
	switch command {
	case svcPlayerInfo:
		walker.playerInfo()
	case svcPacketEntities, svcDeltaPacketEntities:
		walker.packetEntities(command)
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

// FORMAT.md section 4.7
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

// playerInfoFlags decodes the flags as the bits that changed since previous.
func (walker *segmentWalker) playerInfoFlags(previous uint16) uint16 {
	sourceChanged := walker.peek16() ^ previous
	sourceCommon := uint8(sourceChanged&0x3f) | uint8((sourceChanged>>10)&1)<<6
	if sourceChanged&^0x43f != 0 {
		sourceCommon |= 0x80
	}
	sourceMiddle := uint8((sourceChanged>>6)&0xf) | uint8((sourceChanged>>11)&0xf)<<4
	sourceTop := uint8(sourceChanged >> 15)

	common := uint16(walker.symbol(streamPlayerFlags, 0x8000|(previous&0x7fff), sourceCommon))
	changed := (common & 0x3f) | (((common >> 6) & 1) << 10)
	hasRareChange := common&0x80 != 0
	if hasRareChange {
		middle := uint16(walker.symbol(streamPlayerFlags, 0x100|((previous>>6)&0xff), sourceMiddle))
		top := uint16(walker.symbol(streamPlayerFlags, 0x200, sourceTop))
		changed |= ((middle & 0xf) << 6) | ((middle >> 4) << 11) | ((top & 1) << 15)
	}
	flags := changed ^ previous
	walker.write16(flags)
	return flags
}

// FORMAT.md section 4.8
func (walker *segmentWalker) packetEntities(command uint8) {
	if command == svcDeltaPacketEntities {
		walker.lastHeader = walker.codeBlobResidual(streamEntityHeader, walker.lastHeader)
	}
	walker.packetNumber++

	previous := -1
	for walker.err == nil {
		sourceWord := walker.peek16()
		sourceGap := 0 // a word of 0 ends the packet
		if sourceWord != 0 {
			sourceGap = (int(sourceWord&511)-previous-1)&511 + 1
		}
		gap := int(walker.blobByte(streamEntityGap, uint8(min(sourceGap, 255))))
		if gap == 255 {
			low := walker.blobByte(streamEntityGap, uint8(sourceGap))
			high := walker.blobByte(streamEntityGap, uint8(sourceGap>>8))
			gap = int(low) | int(high)<<8
		}
		if gap == 0 {
			walker.write16(0)
			break
		}
		number := (previous + gap) & 511
		entity := &walker.entities[number]
		sourceBits := uint8(sourceWord>>9) ^ entity.bits
		highBits := walker.symbol(streamEntityBits, uint16(entity.bits)|uint16(entity.model)<<7, sourceBits) ^ entity.bits
		word := uint16(highBits)<<9 | uint16(number)
		if word == 0 {
			walker.fail("entity word")
			break
		}
		walker.write16(word)
		previous = number
		entity.bits = uint8(word >> 9)
		walker.entityDelta(word)
	}
}

func (walker *segmentWalker) entityDelta(word uint16) {
	bits := word &^ 511
	number := word & 511
	if bits&uMoreBits != 0 {
		moreContext := bits>>9 | uint16(walker.entities[number].model)<<8
		bits |= uint16(walker.codeSymbol(streamEntityMore, moreContext))
	}
	var more uint16
	if bits&uFteEvenMore != 0 && walker.protocol.fte != 0 {
		more = uint16(walker.codeSymbol(streamEntityMore, 0x8000))
		if more&uFteYetMore != 0 {
			more |= uint16(walker.codeSymbol(streamEntityMore, 0x8001)) << 8
		}
	}
	if more&uFteEntityDbl != 0 {
		number += 512
	}
	if more&uFteEntityDbl2 != 0 {
		number += 1024
	}
	entity := &walker.entities[number]
	isInPrevious := entity.lastPacket == walker.packetNumber-1
	entity.lastPacket = walker.packetNumber

	if bits&uModel != 0 {
		entity.model = walker.codeBlobByte(streamEntityModel)
	} else if more&uFteModelDbl != 0 {
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
		walker.entityOrigin(entity, axis, isOriginSent, isInPrevious)
		if bits&entityAngleBits[axis] != 0 {
			walker.entityAngle(entity, axis)
		}
	}

	if more&uFteTrans != 0 && walker.protocol.fte&ftePextTrans != 0 {
		walker.codeBlobByte(streamEntityMisc)
	}
	if more&uFteScale != 0 {
		walker.codeBlobByte(streamEntityMisc)
	}
	if more&uFteFatness != 0 {
		walker.codeBlobByte(streamEntityMisc)
	}
	if more&uFteColorMod != 0 && walker.protocol.fte&ftePextColorMod != 0 {
		walker.codeBlobBytes(streamEntityMisc, 3)
	}
}

func (walker *segmentWalker) entityAngle(entity *entityState, axis int) {
	modelContext := uint16(entity.model) << 6
	if walker.protocol.hasFteFloatCoords() {
		walker.codeSymbol(streamEntityAngle, 3|modelContext)
		walker.codeSymbol(streamEntityAngle, 3|modelContext|1<<14)
		return
	}
	bucket := min(residualBucket(int32(int8(entity.angleResiduals[axis]))), 15)
	previous := entity.angles[axis]
	angle := walker.codeSymbolResidual(streamEntityAngle, uint16(axis)|uint16(bucket)<<2|modelContext, previous)
	entity.angleResiduals[axis] = angle - previous
	entity.angles[axis] = angle
}

// FORMAT.md section 4.9
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
	number := walker.codeSymbol(streamSoundNumber, (channel&7)|min(entity, 33)<<3)
	if walker.protocol.hasFloatCoords() {
		walker.copyBlob(streamSoundFloat, 12)
		return
	}
	isPlayer := 1 <= entity && entity <= 32
	if isPlayer {
		walker.soundOrigin(lastValues(&walker.players[entity-1].origins), 1, number)
	} else {
		walker.soundOrigin(lastValues(&walker.entities[entity].origins), 2, number)
	}
}

// FORMAT.md section 4.10
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

// FORMAT.md section 4.11
func (walker *segmentWalker) hiddenPayload() {
	for walker.err == nil {
		switch walker.blobByte(streamHiddenTag, walker.sourceHiddenTag()) {
		case 0:
			walker.escape()
			return
		case 1:
		case 2:
			return
		default:
			walker.fail("hidden block tag")
			return
		}
		length := walker.codeBlobBytes(streamHiddenLength, 4)
		blockType := walker.codeBlobBytes(streamHiddenType, 2)
		if blockType == hiddenUsercmd && length == hiddenUsercmdSize {
			walker.usercmd()
		} else {
			walker.copyBlob(streamHiddenMisc, length)
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
		if _, isOnGrid := gridFromF32Bits(walker.peek32(4 * axis)); isOnGrid {
			sourceGrid |= 1 << axis
		}
	}
	grid := walker.symbol(streamUsercmd, 3<<12|uint16(state.grid), sourceGrid)
	if grid > 7 {
		walker.fail("usercmd grid flags")
		return
	}
	state.grid = grid

	for axis := range 3 {
		isOnGrid := grid&(1<<axis) != 0
		if isOnGrid {
			walker.usercmdGridAngle(axis, &state.angles[axis])
			continue
		}
		for index := range uint16(4) {
			walker.codeSymbol(streamUsercmd, 4<<12|uint16(axis)<<2|index)
		}
	}

	for move := range state.moves {
		low := walker.codeSymbol(streamUsercmd, 5<<12|uint16(move)<<8|(state.moves[move]&0xff))
		high := walker.codeSymbol(streamUsercmd, 6<<12|uint16(move)<<8|uint16(low))
		state.moves[move] = uint16(low) | uint16(high)<<8
	}
	state.buttons = walker.codeSymbol(streamUsercmd, 7<<12|uint16(state.buttons))
	state.impulse = walker.codeSymbol(streamUsercmd, 8<<12|uint16(state.impulse))
}

// sourceCommand is the CMD symbol of the source, FORMAT.md section 4.5: 255 at
// the payload end, 0 (an escape) before a command that cannot be walked.
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

// sourceHiddenTag is the hidden-block tag of the source, FORMAT.md section
// 4.11: 2 at the payload end, 0 (an escape) before a block that cannot be
// walked, else 1.
func (walker *segmentWalker) sourceHiddenTag() uint8 {
	rest := walker.payloadRest()
	if len(rest) == 0 {
		return 2
	}
	if len(rest) < 6 {
		return 0
	}
	length := binary.LittleEndian.Uint32(rest)
	isWholeBlock := length != 0 && uint64(length) <= uint64(len(rest)-6)
	if !isWholeBlock {
		return 0
	}
	return 1
}

// payloadRest is the source from the cursor to the end of the payload.
func (walker *segmentWalker) payloadRest() []byte {
	isInPayload := walker.position <= walker.payloadEnd && walker.payloadEnd <= uint32(len(walker.segment))
	if !isInPayload {
		return nil
	}
	return walker.segment[walker.position:walker.payloadEnd]
}

// peek is the source byte at offset from the cursor, 0 past the end.
func (walker *segmentWalker) peek(offset uint32) uint8 {
	index := uint64(walker.position) + uint64(offset)
	if index >= uint64(len(walker.segment)) {
		return 0
	}
	return walker.segment[index]
}

func (walker *segmentWalker) peek16() uint16 {
	return uint16(walker.peek(0)) | uint16(walker.peek(1))<<8
}

func (walker *segmentWalker) peek32(offset uint32) uint32 {
	var value uint32
	for index := range uint32(4) {
		value |= uint32(walker.peek(offset+index)) << (8 * index)
	}
	return value
}

// Walker primitives, FORMAT.md section 4. The streams are the only part that
// depends on the direction.

func (walker *segmentWalker) fail(rule string) {
	if walker.err == nil {
		walker.err = corrupt(rule)
	}
}

// sourceSymbol is the symbol an encoder codes.
func (walker *segmentWalker) symbol(stream int, context uint16, sourceSymbol uint8) uint8 {
	if walker.err != nil {
		return 255
	}
	symbol, isCoded := walker.streams.symbol(stream, context, sourceSymbol)
	if !isCoded {
		walker.fail("structural stream exhausted or absent")
		return 255
	}
	return symbol
}

func (walker *segmentWalker) write(value uint8) {
	if walker.err != nil {
		return
	}
	if walker.position >= uint32(len(walker.segment)) {
		walker.fail("segment overflow")
		return
	}
	walker.segment[walker.position] = value
	walker.position++
}

func (walker *segmentWalker) write16(value uint16) {
	walker.write(uint8(value))
	walker.write(uint8(value >> 8))
}

func (walker *segmentWalker) write32(value uint32) {
	walker.write16(uint16(value))
	walker.write16(uint16(value >> 16))
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

// sourceByte is the byte an encoder puts.
func (walker *segmentWalker) blobByte(stream int, sourceByte uint8) uint8 {
	if walker.err != nil {
		return 0
	}
	value := [1]byte{sourceByte}
	if !walker.streams.blobBytes(stream, value[:]) {
		walker.fail("blob stream exhausted")
		return 0
	}
	return value[0]
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
func (walker *segmentWalker) codeBlobBytes(stream int, count int) uint32 {
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
		walker.fail("segment overflow")
		return
	}
	if !walker.streams.blobBytes(stream, walker.segment[walker.position:][:count]) {
		walker.fail("blob stream exhausted")
		return
	}
	walker.position += count
}

// copyCommandBody sizes the body from the bytes that hold it, the blob
// stream's when decoding and the payload's when encoding.
func (walker *segmentWalker) copyCommandBody(stream int, command uint8) {
	if walker.err != nil {
		return
	}
	body := walker.streams.blobs[stream]
	if walker.streams.isEncoding {
		body = walker.payloadRest()
	}
	size, isSized := walker.protocol.commandBodySize(command, body)
	if !isSized {
		walker.fail("command body")
		return
	}
	walker.copyBlob(stream, size)
}
