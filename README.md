<p align="center">
  <img src=".github/assets/banner.png" alt="Tapline - Click. Enter. Repeat." width="100%">
</p>

<p align="center">
  <a href="https://github.com/HanyJr/tapline/releases/latest/download/Tapline_1.0.0_x64-setup.exe"><img src=".github/assets/download-windows.png" alt="Download for Windows" height="56"></a>
  &nbsp;
  <a href="https://github.com/HanyJr/tapline/releases/latest/download/Tapline_1.0.0_universal.dmg"><img src=".github/assets/download-mac.png" alt="Download for Mac" height="56"></a>
</p>

<p align="center">
  <b>Version 1.0.0</b> &nbsp;·&nbsp; Free &nbsp;·&nbsp; Windows 10 and 11 &nbsp;·&nbsp; macOS 11 or later &nbsp;·&nbsp; <a href="https://github.com/HanyJr/tapline/releases">All releases</a>
</p>

<br>

## What it does

Tapline clicks the spot you choose, presses **Enter**, waits, and does it again as many times as you set. One key starts it, the same key stops it.

| | |
|---|---|
| **Pick the spot once** | Press *Set position*, hold your mouse over the button you want clicked. Saved for next time. |
| **Any number of rounds** | Choose how many times to click, from 1 to 999. |
| **Your pace** | Wait between rounds from 0 seconds to 10 minutes. |
| **F8 from anywhere** | Start and stop without switching windows. On a Mac keyboard: <kbd>fn</kbd> + <kbd>F8</kbd>. |
| **Always on top** | A small window that floats above your work and shows live progress. |
| **Private** | No account, no internet. Everything runs on your computer. |

## Install

### Windows
1. Download **Tapline_1.0.0_x64-setup.exe** with the button above.
2. Open it. If you see *Windows protected your PC*, press **More info**, then **Run anyway**.
3. Tapline installs for your user only (no admin needed) and appears in the Start menu.

### Mac
1. Download **Tapline_1.0.0_universal.dmg** with the button above. It runs on Apple Silicon and Intel.
2. Open the .dmg and drag **Tapline** into **Applications**.
3. First launch: right-click Tapline, choose **Open**, then **Open** again. On macOS 15 or later, go to *System Settings > Privacy & Security* and press **Open Anyway**.
4. When asked, allow Tapline under *System Settings > Privacy & Security > Accessibility*. This is what lets it click for you.

## How to use

1. **Set position**, then hold the mouse over the spot. It saves after 3 seconds.
2. Set **Times to click** and **Wait between**.
3. Press **Run** or **F8**. Press **Stop** or **F8** again to end early.

## For developers

Built with [Tauri 2](https://tauri.app) (Rust) and a plain HTML/CSS/JS interface, so the app is small and opens instantly.

```
ui/            App window
src-tauri/     Rust backend: clicking, Enter, timing, F8, saved settings
docs/          Product website (ready for GitHub Pages)
scripts/       Logo generator
```

Every push to `main` builds the Windows installer and the universal Mac app on GitHub Actions and attaches them to a draft release. To build locally: install Rust and Node 22, then run `npm ci` and `npx tauri build`.

<br>

<p align="center"><sub>Copyright 2026 Ian Brown</sub></p>
