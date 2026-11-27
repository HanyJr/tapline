# Tapline

**Click. Enter. Repeat.** Tapline clicks a spot you choose, presses Enter, waits, and repeats as many times as you set. Windows and macOS.

## Use it

1. **Set position.** Press it, then hold your mouse over the spot to click. It saves after 3 seconds.
2. **Times to click** and **Wait between** (seconds).
3. **Run**, or press **F8** from any window. Press F8 or **Stop** to end early. On a Mac keyboard use fn + F8.

Settings are saved between launches.

### First launch on a Mac
- The app isn't notarized yet, so macOS blocks a plain double-click. Right-click Tapline, choose **Open**, then **Open** again. On macOS 15 or later, go to System Settings, Privacy & Security, and press **Open Anyway**.
- Allow Tapline under System Settings, Privacy & Security, **Accessibility**. Without this it can't click for you.

### First launch on Windows
SmartScreen may show "Windows protected your PC". Press **More info**, then **Run anyway**. This goes away once the installer is code-signed.

## Build

Every push to `main` builds the Windows installer and a universal Mac app on GitHub Actions and attaches them to a draft release.

Local build: install Rust and Node 22, then

```
npm ci
npx tauri build
```

## Project layout

```
ui/              The app window (HTML/CSS/JS)
src-tauri/       Rust backend: clicking, Enter, timing, F8, settings
docs/            Product website (GitHub Pages ready)
scripts/         Logo generator
```

Copyright 2026 Ian Brown.
