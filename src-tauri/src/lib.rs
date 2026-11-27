//! Tapline: run a sequence of clicks and keys, repeat it, and optionally start it
//! automatically when a spot on screen turns a chosen color.

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
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent, Wry};
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
    color: Option<[u8; 3]>,
    tolerance: u32, // 1..100
    cooldown: f64,  // seconds
}

impl Default for Watch {
    fn default() -> Self {
        Watch { x: None, y: None, color: None, tolerance: 12, cooldown: 10.0 }
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
    quiet_until: Mutex<Instant>, // ignore our own key presses reaching the global shortcuts
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
    color: Option<[u8; 3]>,
    matched: bool,
}

fn emit_watch(app: &AppHandle, s: WatchStatus) {
    let _ = app.emit("watch", s);
}

fn start_watch(app: &AppHandle) -> Result<(), String> {
    let sh = app.state::<Arc<Shared>>().inner().clone();
    let preset = sh.settings.lock().unwrap().active_preset();
    let w = &preset.watch;
    if w.x.is_none() || w.y.is_none() || w.color.is_none() {
        return Err("Set the watch spot and trigger color first".into());
    }
    if !pixel::has_permission(true) {
        return Err("Allow Tapline under Privacy & Security, Screen Recording, then reopen it".into());
    }
    pixel::read(w.x.unwrap(), w.y.unwrap())?;

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
            let (Some(x), Some(y), Some(target)) = (w.x, w.y, w.color) else { break };
            let limit = w.tolerance as f64 / 100.0 * 0.5; // 100% tolerance = half the color space
            match pixel::read(x, y) {
                Err(e) => {
                    errors += 1;
                    if errors >= 8 {
                        emit_watch(&app, WatchStatus { on: false, state: "error", text: e, color: None, matched: false });
                        break;
                    }
                }
                Ok(c) => {
                    errors = 0;
                    let matched = pixel::distance(c, target) <= limit;
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
                                        emit_watch(&app, WatchStatus { on: false, state: "error", text: e, color: Some(c), matched });
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
                        "armed" => "Watching for the trigger color".to_string(),
                        "running" => format!("Running {name}"),
                        "reset" => "Waiting for the button to go back to normal".to_string(),
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
                    emit_watch(&app, WatchStatus { on: true, state, text, color: Some(c), matched });
                }
            }
            thread::sleep(Duration::from_millis(250));
        }
        // only the newest watcher may switch Watch off
        if sh.watch_gen.load(Ordering::SeqCst) == gen {
            let was_on = sh.watching.swap(false, Ordering::SeqCst);
            if was_on {
                emit_watch(&app, WatchStatus { on: false, state: "off", text: "Watch is off".into(), color: None, matched: false });
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
        emit_watch(app, WatchStatus { on: false, state: "off", text: "Watch is off".into(), color: None, matched: false });
    }
    refresh_tray(app);
}

fn stop_everything(app: &AppHandle) {
    let sh = app.state::<Arc<Shared>>();
    sh.stop.store(true, Ordering::SeqCst);
    stop_watch(app);
}

fn toggle_run(app: &AppHandle) {
    let sh = app.state::<Arc<Shared>>();
    if sh.running.load(Ordering::SeqCst) {
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
struct Sample {
    x: i32,
    y: i32,
    color: [u8; 3],
}

#[tauri::command]
fn capture_color() -> Result<Sample, String> {
    let (x, y) = new_enigo()?.location().map_err(|e| e.to_string())?;
    if !pixel::has_permission(true) {
        return Err("Allow Tapline under Privacy & Security, Screen Recording, then reopen it".into());
    }
    let color = pixel::read(x, y)?;
    Ok(Sample { x, y, color })
}

#[tauri::command]
fn sample_color(x: i32, y: i32) -> Result<[u8; 3], String> {
    if !pixel::has_permission(false) {
        return Err("Screen Recording permission needed".into());
    }
    pixel::read(x, y)
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
        p.watch.x = old.x;
        p.watch.y = old.y;
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
                            emit_watch(app, WatchStatus { on: false, state: "error", text: e, color: None, matched: false });
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
            capture_color,
            sample_color,
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
