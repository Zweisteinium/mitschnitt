use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use mitschnitt_core::analyze::{self, Master, Severity};

#[derive(Parser)]
#[command(name = "mitschnitt", version, about = "Bit-exact per-app audio capture")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List output devices with their engine sample rate
    Devices,
    /// Forensic check of recordings (files or folders): dropouts, limiter, fades, length
    Check {
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
    /// What an app is playing right now, as Mitschnitt sees it (track, position, ad detection)
    Now {
        #[arg(default_value = "spotify")]
        app: String,
    },
    /// Setup health checks for recording an app (read-only)
    Setup {
        #[arg(default_value = "spotify")]
        app: String,
    },
    /// Diagnostics (Windows): show which media keys reach Windows (keyboard, headset, earbuds), without blocking any
    Keys {
        #[arg(long, default_value_t = 60)]
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
    /// Record the songs of an Echolot export one after another (starts each in Spotify; Ctrl+C stops)
    Queue {
        /// The JSON from Echolot's export
        file: PathBuf,
    },
    /// Echolot: connect this PC as a recorder and work through its queue
    Echolot {
        #[command(subcommand)]
        action: EcholotAction,
    },
}

#[derive(Subcommand)]
enum EcholotAction {
    /// Connect to an Echolot (opens the approval page; paste the code or link it shows)
    Connect {
        /// Echolot's address, e.g. https://echolot.example.org
        url: String,
    },
    /// Show the connection and the current session
    Status,
    /// Work through Echolot's queue until Ctrl+C
    Run,
    /// Pause, resume or stop the current session
    Session {
        #[arg(value_parser = ["pause", "resume", "stop"])]
        action: String,
    },
    /// Disconnect this PC from Echolot
    Disconnect,
}

fn main() -> ExitCode {
    mitschnitt_core::log::init("cli");
    match Cli::parse().command {
        Command::Devices => devices(),
        Command::Check { paths } => check(&paths),
        Command::Now { app } => now(&app),
        Command::Setup { app } => setup(&app),
        Command::Listen { app, dry, trace } => listen(app, dry, trace),
        Command::Keys { secs } => keys(secs),
        Command::Queue { file } => queue::list(&file),
        Command::Echolot { action } => queue::echolot(action),
    }
}

fn fail(e: impl std::fmt::Display) -> ExitCode {
    eprintln!("error: {e}");
    ExitCode::FAILURE
}

#[cfg(windows)]
fn keys(secs: u64) -> ExitCode {
    use mitschnitt_core::platform::windows::keys::{KeyGuard, key_name};
    let guard = match KeyGuard::start() {
        Ok(g) => g,
        Err(e) => return fail(e),
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

#[cfg(not(windows))]
fn keys(_secs: u64) -> ExitCode {
    eprintln!("only available on Windows");
    ExitCode::FAILURE
}

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
        return fail(e);
    }
    // Unsupported apps work too, for experiments: the exact24 line shows whether an app is worth a profile.
    let profile = profile::get(&app).cloned().unwrap_or_else(|| Profile::adhoc(&app));
    let (engine, info) = match Engine::start(Config { profile, settings: settings.clone(), trace, queue: None }, move |u| {
        let _ = tx.send(Some(u));
    }) {
        Ok(started) => started,
        Err(e) => return fail(e),
    };
    let gain = mitschnitt_core::engine::GAIN * 100.0;
    println!("Listening to {} on {} ({} Hz), app volume {gain} %. Ctrl+C stops.", info.app, info.device, info.rate);
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

fn now(app: &str) -> ExitCode {
    use mitschnitt_core::{media, profile};
    mitschnitt_core::init_thread();
    let profile = profile::get(app).cloned().unwrap_or_else(|| profile::Profile::adhoc(app));
    let snapshot = media::find(&profile).map_err(|e| e.to_string()).and_then(|s| s.map(|s| media::Watcher::new(s).snapshot().map_err(|e| e.to_string())).transpose());
    match snapshot {
        Ok(Some(m)) => {
            let state = if m.playing { "playing" } else { "paused" };
            let clock = |ms: i64| format!("{}:{:02}", ms / 60_000, (ms / 1000) % 60);
            match m.track {
                Some(t) => {
                    let ad = if (profile.is_ad)(&t) { "  [ad, not recorded]" } else { "" };
                    println!("{} {state}: {} - {} ({})  {} / {}{ad}", profile.name, t.artist, t.title, t.album, clock(m.position_ms), clock(t.duration_ms as i64));
                }
                None => println!("{} {state}, no track", profile.name),
            }
            ExitCode::SUCCESS
        }
        Ok(None) => {
            eprintln!("{} shows no media session", profile.name);
            ExitCode::FAILURE
        }
        Err(e) => fail(e),
    }
}

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

fn devices() -> ExitCode {
    match mitschnitt_core::devices::render_endpoints() {
        Ok(list) => {
            for ep in list {
                println!("{:<55} {:>6} Hz  {} ch", ep.name, ep.sample_rate, ep.channels);
            }
            ExitCode::SUCCESS
        }
        Err(e) => fail(e),
    }
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

fn check_one(path: &Path) -> Result<Severity, String> {
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

/// Queue runs in the terminal: an exported list, or Echolot's queue.
mod queue {
    use std::path::Path;
    use std::process::ExitCode;
    use std::sync::{Arc, Mutex};

    use mitschnitt_core::echolot::{self, Client, Export, Pairing, store};
    use mitschnitt_core::engine::{Config, Engine, Queue, Update};
    use mitschnitt_core::profile::SPOTIFY;
    use mitschnitt_core::recorder::{self, Host, RecorderUpdate};
    use mitschnitt_core::settings::Settings;

    use super::EcholotAction;

    /// The terminal owns one engine, started for the queue.
    struct CliHost {
        engine: Mutex<Option<Engine>>,
        settings: Settings,
        last: Mutex<String>,
    }

    impl Host for CliHost {
        fn attach(&self, queue: Queue) -> Result<(), String> {
            let mut slot = self.engine.lock().map_err(|e| e.to_string())?;
            // An engine that stopped by itself (Spotify closed) gives the queue back: start a new one.
            let queue = match slot.as_ref() {
                Some(engine) => match engine.attach(queue) {
                    Ok(()) => return Ok(()),
                    Err(queue) => queue,
                },
                None => queue,
            };
            let config = Config { profile: SPOTIFY, settings: self.settings.clone(), trace: None, queue: Some(queue) };
            let (engine, info) = Engine::start(config, |u| match u {
                Update::Started { track } => println!("  --> recording {} - {}", track.artist, track.title),
                Update::Dropped { track, message, .. } => println!("  --- {} - {}: {message}", track.artist, track.title),
                Update::Saved { track, verdict, exact, .. } => println!("  ==> {} - {}: {verdict:?}, exact source: {exact}", track.artist, track.title),
                Update::Failed { track, error } => println!("  !!! {} - {}: {error}", track.artist, track.title),
                Update::Stopped { error: Some(e) } => println!("  engine stopped: {e}"),
                _ => {}
            })?;
            println!("Listening to {} on {} ({} Hz)", info.app, info.device, info.rate);
            *slot = Some(engine);
            Ok(())
        }

        fn detach(&self) {
            if let Ok(mut slot) = self.engine.lock() {
                slot.take(); // stops, restores volumes and the monitor
            }
        }

        fn settings(&self) -> Settings {
            self.settings.clone()
        }

        fn update(&self, update: RecorderUpdate) {
            match update {
                RecorderUpdate::Song { job, result, detail, .. } => {
                    let detail = if detail.is_empty() { String::new() } else { format!(": {detail}") };
                    println!("[{result}] {}{detail}", job.label());
                }
                RecorderUpdate::Snapshot { snapshot: s } => {
                    let session = s
                        .session
                        .map(|x| format!("{} [{}{}] {}/{} filed {} skipped {} failed {}", x.title, x.state, if x.reason.is_empty() { String::new() } else { format!(": {}", x.reason) }, x.done, x.total, x.filed, x.skipped, x.failed))
                        .unwrap_or_else(|| "no session".into());
                    let error = s.error.map(|e| format!(" | {e}")).unwrap_or_default();
                    let line = format!("{} {}@{} ({}) {:?} {} | {session}{error}", s.connection, s.user, s.host, s.device, s.state, s.detail);
                    if let Ok(mut last) = self.last.lock()
                        && *last != line
                    {
                        println!("{line}");
                        *last = line;
                    }
                }
            }
        }
    }

    fn host() -> Arc<CliHost> {
        Arc::new(CliHost { engine: Mutex::new(None), settings: Settings::load(&Settings::default_path()), last: Mutex::new(String::new()) })
    }

    /// Wait for Ctrl+C (or, for a list, its end).
    fn wait(recorder: &recorder::Recorder, until_done: bool) {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = ctrlc::set_handler(move || {
            let _ = tx.send(());
        });
        loop {
            if rx.recv_timeout(std::time::Duration::from_millis(500)).is_ok() {
                recorder.stop();
                break;
            }
            if until_done && recorder.snapshot().connection.is_empty() {
                break;
            }
        }
    }

    pub fn list(file: &Path) -> ExitCode {
        mitschnitt_core::init_thread();
        let export = match Export::load(file) {
            Ok(e) => e,
            Err(e) => return super::fail(e),
        };
        let host = host();
        let out = host.settings.out_dir.clone();
        println!("{} songs from \"{}\"; recordings go to {}", export.jobs().len(), export.title, out.join(recorder::PENDING_DIR).display());
        let recorder = recorder::run_list(export, SPOTIFY, out, host.clone());
        wait(&recorder, true);
        host.detach();
        ExitCode::SUCCESS
    }

    fn connected() -> Result<(store::Saved, Client), String> {
        let (saved, token) = store::load().ok_or("not connected (mitschnitt echolot connect)")?;
        let client = Client::new(&saved.base, &token);
        Ok((saved, client))
    }

    pub fn echolot(action: EcholotAction) -> ExitCode {
        mitschnitt_core::init_thread();
        let result = match action {
            EcholotAction::Connect { url } => connect(&url),
            EcholotAction::Status => connected().and_then(|(saved, client)| {
                let me = client.me().map_err(|e| e.to_string())?;
                println!("Connected to {} as {} (device \"{}\", Echolot {})", saved.base, me.user, me.recorder.name, me.echolot);
                println!("Session: {:?}", me.session);
                Ok(())
            }),
            EcholotAction::Run => connected().map(|(saved, client)| {
                let host = host();
                println!("Working through Echolot's queue (Ctrl+C stops)");
                let out = host.settings.out_dir.clone();
                let recorder = recorder::connect(client, saved, SPOTIFY, out, host.clone());
                wait(&recorder, false);
                host.detach();
            }),
            EcholotAction::Session { action } => connected().and_then(|(_, client)| {
                let session = client.session(&action).map_err(|e| e.to_string())?;
                println!("{session:?}");
                Ok(())
            }),
            EcholotAction::Disconnect => {
                if let Ok((_, client)) = connected() {
                    let _ = client.disconnect();
                }
                store::forget();
                println!("Disconnected.");
                Ok(())
            }
        };
        match result {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => super::fail(e),
        }
    }

    fn connect(url: &str) -> Result<(), String> {
        let pairing = Pairing::new(url);
        let name = echolot::pc_name();
        let page = pairing.url(&name);
        println!("Echolot will be able to start Spotify on this PC and put recordings into your library.");
        println!("Approve in the browser: {page}");
        let _ = mitschnitt_core::desktop::open(&page);
        println!("Paste the code (or the mitschnitt://connected link) Echolot shows:");
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).map_err(|e| e.to_string())?;
        let code = match echolot::parse_link(&line) {
            Some(echolot::Link::Connected { code: Some(code), state, .. }) if state == pairing.state => code,
            Some(echolot::Link::Connected { error: Some(e), .. }) => return Err(format!("not connected: {e}")),
            Some(_) => return Err("that link belongs to another sign-in".into()),
            None => line.trim().to_owned(),
        };
        let connected = pairing.claim(&code, &name).map_err(|e| e.to_string())?;
        let saved = store::Saved { base: pairing.base.clone(), user: connected.user.clone(), recorder: connected.recorder.clone() };
        store::save(&saved, &connected.token)?;
        println!("Connected · {}@{} (device \"{}\")", connected.user, echolot::host(&pairing.base), connected.recorder.name);
        Ok(())
    }
}
