//! Tapline: run a sequence of clicks and keys, repeat it, and optionally start it
//! automatically when a spot on screen turns a chosen color.

mod input;
mod pixel;

use enigo::{Button, Coordinate, Direction, Enigo, Key, Keyboard, Mouse, Settings as EnigoSettings};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder, WindowEvent, Wry};
use tauri_plugin_autostart::ManagerExt as _;
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Shortcut, ShortcutState};
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_opener::OpenerExt;

// ---------------------------------------------------------------- settings

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
struct Step {
    kind: String, // "click" | "key" | "text" | "wait"
    x: Option<i32>,
    y: Option<i32>,
    button: String, // "left" | "right" | "double"
    key: String,
    mods: Vec<String>,
    text: String,
    secs: f64,
    delay: f64, // pause after this step
    travel_ms: u32, // time for the mouse to glide to the spot; 0 = jump
}

impl Default for Step {
    fn default() -> Self {
        Step {
            kind: "click".into(),
            x: None,
            y: None,
            button: "left".into(),
            key: "Enter".into(),
            mods: vec![],
            text: String::new(),
            secs: 1.0,
            delay: 0.3,
            travel_ms: 0,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
struct Watch {
    x: Option<i32>,
    y: Option<i32>,
    w: i32,
    h: i32,
    color: Option<[u8; 3]>, // 1.1 single-spot trigger, kept for older settings
    grid: Vec<[u8; 3]>,     // small picture of the snapshot
    gw: u32,
    gh: u32,
    mode: String,   // "match" = looks like the snapshot, "change" = differs from it
    tolerance: u32, // % of the area that may differ, 1..100
    cooldown: f64,  // seconds
}

impl Default for Watch {
    fn default() -> Self {
        Watch { x: None, y: None, w: 1, h: 1, color: None, grid: vec![], gw: 0, gh: 0, mode: "match".into(), tolerance: 12, cooldown: 10.0 }
    }
}

impl Watch {
    fn ready(&self) -> bool {
        self.x.is_some() && self.y.is_some() && !self.grid.is_empty() && self.grid.len() == (self.gw * self.gh) as usize
    }
    /// Share of the area that differs from the snapshot right now, and whether that triggers.
    fn probe(&self) -> Result<(f64, bool), String> {
        let (x, y) = (self.x.unwrap_or(0), self.y.unwrap_or(0));
        let (px, pw, ph) = pixel::read_area(x, y, self.w.max(1), self.h.max(1))?;
        let (now, gw, gh) = pixel::fingerprint(&px, pw, ph, 40);
        let d = if gw == self.gw && gh == self.gh { pixel::diff(&now, &self.grid) } else { 1.0 };
        let limit = self.tolerance as f64 / 100.0;
        let hit = if self.mode == "change" { d > limit } else { d <= limit };
        Ok((d, hit))
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
struct Preset {
    id: String,
    name: String,
    steps: Vec<Step>,
    rounds: u32,
    round_wait: f64,
    watch: Watch,
}

impl Default for Preset {
    fn default() -> Self {
        Preset {
            id: "p1".into(),
            name: "New leads".into(),
            steps: vec![
                Step::default(),
                Step { kind: "key".into(), key: "Enter".into(), ..Step::default() },
            ],
            rounds: 2,
            round_wait: 2.0,
            watch: Watch::default(),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    presets: Vec<Preset>,
    active: String,
    hotkey: String,
    sound: bool,
    notify: bool,
    tray_on_close: bool,
    corner_stop: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            presets: vec![Preset::default()],
            active: "p1".into(),
            hotkey: "F8".into(),
            sound: true,
            notify: true,
            tray_on_close: true,
            corner_stop: true,
        }
    }
}

impl Settings {
    fn fix(&mut self) {
        if self.presets.is_empty() {
            self.presets.push(Preset::default());
        }
        if !self.presets.iter().any(|p| p.id == self.active) {
            self.active = self.presets[0].id.clone();
        }
        for p in &mut self.presets {
            p.rounds = p.rounds.clamp(1, 999);
            p.round_wait = p.round_wait.clamp(0.0, 600.0);
            p.watch.tolerance = p.watch.tolerance.clamp(1, 100);
            p.watch.cooldown = p.watch.cooldown.clamp(1.0, 3600.0);
            p.watch.w = p.watch.w.clamp(1, 1600);
            p.watch.h = p.watch.h.clamp(1, 1200);
            if p.watch.mode != "change" {
                p.watch.mode = "match".into();
            }
            // settings from 1.1: one spot and one color
            if p.watch.grid.is_empty() {
                if let (Some(c), Some(_)) = (p.watch.color, p.watch.x) {
                    p.watch.grid = vec![c];
                    p.watch.gw = 1;
                    p.watch.gh = 1;
                    p.watch.w = 1;
                    p.watch.h = 1;
                }
            }
            for s in &mut p.steps {
                s.delay = s.delay.clamp(0.0, 60.0);
                s.secs = s.secs.clamp(0.0, 600.0);
                s.travel_ms = s.travel_ms.min(5000);
            }
        }
        if self.hotkey.trim().is_empty() {
            self.hotkey = "F8".into();
        }
    }
    fn active_preset(&self) -> Preset {
        self.presets.iter().find(|p| p.id == self.active).cloned().unwrap_or_default()
    }
}

/// Settings file from Tapline 1.0: one click spot, rounds and wait.
#[derive(Deserialize)]
struct LegacySettings {
    x: Option<i32>,
    y: Option<i32>,
    count: Option<u32>,
    wait: Option<f64>,
}

#[derive(Clone, Serialize, Deserialize)]
struct LogEntry {
    at: u64,
    preset: String,
    trigger: String,
    done: u32,
    total: u32,
    result: String,
}

// ---------------------------------------------------------------- shared state

struct Shared {
    settings: Mutex<Settings>,
    running: AtomicBool,
    stop: AtomicBool,
    watching: AtomicBool,
    watch_gen: AtomicU64,
    log: Mutex<Vec<LogEntry>>,
    dir: PathBuf,
    hotkey: Mutex<Option<Shortcut>>,
    quiet_until: Mutex<Instant>,
    recording: AtomicBool,
    rec_stop: AtomicBool, // ignore our own key presses reaching the global shortcuts
    tray: Mutex<Option<(MenuItem<Wry>, MenuItem<Wry>)>>,
}

type Sh<'a> = State<'a, Arc<Shared>>;

impl Shared {
    fn save(&self) {
        let _ = std::fs::create_dir_all(&self.dir);
        if let Ok(s) = self.settings.lock() {
            let _ = std::fs::write(self.dir.join("settings.v2.json"), serde_json::to_string_pretty(&*s).unwrap_or_default());
        }
    }
    fn save_log(&self) {
        let _ = std::fs::create_dir_all(&self.dir);
        if let Ok(l) = self.log.lock() {
            let _ = std::fs::write(self.dir.join("log.json"), serde_json::to_string(&*l).unwrap_or_default());
        }
    }
    fn add_log(&self, e: LogEntry) {
        if let Ok(mut l) = self.log.lock() {
            l.push(e);
            let n = l.len();
            if n > 300 {
                l.drain(0..n - 300);
            }
        }
        self.save_log();
    }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "mac"
    } else if cfg!(target_os = "windows") {
        "win"
    } else {
        "other"
    }
}

fn esc() -> Shortcut {
    Shortcut::new(None, Code::Escape)
}

fn new_enigo() -> Result<Enigo, String> {
    Enigo::new(&EnigoSettings::default()).map_err(|e| match e {
        enigo::NewConError::NoPermission => {
            "Allow Tapline in System Settings, Privacy & Security, Accessibility".to_string()
        }
        other => format!("Could not start: {other}"),
    })
}

fn key_from(name: &str) -> Option<Key> {
    Some(match name {
        "Enter" => Key::Return,
        "Tab" => Key::Tab,
        "Space" => Key::Space,
        "Backspace" => Key::Backspace,
        "Delete" => Key::Delete,
        "Escape" => Key::Escape,
        "Up" => Key::UpArrow,
        "Down" => Key::DownArrow,
        "Left" => Key::LeftArrow,
        "Right" => Key::RightArrow,
        "Home" => Key::Home,
        "End" => Key::End,
        "PageUp" => Key::PageUp,
        "PageDown" => Key::PageDown,
        "F1" => Key::F1,
        "F2" => Key::F2,
        "F3" => Key::F3,
        "F4" => Key::F4,
        "F5" => Key::F5,
        "F6" => Key::F6,
        "F7" => Key::F7,
        "F8" => Key::F8,
        "F9" => Key::F9,
        "F10" => Key::F10,
        "F11" => Key::F11,
        "F12" => Key::F12,
        other => {
            let mut chars = other.chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            Key::Unicode(c.to_ascii_lowercase())
        }
    })
}

fn mod_from(name: &str) -> Option<Key> {
    Some(match name {
        "Ctrl" => Key::Control,
        "Shift" => Key::Shift,
        "Alt" => Key::Alt,
        "Cmd" => Key::Meta,
        _ => return None,
    })
}

// ---------------------------------------------------------------- tray

fn refresh_tray(app: &AppHandle) {
    let sh = app.state::<Arc<Shared>>();
    let running = sh.running.load(Ordering::SeqCst);
    let watching = sh.watching.load(Ordering::SeqCst);
    let name = sh.settings.lock().map(|s| s.active_preset().name).unwrap_or_default();
    if let Ok(t) = sh.tray.lock() {
        if let Some((run, watch)) = t.as_ref() {
            let _ = run.set_text(if running { "Stop".to_string() } else { format!("Run {name}") });
            let _ = watch.set_text(if watching { "Turn Watch off" } else { "Turn Watch on" });
        }
    }
    if let Some(tray) = app.tray_by_id("main") {
        let tip = match (running, watching) {
            (true, _) => format!("Tapline: running {name}"),
            (false, true) => format!("Tapline: watching for {name}"),
            _ => "Tapline: ready".into(),
        };
        let _ = tray.set_tooltip(Some(tip));
    }
}

// ---------------------------------------------------------------- run engine

#[derive(Clone, Serialize)]
struct Progress {
    kind: &'static str,
    text: String,
    done: u32,
    total: u32,
    finished: bool,
    trigger: String,
}

fn start_run(app: &AppHandle, trigger: &str) -> Result<(), String> {
    let sh = app.state::<Arc<Shared>>().inner().clone();
    let (preset, corner) = {
        let s = sh.settings.lock().unwrap();
        (s.active_preset(), s.corner_stop)
    };
    if preset.steps.is_empty() {
        return Err("Add at least one step first".into());
    }
    for (i, st) in preset.steps.iter().enumerate() {
        if st.kind == "click" && (st.x.is_none() || st.y.is_none()) {
            return Err(format!("Set the click spot for step {}", i + 1));
        }
        if st.kind == "key" && key_from(&st.key).is_none() {
            return Err(format!("Step {} has an unknown key", i + 1));
        }
    }
    if sh.running.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    sh.stop.store(false, Ordering::SeqCst);
    let _ = app.global_shortcut().register(esc());
    refresh_tray(app);

    let app = app.clone();
    let trigger = trigger.to_string();
    thread::spawn(move || {
        let total = preset.rounds;
        let emit = |kind: &'static str, text: String, done: u32, finished: bool| {
            let _ = app.emit("progress", Progress { kind, text, done, total, finished, trigger: trigger.clone() });
        };
        let mut done = 0u32;
        let outcome: Result<bool, String> = (|| {
            let mut enigo = new_enigo()?;
            let screen = enigo.main_display().unwrap_or((0, 0));
            let placed: std::cell::Cell<Option<(i32, i32)>> = std::cell::Cell::new(None);
            let stop_now = |enigo: &Enigo| -> bool {
                if sh.stop.load(Ordering::SeqCst) {
                    return true;
                }
                if corner && screen.0 > 0 {
                    if let Ok((x, y)) = enigo.location() {
                        let (w, h) = screen;
                        let near = |a: i32, b: i32| (a - b).abs() <= 2;
                        let in_corner = (near(x, 0) || near(x, w - 1)) && (near(y, 0) || near(y, h - 1));
                        // ignore it when Tapline itself put the mouse there
                        let ours = placed.get().map(|(px, py)| near(x, px) && near(y, py)).unwrap_or(false);
                        if in_corner && !ours {
                            sh.stop.store(true, Ordering::SeqCst);
                            return true;
                        }
                    }
                }
                false
            };
            let pause = |enigo: &Enigo, secs: f64| -> bool {
                let end = Instant::now() + Duration::from_secs_f64(secs.max(0.0));
                loop {
                    if stop_now(enigo) {
                        return false;
                    }
                    let now = Instant::now();
                    if now >= end {
                        return true;
                    }
                    thread::sleep((end - now).min(Duration::from_millis(40)));
                }
            };

            for r in 0..total {
                for (i, st) in preset.steps.iter().enumerate() {
                    if stop_now(&enigo) {
                        return Ok(false);
                    }
                    emit("busy", format!("Round {} of {} · step {}", r + 1, total, i + 1), done, false);
                    let typing = st.kind == "key" || st.kind == "text";
                    if typing {
                        *sh.quiet_until.lock().unwrap() = Instant::now() + Duration::from_secs(30);
                    }
                    let res: Result<(), enigo::InputError> = match st.kind.as_str() {
                        "click" => (|| {
                            let (tx, ty) = (st.x.unwrap_or(0), st.y.unwrap_or(0));
                            if st.travel_ms > 0 {
                                let (sx, sy) = enigo.location().unwrap_or((tx, ty));
                                let n = (st.travel_ms / 8).max(1);
                                let each = Duration::from_millis(st.travel_ms as u64) / n;
                                for k in 1..=n {
                                    if sh.stop.load(Ordering::SeqCst) {
                                        break;
                                    }
                                    let t = k as f64 / n as f64;
                                    let e = t * t * (3.0 - 2.0 * t); // ease in and out
                                    let x = sx as f64 + (tx - sx) as f64 * e;
                                    let y = sy as f64 + (ty - sy) as f64 * e;
                                    enigo.move_mouse(x.round() as i32, y.round() as i32, Coordinate::Abs)?;
                                    thread::sleep(each);
                                }
                            }
                            enigo.move_mouse(tx, ty, Coordinate::Abs)?;
                            placed.set(Some((tx, ty)));
                            thread::sleep(Duration::from_millis(40));
                            match st.button.as_str() {
                                "right" => enigo.button(Button::Right, Direction::Click)?,
                                "double" => {
                                    enigo.button(Button::Left, Direction::Click)?;
                                    thread::sleep(Duration::from_millis(60));
                                    enigo.button(Button::Left, Direction::Click)?;
                                }
                                _ => enigo.button(Button::Left, Direction::Click)?,
                            }
                            Ok(())
                        })(),
                        "key" => (|| {
                            let mods: Vec<Key> = st.mods.iter().filter_map(|m| mod_from(m)).collect();
                            for m in &mods {
                                enigo.key(*m, Direction::Press)?;
                            }
                            let r = enigo.key(key_from(&st.key).unwrap_or(Key::Return), Direction::Click);
                            for m in mods.iter().rev() {
                                let _ = enigo.key(*m, Direction::Release);
                            }
                            r
                        })(),
                        "text" => enigo.text(&st.text),
                        "wait" => {
                            if !pause(&enigo, st.secs) {
                                return Ok(false);
                            }
                            Ok(())
                        }
                        _ => Ok(()),
                    };
                    if typing {
                        *sh.quiet_until.lock().unwrap() = Instant::now() + Duration::from_millis(300);
                    }
                    res.map_err(|e| format!("Could not do step {}: {e}", i + 1))?;
                    if st.kind != "wait" && !pause(&enigo, st.delay) {
                        return Ok(false);
                    }
                }
                done += 1;
                if r + 1 < total {
                    let w = preset.round_wait;
                    emit("busy", format!("Waiting {} sec", trim(w)), done, false);
                    if !pause(&enigo, w) {
                        return Ok(false);
                    }
                }
            }
            Ok(true)
        })();

        let (kind, text, result) = match outcome {
            Ok(true) => ("done", "Done".to_string(), "done"),
            Ok(false) => ("error", format!("Stopped after {done} of {total}"), "stopped"),
            Err(e) => ("error", e, "error"),
        };
        emit(kind, text, done, true);
        sh.add_log(LogEntry { at: now_ms(), preset: preset.name.clone(), trigger: trigger.clone(), done, total, result: result.into() });
        let _ = app.emit("log", ());
        sh.running.store(false, Ordering::SeqCst);
        let _ = app.global_shortcut().unregister(esc());
        refresh_tray(&app);
    });
    Ok(())
}

fn trim(v: f64) -> String {
    let s = format!("{v:.1}");
    s.trim_end_matches(".0").to_string()
}

// ---------------------------------------------------------------- watch mode

#[derive(Clone, Serialize)]
struct WatchStatus {
    on: bool,
    state: &'static str, // armed | running | reset | cooldown | off | error
    text: String,
    diff: Option<f64>,
    matched: bool,
}

fn emit_watch(app: &AppHandle, s: WatchStatus) {
    let _ = app.emit("watch", s);
}

fn start_watch(app: &AppHandle) -> Result<(), String> {
    let sh = app.state::<Arc<Shared>>().inner().clone();
    let preset = sh.settings.lock().unwrap().active_preset();
    let w = &preset.watch;
    if !w.ready() {
        return Err("Select the area to watch first".into());
    }
    if !pixel::has_permission(true) {
        return Err("Allow Tapline under Privacy & Security, Screen Recording, then reopen it".into());
    }
    w.probe()?;

    let gen = sh.watch_gen.fetch_add(1, Ordering::SeqCst) + 1;
    sh.watching.store(true, Ordering::SeqCst);
    refresh_tray(app);
    let app = app.clone();
    let preset_id = preset.id.clone();

    thread::spawn(move || {
        let alive = || sh.watching.load(Ordering::SeqCst) && sh.watch_gen.load(Ordering::SeqCst) == gen;
        let mut hits = 0u32;
        let mut misses = 0u32;
        let mut errors = 0u32;
        let mut phase = "armed";
        let mut cool_until = Instant::now();

        while alive() {
            let (w, sound_notify, name) = {
                let s = sh.settings.lock().unwrap();
                match s.presets.iter().find(|p| p.id == preset_id) {
                    Some(p) => (p.watch.clone(), s.notify, p.name.clone()),
                    None => break,
                }
            };
            if !w.ready() {
                break;
            }
            match w.probe() {
                Err(e) => {
                    errors += 1;
                    if errors >= 8 {
                        emit_watch(&app, WatchStatus { on: false, state: "error", text: e, diff: None, matched: false });
                        break;
                    }
                }
                Ok((d, matched)) => {
                    errors = 0;
                    let running = sh.running.load(Ordering::SeqCst);
                    match phase {
                        "armed" => {
                            hits = if matched { hits + 1 } else { 0 };
                            if hits >= 2 && !running {
                                hits = 0;
                                match start_run(&app, "watch") {
                                    Ok(()) => {
                                        phase = "running";
                                        let _ = app.emit("fired", name.clone());
                                        if sound_notify {
                                            let _ = app.notification().builder().title("Tapline").body(format!("Started {name}")).show();
                                        }
                                    }
                                    Err(e) => {
                                        emit_watch(&app, WatchStatus { on: false, state: "error", text: e, diff: Some(d), matched });
                                        break;
                                    }
                                }
                            }
                        }
                        "running" => {
                            if !running {
                                phase = "reset";
                                misses = 0;
                            }
                        }
                        "reset" => {
                            misses = if matched { 0 } else { misses + 1 };
                            if misses >= 2 {
                                phase = "cooldown";
                                cool_until = Instant::now() + Duration::from_secs_f64(w.cooldown);
                            }
                        }
                        _ => {
                            // cooldown
                            if Instant::now() >= cool_until {
                                phase = "armed";
                                hits = 0;
                            }
                        }
                    }
                    let text = match phase {
                        "armed" => if w.mode == "change" { "Watching for a change".to_string() } else { "Watching for the snapshot".to_string() },
                        "running" => format!("Running {name}"),
                        "reset" => "Waiting for the area to go back to normal".to_string(),
                        _ => format!(
                            "Ready again in {} sec",
                            cool_until.saturating_duration_since(Instant::now()).as_secs() + 1
                        ),
                    };
                    let state = match phase {
                        "armed" => "armed",
                        "running" => "running",
                        "reset" => "reset",
                        _ => "cooldown",
                    };
                    emit_watch(&app, WatchStatus { on: true, state, text, diff: Some(d), matched });
                }
            }
            thread::sleep(Duration::from_millis(250));
        }
        // only the newest watcher may switch Watch off
        if sh.watch_gen.load(Ordering::SeqCst) == gen {
            let was_on = sh.watching.swap(false, Ordering::SeqCst);
            if was_on {
                emit_watch(&app, WatchStatus { on: false, state: "off", text: "Watch is off".into(), diff: None, matched: false });
            }
            refresh_tray(&app);
        }
    });
    Ok(())
}

fn stop_watch(app: &AppHandle) {
    let sh = app.state::<Arc<Shared>>();
    sh.watch_gen.fetch_add(1, Ordering::SeqCst);
    if sh.watching.swap(false, Ordering::SeqCst) {
        emit_watch(app, WatchStatus { on: false, state: "off", text: "Watch is off".into(), diff: None, matched: false });
    }
    refresh_tray(app);
}

fn stop_everything(app: &AppHandle) {
    let sh = app.state::<Arc<Shared>>();
    sh.stop.store(true, Ordering::SeqCst);
    sh.rec_stop.store(true, Ordering::SeqCst);
    stop_watch(app);
}

fn toggle_run(app: &AppHandle) {
    let sh = app.state::<Arc<Shared>>();
    if sh.recording.load(Ordering::SeqCst) {
        sh.rec_stop.store(true, Ordering::SeqCst);
    } else if sh.running.load(Ordering::SeqCst) {
        sh.stop.store(true, Ordering::SeqCst);
    } else if let Err(e) = start_run(app, "hotkey") {
        let _ = app.emit("progress", Progress { kind: "error", text: e, done: 0, total: 0, finished: true, trigger: "hotkey".into() });
    }
}

fn register_hotkey(app: &AppHandle, text: &str) -> Result<Shortcut, String> {
    let sc = Shortcut::from_str(text).map_err(|_| format!("\"{text}\" is not a key Tapline can use"))?;
    if sc == esc() {
        return Err("Escape is reserved for the emergency stop".into());
    }
    app.global_shortcut()
        .register(sc)
        .map_err(|_| format!("{text} is already used by another app. Pick a different key."))?;
    Ok(sc)
}

// ---------------------------------------------------------------- commands

#[derive(Serialize)]
struct AllOut {
    settings: Settings,
    platform: &'static str,
    version: String,
    autostart: bool,
    running: bool,
    watching: bool,
    log: Vec<LogEntry>,
    hotkey_ok: bool,
}

#[tauri::command]
fn get_all(app: AppHandle, sh: Sh) -> AllOut {
    AllOut {
        settings: sh.settings.lock().unwrap().clone(),
        platform: platform(),
        version: app.package_info().version.to_string(),
        autostart: app.autolaunch().is_enabled().unwrap_or(false),
        running: sh.running.load(Ordering::SeqCst),
        watching: sh.watching.load(Ordering::SeqCst),
        log: sh.log.lock().unwrap().clone(),
        hotkey_ok: sh.hotkey.lock().unwrap().is_some(),
    }
}

#[tauri::command]
fn save_settings(app: AppHandle, sh: Sh, mut settings: Settings) -> Result<Settings, String> {
    settings.fix();
    let (old_key, old_active) = {
        let s = sh.settings.lock().unwrap();
        (s.hotkey.clone(), s.active.clone())
    };
    let mut err = None;
    if settings.hotkey != old_key || sh.hotkey.lock().unwrap().is_none() {
        if let Some(old) = sh.hotkey.lock().unwrap().take() {
            let _ = app.global_shortcut().unregister(old);
        }
        match register_hotkey(&app, &settings.hotkey) {
            Ok(sc) => *sh.hotkey.lock().unwrap() = Some(sc),
            Err(e) => {
                // put the previous key back
                if let Ok(sc) = register_hotkey(&app, &old_key) {
                    *sh.hotkey.lock().unwrap() = Some(sc);
                }
                settings.hotkey = old_key.clone();
                err = Some(e);
            }
        }
    }
    if settings.active != old_active && sh.watching.load(Ordering::SeqCst) {
        stop_watch(&app);
    }
    *sh.settings.lock().unwrap() = settings.clone();
    sh.save();
    refresh_tray(&app);
    match err {
        Some(e) => Err(e),
        None => Ok(settings),
    }
}

#[tauri::command]
fn capture_position() -> Result<(i32, i32), String> {
    new_enigo()?.location().map_err(|e| e.to_string())
}

#[derive(Serialize)]
struct Probe {
    diff: f64,
    matched: bool,
}

#[tauri::command]
fn watch_probe(sh: Sh) -> Result<Probe, String> {
    if !pixel::has_permission(false) {
        return Err("Screen Recording permission needed".into());
    }
    let w = sh.settings.lock().unwrap().active_preset().watch;
    if !w.ready() {
        return Err("No area selected".into());
    }
    let (diff, matched) = w.probe()?;
    Ok(Probe { diff, matched })
}

/// Save one spot under the mouse as a 1 x 1 snapshot.
#[tauri::command]
fn watch_spot(app: AppHandle, sh: Sh) -> Result<Watch, String> {
    let (x, y) = new_enigo()?.location().map_err(|e| e.to_string())?;
    if !pixel::has_permission(true) {
        return Err("Allow Tapline under Privacy & Security, Screen Recording, then reopen it".into());
    }
    let c = pixel::read(x, y)?;
    let w = {
        let mut s = sh.settings.lock().unwrap();
        let id = s.active.clone();
        let p = s.presets.iter_mut().find(|p| p.id == id).ok_or("No preset")?;
        p.watch = Watch { x: Some(x), y: Some(y), w: 1, h: 1, color: Some(c), grid: vec![c], gw: 1, gh: 1, ..p.watch.clone() };
        p.watch.clone()
    };
    sh.save();
    let _ = app.emit("snipped", ());
    Ok(w)
}

// ---------------------------------------------------------------- snip an area

#[tauri::command]
async fn start_snip(app: AppHandle) -> Result<(), String> {
    if !pixel::has_permission(true) {
        return Err("Allow Tapline under Privacy & Security, Screen Recording, then reopen it".into());
    }
    if let Some(old) = app.get_webview_window("snip") {
        let _ = old.close();
    }
    let cur = app.cursor_position().map_err(|e| e.to_string())?;
    let mon = app
        .monitor_from_point(cur.x, cur.y)
        .ok()
        .flatten()
        .or_else(|| app.primary_monitor().ok().flatten())
        .ok_or("No screen found")?;
    let scale = mon.scale_factor();
    let (pos, size) = (mon.position(), mon.size());
    if let Some(main) = app.get_webview_window("main") {
        let _ = main.hide();
    }
    thread::sleep(Duration::from_millis(150));
    WebviewWindowBuilder::new(&app, "snip", WebviewUrl::App("snip.html".into()))
        .title("Select area")
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .shadow(false)
        .position(pos.x as f64 / scale, pos.y as f64 / scale)
        .inner_size(size.width as f64 / scale, size.height as f64 / scale)
        .focused(true)
        .build()
        .map_err(|e| {
            show_main(&app);
            format!("Could not open the area picker: {e}")
        })?;
    Ok(())
}

#[tauri::command]
fn snip_cancel(app: AppHandle) {
    if let Some(w) = app.get_webview_window("snip") {
        let _ = w.close();
    }
    show_main(&app);
}

/// Called by the area picker with the box in its own (CSS pixel) coordinates.
#[tauri::command]
fn snip_done(app: AppHandle, x: f64, y: f64, w: f64, h: f64) -> Result<(), String> {
    let win = app.get_webview_window("snip").ok_or("Area picker closed")?;
    let pos = win.outer_position().map_err(|e| e.to_string())?;
    let scale = win.scale_factor().map_err(|e| e.to_string())?;
    let _ = win.close();
    // Windows works in physical pixels, macOS in points
    let (sx, sy, sw, sh_) = if cfg!(target_os = "macos") {
        (pos.x as f64 / scale + x, pos.y as f64 / scale + y, w, h)
    } else {
        (pos.x as f64 + x * scale, pos.y as f64 + y * scale, w * scale, h * scale)
    };
    let (ax, ay) = (sx.round() as i32, sy.round() as i32);
    let (aw, ah) = ((sw.round() as i32).clamp(1, 1600), (sh_.round() as i32).clamp(1, 1200));
    let app2 = app.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(250)); // let the picker disappear first
        let sh = app2.state::<Arc<Shared>>();
        let result = pixel::read_area(ax, ay, aw, ah).map(|(px, pw, ph)| pixel::fingerprint(&px, pw, ph, 40));
        match result {
            Ok((grid, gw, gh)) if !grid.is_empty() => {
                {
                    let mut s = sh.settings.lock().unwrap();
                    let id = s.active.clone();
                    if let Some(p) = s.presets.iter_mut().find(|p| p.id == id) {
                        p.watch = Watch { x: Some(ax), y: Some(ay), w: aw, h: ah, color: grid.first().copied(), grid, gw, gh, ..p.watch.clone() };
                    }
                }
                sh.save();
                let _ = app2.emit("snipped", ());
            }
            Ok(_) => {
                let _ = app2.emit("snip_error", "That area was empty. Try again.");
            }
            Err(e) => {
                let _ = app2.emit("snip_error", e);
            }
        }
        show_main(&app2);
    });
    Ok(())
}

