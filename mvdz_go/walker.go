package mvdz

import (
	"encoding/binary"
	"math/bits"
)

const (
	noOffset          = 0xFFFFFFFF
	hiddenDamageDone  = 7
	hiddenUsercmd     = 1
	hiddenUsercmdSize = 23
)

// segmentWalker walks the structure of a segment and records where its
// numeric fields are, FORMAT.md section 4. After the first error every
// primitive does nothing and every loop stops.
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
	records    recordLists
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
	previousEntityGap      uint8
	players                [256]playerState
	usercmdPreviousPlayers [2]uint8
	usercmds               [256]usercmdState
	entities               [2048]entityState
	stats                  [32][256]uint8
	playerOriginOffsets    [256][3]uint32
	entityOriginOffsets    [2048][3]uint32
	previousPacket         []uint16
	currentPacket          []uint16
	previousPacketIndex    int
	packetNumber           uint32
}

type playerState struct {
	flags       uint16
	frame       uint8
	weaponFrame uint8
}

type usercmdState struct {
	drop, msec, grid, buttons, impulse uint8
	moves                              [3]uint16
}

type entityState struct {
	angles         [3]uint8
	angleResiduals [3]uint8
	frame          uint8
	model          uint8
	bits           uint8
	lastPacket     uint32
}

// recordLists list the numeric fields in walk order, for the numeric groups.
type recordLists struct {
	playerInfos    []playerInfoRecord
	packetEntities []packetEntityRecord
	sounds         []soundRecord
	tempEntities   []tempEntityRecord
	usercmds       []usercmdRecord
}

type playerInfoRecord struct {
	offset         uint32
	time           uint32
	flags          uint16
	number         uint8
	hasFloatCoords bool
}

type packetEntityRecord struct {
	number         uint16
	originOffsets  [3]uint32 // noOffset for an origin that is not sent
	isInPrevious   bool
	hasFloatCoords bool
}

type soundRecord struct {
	offset     uint32
	references [3]uint32
	kind       uint8
	number     uint8
}

type tempEntityRecord struct {
	offset     uint32
	references [3]uint32
	kind       uint8
}

type usercmdRecord struct {
	pitchOffset uint32
	number      uint8
	gridAxes    uint8
}

func newWalker(segment []byte, streams *segmentStreams) *segmentWalker {
	walker := &segmentWalker{
		segment:           segment,
		streams:           streams,
		protocol:          streams.protocol,
		statPreviousIndex: 255,
		packetNumber:      1,
	}
	for player := range walker.playerOriginOffsets {
		walker.playerOriginOffsets[player] = [3]uint32{noOffset, noOffset, noOffset}
	}
	for entity := range walker.entityOriginOffsets {
		walker.entityOriginOffsets[entity] = [3]uint32{noOffset, noOffset, noOffset}
	}
	return walker
}

