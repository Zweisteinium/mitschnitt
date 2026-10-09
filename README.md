<h1 align="center">
  <img src="app/icon.svg" width="72" alt=""><br>
  Mitschnitt
</h1>

<p align="center">Keeps what you listen to — exactly as it arrived, proven sample by sample.</p>

<p align="center"><img src="docs/screenshot.png" width="820" alt="Mitschnitt recording a track"></p>

Mitschnitt runs next to your music app. You just listen. Every track you play from its first second is kept
as its own file, from its first sample to its last, with no gap, no trim and no level change. Then Mitschnitt
checks the file and tells you whether it really is the original.

- **Identical, not similar.** 16- or 24-bit FLAC with the same samples the app received. You can also save
  WAV, or MP3 made from the verified file.
- **Proven on arrival.** Every sample is checked as it comes in. A lossless stream passes 100 %. Lossy
  streams, normalization, an equalizer or resampling don't, and you see that live.
- **Whole tracks or nothing.** Pausing, skipping, seeking, a buffering stall or a volume change discards the
  track. You never get a file with a hole in it.
- **Forensic check.** Each file is read back and compared, then checked for dropouts, limiting, fades and
  clipping, with timestamps.
- **Library-ready.** Title, artist, album, track number and cover are filled in. Names follow your own
  template, and tracks you already have are skipped.
- **One-click setup.** The Setup tab installs the virtual cable, sets its rate, routes the app to it and
  checks the app's audio settings.

## Supported

| App | Delivery | Status |
|---|---|---|
| Spotify (Windows desktop) | 16/24-bit · 44.1 kHz | verified bit-exact |

Each app gets its own profile, and only after its output has been proven untouched.

Requirements: Windows 10/11, and [VB-Cable](https://vb-audio.com/Cable/) (donationware by VB-Audio). The Setup
tab downloads VB-Cable from VB-Audio's site; it is not bundled with Mitschnitt.

## How it works

The app plays into a silent virtual cable, and Mitschnitt plays it back to you. Only that app's own stream is
captured, before Windows mixes it with anything else. While Mitschnitt is listening, the app's volume is set to
exactly 50 %. That keeps Windows' peak limiter away from loud masters. Halving is an exact operation, so
Mitschnitt doubles every sample again and you get the original back. An untouched 16- or 24-bit source then
arrives as exact 24-bit values, which is the proof described above.

A track starts at the silence-to-sound edge (or where the player starts its stream), at the position the app
reports. It ends after exactly the length the app reports, to the millisecond.

## Build

```sh
cargo test --workspace --exclude mitschnitt-app
cd app && npm install && npx tauri build
```

`crates/core` holds capture, checks and file writing. `crates/cli` is a command-line front end with the
commands `check`, `setup` and `listen`. `app` is the desktop app (Tauri 2 + Svelte 5).

## License

[GNU AGPL v3](LICENSE)

---

<sub>Mitschnitt records audio while it plays on your own computer, in real time. It does not download, decrypt
or bypass anything. Use it for your personal listening, and respect the terms of the services you use and
the copyright laws where you live.</sub>
