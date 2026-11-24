package mvdz

import (
	"encoding/binary"
	"math/bits"
)

const (
	protocolVersion     = 28
	protocolVersionFTE  = 0x58455446 // "FTEX"
	protocolVersionFTE2 = 0x32455446 // "FTE2"
	protocolVersionMVD1 = 0x3144564D // "MVD1"

	ftePextTrans        = 1 << 3
	ftePextFloatCoords  = 1 << 15
	ftePextColorMod     = 1 << 19
	mvdPext1FloatCoords = 1 << 0

	demRead     = 1
	demSet      = 2
	demMultiple = 3
	demSingle   = 4
	demStats    = 5
	demAll      = 6

	svcNop                 = 1
	svcDisconnect          = 2
	svcUpdateStat          = 3
	svcSound               = 6
	svcPrint               = 8
	svcStuffText           = 9
	svcSetAngle            = 10
	svcServerData          = 11
	svcLightStyle          = 12
	svcUpdateFrags         = 14
	svcDamage              = 19
	svcSpawnStatic         = 20
	svcFTESpawnStatic2     = 21
	svcSpawnBaseline       = 22
	svcTempEntity          = 23
	svcSetPause            = 24
	svcCenterPrint         = 26
	svcKilledMonster       = 27
	svcFoundSecret         = 28
	svcSpawnStaticSound    = 29
	svcIntermission        = 30
	svcFinale              = 31
	svcCDTrack             = 32
	svcSmallKick           = 34
	svcBigKick             = 35
	svcUpdatePing          = 36
	svcUpdateEnterTime     = 37
	svcUpdateStatLong      = 38
	svcMuzzleFlash         = 39
	svcUpdateUserInfo      = 40
	svcDownload            = 41
	svcPlayerInfo          = 42
	svcNails               = 43
	svcChokeCount          = 44
	svcModelList           = 45
	svcSoundList           = 46
	svcPacketEntities      = 47
	svcDeltaPacketEntities = 48
	svcMaxSpeed            = 49
	svcEntGravity          = 50
	svcSetInfo             = 51
	svcServerInfo          = 52
	svcUpdatePl            = 53
	svcNails2              = 54
	svcFTESpawnBaseline2   = 66

	sndAttenuation = 1 << 14
	sndVolume      = 1 << 15

	teSpike          = 0
	teSuperSpike     = 1
	teGunshot        = 2
	teExplosion      = 3
	teTarExplosion   = 4
	teLightning1     = 5
	teLightning2     = 6
	teWizSpike       = 7
	teKnightSpike    = 8
	teLightning3     = 9
	teLavaSplash     = 10
	teTeleport       = 11
	teBlood          = 12
	teLightningBlood = 13

	// Bits of the entity word and its U_MORE_BITS byte.
	uAngle1      = 1 << 0
	uAngle3      = 1 << 1
	uModel       = 1 << 2
	uColorMap    = 1 << 3
	uSkin        = 1 << 4
	uEffects     = 1 << 5
	uFTEEvenMore = 1 << 7
	uOrigin1     = 1 << 9
	uOrigin2     = 1 << 10
	uOrigin3     = 1 << 11
	uAngle2      = 1 << 12
	uFrame       = 1 << 13
	uMoreBits    = 1 << 15

	// Bits of the FTE extension bytes.
	uFTEScale      = 1 << 0
	uFTETrans      = 1 << 1
	uFTEFatness    = 1 << 2
	uFTEModelDbl   = 1 << 3
	uFTEEntityDbl  = 1 << 5
	uFTEEntityDbl2 = 1 << 6
	uFTEYetMore    = 1 << 7
	uFTEColorMod   = 1 << 10

	dfOrigin      = 1 << 0
	dfAngles      = 1 << 3
	dfEffects     = 1 << 6
	dfSkinNum     = 1 << 7
	dfDead        = 1 << 8
	dfGib         = 1 << 9
	dfWeaponFrame = 1 << 10
	dfModel       = 1 << 11
	originFlags   = dfOrigin | dfOrigin<<1 | dfOrigin<<2
	angleFlags    = dfAngles | dfAngles<<1 | dfAngles<<2
)

var (
	entityOriginBits = [3]uint16{uOrigin1, uOrigin2, uOrigin3}
	entityAngleBits  = [3]uint16{uAngle1, uAngle2, uAngle3}
)

// protocolState is the protocol extension state, FORMAT.md 4.1.
type protocolState struct {
	fte  uint32
	fte2 uint32 // carried to the next segment's transform header; no rule reads it
	mvd1 uint32
}

