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
- **Linux:** `…_amd64.deb` for Debian 12, Ubuntu 22.04, Mint 21 or newer (6 MB, uses the system's WebKit). Otherwise
  `…_amd64.AppImage`, which needs glibc 2.36 or newer, so Fedora 37, Arch or anything newer. Both need PipeWire, which
  Ubuntu 22.04 doesn't use by default.
- **macOS:** planned, low priority.

For lossless recordings you need Spotify Premium with the streaming quality set to Lossless (up to 24 bit /
44.1 kHz).

## Features

- **Bit-exact.** Only the app's own stream is recorded, before the system mixes it with anything else. Nothing is
  resampled, normalized or turned up or down.
- **Live check.** Every sample is checked as it arrives. A lossless stream shows 100 %. A lossy stream, an
  equalizer or resampling shows up while the track is still playing.
- **Whole tracks.** A track is recorded from the point where its sound starts until Spotify's pause after it. The
  length Spotify reports only says where to look: it can be a few milliseconds short, and the sound after it is kept.
  A pause, skip, seek, buffering stall or dropout is detected and reported, so a broken recording never passes as a
  complete one.
- **Labels.** Every file gets one of five labels, with the reason. The written file is decoded again and compared
  with the recording before it gets its name.
  - **EXACT:** every sample with sound is there and bit-exact, from the first to the last. Only the digital silence
    at the very start and end can be a few milliseconds longer or shorter than in the original.
  - **FINE:** the same, except for Spotify's fade-in of about 10 ms at the start.
  - **LOSSY:** Spotify streamed the song lossy only, or its master was made from a lossy file (found in the
    spectrum). When you record yourself, it is saved as MP3, since a lossless file would only pretend to be more.
  - **ALT:** something is not original, or the end could not be proven (see [Limits](#limits)).
  - **BAD:** damaged, for example by a dropout.
- **Formats and tags.** FLAC in 16 or 24 bit, as the source was, WAV, or MP3 encoded from the verified recording.
  Title, artist, album, track number and cover are filled in, names follow your own template, and tracks you
  already have are skipped.
- **Auto setup.** The Setup tab checks Spotify's settings (quality, normalization, equalizer, crossfade, automix)
  and tells you what to switch. On Windows it also installs VB-Cable, matches its sample rate and routes Spotify
  into it, one click each. The goal is a setup without any manual steps.
- **Guards.** While Mitschnitt listens, it keeps away what would spoil a recording. The computer doesn't go to sleep,
  though the screen still turns off and locks. With media key protection switched on in the settings, media keys,
  headset buttons and Bluetooth earbuds that pause when you take them out are held back as well. The player's own
  pause button still works, and the keyboard hook behind this looks at media keys and nothing else.
- **Song lists.** Together with [Echolot](#echolot), Mitschnitt records a whole list of songs on its own.

|  | Windows | Linux |
|---|:---:|:---:|
| Recording, live check, labels, formats and tags | ✓ | ✓ |
| Song end proven by Spotify's pause after it | – | ✓ |
| Auto setup | ✓ | Spotify settings only |
| No sleep while listening | ✓ | ✓ |
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
starts it in Spotify, checks that the right song plays, records it and uploads it. EXACT and FINE recordings go
up, and so does one whose master was made from a lossy file: Echolot judges that with its own spectrum check. A
damaged recording is played again, a song Spotify streams lossy only is left out.

On Linux, while a song records, Mitschnitt switches Spotify to repeat that song. Otherwise Spotify might go on with
the album's next song without a pause, and the song's end could not be proven. Before repeating, Spotify pauses, and
Mitschnitt stops it there. Your repeat setting comes back after each song, when you take over and when you stop.

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
- Gapless albums, where one track flows into the next, can't be recorded in full. Without a pause before a track
  there is no exact start, so those tracks are skipped. Without a pause after a track its end can't be proven: on
  Linux it is saved as ALT ("end not verified"), because a skip in its last moments would look the same. Song lists
  with Echolot avoid this with Spotify's repeat (see [Echolot](#echolot)).

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
`make test-live` checks the capture against your running PipeWire without making a sound, and `make linux`
builds the AppImage and the .deb inside a Debian 12 container.

The code lives in three places. `crates/core` does the capture, the checks and the file writing, with the
platform specific parts in `crates/core/src/platform`. `crates/cli` is a command line front end with `check`,
`setup` and `listen`. `app` is the desktop app, built with Tauri 2 and Svelte 5.

## License

[GNU AGPL v3](LICENSE)
