package mvdz

// Stream ids, FORMAT.md 3.2: the names there with a stream prefix.
const (
	// Structural streams: rANS symbols, decoded by the walker.
	streamTime              = 0
	streamType              = 1
	streamCommand           = 2
	streamPlayerFlags       = 3
	streamPlayerFrame       = 4
	streamPlayerWeaponFrame = 5
	streamEntityBits        = 6
	streamEntityMore        = 7
	streamEntityAngle       = 8
	streamStatIndex         = 9
	streamStatValue         = 10
	streamSoundChannel      = 11
	streamSoundNumber       = 12
	streamTempEntityType    = 13
	streamUsercmd           = 14
	streamLzLiteral         = 15 // literal bytes of the blob tokens

	// Numeric streams: rANS values, decoded by the walk and the blob tokens.
	// Origins and angles have one stream per axis: the id below plus the axis.
	streamPlayerOrigin      = 16
	streamPlayerAngle       = 19
	streamEntityOrigin      = 22
	streamSoundOrigin       = 25
	streamTempEntityOrigin  = 26
	streamPlayerFloatOrigin = 27
	streamEntityFloatOrigin = 30
	streamUsercmdAngle      = 33
	streamLzLength          = 34 // match lengths of the blob tokens, 0 for a literal
	streamLzDistance        = 35 // match distances of the blob tokens

	// Blob streams: plain bytes from the blob tokens, consumed by the walker.
	streamMask            = 36
	streamEscapeLength    = 37
	streamHiddenTag       = 38
	streamHiddenLength    = 39
	streamHiddenType      = 40
	streamPlayerNumber    = 41
	streamPlayerModel     = 42
	streamPlayerSkin      = 43
	streamPlayerEffects   = 44
	streamEntityHeader    = 45
	streamEntityGap       = 46
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

	firstNumericStream = 16
	firstBlobStream    = 36
	streamCount        = 315
)
