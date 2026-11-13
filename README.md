# Syntax Downloader

Syntax Downloader is a Windows download manager. The desktop shell and download backend use **Tauri 2 and Rust**. The existing interface remains **HTML, CSS, and vanilla JavaScript**, built with Vite.

## Current functionality

- HTTP and HTTPS downloads with parallel byte-range segments when the server supports them, an automatic single-connection fallback when it does not, resumable partial files, retries with exponential backoff and `Retry-After` support, bounded network timeouts, and server-provided filenames.
- A global speed limiter shared by every connection, smoothed speed/ETA reporting, and optional bandwidth schedules through the settings API.
- Pause, resume, cancel, concurrency queueing, scheduled starts, automatic restoration of schedules and queued items at startup, and optional SHA-256 verification of completed files.
- Download history, settings persistence with corruption recovery, and download-history JSON import/export (imported entries are added paused).
- A settings window for the default save folder, maximum concurrent downloads, default segment count, global speed limit, in-app notifications, start-minimized behavior, and shutdown when the queue finishes.
- Search, category filters, sorting, dark/light themes, context menus, and opening completed files or their folders.

The Chrome/Edge extension connects to the desktop app through a loopback-only Rust bridge. It honors the extension's enable/interception switches, Alt+click link capture, configurable file-type filtering, and exclusion lists; provides a floating media grabber, a popup quick-download form, and page-file batch scanning; forwards referrer, cookie, and user-agent headers; and focuses the app after an accepted transfer. The Windows installer registers a native messaging host so the extension can launch the desktop app when a browser download starts. After installing or updating the app, reload the unpacked extension in Chrome or Edge. The extension bridge accepts only the extension's fixed ID and is not exposed on the network.

Video downloads from supported streaming sites use the bundled `yt-dlp.exe` and `deno.exe` tools. Video progress is read from yt-dlp's custom progress lines and drained after process exit so even short downloads report their final progress. FFmpeg is optional: a valid FFmpeg executable on `PATH` enables higher-quality separate-stream merging and audio extraction; otherwise the app downloads a site's best pre-merged video format. The app validates media executables before use and suppresses their Windows console windows. Site availability, login requirements, and the site's own restrictions still apply. The local share server, system-tray behavior, clipboard monitoring, media preview, and Wi-Fi sharing are not supported.

## Build on Windows

Install Node.js, the Rust MSVC toolchain, Visual Studio C++ Build Tools with the Windows SDK, and the WebView2 Runtime. In a Visual Studio Developer Command Prompt, run:

```powershell
npm install
npm start
```

`npm start` starts the Tauri development app. To create a Windows installer, run `npm run tauri:build`.

The GNU Rust toolchain is not suitable for producing this Windows app. An MSVC linker (`link.exe`) is required.

## Load the companion extension manually

In Chrome or Edge, open `chrome://extensions` or `edge://extensions`, enable Developer mode, select **Load unpacked**, and choose the project's `extension` folder. Keep Syntax Downloader running while using extension features. If the extension was already installed, reload it after updating its files.
