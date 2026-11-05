# alac-encoder 0.3.0, patched

[alac-encoder](https://crates.io/crates/alac-encoder) 0.3.0 by Linus Unnebäck, a Rust port of Apple's open source ALAC
encoder, under its own license (Apache-2.0 or MIT). Upstream has been quiet since 2022, and 0.3.0 only writes 16 bit.
Changes:

- `FormatDescription::alac` takes the bit depth (16, 24 or 32), and `encode` refuses input of another width.
- 20 and 24-bit input is read little-endian, like Apple's original on little-endian machines and like the 16 and 32
  bit reads beside it. Unreachable in 0.3.0, so nothing noticed.
- 16 and 32-bit samples are read by value instead of through unaligned pointer casts.
- The bit writer stays inside its buffer: it wrote whole 32-bit words that could reach past the end.
- The tests are left out, they need 12 MB of fixtures. Mitschnitt's own tests decode its 16 and 24-bit files with
  Symphonia and ffmpeg and compare every sample.
