package mvdz

// Stream ids, FORMAT.md section 3.2: the names there with a stream prefix.
const (
	// Structural streams: rANS symbols, decoded by the walker.
	streamTime              = 0
	streamType              = 1
	streamCommand           = 2
	streamPlayerFlags       = 3
	streamPlayerFrame       = 4
	streamPlayerWeaponFrame = 5
	streamEntityGap         = 6
	streamEntityBits        = 7
	streamEntityMore        = 8
	streamEntityAngle       = 9
	streamStatIndex         = 10
	streamStatValue         = 11
	streamSoundChannel      = 12
	streamSoundNumber       = 13
	streamTempEntityType    = 14
	streamUsercmd           = 15
	streamLzLiteral         = 16 // literal bytes of the blob tokens

	// Numeric streams: rANS values, decoded by the walk and the blob tokens.
	// Origins and angles have one stream per axis: the id below plus the axis.
	streamPlayerOrigin      = 17
	streamPlayerAngle       = 20
	streamEntityOrigin      = 23
	streamSoundOrigin       = 26
	streamTempEntityOrigin  = 27
	streamPlayerFloatOrigin = 28
	streamEntityFloatOrigin = 31
	streamUsercmdAngle      = 34
	streamLzLength          = 35 // match lengths of the blob tokens, 0 for a literal
	streamLzDistance        = 36 // match distances of the blob tokens

	// Blob streams: plain bytes from the blob tokens, consumed by the walker.
	streamMask            = 37
	streamEscapeLength    = 38
	streamHiddenTag       = 39
	streamHiddenLength    = 40
	streamHiddenType      = 41
	streamPlayerNumber    = 42
	streamPlayerModel     = 43
	streamPlayerSkin      = 44
	streamPlayerEffects   = 45
	streamEntityHeader    = 46
	streamEntityModel     = 47
	streamEntityFrame     = 48
	streamEntityMisc      = 49
	streamSoundMisc       = 50
	streamTempEntityMisc  = 51
	streamRaw             = 52
	streamTail            = 53
	streamSet             = 54
	streamNails           = 55
	streamHiddenMisc      = 56
	streamSoundFloat      = 57
	streamTempEntityFloat = 58
	streamGeneric         = 59 // the id is 59 plus the command byte

	firstNumericStream = 17
	firstBlobStream    = 37
	streamCount        = 315
)
