use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use mitschnitt_core::analyze::{self, Master, Severity};

#[derive(Parser)]
#[command(name = "mitschnitt", version, about = "Bit-exact per-app audio capture for Windows")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List output devices with their engine sample rate
    Devices,
    /// Forensic check of recordings (files or folders): dropouts, Windows limiter, fades, length
    Check {
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
    /// Setup health checks for recording an app (read-only)
    Setup {
        #[arg(default_value = "spotify")]
        app: String,
    },
    /// Diagnostics: show which media keys reach Windows (keyboard, headset, earbuds), without blocking any
    Keys {
        #[arg(long, default_value_t = 60)]
        secs: u64,
    },
    /// Experiment: register Mitschnitt's own media session and show who receives media commands
    Smtc {
        #[arg(long, default_value_t = 90)]
        secs: u64,
    },
    /// Passive recording in the terminal, with a live check of how exact the audio arrives (Ctrl+C stops)
    Listen {
        #[arg(default_value = "spotify")]
        app: String,
        /// Don't write files - only show the live check
        #[arg(long)]
        dry: bool,
        /// Diagnostics: write the raw capture and an event timeline into this folder
        #[arg(long)]
        trace: Option<PathBuf>,
    },
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Devices => devices(),
        Command::Check { paths } => check(&paths),
        Command::Setup { app } => setup(&app),
        Command::Listen { app, dry, trace } => listen(app, dry, trace),
        Command::Keys { secs } => keys(secs),
        Command::Smtc { secs } => smtc(secs),
    }
}

