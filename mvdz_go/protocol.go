package mvdz

import "math/bits"

const (
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
	svcFteSpawnStatic2     = 21
	svcSpawnBaseline       = 22
	svcTempEntity          = 23
	svcSetPause            = 24
	svcCenterPrint         = 26
	svcKilledMonster       = 27
	svcFoundSecret         = 28
	svcSpawnStaticSound    = 29
	svcIntermission        = 30
	svcFinale              = 31
	svcCdTrack             = 32
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
	svcFteSpawnBaseline2   = 66

	protocolVersion     = 28
	protocolVersionFte  = 0x58455446 // "FTEX"
	protocolVersionFte2 = 0x32455446 // "FTE2"
	protocolVersionMvd1 = 0x3144564D // "MVD1"

	ftePextTrans        = 1 << 3
	ftePextFloatCoords  = 1 << 15
	ftePextColorMod     = 1 << 19
	mvdPext1FloatCoords = 1 << 0

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

	// Bits of the entity word and its U_MORE_BITS byte.
	uAngle1      = 1 << 0
	uAngle3      = 1 << 1
	uModel       = 1 << 2
	uColorMap    = 1 << 3
	uSkin        = 1 << 4
	uEffects     = 1 << 5
	uFteEvenMore = 1 << 7
	uOrigin1     = 1 << 9
	uOrigin2     = 1 << 10
	uOrigin3     = 1 << 11
	uAngle2      = 1 << 12
	uFrame       = 1 << 13
	uMoreBits    = 1 << 15

	// Bits of the FTE extension bytes.
	uFteScale      = 1 << 0
	uFteTrans      = 1 << 1
	uFteFatness    = 1 << 2
	uFteModelDbl   = 1 << 3
	uFteEntityDbl  = 1 << 5
	uFteEntityDbl2 = 1 << 6
	uFteYetMore    = 1 << 7
	uFteColorMod   = 1 << 10

	sndAttenuation = 1 << 14
	sndVolume      = 1 << 15

	teGunshot    = 2
	teLightning1 = 5
	teLightning2 = 6
	teLightning3 = 9
	teBlood      = 12
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

func (protocol protocolState) hasFteFloatCoords() bool {
	return protocol.fte&ftePextFloatCoords != 0
}

func (protocol protocolState) hasFloatCoords() bool {
	return protocol.hasFteFloatCoords() || protocol.mvd1&mvdPext1FloatCoords != 0
}

// coordSize is the size of entity, player, sound and temp-entity coordinates.
func (protocol protocolState) coordSize() uint32 {
	if protocol.hasFloatCoords() {
		return 4
	}
	return 2
}

// wideCoordSize is the size of SVC_INTERMISSION coordinates.
func (protocol protocolState) wideCoordSize() uint32 {
	if protocol.hasFteFloatCoords() {
		return 4
	}
	return 2
}

func (protocol protocolState) angleSize() uint32 {
	if protocol.hasFteFloatCoords() {
		return 2
	}
	return 1
}

// observeServerData takes the extensions an SVC_SERVER_DATA body announces.
// A server data starts a match: an extension it does not announce is off.
func (protocol *protocolState) observeServerData(body []byte) {
	*protocol = protocolState{}
	fields := byteCursor{data: body}
	for {
		version := fields.u32()
		if fields.isShort || version == protocolVersion {
			return
		}
		extensions := fields.u32()
		if fields.isShort {
			return
		}
		switch version {
		case protocolVersionFte:
			protocol.fte = extensions
		case protocolVersionFte2:
			protocol.fte2 = extensions
		case protocolVersionMvd1:
			protocol.mvd1 = extensions
		}
	}
}

// commandBodySize is the length of the body of command that starts body, or
// false if the command is unknown or its body does not fit, FORMAT.md
// 4.2 and 8 rule 12.
func (protocol protocolState) commandBodySize(command uint8, body []byte) (uint32, bool) {
	fields := byteCursor{data: body}
	coord := protocol.coordSize()
	angle := protocol.angleSize()
	switch command {
	case svcNop, svcKilledMonster, svcFoundSecret, svcSmallKick, svcBigKick:
	case svcDisconnect, svcStuffText, svcCenterPrint, svcFinale:
		fields.str()
	case svcUpdateStat:
		fields.skip(2)
	case svcSound:
		channel := fields.u16()
		if channel&sndVolume != 0 {
			fields.skip(1)
		}
		if channel&sndAttenuation != 0 {
			fields.skip(1)
		}
		fields.skip(1 + 3*coord)
	case svcPrint:
		fields.skip(1)
		fields.str()
	case svcSetAngle:
		fields.skip(1 + 3*angle)
	case svcServerData:
		for fields.u32() != protocolVersion && !fields.isShort {
			fields.skip(4)
		}
		fields.skip(4)
		fields.str()
		fields.skip(4)
		fields.str()
		fields.skip(40)
	case svcLightStyle:
		fields.skip(1)
		fields.str()
	case svcUpdateFrags:
		fields.skip(3)
	case svcDamage:
		fields.skip(2 + 3*coord)
	case svcSpawnStatic:
		fields.skip(4 + 3*(coord+angle))
	case svcFteSpawnStatic2, svcFteSpawnBaseline2:
		protocol.skipEntityDelta(&fields)
	case svcSpawnBaseline:
		fields.skip(2 + 4 + 3*(coord+angle))
	case svcTempEntity:
		switch fields.u8() {
		case teLightning1, teLightning2, teLightning3:
			fields.skip(2 + 6*coord)
		case 0, 1, 3, 4, 7, 8, 10, 11, 13:
			fields.skip(3 * coord)
		case teGunshot, teBlood:
			fields.skip(1 + 3*coord)
		default:
			return 0, false
		}
	case svcSetPause, svcCdTrack, svcChokeCount:
		fields.skip(1)
	case svcSpawnStaticSound:
		fields.skip(3*coord + 3)
	case svcIntermission:
		fields.skip(3*protocol.wideCoordSize() + 3*angle)
	case svcUpdatePing:
		fields.skip(3)
	case svcUpdateEnterTime, svcUpdateStatLong:
		fields.skip(5)
	case svcMuzzleFlash, svcUpdatePl:
		fields.skip(2)
	case svcUpdateUserInfo:
		fields.skip(5)
		fields.str()
	case svcDownload:
		size := int16(fields.u16())
		fields.skip(1)
		if size > 0 {
			fields.skip(uint32(size))
		}
	case svcPlayerInfo:
		fields.skip(1)
		flags := fields.u16()
		fields.skip(1)
		originCount := uint32(bits.OnesCount16(flags & originFlags))
		angleCount := uint32(bits.OnesCount16(flags & angleFlags))
		fields.skip(originCount*coord + angleCount*2)
		fields.skip(uint32(bits.OnesCount16(flags & (dfModel | dfSkinNum | dfEffects | dfWeaponFrame))))
	case svcNails:
		fields.skip(6 * uint32(fields.u8()))
	case svcModelList, svcSoundList:
		fields.skip(1)
		for fields.str() > 1 {
		}
		fields.skip(1)
	case svcPacketEntities, svcDeltaPacketEntities:
		if command == svcDeltaPacketEntities {
			fields.skip(1)
		}
		for fields.peek16() != 0 {
			protocol.skipEntityDelta(&fields)
		}
		fields.skip(2)
	case svcMaxSpeed, svcEntGravity:
		fields.skip(4)
	case svcSetInfo:
		fields.skip(1)
		fields.str()
		fields.str()
	case svcServerInfo:
		fields.str()
		fields.str()
	case svcNails2:
		fields.skip(7 * uint32(fields.u8()))
	default:
		return 0, false
	}
	return uint32(fields.position), !fields.isShort
}

// skipEntityDelta skips an entity word and its delta, FORMAT.md 4.2.
func (protocol protocolState) skipEntityDelta(fields *byteCursor) {
	bits := fields.u16() &^ 511
	var more uint16
	if bits&uMoreBits != 0 {
		bits |= uint16(fields.u8())
	}
	if bits&uFteEvenMore != 0 && protocol.fte != 0 {
		more = uint16(fields.u8())
		if more&uFteYetMore != 0 {
			more |= uint16(fields.u8()) << 8
		}
	}
	if bits&uModel != 0 {
		fields.skip(1)
	} else if more&uFteModelDbl != 0 {
		fields.skip(2)
	}
	for _, bit := range [4]uint16{uFrame, uColorMap, uSkin, uEffects} {
		if bits&bit != 0 {
			fields.skip(1)
		}
	}
	for axis := range 3 {
		if bits&entityOriginBits[axis] != 0 {
			fields.skip(protocol.coordSize())
		}
		if bits&entityAngleBits[axis] != 0 {
			fields.skip(protocol.angleSize())
		}
	}
	if more&uFteTrans != 0 && protocol.fte&ftePextTrans != 0 {
		fields.skip(1)
	}
	if more&uFteScale != 0 {
		fields.skip(1)
	}
	if more&uFteFatness != 0 {
		fields.skip(1)
	}
	if more&uFteColorMod != 0 && protocol.fte&ftePextColorMod != 0 {
		fields.skip(3)
	}
}

// scanFrames is the length of the whole frames data starts with and the sum
// of their time bytes, FORMAT.md 8 rules 9 and 11.
func scanFrames(data []byte) (framedLen int, durationMs uint32) {
	for {
		frameLen, isFrame := frameLen(data[framedLen:])
		if !isFrame {
			return framedLen, durationMs
		}
		durationMs += uint32(data[framedLen])
		framedLen += frameLen
	}
}

// frameLen is the length of the frame that starts data, header included;
// false if data does not start with a whole frame.
func frameLen(data []byte) (int, bool) {
	fields := byteCursor{data: data}
	fields.skip(1)
	switch fields.u8() & 7 {
	case demSet:
		fields.skip(8)
	case demMultiple:
		fields.skip(4)
		fields.skip(fields.u32())
	case demRead, demSingle, demStats, demAll:
		fields.skip(fields.u32())
	default:
		return 0, false
	}
	return fields.position, !fields.isShort
}