func (protocol *protocolState) hasFTEFloatCoords() bool {
	return protocol.fte&ftePextFloatCoords != 0
}

func (protocol *protocolState) hasFloatCoords() bool {
	return protocol.hasFTEFloatCoords() || protocol.mvd1&mvdPext1FloatCoords != 0
}

// coordSize is of entity, player, sound and temp-entity coordinates.
func (protocol *protocolState) coordSize() uint32 {
	if protocol.hasFloatCoords() {
		return 4
	}
	return 2
}

// wideCoordSize is of SVC_INTERMISSION coordinates.
func (protocol *protocolState) wideCoordSize() uint32 {
	if protocol.hasFTEFloatCoords() {
		return 4
	}
	return 2
}

func (protocol *protocolState) angleSize() uint32 {
	if protocol.hasFTEFloatCoords() {
		return 2
	}
	return 1
}

// A server data starts a match: an extension it does not announce is off.
func (protocol *protocolState) observeServerData(body []byte) {
	*protocol = protocolState{}
	fields := byteCursor{data: body}
	for {
		version := fields.readUint32()
		if fields.isShort || version == protocolVersion {
			return
		}
		extensions := fields.readUint32()
		if fields.isShort {
			return
		}
		switch version {
		case protocolVersionFTE:
			protocol.fte = extensions
		case protocolVersionFTE2:
			protocol.fte2 = extensions
		case protocolVersionMVD1:
			protocol.mvd1 = extensions
		}
	}
}

// commandBodySize is false if the command is unknown or its body does not
// fit, FORMAT.md 8 rule 12.
func (protocol *protocolState) commandBodySize(command uint8, body []byte) (uint32, bool) {
	fields := byteCursor{data: body}
	switch command {
	case svcNop, svcKilledMonster, svcFoundSecret, svcSmallKick, svcBigKick:
	case svcDisconnect, svcStuffText, svcCenterPrint, svcFinale:
		fields.skipCString()
	case svcUpdateStat:
		fields.skip(2)
	case svcSound:
		channel := fields.readUint16()
		if channel&sndVolume != 0 {
			fields.skip(1)
		}
		if channel&sndAttenuation != 0 {
			fields.skip(1)
		}
		fields.skip(1 + 3*protocol.coordSize())
	case svcPrint:
		fields.skip(1)
		fields.skipCString()
	case svcSetAngle:
		fields.skip(1 + 3*protocol.angleSize())
	case svcServerData:
		for fields.readUint32() != protocolVersion && !fields.isShort {
			fields.skip(4)
		}
		fields.skip(4)
		fields.skipCString()
		fields.skip(4)
		fields.skipCString()
		fields.skip(40)
	case svcLightStyle:
		fields.skip(1)
		fields.skipCString()
	case svcUpdateFrags:
		fields.skip(3)
	case svcDamage:
		fields.skip(2 + 3*protocol.coordSize())
	case svcSpawnStatic:
		fields.skip(4 + 3*(protocol.coordSize()+protocol.angleSize()))
	case svcFTESpawnStatic2, svcFTESpawnBaseline2:
		protocol.skipEntityDelta(&fields)
	case svcSpawnBaseline:
		fields.skip(2 + 4 + 3*(protocol.coordSize()+protocol.angleSize()))
	case svcTempEntity:
		switch fields.readUint8() {
		case teLightning1, teLightning2, teLightning3:
			fields.skip(2 + 6*protocol.coordSize())
		case teSpike, teSuperSpike, teExplosion, teTarExplosion, teWizSpike, teKnightSpike,
			teLavaSplash, teTeleport, teLightningBlood:
			fields.skip(3 * protocol.coordSize())
		case teGunshot, teBlood:
			fields.skip(1 + 3*protocol.coordSize())
		default:
			return 0, false
		}
	case svcSetPause, svcCDTrack, svcChokeCount:
		fields.skip(1)
	case svcSpawnStaticSound:
		fields.skip(3*protocol.coordSize() + 3)
	case svcIntermission:
		fields.skip(3*protocol.wideCoordSize() + 3*protocol.angleSize())
	case svcUpdatePing:
		fields.skip(3)
	case svcUpdateEnterTime, svcUpdateStatLong:
		fields.skip(5)
	case svcMuzzleFlash, svcUpdatePl:
		fields.skip(2)
	case svcUpdateUserInfo:
		fields.skip(5)
		fields.skipCString()
	case svcDownload:
		size := int16(fields.readUint16())
		fields.skip(1)
		if size > 0 {
			fields.skip(uint32(size))
		}
	case svcPlayerInfo:
		fields.skip(1)
		flags := fields.readUint16()
		fields.skip(1)
		originCount := uint32(bits.OnesCount16(flags & originFlags))
		angleCount := uint32(bits.OnesCount16(flags & angleFlags))
		fields.skip(originCount*protocol.coordSize() + angleCount*2)
		fields.skip(uint32(bits.OnesCount16(flags & (dfModel | dfSkinNum | dfEffects | dfWeaponFrame))))
	case svcNails:
		fields.skip(6 * uint32(fields.readUint8()))
	case svcModelList, svcSoundList:
		fields.skip(1)
		isListEnd := false
		for !isListEnd {
			isListEnd = fields.skipCString() <= 1
		}
		fields.skip(1)
	case svcPacketEntities, svcDeltaPacketEntities:
		if command == svcDeltaPacketEntities {
			fields.skip(1)
		}
		for fields.peekUint16() != 0 {
			protocol.skipEntityDelta(&fields)
		}
		fields.skip(2)
	case svcMaxSpeed, svcEntGravity:
		fields.skip(4)
	case svcSetInfo:
		fields.skip(1)
		fields.skipCString()
		fields.skipCString()
	case svcServerInfo:
		fields.skipCString()
		fields.skipCString()
	case svcNails2:
		fields.skip(7 * uint32(fields.readUint8()))
	default:
		return 0, false
	}
	return uint32(fields.position), !fields.isShort
}