// FORMAT.md section 4.4
func (walker *segmentWalker) run(tail uint32) {
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
	lengthOffset := walker.reserve(4)
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
	for group := 0; ; group++ {
		sourceByte := uint8(sourceLen>>(7*group)) & 0x7f
		hasMoreGroups := sourceLen>>(7*(group+1)) != 0
		if hasMoreGroups {
			sourceByte |= 0x80
		}
		lengthByte := walker.blobByte(streamEscapeLen, sourceByte)
		length |= uint32(lengthByte&0x7f) << (7 * group)
		if lengthByte < 0x80 {
			break
		}
		if group == 4 {
			walker.fail("escape length")
			return
		}
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
	case svcUpdateStatLong:
		walker.codeBlobByte(streamStatLongIndex)
		walker.copyBlob(streamStatLongValue, 4)
	case svcSound:
		walker.sound()
	case svcTempEntity:
		walker.tempEntity()
	case svcPrint:
		walker.codeBlobByte(streamPrintLevel)
		walker.copyCommandBody(streamText, svcStuffText)
	case svcStuffText, svcCenterPrint:
		walker.copyCommandBody(streamText, svcStuffText)
	case svcUpdateFrags:
		walker.copyBlob(streamFrags, 3)
	case svcUpdatePing, svcUpdateEnterTime, svcUpdatePl:
		walker.copyCommandBody(streamPing, command)
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

	coordSize := walker.protocol.coordSize()
	originCount := uint32(bits.OnesCount16(flags & 0x7))
	angleCount := uint32(bits.OnesCount16(flags & 0x38))
	offset := walker.reserve(originCount*coordSize + angleCount*2)
	walker.records.playerInfos = append(walker.records.playerInfos, playerInfoRecord{
		offset:         offset,
		time:           walker.time,
		flags:          flags,
		number:         number,
		hasFloatCoords: walker.protocol.hasFloatCoords(),
	})
	for axis := range 3 {
		if flags&(dfOrigin<<axis) != 0 {
			walker.playerOriginOffsets[number][axis] = offset
			offset += coordSize
		}
	}

	if flags&dfModel != 0 {
		walker.codeBlobByte(streamPlayerModel)
	}
	if flags&dfSkinNum != 0 {
		walker.codeBlobByte(streamPlayerSkin)
	}
	if flags&dfEffects != 0 {
		walker.codeBlobByte(streamPlayerEffects)
	}
	weaponFrame := player.weaponFrame
	if flags&dfWeaponFrame != 0 {
		weaponFrame = walker.codeSymbolResidual(streamPlayerWeaponFrame, uint16(weaponFrame), weaponFrame)
	}
	*player = playerState{flags, frame, weaponFrame}
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
	walker.previousEntityGap = 0
	walker.previousPacketIndex = 0
	walker.currentPacket = walker.currentPacket[:0]
	walker.packetNumber++

	previous := -1
	for walker.err == nil {
		predictedGap := walker.predictEntity(previous) - previous
		gapContext := uint16(min(predictedGap, 255)) | uint16(min(walker.previousEntityGap, 15))<<8
		sourceWord := walker.peek16()
		sourceGap := 0 // a word of 0 ends the packet
		if sourceWord != 0 {
			sourceGap = (int(sourceWord&511)-previous-1)&511 + 1
		}
		gap := int(walker.symbol(streamEntityGap, gapContext, uint8(min(sourceGap, 255))))
		if gap == 255 {
			low := walker.symbol(streamEntityGap, 1<<12, uint8(sourceGap))
			high := walker.symbol(streamEntityGap, 2<<12, uint8(sourceGap>>8))
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
		walker.previousEntityGap = uint8(min(max(number-previous, 0), 255))
		previous = number
		walker.currentPacket = append(walker.currentPacket, uint16(number))
		entity.bits = uint8(word >> 9)
		walker.entityDelta(word)
	}
	walker.previousPacket, walker.currentPacket = walker.currentPacket, walker.previousPacket
}

// predictEntity is the next entity of the previous packet after previous.
func (walker *segmentWalker) predictEntity(previous int) int {
	for walker.previousPacketIndex < len(walker.previousPacket) {
		candidate := int(walker.previousPacket[walker.previousPacketIndex])
		if candidate > previous {
			return candidate
		}
		walker.previousPacketIndex++
	}
	return previous + 1
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
	record := packetEntityRecord{
		number:         number,
		originOffsets:  [3]uint32{noOffset, noOffset, noOffset},
		isInPrevious:   entity.lastPacket == walker.packetNumber-1,
		hasFloatCoords: walker.protocol.hasFloatCoords(),
	}
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
		if bits&entityOriginBits[axis] != 0 {
			record.originOffsets[axis] = walker.reserve(walker.protocol.coordSize())
			walker.entityOriginOffsets[number][axis] = record.originOffsets[axis]
		}
		if bits&entityAngleBits[axis] != 0 {
			walker.entityAngle(entity, axis)
		}
	}
	walker.records.packetEntities = append(walker.records.packetEntities, record)

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
	record := soundRecord{references: walker.entityOriginOffsets[entity], kind: 2, number: number}
	isPlayer := 1 <= entity && entity <= 32
	if isPlayer {
		record.references = walker.playerOriginOffsets[entity-1]
		record.kind = 1
	}
	record.offset = walker.reserve(6)
	walker.records.sounds = append(walker.records.sounds, record)
}

// FORMAT.md section 4.10
func (walker *segmentWalker) tempEntity() {
	context := uint16(walker.previousTempEntity)
	if walker.previousCommand == svcTempEntity {
		context |= 1 << 8
	}
	kind := walker.codeSymbol(streamTempEntityType, context)
	walker.previousTempEntity = kind

	record := tempEntityRecord{references: [3]uint32{noOffset, noOffset, noOffset}, kind: kind}
	coordCount := uint32(3)
	switch kind {
	case teLightning1, teLightning2, teLightning3:
		entity := walker.codeBlobBytes(streamTempEntityMisc, 2)
		isPlayer := 1 <= entity && entity <= 32
		if isPlayer {
			record.references = walker.playerOriginOffsets[entity-1]
		}
		coordCount = 6
	case teGunshot, teBlood:
		walker.codeBlobByte(streamTempEntityMisc)
	}
	if walker.protocol.hasFloatCoords() {
		walker.copyBlob(streamTempEntityFloat, coordCount*4)
		return
	}
	record.offset = walker.reserve(coordCount * 2)
	walker.records.tempEntities = append(walker.records.tempEntities, record)
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
		length := walker.codeBlobBytes(streamHiddenLen, 4)
		blockType := walker.codeBlobBytes(streamHiddenType, 2)
		switch {
		case blockType == hiddenDamageDone && length == 8:
			walker.copyBlob(streamHiddenDamage, 8)
		case blockType == hiddenUsercmd && length == hiddenUsercmdSize:
			walker.usercmd()
		default:
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
		if _, isOnGrid := onGrid(walker.peek32(4 * axis)); isOnGrid {
			sourceGrid |= 1 << axis
		}
	}
	grid := walker.symbol(streamUsercmd, 3<<12|uint16(state.grid), sourceGrid)
	if grid > 7 {
		walker.fail("usercmd grid flags")
		return
	}
	state.grid = grid

	pitchOffset := walker.position
	for axis := range uint16(3) {
		isOnGrid := grid&(1<<axis) != 0
		if isOnGrid {
			walker.reserve(4)
			continue
		}
		for index := range uint16(4) {
			walker.codeSymbol(streamUsercmd, 4<<12|axis<<2|index)
		}
	}
	if grid != 0 {
		walker.records.usercmds = append(walker.records.usercmds, usercmdRecord{pitchOffset, player, grid})
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

// Walker primitives, FORMAT.md section 4. The comments give the names used
// there. The streams are the only part that depends on the direction.

func (walker *segmentWalker) fail(rule string) {
	if walker.err == nil {
		walker.err = corrupt(rule)
	}
}

// sym: sourceSymbol is the symbol an encoder codes.
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

// b8
func (walker *segmentWalker) codeSymbol(stream int, context uint16) uint8 {
	value := walker.symbol(stream, context, walker.peek(0))
	walker.write(value)
	return value
}

// r8
func (walker *segmentWalker) codeSymbolResidual(stream int, context uint16, prediction uint8) uint8 {
	value := walker.symbol(stream, context, walker.peek(0)-prediction) + prediction
	walker.write(value)
	return value
}

// take: sourceByte is the byte an encoder puts.
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

// p8
func (walker *segmentWalker) codeBlobByte(stream int) uint8 {
	value := walker.blobByte(stream, walker.peek(0))
	walker.write(value)
	return value
}

// pr8
func (walker *segmentWalker) codeBlobResidual(stream int, prediction uint8) uint8 {
	value := walker.blobByte(stream, walker.peek(0)-prediction) + prediction
	walker.write(value)
	return value
}

// pn: returns the little-endian value of the bytes.
func (walker *segmentWalker) codeBlobBytes(stream int, count int) uint32 {
	var value uint32
	for index := range count {
		value |= uint32(walker.codeBlobByte(stream)) << (8 * index)
	}
	return value
}

// blob
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

// reserve leaves count bytes to the numeric groups and returns their offset.
func (walker *segmentWalker) reserve(count uint32) uint32 {
	offset := walker.position
	if walker.err != nil {
		return offset
	}
	if count > uint32(len(walker.segment))-walker.position {
		walker.fail("segment overflow")
		return offset
	}
	walker.position += count
	return offset
}

// generic: the body is sized from the bytes that hold it, the blob stream's
// when decoding and the payload's when encoding.
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