#[tauri::command]
fn run(app: AppHandle) -> Result<(), String> {
    start_run(&app, "manual")
}

#[tauri::command]
fn stop(sh: Sh) {
    sh.stop.store(true, Ordering::SeqCst);
}

#[tauri::command]
fn set_watch(app: AppHandle, on: bool) -> Result<(), String> {
    if on {
        start_watch(&app)
    } else {
        stop_watch(&app);
        Ok(())
    }
}

#[tauri::command]
fn set_autostart(app: AppHandle, on: bool) -> Result<bool, String> {
    let m = app.autolaunch();
    let r = if on { m.enable() } else { m.disable() };
    r.map_err(|e| format!("Could not change start at login: {e}"))?;
    Ok(m.is_enabled().unwrap_or(on))
}

#[tauri::command]
fn clear_log(sh: Sh) {
    sh.log.lock().unwrap().clear();
    sh.save_log();
}

#[tauri::command]
fn open_url(app: AppHandle, url: String) -> Result<(), String> {
    let ok = url.starts_with("https://github.com/HanyJr/tapline") || url.starts_with("https://hanyjr.github.io/tapline");
    if !ok {
        return Err("Blocked link".into());
    }
    app.opener().open_url(url, None::<&str>).map_err(|e| e.to_string())
}