// skipEntityDelta skips an entity word and its delta, FORMAT.md 8 rule 12.
func (protocol *protocolState) skipEntityDelta(fields *byteCursor) {
	entityBits := fields.readUint16() &^ 511
	var more uint16
	if entityBits&uMoreBits != 0 {
		entityBits |= uint16(fields.readUint8())
	}
	if entityBits&uFTEEvenMore != 0 && protocol.fte != 0 {
		more = uint16(fields.readUint8())
		if more&uFTEYetMore != 0 {
			more |= uint16(fields.readUint8()) << 8
		}
	}
	if entityBits&uModel != 0 {
		fields.skip(1)
	} else if more&uFTEModelDbl != 0 {
		fields.skip(2)
	}
	for _, bit := range [4]uint16{uFrame, uColorMap, uSkin, uEffects} {
		if entityBits&bit != 0 {
			fields.skip(1)
		}
	}
	for axis := range 3 {
		if entityBits&entityOriginBits[axis] != 0 {
			fields.skip(protocol.coordSize())
		}
		if entityBits&entityAngleBits[axis] != 0 {
			fields.skip(protocol.angleSize())
		}
	}
	if more&uFTETrans != 0 && protocol.fte&ftePextTrans != 0 {
		fields.skip(1)
	}
	if more&uFTEScale != 0 {
		fields.skip(1)
	}
	if more&uFTEFatness != 0 {
		fields.skip(1)
	}
	if more&uFTEColorMod != 0 && protocol.fte&ftePextColorMod != 0 {
		fields.skip(3)
	}
}

// frameLen includes the frame header; false if data does not start with a
// whole frame of at most a segment, FORMAT.md 8 rule 11.
func frameLen(data []byte) (int, bool) {
	length, match := frameAt(data)
	return length, match == frameYes
}

// frameMatch tells whether bytes start with frames.
type frameMatch uint8

const (
	frameNo         frameMatch = iota
	frameIncomplete            // more bytes could make it frameYes
	frameYes
)

// frameAt gives the length of the frame data starts with: a frame is at most
// a segment long.
func frameAt(data []byte) (int, frameMatch) {
	if len(data) < 2 {
		return 0, frameIncomplete
	}
	headerSize, payloadLenOffset := 6, 2
	switch data[1] & 7 {
	case demSet:
		headerSize, payloadLenOffset = 10, -1
	case demMultiple:
		headerSize, payloadLenOffset = 10, 6
	case demRead, demSingle, demStats, demAll:
	default:
		return 0, frameNo
	}
	if len(data) < headerSize {
		return 0, frameIncomplete
	}
	payloadLen := uint32(0)
	if payloadLenOffset >= 0 {
		payloadLen = binary.LittleEndian.Uint32(data[payloadLenOffset:])
	}
	if payloadLen > maxRawLen-uint32(headerSize) {
		return 0, frameNo
	}
	length := headerSize + int(payloadLen)
	if len(data) < length {
		return 0, frameIncomplete
	}
	return length, frameYes
}

// frameRun tells frames from bytes that frame by chance.
func frameRun(data []byte) frameMatch {
	const frameRunLen = 16
	for range frameRunLen {
		length, match := frameAt(data)
		if match != frameYes {
			return match
		}
		data = data[length:]
	}
	return frameYes
}
