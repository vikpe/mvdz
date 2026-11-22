package mvdz

// Stream ids, FORMAT.md section 3.2. The comments give the names used there.
const (
	// Structural streams: rANS symbols, decoded by the walker.
	streamTime              = 0  // time
	streamType              = 1  // type
	streamCommand           = 2  // cmd
	streamPlayerFlags       = 3  // pi.flags
	streamPlayerFrame       = 4  // pi.frame
	streamPlayerWeaponFrame = 5  // pi.wframe
	streamEntityGap         = 6  // pe.ent
	streamEntityBits        = 7  // pe.bits
	streamEntityMore        = 8  // pe.more
	streamEntityAngle       = 9  // pe.a
	streamStatIndex         = 10 // st.idx
	streamStatValue         = 11 // st.val
	streamSoundChannel      = 12 // snd.ch
	streamSoundNumber       = 13 // snd.num
	streamTempEntityType    = 14 // te.type
	streamUsercmd           = 15 // uc

	// Numeric streams: rANS residuals, decoded by the numeric groups.
	// Origins and angles have one stream per axis: the id below plus the axis.
	streamPlayerOrigin      = 16 // pi.ox, pi.oy, pi.oz
	streamPlayerAngle       = 19 // pi.ap, pi.ay, pi.ar
	streamEntityOrigin      = 22 // pe.ox, pe.oy, pe.oz
	streamSoundOrigin       = 25 // snd.o
	streamTempEntityOrigin  = 26 // te.o
	streamPlayerFloatOrigin = 27 // pi.fx, pi.fy, pi.fz
	streamEntityFloatOrigin = 30 // pe.fx, pe.fy, pe.fz
	streamUsercmdAngle      = 33 // uc.a

	// Blob streams: plain bytes from the blob body, consumed by the walker.
	streamMask            = 34 // mask
	streamEscapeLength    = 35 // esclen
	streamHiddenTag       = 36 // hid.tag
	streamHiddenLength    = 37 // hid.len
	streamHiddenType      = 38 // hid.type
	streamHiddenDamage    = 39 // hid.dmg
	streamPlayerNumber    = 40 // pi.num
	streamPlayerModel     = 41 // pi.model
	streamPlayerSkin      = 42 // pi.skin
	streamPlayerEffects   = 43 // pi.eff
	streamEntityHeader    = 44 // pe.hdr
	streamEntityModel     = 45 // pe.model
	streamEntityFrame     = 46 // pe.frame
	streamEntityMisc      = 47 // pe.misc
	streamStatLongIndex   = 48 // stl.idx
	streamSoundMisc       = 49 // snd.misc
	streamTempEntityMisc  = 50 // te.misc
	streamPrintLevel      = 51 // pr.level
	streamFrags           = 52 // frags
	streamStatLongValue   = 53 // stl.val
	streamRaw             = 54 // raw
	streamTail            = 55 // tail
	streamSet             = 56 // set
	streamText            = 57 // text
	streamPing            = 58 // ping
	streamNails           = 59 // nails
	streamHiddenMisc      = 60 // hid.misc
	streamSoundFloat      = 61 // snd.f
	streamTempEntityFloat = 62 // te.f
	streamGeneric         = 63 // gen.c: the id is 63 plus the command byte

	firstNumericStream = 16
	firstBlobStream    = 34
	streamCount        = 319
)
