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
	streamLzLit             = 16 // lz.lit: literal bytes of the blob tokens

	// Numeric streams: rANS values, decoded by the numeric groups and the blob tokens.
	// Origins and angles have one stream per axis: the id below plus the axis.
	streamPlayerOrigin      = 17 // pi.ox, pi.oy, pi.oz
	streamPlayerAngle       = 20 // pi.ap, pi.ay, pi.ar
	streamEntityOrigin      = 23 // pe.ox, pe.oy, pe.oz
	streamSoundOrigin       = 26 // snd.o
	streamTempEntityOrigin  = 27 // te.o
	streamPlayerFloatOrigin = 28 // pi.fx, pi.fy, pi.fz
	streamEntityFloatOrigin = 31 // pe.fx, pe.fy, pe.fz
	streamUsercmdAngle      = 34 // uc.a
	streamLzLen             = 35 // lz.len: match lengths of the blob tokens, 0 for a literal
	streamLzDist            = 36 // lz.dist: match distances of the blob tokens

	// Blob streams: plain bytes from the blob tokens, consumed by the walker.
	streamMask            = 37 // mask
	streamEscapeLen       = 38 // esclen
	streamHiddenTag       = 39 // hid.tag
	streamHiddenLen       = 40 // hid.len
	streamHiddenType      = 41 // hid.type
	streamHiddenDamage    = 42 // hid.dmg
	streamPlayerNumber    = 43 // pi.num
	streamPlayerModel     = 44 // pi.model
	streamPlayerSkin      = 45 // pi.skin
	streamPlayerEffects   = 46 // pi.eff
	streamEntityHeader    = 47 // pe.hdr
	streamEntityModel     = 48 // pe.model
	streamEntityFrame     = 49 // pe.frame
	streamEntityMisc      = 50 // pe.misc
	streamStatLongIndex   = 51 // stl.idx
	streamSoundMisc       = 52 // snd.misc
	streamTempEntityMisc  = 53 // te.misc
	streamPrintLevel      = 54 // pr.level
	streamFrags           = 55 // frags
	streamStatLongValue   = 56 // stl.val
	streamRaw             = 57 // raw
	streamTail            = 58 // tail
	streamSet             = 59 // set
	streamText            = 60 // text
	streamPing            = 61 // ping
	streamNails           = 62 // nails
	streamHiddenMisc      = 63 // hid.misc
	streamSoundFloat      = 64 // snd.f
	streamTempEntityFloat = 65 // te.f
	streamGeneric         = 66 // gen.c: the id is 66 plus the command byte

	firstNumericStream = 17
	firstBlobStream    = 37
	streamCount        = 322
)
