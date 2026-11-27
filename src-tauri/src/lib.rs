//! Tapline: click a saved spot, press Enter, wait, repeat.

use enigo::{Button, Coordinate, Direction, Enigo, Key, Keyboard, Mouse, Settings as EnigoSettings};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_global_shortcut::{Code, Shortcut, ShortcutState};

const ENTER_DELAY_MS: u64 = 300;

#[derive(Clone, Serialize, Deserialize)]
struct Saved {
    x: Option<i32>,
    y: Option<i32>,
    count: u32,
    wait: f64,
}

impl Default for Saved {
    fn default() -> Self {
        Saved { x: None, y: None, count: 2, wait: 2.0 }
    }
}

struct AppState {
    saved: Mutex<Saved>,
    running: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    file: PathBuf,
}

impl AppState {
    fn persist(&self) {
        if let Ok(s) = self.saved.lock() {
            if let Some(dir) = self.file.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(&self.file, serde_json::to_string_pretty(&*s).unwrap_or_default());
        }
    }
}

#[derive(Clone, Serialize)]
struct Progress {
    kind: &'static str,
    text: String,
    done: u32,
    total: u32,
    finished: bool,
}

#[derive(Serialize)]
struct StateOut {
    x: Option<i32>,
    y: Option<i32>,
    count: u32,
    wait: f64,
    platform: &'static str,
    version: String,
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

fn new_enigo() -> Result<Enigo, String> {
    Enigo::new(&EnigoSettings::default()).map_err(|e| match e {
        enigo::NewConError::NoPermission => {
            "Allow Tapline in System Settings, Privacy & Security, Accessibility".to_string()
        }
        other => format!("Could not start: {other}"),
    })
}

#[tauri::command]
fn get_state(app: AppHandle, state: State<AppState>) -> StateOut {
    let s = state.saved.lock().unwrap().clone();
    StateOut {
        x: s.x,
        y: s.y,
        count: s.count,
        wait: s.wait,
        platform: platform(),
        version: app.package_info().version.to_string(),
    }
}

#[tauri::command]
fn capture_position(state: State<AppState>) -> Result<(i32, i32), String> {
    let enigo = new_enigo()?;
    let (x, y) = enigo.location().map_err(|e| e.to_string())?;
    {
        let mut s = state.saved.lock().unwrap();
        s.x = Some(x);
        s.y = Some(y);
    }
    state.persist();
    Ok((x, y))
}

#[tauri::command]
fn save(count: u32, wait: f64, state: State<AppState>) {
    {
        let mut s = state.saved.lock().unwrap();
        s.count = count.clamp(1, 999);
        s.wait = wait.clamp(0.0, 600.0);
    }
    state.persist();
}

#[tauri::command]
fn stop(state: State<AppState>) {
    state.stop.store(true, Ordering::SeqCst);
}

#[tauri::command]
fn start(app: AppHandle, count: u32, wait: f64, state: State<AppState>) -> Result<(), String> {
    if state.running.swap(true, Ordering::SeqCst) {
        return Ok(()); // already running
    }
    let (x, y) = {
        let mut s = state.saved.lock().unwrap();
        s.count = count.clamp(1, 999);
        s.wait = wait.clamp(0.0, 600.0);
        match (s.x, s.y) {
            (Some(x), Some(y)) => (x, y),
            _ => {
                state.running.store(false, Ordering::SeqCst);
                return Err("Set the click position first".into());
            }
        }
    };
    state.persist();
    let count = count.clamp(1, 999);
    let wait = Duration::from_secs_f64(wait.clamp(0.0, 600.0));

    let mut enigo = match new_enigo() {
        Ok(e) => e,
        Err(e) => {
            state.running.store(false, Ordering::SeqCst);
            return Err(e);
        }
    };

    state.stop.store(false, Ordering::SeqCst);
    let stop_flag = state.stop.clone();
    let running = state.running.clone();

    thread::spawn(move || {
        let emit = |kind: &'static str, text: String, done: u32, finished: bool| {
            let _ = app.emit("progress", Progress { kind, text, done, total: count, finished });
        };
        // Wait that reacts to Stop within 50 ms
        let pause = |d: Duration| -> bool {
            let end = Instant::now() + d;
            while Instant::now() < end {
                if stop_flag.load(Ordering::SeqCst) {
                    return false;
                }
                thread::sleep(Duration::from_millis(50).min(end - Instant::now()));
            }
            !stop_flag.load(Ordering::SeqCst)
        };

        let mut done = 0;
        let mut error: Option<String> = None;

        for i in 0..count {
            if stop_flag.load(Ordering::SeqCst) {
                break;
            }
            emit("busy", format!("Click {} of {}", i + 1, count), done, false);
            let step = (|| -> Result<(), enigo::InputError> {
                enigo.move_mouse(x, y, Coordinate::Abs)?;
                thread::sleep(Duration::from_millis(40));
                enigo.button(Button::Left, Direction::Click)?;
                thread::sleep(Duration::from_millis(ENTER_DELAY_MS));
                enigo.key(Key::Return, Direction::Click)?;
                Ok(())
            })();
            if let Err(e) = step {
                error = Some(format!("Could not click: {e}"));
                break;
            }
            done += 1;
            if i + 1 < count {
                let secs = wait.as_secs_f64();
                emit("busy", format!("Waiting {} sec", trim(secs)), done, false);
                if !pause(wait) {
                    break;
                }
            }
        }

        match error {
            Some(e) => emit("error", e, done, true),
            None if done < count => emit("error", format!("Stopped after {done}"), done, true),
            None => emit("done", "Done".into(), done, true),
        }
        running.store(false, Ordering::SeqCst);
    });
    Ok(())
}

fn trim(v: f64) -> String {
    let s = format!("{v:.1}");
    s.trim_end_matches(".0").to_string()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let f8 = Shortcut::new(None, Code::F8);

    tauri::Builder::default()
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(move |app, shortcut, event| {
                    if shortcut == &f8 && event.state() == ShortcutState::Pressed {
                        let _ = app.emit("hotkey", ());
                    }
                })
                .build(),
        )
        .setup(move |app| {
            let file = app
                .path()
                .app_config_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join("settings.json");
            let saved: Saved = std::fs::read_to_string(&file)
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or_default();
            app.manage(AppState {
                saved: Mutex::new(saved),
                running: Arc::new(AtomicBool::new(false)),
                stop: Arc::new(AtomicBool::new(false)),
                file,
            });
            #[cfg(desktop)]
            {
                use tauri_plugin_global_shortcut::GlobalShortcutExt;
                // F8 is optional: if another app owns it, Tapline still works with the buttons.
                let _ = app.global_shortcut().register(f8);
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![get_state, capture_position, save, start, stop])
        .run(tauri::generate_context!())
        .expect("error while running Tapline");
}
