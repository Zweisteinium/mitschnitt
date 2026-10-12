<h1 align="center">
  <img src="app/icon.svg" width="72" alt=""><br>
  Mitschnitt
</h1>

<p align="center">Keeps what you listen to, exactly as it arrived, and proves it sample by sample.</p>

<p align="center"><img src="docs/screenshot.png" width="820" alt="Mitschnitt recording a track"></p>

Mitschnitt runs next to your music app while you listen. Every track you play from the start is saved as its
own file, from the first sample to the last, without gaps, trimming or level changes. Afterwards Mitschnitt
checks the file and tells you whether it really is the original.

## What it does

Files are 16 or 24 bit FLAC with the very samples the app played. WAV works too, and so does MP3, which is
encoded from the verified recording.

Every sample is checked the moment it arrives. A lossless stream passes at 100 %. Lossy streams, volume
normalization, an equalizer or resampling don't, and you see that live while the track plays.

A track is kept whole or not at all. If you pause, skip, seek, or the player stalls while buffering, that track
is dropped, so you never end up with a file that has a hole in it.

Each finished file is read back, compared and checked for dropouts, limiting, fades and clipping. Title, artist,
album, track number and cover are filled in, file names follow your own template, and tracks you already have
are skipped.

Mitschnitt can also work through a list of songs on its own. It starts each one in Spotify, checks that the right
song plays, records it and pauses Spotify at the end. The list comes from Echolot, a self-hosted music library that
takes the recordings straight into the right songs, or from a list exported from it. You can keep using your PC
meanwhile: when Spotify grabs the focus for a new song, Mitschnitt hands it back. This works on Windows for now.

On Windows, a recording can't be interrupted by media keys, headset buttons or earbuds taking themselves off.
Only the player's own pause button stops it. This uses a keyboard hook that looks at media keys and nothing
else, plus a media session of its own.

## Platforms

| Platform | Status |
|---|---|
| Windows 10/11 | Stable. Spotify is verified bit-exact. |
| Linux | Experimental and under active development. |
| macOS | Planned, low priority. |

**Windows** needs [VB-Cable](https://vb-audio.com/Cable/), donationware by VB-Audio. The Setup tab downloads it
from their site and installs it. It is not bundled with Mitschnitt.

**Linux** needs PipeWire as the sound server and glibc 2.36 or newer. That covers Debian 12, Ubuntu 22.10,
Fedora 37, Mint 22, Arch and anything newer. Mitschnitt ships as an AppImage. The media key protection is not
available on Linux yet.

Spotify is the only supported app so far. An app only gets a profile once its output has been proven untouched.

## How it works

On **Windows** the app plays into a silent virtual cable, and Mitschnitt plays it back to you. Only that app's
own audio is captured, before Windows mixes it with anything else. While Mitschnitt listens, the app's volume
sits at exactly 50 %. That keeps the Windows peak limiter away from loud masters. Halving a sample is exact, so
Mitschnitt doubles it again and gets the original back.

On **Linux** Mitschnitt connects straight to the app's PipeWire stream, before anything else is mixed in. You
keep listening on your normal speakers or headphones. While Mitschnitt listens, the app's volume is held at
100 % (use your system volume instead) and PipeWire runs at the app's sample rate, 44.1 kHz for Spotify. Both
go back to how they were when you stop.

On both systems a track starts at the edge from silence to sound and ends after exactly the length the player
reports, to the millisecond. An untouched 16 or 24 bit source arrives as exact 24 bit values, and that is the
proof Mitschnitt checks for.

## Building

```sh
cargo test --workspace --exclude mitschnitt-app
cd app && npm install && npx tauri build
```

On Linux you also need the PipeWire headers and libclang (`pipewire clang` on Arch, `libpipewire-0.3-dev
libclang-dev` on Debian and Ubuntu) next to the usual [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/).
`make test-live` checks the capture against your running PipeWire without making a sound, and `make appimage`
builds the portable AppImage inside a Debian 12 container.

The code lives in three places. `crates/core` does the capture, the checks and the file writing, with the
platform specific parts in `crates/core/src/platform`. `crates/cli` is a command line front end with `check`,
`setup` and `listen`. `app` is the desktop app, built with Tauri 2 and Svelte 5.

## License

[GNU AGPL v3](LICENSE)

---

<sub>Mitschnitt records audio while it plays on your own computer, in real time. It does not download, decrypt
or bypass anything. Use it for your personal listening, and respect the terms of the services you use and the
copyright laws where you live.</sub>