// ---------------------------------------------------------------- record mode

#[derive(Clone, Serialize)]
struct RecStatus {
    on: bool,
    count: usize,
    text: String,
}

#[tauri::command]
fn start_record(app: AppHandle) -> Result<(), String> {
    let sh = app.state::<Arc<Shared>>().inner().clone();
    if sh.running.load(Ordering::SeqCst) {
        return Err("Stop the run before recording".into());
    }
    if !input::has_permission(true) {
        return Err("Allow Tapline under Privacy & Security, Input Monitoring, then reopen it".into());
    }
    drop(new_enigo()?); // surfaces a missing Accessibility permission here
    if sh.recording.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    stop_watch(&app);
    sh.rec_stop.store(false, Ordering::SeqCst);
    let _ = app.global_shortcut().register(esc());
    let hotkey = sh.settings.lock().unwrap().hotkey.clone();
    let stop_key: String = hotkey.rsplit('+').next().unwrap_or("F8").to_string();
    let app = app.clone();

    thread::spawn(move || {
        let emit = |on: bool, count: usize, text: String| {
            let _ = app.emit("rec", RecStatus { on, count, text });
        };
        let enigo = match new_enigo() {
            Ok(e) => e,
            Err(e) => {
                sh.recording.store(false, Ordering::SeqCst);
                let _ = app.global_shortcut().unregister(esc());
                emit(false, 0, e);
                return;
            }
        };
        // 3 second head start
        for n in (1..=3).rev() {
            if sh.rec_stop.load(Ordering::SeqCst) {
                break;
            }
            emit(true, 0, format!("Recording starts in {n}..."));
            thread::sleep(Duration::from_secs(1));
        }
        let mut steps: Vec<Step> = Vec::new();
        let mut last_at = Instant::now();
        let mut last_click: Option<(Instant, i32, i32)> = None;
        let mut last_char = Instant::now() - Duration::from_secs(10);
        let mut prev = input::read();
        let started = Instant::now();
        if !sh.rec_stop.load(Ordering::SeqCst) {
            emit(true, 0, "Recording. Do your clicks and keys now.".into());
        }

        // area of Tapline's own window, so clicks on it are not recorded
        let own_rect = || -> Option<(i32, i32, i32, i32)> {
            let w = app.get_webview_window("main")?;
            if !w.is_visible().unwrap_or(false) {
                return None;
            }
            let p = w.outer_position().ok()?;
            let z = w.outer_size().ok()?;
            let k = if cfg!(target_os = "macos") { w.scale_factor().unwrap_or(1.0) } else { 1.0 };
            Some(((p.x as f64 / k) as i32, (p.y as f64 / k) as i32, (z.width as f64 / k) as i32, (z.height as f64 / k) as i32))
        };

        let push = |steps: &mut Vec<Step>, last_at: &mut Instant, st: Step| {
            let gap = last_at.elapsed().as_secs_f64();
            if let Some(prev) = steps.last_mut() {
                prev.delay = ((gap * 10.0).round() / 10.0).clamp(0.1, 60.0);
            }
            steps.push(st);
            *last_at = Instant::now();
        };

        while !sh.rec_stop.load(Ordering::SeqCst) && steps.len() < 300 && started.elapsed() < Duration::from_secs(600) {
            thread::sleep(Duration::from_millis(10));
            let now = input::read();
            // mouse button pressed
            for (is_down, was_down, button) in [(now.left, prev.left, "left"), (now.right, prev.right, "right")] {
                if is_down && !was_down {
                    if let Ok((x, y)) = enigo.location() {
                        let mine = own_rect().map(|(rx, ry, rw, rh)| x >= rx && x < rx + rw && y >= ry && y < ry + rh).unwrap_or(false);
                        if mine {
                            continue;
                        }
                        // second left click close by = double-click
                        let dbl = button == "left"
                            && last_click.map(|(t, lx, ly)| t.elapsed() < Duration::from_millis(400) && (lx - x).abs() <= 4 && (ly - y).abs() <= 4).unwrap_or(false)
                            && steps.last().map(|s| s.kind == "click" && s.button == "left").unwrap_or(false);
                        if dbl {
                            if let Some(s) = steps.last_mut() {
                                s.button = "double".into();
                            }
                            last_click = None;
                        } else {
                            push(&mut steps, &mut last_at, Step { kind: "click".into(), x: Some(x), y: Some(y), button: button.into(), ..Step::default() });
                            if button == "left" {
                                last_click = Some((Instant::now(), x, y));
                            }
                        }
                        emit(true, steps.len(), format!("Recording · {} steps", steps.len()));
                    }
                }
            }
            // keys pressed
            for k in now.keys.iter().filter(|k| !prev.keys.contains(k)) {
                if *k == "Escape" || *k == stop_key.as_str() {
                    continue; // these stop the recording
                }
                let mods: Vec<String> = now.mods.iter().map(|m| m.to_string()).collect();
                let only_shift = mods.iter().all(|m| m == "Shift");
                let typable = k.len() == 1 || *k == "Space";
                if typable && only_shift && !(mods.len() == 1 && k.chars().all(|c| c.is_ascii_digit())) {
                    let c = if *k == "Space" {
                        ' '
                    } else {
                        let ch = k.chars().next().unwrap_or(' ');
                        if mods.is_empty() { ch.to_ascii_lowercase() } else { ch.to_ascii_uppercase() }
                    };
                    let join = last_char.elapsed() < Duration::from_millis(1500) && steps.last().map(|s| s.kind == "text").unwrap_or(false);
                    if join {
                        if let Some(s) = steps.last_mut() {
                            s.text.push(c);
                        }
                        last_at = Instant::now();
                    } else {
                        push(&mut steps, &mut last_at, Step { kind: "text".into(), text: c.to_string(), ..Step::default() });
                    }
                    last_char = Instant::now();
                } else {
                    push(&mut steps, &mut last_at, Step { kind: "key".into(), key: k.to_string(), mods, ..Step::default() });
                }
                emit(true, steps.len(), format!("Recording · {} steps", steps.len()));
            }
            prev = now;
        }

        let _ = app.global_shortcut().unregister(esc());
        sh.recording.store(false, Ordering::SeqCst);
        if steps.is_empty() {
            emit(false, 0, "Nothing was recorded".into());
            return;
        }
        if let Some(s) = steps.last_mut() {
            s.delay = 0.3;
        }
        let count = steps.len();
        let name = {
            let mut s = sh.settings.lock().unwrap();
            let n = s.presets.iter().filter(|p| p.name.starts_with("Recording")).count() + 1;
            let p = Preset { id: format!("r{}", now_ms()), name: format!("Recording {n}"), steps, ..Preset::default() };
            let name = p.name.clone();
            s.active = p.id.clone();
            s.presets.push(p);
            name
        };
        sh.save();
        refresh_tray(&app);
        emit(false, count, format!("Saved {count} steps as \"{name}\""));
    });
    Ok(())
}