#[cfg(windows)]
fn keys(secs: u64) -> ExitCode {
    use mitschnitt_core::keys::{KeyGuard, key_name};
    let guard = match KeyGuard::start() {
        Ok(g) => g,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    guard.log(true);
    println!("Watching media keys for {secs} s (nothing is blocked). Press keys, use headset buttons, take earbuds out...");
    let start = std::time::Instant::now();
    while start.elapsed().as_secs() < secs {
        std::thread::sleep(std::time::Duration::from_millis(100));
        for vk in guard.take_log() {
            println!("{:>7.2} s  {vk:#04x}  {}", start.elapsed().as_secs_f64(), key_name(vk));
        }
    }
    ExitCode::SUCCESS
}

#[cfg(windows)]
fn smtc(secs: u64) -> ExitCode {
    use mitschnitt_core::media;
    use mitschnitt_core::own_session::OwnSession;
    use mitschnitt_core::profile::SPOTIFY;
    mitschnitt_core::devices::com_init();
    let session = match OwnSession::start("rec. (experiment)", "Mitschnitt", None) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("Own media session registered as \"playing\" for {secs} s. Take an earbud out, put it back, press its button...");
    let start = std::time::Instant::now();
    let (mut current, mut spotify) = (String::new(), String::new());
    while start.elapsed().as_secs() < secs {
        session.pump();
        let t = start.elapsed().as_secs_f64();
        for b in session.take_pressed() {
            println!("{t:>7.2} s  MITSCHNITT received: {b}");
        }
        let now = media::manager().and_then(|m| m.GetCurrentSession()).and_then(|s| s.SourceAppUserModelId()).map(|s| s.to_string()).unwrap_or_else(|_| "-".into());
        if now != current {
            println!("{t:>7.2} s  current session: {now}");
            current = now;
        }
        let state = media::find(&SPOTIFY)
            .ok()
            .flatten()
            .and_then(|s| s.GetPlaybackInfo().and_then(|p| p.PlaybackStatus()).ok())
            .map(|s| format!("{s:?}"))
            .unwrap_or_else(|| "-".into());
        if state != spotify {
            println!("{t:>7.2} s  Spotify: {state}");
            spotify = state;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    drop(session);
    std::thread::sleep(std::time::Duration::from_millis(500));
    let after = media::manager().and_then(|m| m.GetCurrentSession()).and_then(|s| s.SourceAppUserModelId()).map(|s| s.to_string()).unwrap_or_else(|_| "-".into());
    println!("after closing our session, current session: {after}");
    ExitCode::SUCCESS
}

#[cfg(not(windows))]
fn smtc(_secs: u64) -> ExitCode {
    eprintln!("only available on Windows");
    ExitCode::FAILURE
}

#[cfg(not(windows))]
fn keys(_secs: u64) -> ExitCode {
    eprintln!("the media key guard is only available on Windows");
    ExitCode::FAILURE
}

#[cfg(windows)]
fn listen(app: String, dry: bool, trace: Option<PathBuf>) -> ExitCode {
    use mitschnitt_core::engine::{Config, Engine, Update};
    use mitschnitt_core::passive::Phase;
    use mitschnitt_core::profile::{self, Profile};
    use mitschnitt_core::settings::Settings;
    use std::sync::mpsc;

    let mut settings = Settings::load(&Settings::default_path());
    if dry {
        settings.out_dir = std::env::temp_dir().join("mitschnitt-dry");
        settings.keep_flagged = false;
    }
    let (tx, rx) = mpsc::channel::<Option<Update>>();
    let stop = tx.clone();
    if let Err(e) = ctrlc::set_handler(move || {
        let _ = stop.send(None);
    }) {
        eprintln!("error: {e}");
        return ExitCode::FAILURE;
    }
    // Unsupported apps work too, for experiments: the exact24 line shows whether an app is worth a profile.
    let profile = profile::get(&app).cloned().unwrap_or_else(|| Profile::adhoc(&app));
    let (engine, info) = match Engine::start(Config { profile, settings: settings.clone(), trace }, move |u| {
        let _ = tx.send(Some(u));
    }) {
        Ok(started) => started,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("Listening to {} on {} ({} Hz), app volume 50 %. Ctrl+C stops.", info.app, info.device, info.rate);
    println!("exact24 = share of audio that arrives as exact 24-bit values (100 % = untouched lossless source)\n");
    let mut error = None;
    while let Ok(Some(update)) = rx.recv() {
        match update {
            Update::Status { status } => {
                let Some(grid) = status.grid else { continue };
                let phase = match status.phase {
                    Phase::Idle => "idle     ",
                    Phase::Waiting => "waiting  ",
                    Phase::Recording => "RECORDING",
                };
                let peak = status.level[0].max(status.level[1]);
                let title = status.track.map(|t| format!("{} - {}", t.artist, t.title)).unwrap_or_default();
                println!(
                    "{phase} exact24 {:>6.2} %  16-bit grid {:>6.2} %  peak {:>6.1} dB  {title}",
                    grid.bit24 * 100.0,
                    grid.bit16 * 100.0,
                    20.0 * peak.max(1e-6).log10()
                );
            }
            Update::Started { track } => println!("--> recording {} - {}", track.artist, track.title),
            Update::Dropped { track, message, .. } => println!("--- skipped {} - {}: {message}", track.artist, track.title),
            Update::Saved { track, file, verdict, exact, issues, .. } => {
                println!("==> {} - {}: {verdict:?}, exact source: {exact}", track.artist, track.title);
                for i in issues {
                    println!("      {i}");
                }
                if let Some(f) = file.filter(|_| !dry) {
                    println!("      {f}");
                }
            }
            Update::Exists { track, file } => println!("--- {} - {}: already recorded ({file})", track.artist, track.title),
            Update::Failed { track, error } => println!("!!! {} - {}: {error}", track.artist, track.title),
            Update::KeyBlocked { count } => println!("--- media key blocked to protect the recording ({count} so far)"),
            Update::MonitorVolume { .. } => {}
            Update::MonitorMuted { muted } => println!("--- monitor {}", if muted { "muted: output device changed" } else { "unmuted" }),
            Update::Stopped { error: e } => {
                error = e;
                break;
            }
        }
    }
    drop(engine); // stops and restores the app volume
    match error {
        Some(e) => {
            eprintln!("stopped: {e}");
            ExitCode::FAILURE
        }
        None => {
            println!("Stopped, volume restored.");
            ExitCode::SUCCESS
        }
    }
}

#[cfg(not(windows))]
fn listen(_app: String, _dry: bool, _trace: Option<PathBuf>) -> ExitCode {
    eprintln!("listening is only available on Windows");
    ExitCode::FAILURE
}

#[cfg(windows)]
fn setup(app: &str) -> ExitCode {
    use mitschnitt_core::setup::{State, health};
    let Some(profile) = mitschnitt_core::profile::get(app) else {
        eprintln!("\"{app}\" is not a supported app");
        return ExitCode::FAILURE;
    };
    for c in health(profile) {
        let state = match c.state {
            State::Ok => "OK   ",
            State::Warn => "WARN ",
            State::Bad => "FIX  ",
            State::Check => "CHECK",
        };
        let fix = c.fix.map(|f| format!("  [fix: {f}]")).unwrap_or_default();
        println!("{state} {:<30} {}{fix}", c.title, c.detail);
    }
    ExitCode::SUCCESS
}

#[cfg(not(windows))]
fn setup(_app: &str) -> ExitCode {
    eprintln!("setup checks are only available on Windows");
    ExitCode::FAILURE
}

#[cfg(windows)]
fn devices() -> ExitCode {
    match mitschnitt_core::devices::render_endpoints() {
        Ok(list) => {
            for ep in list {
                println!("{:<55} {:>6} Hz  {} ch", ep.name, ep.sample_rate, ep.channels);
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(windows))]
fn devices() -> ExitCode {
    eprintln!("device listing is only available on Windows");
    ExitCode::FAILURE
}

fn check(paths: &[PathBuf]) -> ExitCode {
    let mut files: Vec<PathBuf> = Vec::new();
    for p in paths {
        if p.is_dir() {
            let mut found: Vec<PathBuf> = std::fs::read_dir(p)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.path())
                .filter(|f| f.extension().is_some_and(|x| x.eq_ignore_ascii_case("flac")))
                .collect();
            found.sort();
            files.extend(found);
        } else {
            files.push(p.clone());
        }
    }
    let mut worst = Severity::Info;
    for f in &files {
        match check_one(f) {
            Ok(v) => worst = worst.max(v),
            Err(e) => {
                println!("ERROR    {}: {e}", name(f));
                worst = Severity::Corrupt;
            }
        }
    }
    if worst >= Severity::Corrupt { ExitCode::FAILURE } else { ExitCode::SUCCESS }
}

fn check_one(path: &Path) -> Result<Severity, claxon::Error> {
    let audio = mitschnitt_core::flac::read(path)?;
    let r = analyze::analyze(&audio);
    let verdict = match r.verdict {
        Severity::Info => "CLEAN",
        Severity::Altered => "ALTERED",
        Severity::Corrupt => "CORRUPT",
    };
    let master = match r.master {
        Master::Bit16Exact => "16-bit, bit-exact",
        Master::Bit16Partly => "mostly 16-bit",
        Master::Bit24 => "24-bit source",
    };
    println!("{verdict:<8} {:<62} {}  peak {:6.2}  {master}", name(path), r.duration, r.peak_dbfs);
    for i in &r.issues {
        let tag = match i.severity {
            Severity::Info => "INFO",
            Severity::Altered => "ALTERED",
            Severity::Corrupt => "CORRUPT",
        };
        println!("           {tag:<7} {}", i.text);
    }
    Ok(r.verdict)
}

fn name(p: &Path) -> String {
    let s = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    s.chars().take(62).collect()
}
