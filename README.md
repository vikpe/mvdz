# mvdz

> Lossless compression of QuakeWorld MVD demos

- **Small**: a fifth of the raw demo, a third of its gzip.
- **Lossless**: exact original bytes, checked by CRC-32.
- **Streaming**: decodes in order as it arrives, segments in parallel.
- **Self-describing**: size, duration and SHA-256 in a 60-byte header.
- **Low memory**: never holds the whole demo.
- **Specified**: [FORMAT.md](FORMAT.md), with test vectors in `vectors/`.
- **Implementations**: [Rust](mvdz_rust/README.md), [Go](mvdz_go/README.md), [C](mvdz_c/README.md).

**Status: 1.0.0-rc.4, a release candidate for comments.**

## How it works

1. Cut the demo into segments of about 256 KiB.
2. Walk each segment command by command and put every field in its own stream.
3. Replace positions and angles by their difference from a prediction.
4. Code the streams with rANS, under models stored once per file.
5. Code every other byte as LZ77 tokens under the same models.
6. Store what does not parse as it is.

## Ratios and speeds

Details in [BENCHMARKS.md](BENCHMARKS.md).

| format       | size / raw | decode   |
| ------------ | ---------- | -------- |
| mvdz, `-j 1` | 0.19       | 97 MB/s  |
| mvdz, `-j 4` | 0.19       | 237 MB/s |
| gzip -9      | 0.57       | 194 MB/s |
| xz -9e       | 0.43       | 56 MB/s  |
| zstd -19     | 0.51       | 508 MB/s |

## Implementations

|                             | encode | decode |
| --------------------------- | ------ | ------ |
| [Rust](mvdz_rust/README.md) | yes    | yes    |
| [Go](mvdz_go/README.md)     | yes    | yes    |
| [C](mvdz_c/README.md)       | no     | yes    |

## FAQ

**Q1. Why not just compress the `.mvd` with zstd or xz?**

On the same stratified sample, `zstd -19` gives 0.51 of raw and `xz -9e`
0.43, against 0.19 for mvdz: mvdz files are about 2.6 times smaller than
zstd -19 and 2.2 times smaller than xz -9e ([BENCHMARKS.md](BENCHMARKS.md)).
A general-purpose compressor sees bytes; mvdz parses the demo, puts each
field in its own stream and codes positions and angles as residuals against
predictions. The container also gives decoding in order as the file arrives,
segments that decode on their own, and the demo's size, duration and
identity hash without decoding.

**Q2. What is the licence?**

MIT ([LICENSE](LICENSE)), and every dependency is permissive (MIT,
Apache-2.0 or BSD). MIT code can be linked into GPL programs such as the
GPL-2 QuakeWorld engines and servers.

**Q3. Is the format stable?**

Not yet. This is a release candidate: the format may change before 1.0, and
files written by a release candidate are not guaranteed to be readable by
1.0, so keep the original `.mvd` files. The format has no extension points,
so the goal is that 1.0 does not change: a reader refuses any version but
its own.

**Q4. Where do I give feedback?**

Open an issue on GitHub (<https://github.com/galfthan/mvdz/issues>), for the
release and the format alike.

## Contributing

Issues and pull requests are welcome on GitHub. Please run the checks of the
implementation you change (its README; CI runs them). Changes to the file format go through FORMAT.md first. See [CHANGELOG.md](CHANGELOG.md) for releases.
Pull requests from automated agents without a human author who reviews
and answers for them are closed.

## Licence

MIT, see [LICENSE](LICENSE).