#[tauri::command]
fn stop_record(sh: Sh) {
    sh.rec_stop.store(true, Ordering::SeqCst);
}

// ---------------------------------------------------------------- app

fn load_settings(dir: &PathBuf) -> Settings {
    if let Some(s) = std::fs::read_to_string(dir.join("settings.v2.json")).ok().and_then(|t| serde_json::from_str::<Settings>(&t).ok()) {
        let mut s = s;
        s.fix();
        return s;
    }
    let mut s = Settings::default();
    if let Some(old) = std::fs::read_to_string(dir.join("settings.json")).ok().and_then(|t| serde_json::from_str::<LegacySettings>(&t).ok()) {
        let p = &mut s.presets[0];
        p.steps[0].x = old.x;
        p.steps[0].y = old.y;
        p.rounds = old.count.unwrap_or(2);
        p.round_wait = old.wait.unwrap_or(2.0);
    }
    s.fix();
    s
}

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run_app() {
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_autostart::init(tauri_plugin_autostart::MacosLauncher::LaunchAgent, None))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    if event.state() != ShortcutState::Pressed {
                        return;
                    }
                    // a key Tapline itself just pressed, not the user
                    let quiet = app.state::<Arc<Shared>>().quiet_until.lock().map(|t| Instant::now() < *t).unwrap_or(false);
                    if quiet {
                        return;
                    }
                    if *shortcut == esc() {
                        stop_everything(app);
                        return;
                    }
                    let mine = app.state::<Arc<Shared>>().hotkey.lock().map(|h| h.as_ref() == Some(shortcut)).unwrap_or(false);
                    if mine {
                        toggle_run(app);
                    }
                })
                .build(),
        )
        .setup(|app| {
            let dir = app.path().app_config_dir().unwrap_or_else(|_| PathBuf::from("."));
            let settings = load_settings(&dir);
            let log: Vec<LogEntry> = std::fs::read_to_string(dir.join("log.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
            let hotkey_text = settings.hotkey.clone();
            let shared = Arc::new(Shared {
                settings: Mutex::new(settings),
                running: AtomicBool::new(false),
                stop: AtomicBool::new(false),
                watching: AtomicBool::new(false),
                watch_gen: AtomicU64::new(0),
                log: Mutex::new(log),
                dir,
                hotkey: Mutex::new(None),
                quiet_until: Mutex::new(Instant::now()),
                recording: AtomicBool::new(false),
                rec_stop: AtomicBool::new(false),
                tray: Mutex::new(None),
            });
            app.manage(shared.clone());
            let handle = app.handle().clone();

            // Start/stop key; Tapline still works with its buttons if this fails
            if let Ok(sc) = register_hotkey(&handle, &hotkey_text) {
                *shared.hotkey.lock().unwrap() = Some(sc);
            }

            // Tray / menu bar
            let show_i = MenuItem::with_id(app, "show", "Show Tapline", true, None::<&str>)?;
            let run_i = MenuItem::with_id(app, "run", "Run", true, None::<&str>)?;
            let watch_i = MenuItem::with_id(app, "watch", "Turn Watch on", true, None::<&str>)?;
            let sep = PredefinedMenuItem::separator(app)?;
            let quit_i = MenuItem::with_id(app, "quit", "Quit Tapline", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show_i, &run_i, &watch_i, &sep, &quit_i])?;
            *shared.tray.lock().unwrap() = Some((run_i.clone(), watch_i.clone()));

            let mut tray = TrayIconBuilder::with_id("main")
                .menu(&menu)
                .tooltip("Tapline: ready")
                .show_menu_on_left_click(cfg!(target_os = "macos"))
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => show_main(app),
                    "run" => toggle_run(app),
                    "watch" => {
                        let on = app.state::<Arc<Shared>>().watching.load(Ordering::SeqCst);
                        if on {
                            stop_watch(app);
                        } else if let Err(e) = start_watch(app) {
                            show_main(app);
                            emit_watch(app, WatchStatus { on: false, state: "error", text: e, diff: None, matched: false });
                        }
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                        if !cfg!(target_os = "macos") {
                            show_main(tray.app_handle());
                        }
                    }
                });
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
            tray.build(app)?;
            refresh_tray(&handle);
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let keep = window.app_handle().state::<Arc<Shared>>().settings.lock().map(|s| s.tray_on_close).unwrap_or(false);
                if keep {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_all,
            save_settings,
            capture_position,
            watch_probe,
            watch_spot,
            start_snip,
            snip_cancel,
            snip_done,
            start_record,
            stop_record,
            run,
            stop,
            set_watch,
            set_autostart,
            clear_log,
            open_url
        ])
        .run(tauri::generate_context!())
        .expect("error while running Tapline");
}
