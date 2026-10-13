<h1 align="center">
  <img src="app/icon.svg" width="72" alt=""><br>
  Mitschnitt
</h1>

<p align="center">Records what your music app plays, bit for bit, and checks that the file really is the original.</p>

<p align="center"><img src="docs/screenshot.png" width="820" alt="Mitschnitt recording a track"></p>

Mitschnitt runs next to your music app. Play a track from the start and it lands in its own file, from the first
sample to the last. Then Mitschnitt checks the file and tells you whether the capture worked flawlessly.

I built it because "lossless" is easy to claim and hard to check. Most recorders take whatever comes out of the
sound card and hope for the best. Mitschnitt takes the app's own stream before Windows or PipeWire touch it, and
the check tells you when something did get in the way, like a lossy stream, an equalizer or a dropout.

It's a hobby project that I use every day. Windows works well, Linux mostly does. Expect the odd rough edge, and
if you find one, [open an issue](https://github.com/Zweisteinium/mitschnitt/issues).

## Download

Get the latest version from [Releases](https://github.com/Zweisteinium/mitschnitt/releases/latest):

- **Windows 10/11:** `…_x64-setup.exe` (installer, no admin rights) or `…_x64_portable.exe` (single file).
  Needs [VB-Cable](https://vb-audio.com/Cable/), donationware by VB-Audio, which the Setup tab downloads from their
  site and installs. The builds are not code-signed, so SmartScreen asks once: More info → Run anyway.
- **Linux:** `…_amd64.AppImage`. Needs PipeWire and glibc 2.36 or newer, so Debian 12, Ubuntu 22.10, Fedora 37,
  Mint 22, Arch or anything newer.
- **macOS:** planned, low priority.

For lossless recordings you need Spotify Premium with the streaming quality set to Lossless (up to 24 bit /
44.1 kHz).

## Features

- **Bit-exact.** Only the app's own stream is recorded, before the system mixes it with anything else. Nothing is
  resampled, normalized or turned up or down.
- **Live check.** Every sample is checked as it arrives. A lossless stream shows 100 %. A lossy stream, an
  equalizer or resampling shows up while the track is still playing.
- **Whole tracks.** A track is recorded from the point where its sound starts to the exact length the player
  reports. A pause, skip, seek, buffering stall or dropout is detected and reported, so a broken recording never
  passes as a complete one.
- **Labels.** Every file gets EXACT, FINE (only Spotify's 10 ms fade-in at the start differs), ALT or BAD, with
  the reason. The written file is decoded again and compared with the recording before it gets its name.
- **Formats and tags.** FLAC in 16 or 24 bit, as the source was, WAV, or MP3 encoded from the verified recording.
  Title, artist, album, track number and cover are filled in, names follow your own template, and tracks you
  already have are skipped.
- **Auto setup.** The Setup tab checks Spotify's settings (quality, normalization, equalizer, crossfade, automix)
  and tells you what to switch. On Windows it also installs VB-Cable, matches its sample rate and routes Spotify
  into it, one click each. The goal is a setup without any manual steps.
- **Media key protection.** While a track records, media keys, headset buttons and Bluetooth earbuds that pause
  when you take them out are held back, so they can't ruin the recording. The player's own pause button still
  works. The keyboard hook behind this looks at media keys and nothing else.
- **Song lists.** Together with [Echolot](#echolot), Mitschnitt records a whole list of songs on its own.

|  | Windows | Linux |
|---|:---:|:---:|
| Recording, live check, labels, formats and tags | ✓ | ✓ |
| Auto setup | ✓ | Spotify settings only |
| Media key protection | ✓ | – |
| Song lists with Echolot | ✓ | ✓ |

## How it works

On **Windows** Spotify plays into a silent virtual cable, and Mitschnitt plays it back to you. Only Spotify's own
audio is captured, before Windows mixes it with anything else. While Mitschnitt listens, Spotify's volume sits at
exactly 50 %. That keeps the Windows peak limiter away from loud masters. Halving a sample is exact, so Mitschnitt
doubles it again and gets the original back.

On **Linux** Mitschnitt connects straight to Spotify's PipeWire stream, before anything else is mixed in, and you
keep listening on your normal speakers or headphones. While Mitschnitt listens, Spotify's volume is held at 100 %
(use your system volume instead) and PipeWire runs at 44.1 kHz. Both go back to how they were when you stop.

On both systems an untouched 16 or 24 bit source arrives as exact 24 bit values. That is what Mitschnitt checks
for, sample by sample.

Recording happens in real time, so it is slower than a downloader. In return you get exactly the version Spotify
plays, not some upload that might be a different edit.

### Check a file yourself

The command line tool (`cargo build --release -p mitschnitt-cli`) runs the same check on any FLAC file, also on
ones Mitschnitt didn't make:

```
$ mitschnitt check Music/
CLEAN    Darude - Sandstorm.flac                3:45.000  peak  -0.10  16-bit, bit-exact
CORRUPT  Rick Astley - Never Gonna Give You Up  3:33.573  peak  -0.21  16-bit, bit-exact
           CORRUPT dropout 1:12.405 - 1:12.437 (31.5 ms of digital silence inside loud audio)
```

## Echolot

[Echolot](https://github.com/Zweisteinium/echolot) is my other project: a self-hosted music library that syncs
your playlists and likes and keeps only the right songs, in the best quality it can get. Connect Mitschnitt to it
and it becomes a recorder for the songs Echolot is still missing. Echolot hands out one song at a time, Mitschnitt
starts it in Spotify, checks that the right song plays, records it and uploads it. Only EXACT and FINE recordings
go up. A damaged one is played again, a lossy one is left out.

You can keep using your PC meanwhile. On Windows, when Spotify grabs the focus for a new song, Mitschnitt gives it
back. On Linux the desktop decides whether Spotify may come to the front.

## Coming from Spytify?

[Spytify](https://github.com/jwallet/spy-spotify) by jwallet has been the go-to Spotify recorder for years and
showed how well recording instead of downloading can work. Mitschnitt picks up some of its long open issues:
lossless FLAC and 24 bit instead of MP3, clean starts without the tail of the previous song, glitches and
dropouts caught instead of saved, and Linux.

## Limits

- Spotify is the only supported app. An app only gets a profile once its output has been proven untouched. If
  you ask nicely, I'll try Deezer, Apple Music or whatever your heart is after next <3
- Gapless albums, where one track flows into the next, are skipped. Without silence before a track there is no
  exact start.

## Your account, your risk

Streaming services don't like being recorded, and an account could get flagged for it. The risk seems small,
nothing has happened to mine so far, but it's there. Mitschnitt only records what plays on your own computer and
doesn't bypass any protection, so keep the recordings for your own listening. Using it is your decision, and I
take no responsibility for what happens to your account or your recordings.

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
