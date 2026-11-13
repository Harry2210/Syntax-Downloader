use chrono::{DateTime, Datelike, Local, NaiveTime, Utc};
use reqwest::Client;
use reqwest::header::{
    CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, RANGE, RETRY_AFTER,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use uuid::Uuid;

const RUNNING: u8 = 0;
const PAUSE_REQUESTED: u8 = 1;
const CANCEL_REQUESTED: u8 = 2;
const EXTENSION_BRIDGE_ADDRESS: &str = "127.0.0.1:36855";
const EXTENSION_ORIGIN: &str = "chrome-extension://bipnlkimdcbalpjhfdohfkglafnikjla";
const MAX_EXTENSION_REQUEST_BYTES: u64 = 1024 * 1024;
const MAX_SEGMENT_ATTEMPTS: usize = 5;
const MAX_PROBE_ATTEMPTS: usize = 3;
const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);
const PROGRESS_PERSIST_INTERVAL: Duration = Duration::from_secs(2);
const DOWNLOAD_CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const DOWNLOAD_READ_TIMEOUT: Duration = Duration::from_secs(20);
const PROGRESS_EVENT_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Segment {
    index: usize,
    start: u64,
    end: u64,
    downloaded: u64,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Download {
    id: String,
    url: String,
    filename: String,
    save_dir: String,
    total_size: u64,
    downloaded_size: u64,
    progress: f64,
    speed: u64,
    status: String,
    #[serde(default)]
    stage: String,
    #[serde(default)]
    segments: Vec<Segment>,
    category: String,
    date_added: String,
    date_completed: Option<String>,
    error: Option<String>,
    resumable: bool,
    mime_type: String,
    #[serde(default)]
    is_stream: bool,
    #[serde(default = "default_segments")]
    max_segments: u64,
    #[serde(default)]
    referrer: String,
    #[serde(default)]
    cookies: String,
    #[serde(default)]
    user_agent: String,
    #[serde(default)]
    scheduled_time: Option<String>,
    #[serde(default)]
    name_locked: bool,
    #[serde(default)]
    average_speed: u64,
    #[serde(default)]
    etag: String,
    #[serde(default)]
    last_modified: String,
    #[serde(default)]
    expected_sha256: String,
    #[serde(default)]
    hash_verified: bool,
}

fn default_segments() -> u64 {
    8
}

struct Inner {
    data_dir: PathBuf,
    settings: Mutex<Value>,
    downloads: Mutex<Vec<Download>>,
    controls: Mutex<HashMap<String, Arc<AtomicU8>>>,
    tasks: Mutex<HashMap<String, JoinHandle<()>>>,
    speed_limit: Mutex<u64>,
    rate_limiter: RateLimiter,
    schedule_tokens: Mutex<HashMap<String, String>>,
    start_lock: Mutex<()>,
    persist_lock: Mutex<()>,
    shutdown_lock: Mutex<()>,
    shutdown_triggered: AtomicU8,
}

#[derive(Clone)]
struct Backend(Arc<Inner>);

/// A process-wide leaky-bucket limiter. Every worker reserves its next write
/// slot here, so the configured limit applies to the sum of all connections
/// instead of being applied independently per segment.
struct RateLimiter {
    next_slot: Mutex<Instant>,
}

impl RateLimiter {
    fn new() -> Self {
        Self {
            next_slot: Mutex::new(Instant::now()),
        }
    }

    fn reset(&self) {
        if let Ok(mut next) = self.next_slot.lock() {
            *next = Instant::now();
        }
    }

    /// Blocks until `bytes` may be transferred at `limit` bytes/second.
    /// Returns false promptly if the download is paused or cancelled.
    fn consume(&self, bytes: u64, limit: u64, control: &AtomicU8) -> bool {
        if limit == 0 || bytes == 0 {
            return control.load(Ordering::SeqCst) == RUNNING;
        }
        let now = Instant::now();
        let deadline = {
            let Ok(mut next) = self.next_slot.lock() else {
                return true;
            };
            let start = (*next).max(now);
            let wait = Duration::from_secs_f64(bytes as f64 / limit as f64);
            *next = start + wait;
            start + wait
        };
        while Instant::now() < deadline {
            if control.load(Ordering::SeqCst) != RUNNING {
                self.reset();
                return false;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            thread::sleep(remaining.min(Duration::from_millis(50)));
        }
        control.load(Ordering::SeqCst) == RUNNING
    }
}

/// Smoothed transfer rate using an exponentially weighted moving average.
/// A long window keeps the number stable while still reacting to stalls.
struct SpeedMeter {
    state: Mutex<SpeedState>,
}

struct SpeedState {
    last_bytes: u64,
    last_sample: Instant,
    ewma: f64,
}

impl SpeedMeter {
    fn new(initial_bytes: u64) -> Self {
        Self {
            state: Mutex::new(SpeedState {
                last_bytes: initial_bytes,
                last_sample: Instant::now(),
                ewma: 0.0,
            }),
        }
    }

    fn sample(&self, total_bytes: u64) -> u64 {
        let Ok(mut state) = self.state.lock() else {
            return 0;
        };
        let now = Instant::now();
        let elapsed = now.duration_since(state.last_sample).as_secs_f64();
        if elapsed < 0.05 {
            return state.ewma as u64;
        }
        let delta = total_bytes.saturating_sub(state.last_bytes);
        state.last_bytes = total_bytes;
        state.last_sample = now;
        let instant = delta as f64 / elapsed;
        state.ewma = if state.ewma <= 0.0 {
            instant
        } else {
            state.ewma * 0.75 + instant * 0.25
        };
        state.ewma.max(0.0) as u64
    }

    fn reset(&self, total_bytes: u64) {
        if let Ok(mut state) = self.state.lock() {
            state.last_bytes = total_bytes;
            state.last_sample = Instant::now();
            state.ewma = 0.0;
        }
    }
}

fn write_json_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let temporary = path.with_extension("tmp");
    {
        let mut file = File::create(&temporary)
            .map_err(|error| format!("Could not create temporary data file: {error}"))?;
        file.write_all(bytes)
            .map_err(|error| format!("Could not write temporary data file: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("Could not flush temporary data file: {error}"))?;
    }
    fs::rename(&temporary, path)
        .map_err(|error| format!("Could not replace data file {path:?}: {error}"))
}

fn strip_utf8_bom(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes)
}

fn load_json_recover<T: DeserializeOwned + Default>(path: &Path) -> T {
    let Ok(bytes) = fs::read(path) else {
        return T::default();
    };
    match serde_json::from_slice(strip_utf8_bom(&bytes)) {
        Ok(value) => value,
        Err(error) => {
            let backup =
                path.with_extension(format!("corrupt-{}", Utc::now().format("%Y%m%d%H%M%S")));
            eprintln!(
                "Could not parse {path:?} ({error}); keeping a backup at {backup:?} and using defaults"
            );
            let _ = fs::rename(path, backup);
            T::default()
        }
    }
}

impl Default for Download {
    fn default() -> Self {
        Self {
            id: String::new(),
            url: String::new(),
            filename: String::new(),
            save_dir: String::new(),
            total_size: 0,
            downloaded_size: 0,
            progress: 0.0,
            speed: 0,
            status: String::new(),
            stage: String::new(),
            segments: Vec::new(),
            category: String::new(),
            date_added: String::new(),
            date_completed: None,
            error: None,
            resumable: false,
            mime_type: String::new(),
            is_stream: false,
            max_segments: default_segments(),
            referrer: String::new(),
            cookies: String::new(),
            user_agent: String::new(),
            scheduled_time: None,
            name_locked: false,
            average_speed: 0,
            etag: String::new(),
            last_modified: String::new(),
            expected_sha256: String::new(),
            hash_verified: false,
        }
    }
}

impl Backend {
    fn new(data_dir: PathBuf) -> Result<Self, String> {
        fs::create_dir_all(&data_dir).map_err(|error| error.to_string())?;
        let settings_path = data_dir.join("settings.json");
        let downloads_path = data_dir.join("downloads.json");
        let mut settings: Value = load_json_recover(&settings_path);
        if !settings.is_object() {
            settings = default_settings();
        }
        let mut downloads: Vec<Download> = load_json_recover(&downloads_path);
        for download in &mut downloads {
            if download.status == "downloading" {
                download.status = "paused".to_string();
                download.stage = "Paused".to_string();
                download.speed = 0;
            }
            download.average_speed = download.average_speed.max(download.speed);
        }
        let speed_limit = effective_speed_limit(&settings, Local::now());
        let backend = Self(Arc::new(Inner {
            data_dir,
            settings: Mutex::new(settings),
            downloads: Mutex::new(downloads),
            controls: Mutex::new(HashMap::new()),
            tasks: Mutex::new(HashMap::new()),
            speed_limit: Mutex::new(speed_limit),
            rate_limiter: RateLimiter::new(),
            schedule_tokens: Mutex::new(HashMap::new()),
            start_lock: Mutex::new(()),
            persist_lock: Mutex::new(()),
            shutdown_lock: Mutex::new(()),
            shutdown_triggered: AtomicU8::new(0),
        }));
        backend.persist()?;
        Ok(backend)
    }

    fn downloads_path(&self) -> PathBuf {
        self.0.data_dir.join("downloads.json")
    }

    fn persist(&self) -> Result<(), String> {
        let _persist_guard = self
            .0
            .persist_lock
            .lock()
            .map_err(|_| "Persistence lock was poisoned".to_string())?;
        let settings = self
            .0
            .settings
            .lock()
            .map_err(|_| "Settings lock was poisoned".to_string())?;
        write_json_atomic(
            &self.0.data_dir.join("settings.json"),
            &serde_json::to_vec_pretty(&*settings).map_err(|error| error.to_string())?,
        )
        .map_err(|error| format!("Failed to save settings: {error}"))?;
        drop(settings);

        let downloads = self
            .0
            .downloads
            .lock()
            .map_err(|_| "Downloads lock was poisoned".to_string())?;
        write_json_atomic(
            &self.downloads_path(),
            &serde_json::to_vec_pretty(&*downloads).map_err(|error| error.to_string())?,
        )
        .map_err(|error| format!("Failed to save downloads: {error}"))
    }

    fn get_download(&self, id: &str) -> Result<Download, String> {
        self.0
            .downloads
            .lock()
            .map_err(|_| "Downloads lock was poisoned".to_string())?
            .iter()
            .find(|download| download.id == id)
            .cloned()
            .ok_or_else(|| format!("Download {id} was not found"))
    }

    fn update_download<F>(&self, id: &str, update: F) -> Result<Download, String>
    where
        F: FnOnce(&mut Download),
    {
        let mut downloads = self
            .0
            .downloads
            .lock()
            .map_err(|_| "Downloads lock was poisoned".to_string())?;
        let download = downloads
            .iter_mut()
            .find(|download| download.id == id)
            .ok_or_else(|| format!("Download {id} was not found"))?;
        update(download);
        let updated = download.clone();
        drop(downloads);
        self.persist()?;
        Ok(updated)
    }

    fn update_progress(
        &self,
        id: &str,
        update: impl FnOnce(&mut Download),
    ) -> Result<Download, String> {
        let mut downloads = self
            .0
            .downloads
            .lock()
            .map_err(|_| "Downloads lock was poisoned".to_string())?;
        let download = downloads
            .iter_mut()
            .find(|download| download.id == id)
            .ok_or_else(|| format!("Download {id} was not found"))?;
        update(download);
        Ok(download.clone())
    }

    fn persist_downloads(&self) -> Result<(), String> {
        let _persist_guard = self
            .0
            .persist_lock
            .lock()
            .map_err(|_| "Persistence lock was poisoned".to_string())?;
        let downloads = self
            .0
            .downloads
            .lock()
            .map_err(|_| "Downloads lock was poisoned".to_string())?;
        write_json_atomic(
            &self.downloads_path(),
            &serde_json::to_vec_pretty(&*downloads).map_err(|error| error.to_string())?,
        )
        .map_err(|error| format!("Failed to save downloads: {error}"))
    }

    fn stop_task(&self, id: &str, signal: u8) -> Result<(), String> {
        if let Some(control) = self
            .0
            .controls
            .lock()
            .map_err(|_| "Download control lock was poisoned".to_string())?
            .get(id)
        {
            control.store(signal, Ordering::SeqCst);
        }
        let task = self
            .0
            .tasks
            .lock()
            .map_err(|_| "Download task lock was poisoned".to_string())?
            .remove(id);
        if let Some(task) = task {
            task.join()
                .map_err(|_| format!("Download worker {id} panicked"))?;
        }
        Ok(())
    }
}

fn default_settings() -> Value {
    json!({
        "downloadDir": default_download_dir().to_string_lossy(),
        "maxConcurrent": 3,
        "maxSegments": 8,
        "speedLimit": 0,
        "clipboardMonitoring": true,
        "theme": "dark",
        "startMinimized": false,
        "startWithWindows": false,
        "closeToTray": true,
        "notifications": true,
        "shutdownAfterComplete": false,
        "updateFeedUrl": "https://gist.githubusercontent.com/Harry2210/d461b829673780c2340d6e54b85d51a9/raw/Syntax%2520downloader%2520version.json",
        "autoCheckUpdates": true,
        "bandwidthSchedule": [],
        "categories": {
            "compressed": {"name":"Compressed","extensions":[".zip",".rar",".7z",".tar",".gz",".bz2",".xz",".iso"],"icon":"archive","dir":"Compressed"},
            "documents": {"name":"Documents","extensions":[".pdf",".doc",".docx",".xls",".xlsx",".ppt",".pptx",".txt",".rtf",".odt",".csv"],"icon":"file-text","dir":"Documents"},
            "music": {"name":"Music","extensions":[".mp3",".flac",".wav",".aac",".ogg",".wma",".m4a"],"icon":"music","dir":"Music"},
            "video": {"name":"Videos","extensions":[".mp4",".mkv",".avi",".mov",".wmv",".flv",".webm",".m4v"],"icon":"video","dir":"Videos"},
            "programs": {"name":"Programs","extensions":[".exe",".msi",".deb",".rpm",".apk"],"icon":"package","dir":"Programs"},
            "images": {"name":"Images","extensions":[".jpg",".jpeg",".png",".gif",".bmp",".svg",".webp",".ico"],"icon":"image","dir":"Images"},
            "other": {"name":"Other","extensions":[],"icon":"file","dir":"Other"}
        }
    })
}

fn default_download_dir() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("Downloads")
}

fn parse_clock(value: &str) -> Option<NaiveTime> {
    NaiveTime::parse_from_str(value.trim(), "%H:%M").ok()
}

fn schedule_day_matches(entry: &Value, weekday: u32) -> bool {
    let Some(days) = entry.get("days").and_then(Value::as_array) else {
        return true;
    };
    days.iter().any(|day| {
        day.as_u64() == Some(weekday as u64)
            || day
                .as_str()
                .and_then(|name| name.parse::<u32>().ok())
                .is_some_and(|number| number == weekday)
    })
}

fn bandwidth_schedule_limit(settings: &Value, now: DateTime<Local>) -> Option<u64> {
    let entries = settings.get("bandwidthSchedule")?.as_array()?;
    let weekday = now.weekday().num_days_from_sunday();
    let current = now.time();
    for entry in entries {
        if !schedule_day_matches(entry, weekday) {
            continue;
        }
        let start = entry
            .get("start")
            .and_then(Value::as_str)
            .and_then(parse_clock);
        let end = entry
            .get("end")
            .and_then(Value::as_str)
            .and_then(parse_clock);
        let (Some(start), Some(end)) = (start, end) else {
            continue;
        };
        let matches = if start <= end {
            current >= start && current < end
        } else {
            // A window such as 22:00-06:00 crosses midnight.
            current >= start || current < end
        };
        if !matches {
            continue;
        }
        if entry.get("unlimited").and_then(Value::as_bool) == Some(true) {
            return Some(0);
        }
        if let Some(limit) = entry.get("limit").and_then(Value::as_u64) {
            return Some(limit);
        }
        if let Some(kbps) = entry.get("limitKbps").and_then(Value::as_u64) {
            return Some(kbps.saturating_mul(1024));
        }
    }
    None
}

/// The configured global limit, after applying the first matching bandwidth
/// schedule entry. A schedule entry can lift or tighten the base limit.
fn effective_speed_limit(settings: &Value, now: DateTime<Local>) -> u64 {
    if let Some(limit) = bandwidth_schedule_limit(settings, now) {
        return limit;
    }
    settings
        .get("speedLimit")
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

fn refresh_speed_limit(backend: &Backend) -> Result<u64, String> {
    let settings = backend
        .0
        .settings
        .lock()
        .map_err(|_| "Settings lock was poisoned".to_string())?;
    let limit = effective_speed_limit(&settings, Local::now());
    drop(settings);
    *backend
        .0
        .speed_limit
        .lock()
        .map_err(|_| "Speed-limit lock was poisoned".to_string())? = limit;
    if limit == 0 {
        backend.0.rate_limiter.reset();
    }
    Ok(limit)
}

fn start_bandwidth_scheduler(app: AppHandle, backend: Backend) {
    thread::spawn(move || {
        loop {
            thread::sleep(Duration::from_secs(30));
            let previous = backend
                .0
                .speed_limit
                .lock()
                .map(|value| *value)
                .unwrap_or(0);
            match refresh_speed_limit(&backend) {
                Ok(current) if current != previous => {
                    emit(
                        &app,
                        "speed:update",
                        &json!({"downloadSpeed": active_download_speed(&backend), "speedLimit": current}),
                    );
                }
                Ok(_) => {}
                Err(error) => eprintln!("Could not apply the bandwidth schedule: {error}"),
            }
        }
    });
}

fn emit<T: Serialize>(app: &AppHandle, name: &str, payload: &T) {
    if let Err(error) = app.emit(name, payload) {
        eprintln!("Failed to emit {name}: {error}");
    }
}

fn sanitize_filename(filename: &str) -> String {
    let cleaned: String = filename
        .chars()
        .map(|character| {
            if character.is_control() || "/\\?%*:|\"<>".contains(character) {
                '_'
            } else {
                character
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches(['.', ' ']);
    if cleaned.is_empty() {
        return "download".to_string();
    }
    let mut cleaned: String = cleaned.chars().take(200).collect();
    cleaned = cleaned.trim_end_matches(['.', ' ']).to_string();
    if cleaned.is_empty() {
        return "download".to_string();
    }
    // Windows reserves these device names even when they have an extension
    // (for example "CON.txt" cannot be created).
    let stem = cleaned
        .split('.')
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    const RESERVED_NAMES: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
        "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if RESERVED_NAMES.contains(&stem.as_str()) {
        cleaned.insert(0, '_');
    }
    cleaned
}

fn normalize_sha256(value: Option<&str>) -> Result<String, String> {
    let value = value.unwrap_or("").trim().to_ascii_lowercase();
    if value.is_empty() {
        return Ok(String::new());
    }
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("The expected SHA-256 value must be 64 hexadecimal characters".to_string());
    }
    Ok(value)
}

fn category_for(filename: &str) -> String {
    let extension = Path::new(filename)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match extension.as_str() {
        "mp4" | "mkv" | "avi" | "mov" | "wmv" | "flv" | "webm" | "m4v" | "mpg" => "Videos",
        "mp3" | "flac" | "wav" | "aac" | "ogg" | "wma" | "m4a" => "Music",
        "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "txt" | "csv" => "Documents",
        "zip" | "rar" | "7z" | "tar" | "gz" | "bz2" | "xz" | "iso" => "Compressed",
        "exe" | "msi" | "deb" | "rpm" | "apk" => "Programs",
        "jpg" | "jpeg" | "png" | "gif" | "bmp" | "svg" | "webp" | "ico" => "Images",
        _ => "Other",
    }
    .to_string()
}

fn allocate_segments(total_size: u64, max_segments: usize) -> Vec<Segment> {
    if total_size == 0 {
        return vec![Segment {
            index: 0,
            start: 0,
            end: 0,
            downloaded: 0,
        }];
    }
    // Use roughly one connection per megabyte, capped by the user setting.
    // This avoids spawning dozens of threads for tiny files while still
    // saturating the link for large ones.
    let useful_segments = total_size.div_ceil(1024 * 1024).max(1);
    let count = (max_segments.max(1) as u64).min(useful_segments) as usize;
    let segment_size = total_size.div_ceil(count as u64);
    (0..count)
        .filter_map(|index| {
            let start = index as u64 * segment_size;
            if start >= total_size {
                return None;
            }
            Some(Segment {
                index,
                start,
                end: ((index as u64 + 1) * segment_size).min(total_size) - 1,
                downloaded: 0,
            })
        })
        .collect()
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ProbeInfo {
    supports_ranges: bool,
    total_size: u64,
    mime_type: String,
    filename: Option<String>,
    etag: String,
    last_modified: String,
}

/// A resume is only safe when the partial file, the segment counters and the
/// remote file all describe the same bytes. A plain (non-range) file cannot be
/// continued at all, and a changed validator means the bytes on disk may no
/// longer belong to the current remote object.
fn should_reset_resume(download: &Download, probe: &ProbeInfo) -> bool {
    let had_partial = download.downloaded_size > 0
        || download
            .segments
            .iter()
            .any(|segment| segment.downloaded > 0);
    let remote_changed =
        (!download.etag.is_empty() && !probe.etag.is_empty() && download.etag != probe.etag)
            || (!download.last_modified.is_empty()
                && !probe.last_modified.is_empty()
                && download.last_modified != probe.last_modified);
    download.segments.is_empty()
        || download.resumable != probe.supports_ranges
        || download.total_size != probe.total_size
        || (!probe.supports_ranges && had_partial)
        || (probe.total_size == 0 && had_partial)
        || remote_changed
}

/// Parses RFC 7233 Content-Range values in both `bytes 0-0/123` and
/// `bytes */123` form. The end is inclusive.
fn parse_content_range(value: &str) -> Option<(u64, u64, Option<u64>)> {
    let value = value.trim();
    if !value.get(..5).is_some_and(|unit| unit.eq_ignore_ascii_case("bytes")) {
        return None;
    }
    let rest = value[5..].trim_start();
    let (range, total) = rest.split_once('/')?;
    let total = total.trim();
    let total = if total == "*" {
        None
    } else {
        Some(total.parse::<u64>().ok()?)
    };
    if range.trim() == "*" {
        return Some((0, 0, total));
    }
    let (start, end) = range.trim().split_once('-')?;
    let start = start.trim().parse::<u64>().ok()?;
    let end = end.trim().parse::<u64>().ok()?;
    (end >= start).then_some((start, end, total))
}

#[cfg(test)]
fn content_range_total(value: &str) -> Option<u64> {
    let (start, end, total) = parse_content_range(value)?;
    if start != 0 || end != 0 {
        return None;
    }
    total.filter(|total| *total > 0)
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let high = (bytes[index + 1] as char).to_digit(16);
            let low = (bytes[index + 2] as char).to_digit(16);
            if let (Some(high), Some(low)) = (high, low) {
                decoded.push((high * 16 + low) as u8);
                index += 3;
                continue;
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&decoded).to_string()
}

fn basename(value: &str) -> String {
    value
        .replace('\\', "/")
        .rsplit('/')
        .next()
        .unwrap_or("")
        .trim()
        .to_string()
}

/// Extracts a filename from Content-Disposition. RFC 5987 `filename*`
/// values take precedence over the legacy `filename` parameter. Path
/// separators are stripped so a remote header cannot escape the save folder.
fn filename_from_content_disposition(value: &str) -> Option<String> {
    let mut fallback = None;
    for parameter in value.split(';').skip(1) {
        let Some((name, raw)) = parameter.split_once('=') else {
            continue;
        };
        let name = name.trim().to_ascii_lowercase();
        let mut raw = raw.trim();
        if raw.len() >= 2 && raw.starts_with('"') && raw.ends_with('"') {
            raw = &raw[1..raw.len() - 1];
        }
        if name == "filename*" {
            let encoded = raw.splitn(3, '\'').nth(2).unwrap_or(raw);
            let decoded = basename(&percent_decode(encoded));
            if !decoded.is_empty() {
                return Some(decoded);
            }
        } else if name == "filename" && fallback.is_none() {
            let candidate = basename(&raw.replace("\\\"", "\""));
            if !candidate.is_empty() {
                fallback = Some(candidate);
            }
        }
    }
    fallback
}

fn probe_download_metadata(
    status: reqwest::StatusCode,
    content_range: Option<&str>,
    content_length: Option<&str>,
    content_disposition: Option<&str>,
    content_type: Option<&str>,
    etag: Option<&str>,
    last_modified: Option<&str>,
) -> Result<ProbeInfo, String> {
    let mime_type = content_type
        .and_then(|value| value.split(';').next())
        .unwrap_or("")
        .trim()
        .to_string();
    let filename = content_disposition.and_then(filename_from_content_disposition);
    let mut info = ProbeInfo {
        supports_ranges: false,
        total_size: 0,
        mime_type,
        filename,
        etag: etag.unwrap_or("").to_string(),
        last_modified: last_modified.unwrap_or("").to_string(),
    };

    if status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
        info.total_size = content_range
            .and_then(parse_content_range)
            .and_then(|(_, _, total)| total)
            .unwrap_or(0);
        return Ok(info);
    }
    if status == reqwest::StatusCode::PARTIAL_CONTENT {
        if let Some((start, end, total)) = content_range.and_then(parse_content_range)
            && let Some(total) = total.filter(|total| *total > 0)
        {
            info.supports_ranges = start == 0 && end < total;
            info.total_size = total;
            return Ok(info);
        }
        // A broken Content-Range header means the server cannot be trusted
        // with parallel ranges. Retry as a normal single-connection download.
        return Ok(info);
    }
    if !status.is_success() {
        return Err(format!("Server returned HTTP {status}"));
    }
    info.total_size = content_length
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    Ok(info)
}

/// Parses Retry-After in either delta-seconds or HTTP-date form.
fn retry_after_delay(value: Option<&str>) -> Option<Duration> {
    let value = value?.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds).min(MAX_RETRY_DELAY));
    }
    let date = DateTime::parse_from_rfc2822(value).ok()?;
    let wait = date.with_timezone(&Utc) - Utc::now();
    wait.to_std().ok().map(|wait| wait.min(MAX_RETRY_DELAY))
}

fn retry_delay(attempt: usize) -> Duration {
    Duration::from_millis(500 * 2u64.pow(attempt.min(3) as u32))
}

fn wait_before_retry(control: &AtomicU8, attempt: usize) -> bool {
    let deadline = Instant::now() + retry_delay(attempt);
    while control.load(Ordering::SeqCst) == RUNNING {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return true;
        }
        thread::sleep(remaining.min(Duration::from_millis(100)));
    }
    false
}

fn progress_event_due(last_emit: &mut Instant, now: Instant, force: bool) -> bool {
    if force || now.duration_since(*last_emit) >= PROGRESS_EVENT_INTERVAL {
        *last_emit = now;
        true
    } else {
        false
    }
}

fn requires_media_extractor(url: &reqwest::Url, requested: bool) -> bool {
    let host = url.host_str().unwrap_or("").to_ascii_lowercase();
    let stream_site = [
        "youtube.com",
        "youtu.be",
        "vimeo.com",
        "tiktok.com",
        "twitter.com",
        "x.com",
        "instagram.com",
        "facebook.com",
        "fb.watch",
        "reddit.com",
        "twitch.tv",
        "dailymotion.com",
        "rumble.com",
        "soundcloud.com",
        "bilibili.com",
    ]
    .iter()
    .any(|site| host == *site || host.ends_with(&format!(".{site}")));
    let path = url.path().to_ascii_lowercase();
    requested || stream_site || path.ends_with(".m3u8") || path.ends_with(".mpd")
}

fn media_progress(line: &str) -> Option<(u64, u64)> {
    let fields = line
        .strip_prefix("SDL_PROGRESS:")?
        .split('|')
        .collect::<Vec<_>>();
    if fields.len() != 3 {
        return None;
    }
    let downloaded = fields[0].parse().ok()?;
    // Only trust yt-dlp's exact byte total. The estimate is frequently a
    // tiny placeholder for fragmented or live streams and must not be shown
    // as if it were the real file size.
    let total = fields[1].parse().ok().unwrap_or(0);
    Some((downloaded, total))
}

fn plausible_media_total(downloaded: u64, total: u64) -> Option<u64> {
    (total > 0 && total >= downloaded).then_some(total)
}

fn media_video_format(has_ffmpeg: bool) -> &'static str {
    if has_ffmpeg {
        "bestvideo*+bestaudio/best"
    } else {
        "b"
    }
}

fn media_output_filename(
    requested: &str,
    output: &Path,
    audio_only: bool,
    has_ffmpeg: bool,
) -> String {
    let requested_has_extension = Path::new(requested).extension().is_some();
    if requested_has_extension && (audio_only || has_ffmpeg) {
        return requested.to_string();
    }
    let Some(extension) = output
        .extension()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
    else {
        return requested.to_string();
    };
    let mut filename = PathBuf::from(requested);
    filename.set_extension(extension);
    filename
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string()
}

fn valid_media_executable(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if metadata.len() < 1024 * 1024 {
        return false;
    }
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let mut dos_header = [0; 64];
    if file.read_exact(&mut dos_header).is_err() || &dos_header[..2] != b"MZ" {
        return false;
    }
    let pe_offset = u32::from_le_bytes([
        dos_header[0x3c],
        dos_header[0x3d],
        dos_header[0x3e],
        dos_header[0x3f],
    ]) as u64;
    if file.seek(SeekFrom::Start(pe_offset)).is_err() {
        return false;
    }
    let mut pe_header = [0; 4];
    file.read_exact(&mut pe_header).is_ok() && pe_header == *b"PE\0\0"
}

fn find_media_executable(app: &AppHandle, name: &str) -> Option<PathBuf> {
    let executable = format!("{name}.exe");
    let mut candidates = Vec::new();
    if let Ok(path) = app.path().resolve(
        format!("bin/{executable}"),
        tauri::path::BaseDirectory::Resource,
    ) {
        candidates.push(path);
    }
    if let Some(project_root) = Path::new(env!("CARGO_MANIFEST_DIR")).parent() {
        candidates.push(project_root.join("bin").join(&executable));
    }
    if let Some(path) = std::env::var_os("PATH") {
        candidates
            .extend(std::env::split_paths(&path).map(|directory| directory.join(&executable)));
    }
    if let Some(path) = candidates
        .into_iter()
        .find(|path| valid_media_executable(path))
    {
        return Some(path);
    }
    None
}

#[cfg(windows)]
fn hide_console_window(command: &mut Command) {
    use std::os::windows::process::CommandExt;

    command.creation_flags(0x08000000);
}

#[cfg(not(windows))]
fn hide_console_window(_command: &mut Command) {}

fn resolve_media_executable(app: &AppHandle, name: &str) -> Result<PathBuf, String> {
    if let Some(path) = find_media_executable(app, name) {
        return Ok(path);
    }
    let executable = format!("{name}.exe");
    Err(format!(
        "{executable} is required for video downloads. Install it or restore the bundled Git LFS files."
    ))
}

fn update_media_progress(
    backend: &Backend,
    app: &AppHandle,
    id: &str,
    downloaded: u64,
    total: u64,
    last_sample: &mut (u64, Instant),
    last_emit: &mut Instant,
) -> Result<(), String> {
    let now = Instant::now();
    let elapsed = now.duration_since(last_sample.1).as_secs_f64().max(0.001);
    let speed = downloaded.saturating_sub(last_sample.0) as f64 / elapsed;
    *last_sample = (downloaded, now);
    let snapshot = backend.update_progress(id, |current| {
        current.downloaded_size = downloaded;
        current.stage = "Downloading video".to_string();
        if let Some(total) = plausible_media_total(downloaded, total) {
            current.total_size = total;
            current.progress = (downloaded as f64 / total as f64 * 100.0).min(100.0);
        } else if current.total_size > 0 && current.total_size < downloaded {
            // A previously accepted total turned out to be wrong (for example
            // a placeholder from the site). Hide it rather than showing
            // something like "9.3 MB / 1 KB".
            current.total_size = 0;
            current.progress = 0.0;
        }
        current.resumable = true;
        current.speed = speed as u64;
    })?;
    let completed =
        plausible_media_total(downloaded, total).is_some_and(|total| downloaded >= total);
    if progress_event_due(last_emit, now, completed) {
        emit(app, "download:progress", &event_snapshot(&snapshot));
        emit(
            app,
            "speed:update",
            &json!({
                "downloadSpeed": active_download_speed(backend),
                "activeDownloads": active_download_count(backend)
            }),
        );
    }
    Ok(())
}

fn update_media_partial_size(backend: &Backend, id: &str, directory: &Path) -> Result<(), String> {
    let prefix = format!(".{id}.");
    let size = fs::read_dir(directory)
        .map_err(|error| format!("Could not inspect partial video download: {error}"))?
        .filter_map(Result::ok)
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.starts_with(&prefix)
                && (name.ends_with(".part")
                    || name.contains(".part-Frag")
                    || name.ends_with(".ytdl"))
        })
        .filter_map(|entry| entry.metadata().ok())
        .map(|metadata| metadata.len())
        .sum();
    backend.update_download(id, |current| {
        current.downloaded_size = size;
        current.speed = 0;
        current.resumable = true;
        if current.total_size > 0 {
            if size > current.total_size {
                current.total_size = 0;
                current.progress = 0.0;
            } else {
                current.progress =
                    (size as f64 / current.total_size as f64 * 100.0).min(100.0);
            }
        }
    })?;
    backend.persist_downloads()
}

fn remove_media_partials(directory: &Path, id: &str) -> Result<(), String> {
    let prefix = format!(".{id}.");
    for entry in fs::read_dir(directory)
        .map_err(|error| format!("Could not inspect partial video download: {error}"))?
    {
        let entry = entry.map_err(|error| error.to_string())?;
        if entry.file_name().to_string_lossy().starts_with(&prefix) {
            fs::remove_file(entry.path())
                .map_err(|error| format!("Could not remove partial video download: {error}"))?;
        }
    }
    Ok(())
}

/// Stop a child process and everything it spawned (yt-dlp launches ffmpeg).
fn terminate_process_tree(child: &mut std::process::Child) -> Result<(), String> {
    #[cfg(windows)]
    {
        let pid = child.id();
        let status = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        if matches!(status, Ok(status) if status.success()) {
            return Ok(());
        }
    }
    match child.kill() {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => Ok(()),
        Err(error) => Err(format!("Could not stop the video downloader: {error}")),
    }
}

fn run_media_download(
    app: &AppHandle,
    backend: &Backend,
    id: &str,
    control: &Arc<AtomicU8>,
    download: &Download,
    destination: &Path,
) -> Result<(), String> {
    backend.update_download(id, |current| {
        current.stage = "Preparing video".to_string();
    })?;
    let media_started = Instant::now();
    let initial_media_bytes = download.downloaded_size;
    let yt_dlp = resolve_media_executable(app, "yt-dlp")?;
    let deno = find_media_executable(app, "deno");
    let output_template = destination
        .join(format!(".{id}.%(ext)s"))
        .to_string_lossy()
        .into_owned();
    let audio_only = Path::new(&download.filename)
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("mp3"));
    let ffmpeg = find_media_executable(app, "ffmpeg");
    if audio_only && ffmpeg.is_none() {
        return Err(
            "ffmpeg.exe is required to extract audio. Install a 64-bit FFmpeg build and add its folder to PATH."
                .to_string(),
        );
    }
    let mut command = Command::new(yt_dlp);
    command
        .arg("--ignore-config")
        .arg("--newline")
        .arg("--no-playlist")
        .arg("--no-colors")
        .arg("--continue")
        .arg("--no-overwrites")
        .arg("--socket-timeout")
        .arg("15")
        .arg("--retries")
        .arg("3")
        .arg("--fragment-retries")
        .arg("3")
        .arg("--concurrent-fragments")
        .arg(download.max_segments.clamp(1, 16).to_string())
        .arg("--extractor-retries")
        .arg("1")
        .arg("--file-access-retries")
        .arg("2")
        .arg("--progress-template")
        .arg("download:SDL_PROGRESS:%(progress.downloaded_bytes)s|%(progress.total_bytes)s|%(progress.total_bytes_estimate)s")
        .arg("--print")
        .arg("after_move:SDL_FILE:%(filepath)s")
        .arg("--print")
        .arg("before_dl:SDL_STAGE:Downloading video")
        .arg("--no-quiet")
        .arg("--output")
        .arg(output_template);
    if let Some(ffmpeg) = ffmpeg.as_ref() {
        command.arg("--ffmpeg-location").arg(ffmpeg);
    }
    if let Some(deno) = deno.as_ref() {
        command
            .arg("--js-runtimes")
            .arg(format!("deno:{}", deno.display()))
            .arg("--remote-components")
            .arg("ejs:github");
    }
    if audio_only {
        command
            .arg("--extract-audio")
            .arg("--audio-format")
            .arg("mp3");
    } else if ffmpeg.is_some() {
        command
            .arg("--format")
            .arg(media_video_format(true))
            .arg("--merge-output-format")
            .arg("mp4");
    } else {
        command.arg("--format").arg(media_video_format(false));
    }
    if !download.referrer.is_empty() {
        command
            .arg("--add-header")
            .arg(format!("Referer:{}", download.referrer));
    }
    if !download.cookies.is_empty() {
        command
            .arg("--add-header")
            .arg(format!("Cookie:{}", download.cookies));
    }
    if !download.user_agent.is_empty() {
        command.arg("--user-agent").arg(&download.user_agent);
    }
    command
        .arg("--")
        .arg(&download.url)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    hide_console_window(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| format!("Could not start yt-dlp. Install yt-dlp and ffmpeg: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Could not read yt-dlp progress".to_string())?;
    let (line_tx, line_rx) = std::sync::mpsc::channel::<String>();
    let stdout_tx = line_tx.clone();
    let reader = thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            match line {
                Ok(line) => {
                    if stdout_tx.send(line).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    let _ = stdout_tx.send(format!("SDL_READER_ERROR:{error}"));
                    break;
                }
            }
        }
    });
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "Could not read yt-dlp errors".to_string())?;
    let stderr_output = Arc::new(Mutex::new(String::new()));
    let stderr_capture = stderr_output.clone();
    let progress_tx = line_tx.clone();
    let stderr_reader = thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            match line {
                Ok(line) => {
                    if line.starts_with("SDL_PROGRESS:") {
                        let _ = progress_tx.send(line);
                        continue;
                    }
                    if let Ok(mut output) = stderr_capture.lock() {
                        output.push_str(&line);
                        output.push('\n');
                        if output.len() > 8192 {
                            let cutoff = output.len() - 8192;
                            let boundary = output[cutoff..]
                                .find('\n')
                                .map(|offset| cutoff + offset + 1)
                                .unwrap_or(cutoff);
                            output.drain(..boundary);
                        }
                    }
                }
                Err(error) => {
                    if let Ok(mut output) = stderr_capture.lock() {
                        output.push_str(&format!("Could not read yt-dlp stderr: {error}\n"));
                    }
                    break;
                }
            }
        }
    });

    let mut last_sample = (download.downloaded_size, Instant::now());
    let mut last_emit = Instant::now()
        .checked_sub(PROGRESS_EVENT_INTERVAL)
        .unwrap_or_else(Instant::now);
    let mut last_persisted = download.downloaded_size;
    let mut output_path = None;
    let exit_status = loop {
        if control.load(Ordering::SeqCst) != RUNNING {
            terminate_process_tree(&mut child)?;
            child
                .wait()
                .map_err(|error| format!("Could not wait for yt-dlp to stop: {error}"))?;
            reader
                .join()
                .map_err(|_| "yt-dlp progress reader crashed".to_string())?;
            stderr_reader
                .join()
                .map_err(|_| "yt-dlp error reader crashed".to_string())?;
            if control.load(Ordering::SeqCst) == CANCEL_REQUESTED {
                remove_media_partials(destination, id)?;
                backend.update_download(id, |current| {
                    current.downloaded_size = 0;
                    current.progress = 0.0;
                    current.speed = 0;
                })?;
            } else {
                update_media_partial_size(backend, id, destination)?;
            }
            return Ok(());
        }

        loop {
            match line_rx.try_recv() {
                Ok(line) => {
                    if let Some(stage) = line.strip_prefix("SDL_STAGE:") {
                        let snapshot = backend.update_download(id, |current| {
                            current.stage = stage.to_string();
                        })?;
                        emit(app, "download:progress", &event_snapshot(&snapshot));
                    } else if let Some((downloaded, total)) = media_progress(&line) {
                        update_media_progress(
                            backend,
                            app,
                            id,
                            downloaded,
                            total,
                            &mut last_sample,
                            &mut last_emit,
                        )?;
                        if downloaded / (1024 * 1024) > last_persisted / (1024 * 1024) {
                            backend.persist_downloads()?;
                            last_persisted = downloaded;
                        }
                    } else if let Some(path) = line.strip_prefix("SDL_FILE:") {
                        output_path = Some(PathBuf::from(path));
                    } else if let Some(error) = line.strip_prefix("SDL_READER_ERROR:") {
                        return Err(format!("Could not read yt-dlp progress: {error}"));
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
            }
        }
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("Could not check yt-dlp status: {error}"))?
        {
            break status;
        }
        thread::sleep(Duration::from_millis(100));
    };
    reader
        .join()
        .map_err(|_| "yt-dlp progress reader crashed".to_string())?;
    stderr_reader
        .join()
        .map_err(|_| "yt-dlp error reader crashed".to_string())?;
    for line in line_rx.try_iter() {
        if let Some(stage) = line.strip_prefix("SDL_STAGE:") {
            let snapshot = backend.update_download(id, |current| {
                current.stage = stage.to_string();
            })?;
            emit(app, "download:progress", &event_snapshot(&snapshot));
        } else if let Some((downloaded, total)) = media_progress(&line) {
            update_media_progress(
                backend,
                app,
                id,
                downloaded,
                total,
                &mut last_sample,
                &mut last_emit,
            )?;
            if downloaded / (1024 * 1024) > last_persisted / (1024 * 1024) {
                backend.persist_downloads()?;
                last_persisted = downloaded;
            }
        } else if let Some(path) = line.strip_prefix("SDL_FILE:") {
            output_path = Some(PathBuf::from(path));
        }
    }
    if control.load(Ordering::SeqCst) != RUNNING {
        if control.load(Ordering::SeqCst) == CANCEL_REQUESTED {
            remove_media_partials(destination, id)?;
            backend.update_download(id, |current| {
                current.downloaded_size = 0;
                current.progress = 0.0;
                current.speed = 0;
            })?;
        } else {
            update_media_partial_size(backend, id, destination)?;
        }
        return Ok(());
    }
    if !exit_status.success() {
        update_media_partial_size(backend, id, destination)?;
        let details = stderr_output
            .lock()
            .map_err(|_| "yt-dlp error-output lock was poisoned".to_string())?
            .trim()
            .to_string();
        return Err(if details.is_empty() {
            format!(
                "yt-dlp could not download this video (exit code {}). Check that the page is available and the bundled extractors are up to date.",
                exit_status.code().unwrap_or(-1)
            )
        } else {
            format!("yt-dlp could not download this video: {details}")
        });
    }
    let output_path = output_path
        .ok_or_else(|| "yt-dlp finished without reporting the output file".to_string())?;
    let total_size = fs::metadata(&output_path)
        .map_err(|error| format!("Could not locate yt-dlp output file: {error}"))?
        .len();
    let final_filename = media_output_filename(
        &download.filename,
        &output_path,
        audio_only,
        ffmpeg.is_some(),
    );
    let final_path = unique_destination(destination, &final_filename);
    fs::rename(&output_path, &final_path)
        .map_err(|error| format!("Could not finalize video download: {error}"))?;
    if let Err(error) = remove_media_partials(destination, id) {
        eprintln!("Could not remove leftover partial video files: {error}");
    }
    let filename = final_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let elapsed = media_started.elapsed().as_secs_f64().max(0.001);
    let average_speed = ((total_size.saturating_sub(initial_media_bytes)) as f64 / elapsed) as u64;
    backend.update_download(id, |current| {
        current.filename = filename;
        current.category = category_for(&current.filename);
        current.total_size = total_size;
        current.downloaded_size = total_size;
        current.progress = 100.0;
        current.speed = 0;
        current.average_speed = average_speed;
        current.resumable = false;
        current.stage = "Complete".to_string();
        current.error = None;
    })?;
    backend.persist_downloads()
}

fn event_snapshot(download: &Download) -> Value {
    json!({
        "id": download.id,
        "filename": download.filename,
        "category": download.category,
        "progress": download.progress,
        "speed": download.speed,
        "downloaded": download.downloaded_size,
        "downloadedSize": download.downloaded_size,
        "total": download.total_size,
        "totalSize": download.total_size,
        "status": download.status,
        "stage": download.stage,
        "segments": download.segments
    })
}

fn start_download(app: AppHandle, backend: Backend, id: String) -> Result<(), String> {
    let download = backend.get_download(&id)?;
    if !matches!(download.status.as_str(), "complete" | "cancelled") {
        cancel_pending_shutdown(&backend);
    }
    if matches!(
        download.status.as_str(),
        "complete" | "cancelled" | "downloading"
    ) {
        return Ok(());
    }
    // Stop any stale worker before replacing its control flag. Using a pause
    // signal (instead of RUNNING) guarantees a leftover worker cannot keep
    // writing to the file alongside the new one.
    backend.stop_task(&id, PAUSE_REQUESTED)?;
    let download = backend.get_download(&id)?;
    if matches!(download.status.as_str(), "complete" | "cancelled") {
        return Ok(());
    }
    if !download.resumable && download.downloaded_size > 0 {
        backend.update_download(&id, |current| {
            current.downloaded_size = 0;
            current.progress = 0.0;
            current
                .segments
                .iter_mut()
                .for_each(|segment| segment.downloaded = 0);
        })?;
    }
    let _start_guard = backend
        .0
        .start_lock
        .lock()
        .map_err(|_| "Download scheduler lock was poisoned".to_string())?;
    let max_concurrent = backend
        .0
        .settings
        .lock()
        .map_err(|_| "Settings lock was poisoned".to_string())?
        .get("maxConcurrent")
        .and_then(Value::as_u64)
        .unwrap_or(3)
        .max(1) as usize;
    let active_count = backend
        .0
        .downloads
        .lock()
        .map_err(|_| "Downloads lock was poisoned".to_string())?
        .iter()
        .filter(|item| item.id != id && item.status == "downloading")
        .count();
    if active_count >= max_concurrent {
        backend.update_download(&id, |current| {
            current.status = "queued".to_string();
            current.stage = "Queued".to_string();
            current.scheduled_time = None;
        })?;
        return Ok(());
    }
    let control = Arc::new(AtomicU8::new(RUNNING));
    backend
        .0
        .controls
        .lock()
        .map_err(|_| "Download control lock was poisoned".to_string())?
        .insert(id.clone(), control.clone());
    let queued = backend.update_download(&id, |current| {
        current.status = "downloading".to_string();
        current.stage = if current.is_stream {
            "Preparing video".to_string()
        } else {
            "Connecting".to_string()
        };
        current.error = None;
        current.scheduled_time = None;
    })?;
    emit(&app, "download:new", &queued);

    let worker_app = app.clone();
    let worker_backend = backend.clone();
    let worker_id = id.clone();
    let handle = thread::spawn(move || {
        let result = run_download(&worker_app, &worker_backend, &worker_id, &control);
        match result {
            Ok(()) => {
                if let Ok(download) = worker_backend.get_download(&worker_id)
                    && download.status == "downloading"
                    && let Ok(completed) =
                        worker_backend.update_download(&worker_id, |current| {
                            current.status = "complete".to_string();
                            current.progress = 100.0;
                            current.speed = 0;
                            current.date_completed = Some(Utc::now().to_rfc3339());
                        })
                {
                    emit(&worker_app, "download:complete", &completed);
                }
            }
            Err(error) => {
                if let Ok(current) = worker_backend.get_download(&worker_id)
                    && current.status == "downloading"
                    && let Ok(failed_download) =
                        worker_backend.update_download(&worker_id, |item| {
                            item.status = "error".to_string();
                            item.stage = "Error".to_string();
                            item.error = Some(error.clone());
                            item.speed = 0;
                        })
                {
                    emit(&worker_app, "download:error", &failed_download);
                }
            }
        }
        if let Ok(mut controls) = worker_backend.0.controls.lock() {
            controls.remove(&worker_id);
        }
        if let Ok(downloads) = worker_backend.0.downloads.lock() {
            let active = downloads
                .iter()
                .filter(|item| item.status == "downloading")
                .count();
            emit(
                &worker_app,
                "speed:update",
                &json!({
                    "downloadSpeed": 0,
                    "activeDownloads": active
                }),
            );
        }
        start_next_queued(&worker_app, &worker_backend);
        maybe_shutdown_after_complete(&worker_backend);
    });
    backend
        .0
        .tasks
        .lock()
        .map_err(|_| "Download task lock was poisoned".to_string())?
        .insert(id, handle);
    Ok(())
}

fn abort_scheduled_shutdown(backend: &Backend) {
    if backend.0.shutdown_triggered.swap(0, Ordering::SeqCst) == 0 {
        return;
    }
    #[cfg(target_os = "windows")]
    {
        let mut command = Command::new("shutdown.exe");
        hide_console_window(&mut command);
        if let Err(error) = command.arg("/a").spawn() {
            eprintln!("Could not cancel the scheduled system shutdown: {error}");
        }
    }
}

fn cancel_pending_shutdown(backend: &Backend) {
    let Ok(_guard) = backend.0.shutdown_lock.lock() else {
        return;
    };
    abort_scheduled_shutdown(backend);
}

fn maybe_shutdown_after_complete(backend: &Backend) {
    // Serialize the countdown start with cancellation so adding or resuming a
    // download cannot race with `shutdown.exe` and leave the timer armed.
    let Ok(_guard) = backend.0.shutdown_lock.lock() else {
        return;
    };
    let enabled = backend
        .0
        .settings
        .lock()
        .map(|settings| {
            settings
                .get("shutdownAfterComplete")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .unwrap_or(false);
    if !enabled {
        return;
    }
    let all_finished = backend
        .0
        .downloads
        .lock()
        .map(|downloads| {
            downloads
                .iter()
                .all(|download| matches!(download.status.as_str(), "complete" | "cancelled"))
        })
        .unwrap_or(false);
    if !all_finished || backend.0.shutdown_triggered.swap(1, Ordering::SeqCst) != 0 {
        return;
    }
    #[cfg(target_os = "windows")]
    {
        let mut command = Command::new("shutdown.exe");
        hide_console_window(&mut command);
        if let Err(error) = command
            .args([
                "/s",
                "/t",
                "60",
                "/c",
                "Syntax Downloader finished its queue.",
            ])
            .spawn()
        {
            eprintln!("Could not schedule system shutdown: {error}");
            backend.0.shutdown_triggered.store(0, Ordering::SeqCst);
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        eprintln!("Shutdown after completion is only supported on Windows");
        backend.0.shutdown_triggered.store(0, Ordering::SeqCst);
    }
}

fn start_next_queued(app: &AppHandle, backend: &Backend) {
    let max_concurrent = match backend.0.settings.lock() {
        Ok(settings) => settings
            .get("maxConcurrent")
            .and_then(Value::as_u64)
            .unwrap_or(3)
            .max(1) as usize,
        Err(_) => {
            eprintln!("Could not read maximum concurrent downloads: settings lock was poisoned");
            return;
        }
    };
    let downloads = match backend.0.downloads.lock() {
        Ok(downloads) => downloads,
        Err(_) => {
            eprintln!("Could not read queued downloads: downloads lock was poisoned");
            return;
        }
    };
    let active_count = downloads
        .iter()
        .filter(|item| item.status == "downloading")
        .count();
    let queued = downloads
        .iter()
        .filter(|item| item.status == "queued")
        .take(max_concurrent.saturating_sub(active_count))
        .map(|item| item.id.clone())
        .collect::<Vec<_>>();
    drop(downloads);
    for id in queued {
        if let Err(error) = start_download(app.clone(), backend.clone(), id) {
            eprintln!("Could not start queued download: {error}");
        }
    }
}

fn apply_download_headers(
    mut request: reqwest::RequestBuilder,
    download: &Download,
) -> reqwest::RequestBuilder {
    request = request.header(reqwest::header::ACCEPT_ENCODING, "identity");
    if !download.referrer.is_empty() {
        request = request.header(reqwest::header::REFERER, &download.referrer);
    }
    if !download.user_agent.is_empty() {
        request = request.header(reqwest::header::USER_AGENT, &download.user_agent);
    }
    if !download.cookies.is_empty() {
        request = request.header(reqwest::header::COOKIE, &download.cookies);
    }
    request
}

fn retryable_status(status: reqwest::StatusCode) -> bool {
    status.is_server_error()
        || status == reqwest::StatusCode::TOO_MANY_REQUESTS
        || status == reqwest::StatusCode::REQUEST_TIMEOUT
        || status == reqwest::StatusCode::TOO_EARLY
}

fn wait_with_delay(control: &AtomicU8, delay: Duration) -> bool {
    let deadline = Instant::now() + delay;
    while control.load(Ordering::SeqCst) == RUNNING {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return true;
        }
        thread::sleep(remaining.min(Duration::from_millis(100)));
    }
    false
}

fn wait_before_retry_with(
    control: &AtomicU8,
    attempt: usize,
    retry_after: Option<Duration>,
) -> bool {
    let delay = retry_after
        .unwrap_or_else(|| retry_delay(attempt))
        .min(MAX_RETRY_DELAY);
    wait_with_delay(control, delay)
}

/// Performs the range probe and captures server metadata used to name the
/// file and to decide whether parallel connections are safe.
fn probe_remote(
    client: &Client,
    download: &Download,
    control: &AtomicU8,
) -> Result<Option<ProbeInfo>, String> {
    let mut last_error = None;
    for attempt in 0..MAX_PROBE_ATTEMPTS {
        if control.load(Ordering::SeqCst) != RUNNING {
            return Ok(None);
        }
        let request = apply_download_headers(
            client.get(&download.url).header(RANGE, "bytes=0-0"),
            download,
        );
        match tauri::async_runtime::block_on(async move { request.send().await }) {
            Ok(response) => {
                let status = response.status();
                let retry_after = response
                    .headers()
                    .get(RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| retry_after_delay(Some(value)));
                if retryable_status(status) && attempt + 1 < MAX_PROBE_ATTEMPTS {
                    if !wait_before_retry_with(control, attempt, retry_after) {
                        return Ok(None);
                    }
                    continue;
                }
                let content_range = response
                    .headers()
                    .get(CONTENT_RANGE)
                    .and_then(|value| value.to_str().ok());
                let content_length = response
                    .headers()
                    .get(CONTENT_LENGTH)
                    .and_then(|value| value.to_str().ok());
                let content_disposition = response
                    .headers()
                    .get(CONTENT_DISPOSITION)
                    .and_then(|value| value.to_str().ok());
                let content_type = response
                    .headers()
                    .get(CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok());
                let etag = response
                    .headers()
                    .get(reqwest::header::ETAG)
                    .and_then(|value| value.to_str().ok());
                let last_modified = response
                    .headers()
                    .get(reqwest::header::LAST_MODIFIED)
                    .and_then(|value| value.to_str().ok());
                return Ok(Some(probe_download_metadata(
                    status,
                    content_range,
                    content_length,
                    content_disposition,
                    content_type,
                    etag,
                    last_modified,
                )?));
            }
            Err(error)
                if (error.is_timeout() || error.is_connect())
                    && attempt + 1 < MAX_PROBE_ATTEMPTS =>
            {
                last_error = Some(error);
                if !wait_before_retry_with(control, attempt, None) {
                    return Ok(None);
                }
            }
            Err(error) => {
                last_error = Some(error);
                break;
            }
        }
    }
    Err(format!(
        "Could not probe download URL: {}",
        last_error
            .map(|error| error.to_string())
            .unwrap_or_else(|| "the server kept returning a temporary error".to_string())
    ))
}

/// Positioned write without moving a shared file cursor. This lets segment
/// workers write concurrently instead of serializing every chunk behind one
/// mutex.
#[cfg(windows)]
fn write_all_at(file: &File, mut offset: u64, mut data: &[u8]) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !data.is_empty() {
        match file.seek_write(data, offset) {
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    "disk write returned zero bytes",
                ));
            }
            Ok(written) => {
                offset += written as u64;
                data = &data[written..];
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(unix)]
fn write_all_at(file: &File, mut offset: u64, mut data: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;
    while !data.is_empty() {
        match file.write_at(data, offset) {
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    "disk write returned zero bytes",
                ));
            }
            Ok(written) => {
                offset += written as u64;
                data = &data[written..];
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|error| error.to_string())?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn run_download(
    app: &AppHandle,
    backend: &Backend,
    id: &str,
    control: &Arc<AtomicU8>,
) -> Result<(), String> {
    let mut download = backend.get_download(id)?;
    let destination = PathBuf::from(&download.save_dir);
    fs::create_dir_all(&destination)
        .map_err(|error| format!("Could not create download directory: {error}"))?;
    if download.is_stream {
        return run_media_download(app, backend, id, control, &download, &destination);
    }
    let client = Client::builder()
        .connect_timeout(DOWNLOAD_CONNECT_TIMEOUT)
        .read_timeout(DOWNLOAD_READ_TIMEOUT)
        .pool_max_idle_per_host(8)
        .tcp_nodelay(true)
        .user_agent("Syntax Downloader/2.0")
        .build()
        .map_err(|error| error.to_string())?;

    let Some(probe) = probe_remote(&client, &download, control)? else {
        return Ok(());
    };

    // Adopt the server-provided filename only when the user did not choose one.
    if !download.name_locked
        && download.downloaded_size == 0
        && let Some(remote_name) = probe.filename.as_deref()
    {
        let candidate = sanitize_filename(remote_name);
        if candidate != "download" && candidate != download.filename {
            download.filename = candidate;
            download.category = category_for(&download.filename);
        }
    }

    // A resume is only safe when the partial file, the segment counters and
    // the remote file all describe the same bytes.
    let reset_resume = should_reset_resume(&download, &probe);
    if reset_resume {
        download.downloaded_size = 0;
        download.progress = 0.0;
        download.total_size = probe.total_size;
        download.resumable = probe.supports_ranges;
        let configured = download.max_segments.clamp(1, 32) as usize;
        let segment_count = if probe.supports_ranges && probe.total_size > 256 * 1024 {
            configured.min(probe.total_size.div_ceil(1024 * 1024).max(1) as usize)
        } else {
            1
        };
        download.segments = allocate_segments(probe.total_size, segment_count);
    } else {
        for segment in &mut download.segments {
            let length = segment.end.saturating_sub(segment.start).saturating_add(1);
            if download.total_size > 0 {
                segment.downloaded = segment.downloaded.min(length);
            }
        }
        download.downloaded_size = download
            .segments
            .iter()
            .map(|segment| segment.downloaded)
            .sum();
    }
    download.mime_type = probe.mime_type.clone();
    download.etag = probe.etag.clone();
    download.last_modified = probe.last_modified.clone();
    download.error = None;

    let file_path = destination.join(format!("{}.syntax_tmp", download.filename));
    let resume_file_exists = file_path.exists();
    if download.downloaded_size > 0 && !resume_file_exists {
        download.downloaded_size = 0;
        download.progress = 0.0;
        download
            .segments
            .iter_mut()
            .for_each(|segment| segment.downloaded = 0);
    }

    backend.update_download(id, |current| {
        current.filename = download.filename.clone();
        current.category = download.category.clone();
        current.total_size = download.total_size;
        current.downloaded_size = download.downloaded_size;
        current.progress = download.progress;
        current.resumable = download.resumable;
        current.segments = download.segments.clone();
        current.mime_type = download.mime_type.clone();
        current.etag = download.etag.clone();
        current.last_modified = download.last_modified.clone();
        current.stage = "Starting transfer".to_string();
    })?;

    let open_result = if download.downloaded_size > 0 && resume_file_exists {
        OpenOptions::new().write(true).open(&file_path)
    } else {
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&file_path)
    };
    let file = open_result.map_err(|error| format!("Could not create temporary file: {error}"))?;
    if download.total_size > 0 {
        file.set_len(download.total_size)
            .map_err(|error| format!("Could not preallocate download file: {error}"))?;
    }
    let file = Arc::new(file);
    let started = Instant::now();
    let initial_downloaded_size = download.downloaded_size;
    let speed_meter = Arc::new(SpeedMeter::new(download.downloaded_size));
    let progress_last_emit = Arc::new(Mutex::new(
        Instant::now()
            .checked_sub(PROGRESS_EVENT_INTERVAL)
            .unwrap_or_else(Instant::now),
    ));
    let last_persist = Arc::new(Mutex::new(
        Instant::now()
            .checked_sub(PROGRESS_PERSIST_INTERVAL)
            .unwrap_or_else(Instant::now),
    ));
    let failed = Arc::new(AtomicU8::new(0));
    let (error_tx, error_rx) = std::sync::mpsc::channel::<String>();
    let mut workers = Vec::new();

    for segment in download.segments.clone() {
        let length = segment.end.saturating_sub(segment.start).saturating_add(1);
        if download.total_size > 0 && segment.downloaded >= length {
            continue;
        }
        let client = client.clone();
        let file = file.clone();
        let app = app.clone();
        let backend = backend.clone();
        let id = id.to_string();
        let control = control.clone();
        let error_tx = error_tx.clone();
        let failed = failed.clone();
        let speed_meter = speed_meter.clone();
        let progress_last_emit = progress_last_emit.clone();
        let last_persist = last_persist.clone();
        let ranged = download.resumable && download.total_size > 0;
        let total_size = download.total_size;
        let download_url = download.url.clone();
        let referrer = download.referrer.clone();
        let cookies = download.cookies.clone();
        let user_agent = download.user_agent.clone();
        let segment_index = segment.index;
        let segment_end = segment.end;
        let base = if ranged {
            segment.start + segment.downloaded
        } else {
            0
        };
        workers.push(thread::spawn(move || {
            let result = (|| -> Result<(), String> {
                if control.load(Ordering::SeqCst) != RUNNING {
                    return Ok(());
                }
                let mut written = 0u64;
                for attempt in 0..MAX_SEGMENT_ATTEMPTS {
                    if control.load(Ordering::SeqCst) != RUNNING
                        || failed.load(Ordering::SeqCst) != 0
                    {
                        return Ok(());
                    }
                    let range_start = base + written;
                    if ranged && range_start > segment_end {
                        return Ok(());
                    }
                    let mut request = client
                        .get(&download_url)
                        .header(reqwest::header::ACCEPT_ENCODING, "identity");
                    if ranged {
                        request =
                            request.header(RANGE, format!("bytes={range_start}-{segment_end}"));
                    }
                    if !referrer.is_empty() {
                        request = request.header(reqwest::header::REFERER, &referrer);
                    }
                    if !user_agent.is_empty() {
                        request = request.header(reqwest::header::USER_AGENT, &user_agent);
                    }
                    if !cookies.is_empty() {
                        request = request.header(reqwest::header::COOKIE, &cookies);
                    }
                    let mut reader =
                        match tauri::async_runtime::block_on(async move { request.send().await }) {
                            Ok(response) => response,
                            Err(error) => {
                                if control.load(Ordering::SeqCst) != RUNNING {
                                    return Ok(());
                                }
                                if attempt + 1 < MAX_SEGMENT_ATTEMPTS
                                    && (error.is_timeout() || error.is_connect())
                                {
                                    if !wait_before_retry(&control, attempt) {
                                        return Ok(());
                                    }
                                    continue;
                                }
                                return Err(format!("HTTP request failed: {error}"));
                            }
                        };

                    let mut response_end = segment_end;
                    if ranged {
                        if reader.status() != reqwest::StatusCode::PARTIAL_CONTENT {
                            let status = reader.status();
                            if retryable_status(status) && attempt + 1 < MAX_SEGMENT_ATTEMPTS {
                                let retry_after = reader
                                    .headers()
                                    .get(RETRY_AFTER)
                                    .and_then(|value| value.to_str().ok())
                                    .and_then(|value| retry_after_delay(Some(value)));
                                if !wait_before_retry_with(&control, attempt, retry_after) {
                                    return Ok(());
                                }
                                continue;
                            }
                            return Err(format!("Server did not honor byte range (HTTP {status})"));
                        }
                        let actual = reader
                            .headers()
                            .get(CONTENT_RANGE)
                            .and_then(|value| value.to_str().ok())
                            .unwrap_or("");
                        let Some((actual_start, actual_end, actual_total)) =
                            parse_content_range(actual)
                        else {
                            return Err("Server returned a malformed byte range".to_string());
                        };
                        if actual_start != range_start || actual_end < actual_start {
                            return Err("Server returned an unexpected byte range".to_string());
                        }
                        if actual_end > segment_end {
                            return Err("Server sent more data than requested".to_string());
                        }
                        if let Some(actual_total) = actual_total
                            && actual_total != total_size
                        {
                            return Err(
                                "The remote file changed while the download was running"
                                    .to_string(),
                            );
                        }
                        response_end = actual_end;
                    } else if !reader.status().is_success() {
                        let status = reader.status();
                        if retryable_status(status) && attempt + 1 < MAX_SEGMENT_ATTEMPTS {
                            let retry_after = reader
                                .headers()
                                .get(RETRY_AFTER)
                                .and_then(|value| value.to_str().ok())
                                .and_then(|value| retry_after_delay(Some(value)));
                            if !wait_before_retry_with(&control, attempt, retry_after) {
                                return Ok(());
                            }
                            continue;
                        }
                        return Err(format!("Server returned HTTP {status}"));
                    }

                    let expected_attempt = if ranged {
                        response_end - range_start + 1
                    } else if total_size > 0 {
                        total_size.saturating_sub(written)
                    } else {
                        u64::MAX
                    };
                    let mut attempt_written = 0u64;
                    let mut read_error = None;
                    loop {
                        if control.load(Ordering::SeqCst) != RUNNING
                            || failed.load(Ordering::SeqCst) != 0
                        {
                            return Ok(());
                        }
                        let chunk =
                            match tauri::async_runtime::block_on(async { reader.chunk().await }) {
                                Ok(Some(chunk)) => chunk,
                                Ok(None) => break,
                                Err(error) => {
                                    if control.load(Ordering::SeqCst) != RUNNING {
                                        return Ok(());
                                    }
                                    read_error = Some(format!("Network read failed: {error}"));
                                    break;
                                }
                            };
                        let count = chunk.len() as u64;
                        if ranged && attempt_written + count > expected_attempt {
                            return Err("Server sent more data than requested".to_string());
                        }
                        if !ranged
                            && total_size > 0
                            && written + attempt_written + count > total_size
                        {
                            return Err("Server sent more data than expected".to_string());
                        }
                        write_all_at(&file, range_start + attempt_written, &chunk)
                            .map_err(|error| format!("Disk write failed: {error}"))?;
                        attempt_written += count;
                        written += count;

                        let snapshot = backend.update_progress(&id, |current| {
                            current.stage = "Downloading".to_string();
                            if let Some(progress) = current
                                .segments
                                .iter_mut()
                                .find(|item| item.index == segment_index)
                            {
                                progress.downloaded += count;
                                if current.total_size > 0 {
                                    let length = progress
                                        .end
                                        .saturating_sub(progress.start)
                                        .saturating_add(1);
                                    progress.downloaded = progress.downloaded.min(length);
                                }
                            }
                            current.downloaded_size =
                                current.segments.iter().map(|item| item.downloaded).sum();
                            current.progress = if current.total_size > 0 {
                                (current.downloaded_size as f64 / current.total_size as f64 * 100.0)
                                    .min(100.0)
                            } else {
                                0.0
                            };
                            current.speed = speed_meter.sample(current.downloaded_size);
                        })?;

                        let limit = backend
                            .0
                            .speed_limit
                            .lock()
                            .map(|value| *value)
                            .unwrap_or(0);
                        if !backend.0.rate_limiter.consume(count, limit, &control) {
                            return Ok(());
                        }

                        let should_emit = {
                            let mut last_emit = progress_last_emit
                                .lock()
                                .map_err(|_| "Progress event lock was poisoned".to_string())?;
                            progress_event_due(
                                &mut last_emit,
                                Instant::now(),
                                snapshot.total_size > 0
                                    && snapshot.downloaded_size >= snapshot.total_size,
                            )
                        };
                        if should_emit {
                            emit(&app, "download:progress", &event_snapshot(&snapshot));
                            emit(
                                &app,
                                "speed:update",
                                &json!({
                                    "downloadSpeed": active_download_speed(&backend),
                                    "activeDownloads": active_download_count(&backend)
                                }),
                            );
                        }

                        let should_persist = {
                            let mut last = last_persist
                                .lock()
                                .map_err(|_| "Persistence timer lock was poisoned".to_string())?;
                            if last.elapsed() >= PROGRESS_PERSIST_INTERVAL {
                                *last = Instant::now();
                                true
                            } else {
                                false
                            }
                        };
                        if should_persist {
                            backend.persist_downloads()?;
                        }
                    }
                    if control.load(Ordering::SeqCst) != RUNNING {
                        return Ok(());
                    }
                    if let Some(error) = read_error {
                        if !ranged {
                            // A plain response cannot be resumed; restart it
                            // from the beginning and discard any stale tail.
                            written = 0;
                            file.set_len(total_size).map_err(|error| {
                                format!("Could not reset partial file: {error}")
                            })?;
                            backend.update_download(&id, |current| {
                                current.downloaded_size = 0;
                                current.progress = 0.0;
                                current.speed = 0;
                                current
                                    .segments
                                    .iter_mut()
                                    .for_each(|segment| segment.downloaded = 0);
                            })?;
                            speed_meter.reset(0);
                        }
                        if attempt + 1 < MAX_SEGMENT_ATTEMPTS {
                            if !wait_before_retry(&control, attempt) {
                                return Ok(());
                            }
                            continue;
                        }
                        return Err(error);
                    }
                    let incomplete = if ranged {
                        attempt_written < expected_attempt
                    } else {
                        total_size > 0 && attempt_written < total_size
                    };
                    if incomplete {
                        if !ranged {
                            written = 0;
                            file.set_len(total_size).map_err(|error| {
                                format!("Could not reset partial file: {error}")
                            })?;
                            backend.update_download(&id, |current| {
                                current.downloaded_size = 0;
                                current.progress = 0.0;
                                current.speed = 0;
                                current
                                    .segments
                                    .iter_mut()
                                    .for_each(|segment| segment.downloaded = 0);
                            })?;
                            speed_meter.reset(0);
                        }
                        if attempt + 1 < MAX_SEGMENT_ATTEMPTS {
                            if !wait_before_retry(&control, attempt) {
                                return Ok(());
                            }
                            continue;
                        }
                        return Err(if ranged {
                            "The server returned an incomplete byte range".to_string()
                        } else {
                            "The server returned an incomplete download".to_string()
                        });
                    }
                    if ranged && response_end < segment_end {
                        // The server clipped the range; continue from the
                        // exact byte where it stopped.
                        if attempt + 1 < MAX_SEGMENT_ATTEMPTS {
                            if !wait_before_retry(&control, attempt) {
                                return Ok(());
                            }
                            continue;
                        }
                    }
                    return Ok(());
                }
                Err("Download segment exhausted its retry attempts".to_string())
            })();
            if let Err(error) = result
                && failed.swap(1, Ordering::SeqCst) == 0
            {
                let _ = error_tx.send(error);
            }
        }));
    }
    drop(error_tx);
    for worker in workers {
        if worker.join().is_err() {
            return Err("A download segment worker crashed".to_string());
        }
    }
    if let Ok(error) = error_rx.try_recv() {
        return Err(error);
    }
    match control.load(Ordering::SeqCst) {
        PAUSE_REQUESTED => {
            backend.0.rate_limiter.reset();
            backend.update_download(id, |current| {
                current.status = "paused".to_string();
                current.stage = "Paused".to_string();
                current.speed = 0;
            })?;
            backend.persist_downloads()?;
            let paused = backend.get_download(id)?;
            emit(app, "download:paused", &paused);
            return Ok(());
        }
        CANCEL_REQUESTED => {
            backend.0.rate_limiter.reset();
            let _ = fs::remove_file(&file_path);
            let canceled = backend
                .update_download(id, |current| {
                    current.status = "cancelled".to_string();
                    current.stage = "Cancelled".to_string();
                    current.speed = 0;
                })
                .ok();
            backend.persist_downloads()?;
            if let Some(canceled) = canceled {
                emit(app, "download:cancelled", &canceled);
            }
            return Ok(());
        }
        _ => {}
    }

    let current = backend.get_download(id)?;
    if current.total_size > 0 && current.downloaded_size != current.total_size {
        return Err(format!(
            "Downloaded {} of {} bytes",
            current.downloaded_size, current.total_size
        ));
    }
    let final_size = if current.total_size > 0 {
        current.total_size
    } else {
        current.downloaded_size
    };
    if current.total_size == 0 {
        file.set_len(final_size)
            .map_err(|error| format!("Could not trim the downloaded file: {error}"))?;
    }
    file.sync_all()
        .map_err(|error| format!("Could not flush download file: {error}"))?;
    let final_path = unique_destination(&destination, &download.filename);
    fs::rename(&file_path, &final_path)
        .map_err(|error| format!("Could not finalize download: {error}"))?;
    let filename = final_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let elapsed = started.elapsed().as_secs_f64().max(0.001);
    let average_speed =
        ((final_size.saturating_sub(initial_downloaded_size)) as f64 / elapsed) as u64;
    let hash_verified = if download.expected_sha256.is_empty() {
        false
    } else {
        let actual = sha256_file(&final_path)?;
        if !actual.eq_ignore_ascii_case(&download.expected_sha256) {
            backend.update_download(id, |current| {
                current.filename = filename.clone();
                current.category = category_for(&current.filename);
                current.total_size = final_size;
                current.downloaded_size = final_size;
                current.progress = 100.0;
                current.speed = 0;
                current.hash_verified = false;
                current.stage = "Checksum mismatch".to_string();
            })?;
            // Do not leave a file that failed verification sitting next to
            // the user's other downloads as if it were valid.
            let _ = fs::remove_file(&final_path);
            return Err(format!(
                "SHA-256 verification failed: expected {}, got {actual}",
                download.expected_sha256
            ));
        }
        true
    };
    backend.update_download(id, |current| {
        current.filename = filename;
        current.category = category_for(&current.filename);
        current.total_size = final_size;
        current.downloaded_size = final_size;
        current.progress = 100.0;
        current.speed = 0;
        current.average_speed = average_speed;
        current.hash_verified = hash_verified;
        current.error = None;
        current.stage = "Complete".to_string();
    })?;
    Ok(())
}

fn active_download_count(backend: &Backend) -> usize {
    backend
        .0
        .downloads
        .lock()
        .map(|downloads| {
            downloads
                .iter()
                .filter(|download| download.status == "downloading")
                .count()
        })
        .unwrap_or(0)
}

fn active_download_speed(backend: &Backend) -> u64 {
    backend
        .0
        .downloads
        .lock()
        .map(|downloads| {
            downloads
                .iter()
                .filter(|download| download.status == "downloading")
                .map(|download| download.speed)
                .sum()
        })
        .unwrap_or(0)
}

fn unique_destination(directory: &Path, filename: &str) -> PathBuf {
    let first = directory.join(filename);
    if !first.exists() {
        return first;
    }
    let path = Path::new(filename);
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let extension = path
        .extension()
        .map(|value| format!(".{}", value.to_string_lossy()))
        .unwrap_or_default();
    for index in 1.. {
        let candidate = directory.join(format!("{stem} ({index}){extension}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!()
}

#[tauri::command]
fn get_settings(backend: State<'_, Backend>) -> Result<Value, String> {
    backend
        .0
        .settings
        .lock()
        .map(|settings| settings.clone())
        .map_err(|_| "Settings lock was poisoned".to_string())
}

#[tauri::command(rename_all = "camelCase")]
fn update_settings(
    app: AppHandle,
    backend: State<'_, Backend>,
    settings: Value,
) -> Result<Value, String> {
    let mut current = backend
        .0
        .settings
        .lock()
        .map_err(|_| "Settings lock was poisoned".to_string())?;
    if let (Some(target), Some(updates)) = (current.as_object_mut(), settings.as_object()) {
        for (key, value) in updates {
            target.insert(key.clone(), value.clone());
        }
    }
    let result = current.clone();
    drop(current);
    backend.persist()?;
    if result.get("shutdownAfterComplete").and_then(Value::as_bool) != Some(true) {
        cancel_pending_shutdown(backend.inner());
    }
    refresh_speed_limit(backend.inner())?;
    // Apply autostart setting whenever it changes.
    apply_autostart(&result);
    // Raising the concurrency limit should let waiting items start without
    // waiting for an unrelated download to finish.
    start_next_queued(&app, backend.inner());
    Ok(result)
}

#[tauri::command]
fn get_downloads(backend: State<'_, Backend>) -> Result<Vec<Download>, String> {
    backend
        .0
        .downloads
        .lock()
        .map(|downloads| downloads.clone())
        .map_err(|_| "Downloads lock was poisoned".to_string())
}

#[tauri::command(rename_all = "camelCase")]
fn add_download(
    app: AppHandle,
    backend: State<'_, Backend>,
    url: String,
    filename: Option<String>,
    save_dir: Option<String>,
    options: Option<Value>,
) -> Result<Download, String> {
    create_download(app, backend.inner(), url, filename, save_dir, options)
}

fn create_download(
    app: AppHandle,
    backend: &Backend,
    url: String,
    filename: Option<String>,
    save_dir: Option<String>,
    options: Option<Value>,
) -> Result<Download, String> {
    let parsed = reqwest::Url::parse(&url).map_err(|error| format!("Invalid URL: {error}"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("Only HTTP and HTTPS downloads are supported".to_string());
    }
    let options = options.unwrap_or(Value::Null);
    let stream = requires_media_extractor(
        &parsed,
        options.get("isStream").and_then(Value::as_bool) == Some(true),
    );
    let provided_name = filename.filter(|value| !value.trim().is_empty());
    let name_locked = provided_name.is_some();
    let name = provided_name
        .or_else(|| {
            parsed
                .path_segments()
                .and_then(Iterator::last)
                .filter(|value| !value.is_empty())
                .map(percent_decode)
        })
        .unwrap_or_else(|| "download".to_string());
    let name = sanitize_filename(&name);

    let settings = backend
        .0
        .settings
        .lock()
        .map_err(|_| "Settings lock was poisoned".to_string())?
        .clone();
    let configured_dir = settings
        .get("downloadDir")
        .and_then(Value::as_str)
        .map(str::to_string);
    let save_dir = save_dir
        .filter(|value| !value.trim().is_empty())
        .or(configured_dir)
        .unwrap_or_else(|| default_download_dir().to_string_lossy().to_string());
    let default_max_segments = settings
        .get("maxSegments")
        .and_then(Value::as_u64)
        .unwrap_or(default_segments());
    let max_segments = options
        .get("maxSegments")
        .and_then(Value::as_u64)
        .unwrap_or(default_max_segments)
        .clamp(1, 32);

    let schedule_time = options
        .get("scheduleTime")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(|value| {
            DateTime::parse_from_rfc3339(value)
                .map(|date| date.with_timezone(&Utc).to_rfc3339())
                .map_err(|error| format!("Invalid scheduled start time: {error}"))
        })
        .transpose()?;
    let start_immediate = options.get("startImmediate").and_then(Value::as_bool) != Some(false);
    let expected_sha256 = normalize_sha256(options.get("sha256").and_then(Value::as_str))?;

    let (status, stage) = if schedule_time.is_some() {
        ("scheduled", "Scheduled")
    } else if start_immediate {
        ("queued", "Queued")
    } else {
        ("paused", "Ready to start")
    };

    let derived_category = category_for(&name);
    let category = if derived_category == "Other" && stream {
        "Videos".to_string()
    } else {
        derived_category
    };

    let download = Download {
        id: Uuid::new_v4().to_string(),
        url,
        filename: name.clone(),
        save_dir,
        total_size: 0,
        downloaded_size: 0,
        progress: 0.0,
        speed: 0,
        status: status.to_string(),
        stage: stage.to_string(),
        segments: Vec::new(),
        category,
        date_added: Utc::now().to_rfc3339(),
        date_completed: None,
        error: None,
        resumable: stream,
        mime_type: String::new(),
        is_stream: stream,
        max_segments,
        referrer: options
            .get("referrer")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        cookies: options
            .get("cookies")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        user_agent: options
            .get("userAgent")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        scheduled_time: schedule_time.clone(),
        name_locked,
        average_speed: 0,
        etag: String::new(),
        last_modified: String::new(),
        expected_sha256,
        hash_verified: false,
    };
    backend
        .0
        .downloads
        .lock()
        .map_err(|_| "Downloads lock was poisoned".to_string())?
        .push(download.clone());
    backend.persist()?;
    emit(&app, "download:new", &download);
    cancel_pending_shutdown(backend);

    if let Some(schedule_time) = schedule_time {
        schedule_download_internal(&app, backend, &download.id, &schedule_time)?;
    } else if start_immediate {
        start_download(app, backend.clone(), download.id.clone())?;
    }
    backend.get_download(&download.id)
}

fn clear_schedule(backend: &Backend, id: &str) {
    if let Ok(mut tokens) = backend.0.schedule_tokens.lock() {
        tokens.remove(id);
    }
}

fn schedule_download_internal(
    app: &AppHandle,
    backend: &Backend,
    id: &str,
    start_time: &str,
) -> Result<(), String> {
    let start = DateTime::parse_from_rfc3339(start_time)
        .map_err(|error| format!("Invalid start time: {error}"))?;
    let current = backend.get_download(id)?;
    if matches!(current.status.as_str(), "complete" | "cancelled") {
        return Err("Completed or cancelled downloads cannot be scheduled".to_string());
    }
    // If this download is already running, stop it before switching it to a
    // scheduled state so no worker keeps writing behind the scheduler.
    backend.stop_task(id, PAUSE_REQUESTED)?;
    let delay = start
        .with_timezone(&Utc)
        .signed_duration_since(Utc::now())
        .to_std()
        .unwrap_or(Duration::ZERO);
    let token = Uuid::new_v4().to_string();
    backend
        .0
        .schedule_tokens
        .lock()
        .map_err(|_| "Schedule lock was poisoned".to_string())?
        .insert(id.to_string(), token.clone());
    let normalized = start.with_timezone(&Utc).to_rfc3339();
    backend.update_download(id, |download| {
        download.status = "scheduled".to_string();
        download.stage = "Scheduled".to_string();
        download.scheduled_time = Some(normalized);
        download.error = None;
        download.speed = 0;
    })?;
    if let Ok(scheduled) = backend.get_download(id) {
        emit(app, "download:progress", &event_snapshot(&scheduled));
    }

    let app = app.clone();
    let backend = backend.clone();
    let id = id.to_string();
    thread::spawn(move || {
        if !delay.is_zero() {
            thread::sleep(delay);
        }
        let current_token = backend
            .0
            .schedule_tokens
            .lock()
            .ok()
            .and_then(|tokens| tokens.get(&id).cloned());
        if current_token.as_deref() != Some(token.as_str()) {
            return;
        }
        clear_schedule(&backend, &id);
        let Ok(current) = backend.get_download(&id) else {
            return;
        };
        if current.status != "scheduled" {
            return;
        }
        if let Err(error) = start_download(app, backend, id) {
            eprintln!("Scheduled download failed to start: {error}");
        }
    });
    Ok(())
}

fn restore_scheduled_downloads(app: &AppHandle, backend: &Backend) {
    let scheduled = backend
        .0
        .downloads
        .lock()
        .map(|downloads| {
            downloads
                .iter()
                .filter(|download| download.status == "scheduled")
                .map(|download| (download.id.clone(), download.scheduled_time.clone()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for (id, start_time) in scheduled {
        let future = start_time
            .as_deref()
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .is_some_and(|date| date.with_timezone(&Utc) > Utc::now());
        if future
            && let Some(start_time) = start_time
            && let Err(error) = schedule_download_internal(app, backend, &id, &start_time)
        {
            eprintln!("Could not restore scheduled download: {error}");
        } else if !future
            && let Err(error) = start_download(app.clone(), backend.clone(), id)
        {
            eprintln!("Could not start restored download: {error}");
        }
    }
}

#[tauri::command]
fn pause_download(app: AppHandle, backend: State<'_, Backend>, id: String) -> Result<(), String> {
    pause_download_for_app(&app, backend.inner(), &id)?;
    Ok(())
}

fn pause_download_for_app(
    app: &AppHandle,
    backend: &Backend,
    id: &str,
) -> Result<Download, String> {
    clear_schedule(backend, id);
    let current = backend.get_download(id)?;
    if !matches!(
        current.status.as_str(),
        "downloading" | "queued" | "scheduled"
    ) {
        return Ok(current);
    }
    if let Some(control) = backend
        .0
        .controls
        .lock()
        .map_err(|_| "Download control lock was poisoned".to_string())?
        .get(id)
    {
        control.store(PAUSE_REQUESTED, Ordering::SeqCst);
    }
    let download = backend.update_download(id, |current| {
        if matches!(
            current.status.as_str(),
            "downloading" | "queued" | "scheduled"
        ) {
            current.status = "paused".to_string();
            current.stage = "Paused".to_string();
            current.scheduled_time = None;
            current.speed = 0;
            current.error = None;
        }
    })?;
    emit(app, "download:paused", &download);
    Ok(download)
}

#[tauri::command]
fn resume_download(app: AppHandle, backend: State<'_, Backend>, id: String) -> Result<(), String> {
    resume_download_for_app(&app, backend.inner(), &id)?;
    Ok(())
}

fn resume_download_for_app(
    app: &AppHandle,
    backend: &Backend,
    id: &str,
) -> Result<Download, String> {
    let current = backend.get_download(id)?;
    if !matches!(current.status.as_str(), "paused" | "error" | "queued") {
        return Ok(current);
    }
    clear_schedule(backend, id);
    let task = backend
        .0
        .tasks
        .lock()
        .map_err(|_| "Download task lock was poisoned".to_string())?
        .remove(id);
    if let Some(task) = task {
        task.join()
            .map_err(|_| "Download worker crashed".to_string())?;
    }
    backend.update_download(id, |current| {
        current.status = "queued".to_string();
        current.stage = "Queued".to_string();
        current.error = None;
        current.scheduled_time = None;
    })?;
    emit(
        app,
        "download:resumed",
        &json!({"id": id, "status": "queued"}),
    );
    start_download(app.clone(), backend.clone(), id.to_string())?;
    backend.get_download(id)
}

#[tauri::command]
fn cancel_download(app: AppHandle, backend: State<'_, Backend>, id: String) -> Result<(), String> {
    cancel_download_for_app(&app, backend.inner(), &id)?;
    Ok(())
}

fn cancel_download_for_app(
    app: &AppHandle,
    backend: &Backend,
    id: &str,
) -> Result<Download, String> {
    clear_schedule(backend, id);
    let current = backend.get_download(id)?;
    if matches!(current.status.as_str(), "complete" | "cancelled") {
        return Ok(current);
    }
    if let Some(control) = backend
        .0
        .controls
        .lock()
        .map_err(|_| "Download control lock was poisoned".to_string())?
        .get(id)
    {
        control.store(CANCEL_REQUESTED, Ordering::SeqCst);
    }
    let download = backend.update_download(id, |current| {
        current.status = "cancelled".to_string();
        current.stage = "Cancelled".to_string();
        current.scheduled_time = None;
        current.speed = 0;
    })?;
    emit(app, "download:cancelled", &download);
    Ok(download)
}

fn remove_download_partials(existing: &Download) {
    let directory = Path::new(&existing.save_dir);
    let temporary_path = directory.join(format!("{}.syntax_tmp", existing.filename));
    if temporary_path.exists()
        && let Err(error) = fs::remove_file(&temporary_path)
    {
        eprintln!("Could not remove partial download: {error}");
    }
    if existing.is_stream
        && let Err(error) = remove_media_partials(directory, &existing.id)
    {
        eprintln!("Could not remove partial video download: {error}");
    }
}

#[tauri::command]
fn delete_download(backend: State<'_, Backend>, id: String) -> Result<(), String> {
    let existing = backend.get_download(&id)?;
    clear_schedule(&backend, &id);
    let running = backend
        .0
        .controls
        .lock()
        .map_err(|_| "Download control lock was poisoned".to_string())?
        .contains_key(&id);
    if running
        && let Some(control) = backend
            .0
            .controls
            .lock()
            .map_err(|_| "Download control lock was poisoned".to_string())?
            .get(&id)
    {
        control.store(CANCEL_REQUESTED, Ordering::SeqCst);
    }
    let mut downloads = backend
        .0
        .downloads
        .lock()
        .map_err(|_| "Downloads lock was poisoned".to_string())?;
    downloads.retain(|download| download.id != id);
    drop(downloads);
    backend.persist()?;
    if running {
        // Joining a worker can take up to the socket read timeout. Do it in
        // the background so the UI remains responsive, then clean up files
        // once the worker has definitely closed its handles.
        let background = backend.inner().clone();
        thread::spawn(move || {
            if let Err(error) = background.stop_task(&id, CANCEL_REQUESTED) {
                eprintln!("Could not stop deleted download: {error}");
            }
            remove_download_partials(&existing);
        });
    } else {
        remove_download_partials(&existing);
    }
    Ok(())
}

#[tauri::command]
fn pause_all(app: AppHandle, backend: State<'_, Backend>) -> Result<(), String> {
    let ids = backend
        .0
        .downloads
        .lock()
        .map_err(|_| "Downloads lock was poisoned".to_string())?
        .iter()
        .filter(|download| {
            download.status == "downloading"
                || download.status == "queued"
                || download.status == "scheduled"
        })
        .map(|download| download.id.clone())
        .collect::<Vec<_>>();
    for id in ids {
        pause_download(app.clone(), backend.clone(), id)?;
    }
    Ok(())
}

#[tauri::command]
fn resume_all(app: AppHandle, backend: State<'_, Backend>) -> Result<(), String> {
    let ids = backend
        .0
        .downloads
        .lock()
        .map_err(|_| "Downloads lock was poisoned".to_string())?
        .iter()
        .filter(|download| {
            download.status == "paused" || download.status == "error" || download.status == "queued"
        })
        .map(|download| download.id.clone())
        .collect::<Vec<_>>();
    for id in ids {
        resume_download(app.clone(), backend.clone(), id)?;
    }
    Ok(())
}

#[tauri::command]
fn cancel_all(app: AppHandle, backend: State<'_, Backend>) -> Result<(), String> {
    let ids = backend
        .0
        .downloads
        .lock()
        .map_err(|_| "Downloads lock was poisoned".to_string())?
        .iter()
        .filter(|download| {
            download.status == "downloading"
                || download.status == "queued"
                || download.status == "paused"
                || download.status == "scheduled"
        })
        .map(|download| download.id.clone())
        .collect::<Vec<_>>();
    for id in ids {
        cancel_download(app.clone(), backend.clone(), id)?;
    }
    Ok(())
}

#[tauri::command]
fn get_categories(backend: State<'_, Backend>) -> Result<Value, String> {
    let settings = backend
        .0
        .settings
        .lock()
        .map_err(|_| "Settings lock was poisoned".to_string())?;
    Ok(settings
        .get("categories")
        .cloned()
        .unwrap_or_else(|| default_settings()["categories"].clone()))
}

#[tauri::command]
fn get_stats(backend: State<'_, Backend>) -> Result<Value, String> {
    let downloads = backend
        .0
        .downloads
        .lock()
        .map_err(|_| "Downloads lock was poisoned".to_string())?;
    let completed = downloads
        .iter()
        .filter(|download| download.status == "complete");
    let total_downloaded = completed
        .clone()
        .map(|download| download.total_size)
        .sum::<u64>();
    let total_files = completed.clone().count();
    let speed_samples = completed
        .clone()
        .filter(|download| download.average_speed > 0)
        .map(|download| download.average_speed)
        .collect::<Vec<_>>();
    let avg_speed = if speed_samples.is_empty() {
        0
    } else {
        speed_samples.iter().sum::<u64>() / speed_samples.len() as u64
    };
    let today = Local::now().date_naive();
    let today_downloaded = completed
        .filter(|download| {
            download
                .date_completed
                .as_ref()
                .and_then(|date| DateTime::parse_from_rfc3339(date).ok())
                .map(|date| date.with_timezone(&Local).date_naive() == today)
                .unwrap_or(false)
        })
        .map(|download| download.total_size)
        .sum::<u64>();
    let active_downloads = downloads
        .iter()
        .filter(|download| download.status == "downloading")
        .count();
    let queued_downloads = downloads
        .iter()
        .filter(|download| download.status == "queued")
        .count();
    Ok(json!({
        "totalDownloaded": total_downloaded,
        "totalFiles": total_files,
        "avgSpeed": avg_speed,
        "todayDownloaded": today_downloaded,
        "activeDownloads": active_downloads,
        "queuedDownloads": queued_downloads
    }))
}

#[tauri::command(rename_all = "camelCase")]
fn open_path(path: String) -> Result<(), String> {
    if !Path::new(&path).exists() {
        return Err(format!("Path does not exist: {path}"));
    }
    #[cfg(target_os = "windows")]
    let result = Command::new("explorer.exe").arg(path).spawn();
    #[cfg(target_os = "macos")]
    let result = Command::new("open").arg(path).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let result = Command::new("xdg-open").arg(path).spawn();
    result
        .map(|_| ())
        .map_err(|error| format!("Could not open path: {error}"))
}

#[tauri::command]
fn set_clipboard_monitoring(
    app: AppHandle,
    backend: State<'_, Backend>,
    enabled: bool,
) -> Result<bool, String> {
    update_settings(app, backend, json!({"clipboardMonitoring": enabled}))?;
    Ok(enabled)
}

#[tauri::command]
fn get_system_info() -> Value {
    json!({
        "platform": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "cpus": thread::available_parallelism().map(|count| count.get()).unwrap_or(1),
        "totalMemory": 0
    })
}

#[tauri::command(rename_all = "camelCase")]
fn verify_file_hash(file_path: String, algorithm: Option<String>) -> Result<String, String> {
    if !algorithm
        .as_deref()
        .unwrap_or("sha256")
        .eq_ignore_ascii_case("sha256")
    {
        return Err("Only SHA-256 verification is currently supported".to_string());
    }
    sha256_file(Path::new(&file_path))
}

#[tauri::command(rename_all = "camelCase")]
fn export_downloads(backend: State<'_, Backend>, file_path: String) -> Result<Value, String> {
    let mut downloads = get_downloads(backend)?;
    // History files are meant to be portable and may be shared. Import never
    // restores request credentials, so never write them into an export.
    for download in &mut downloads {
        download.cookies.clear();
        download.user_agent.clear();
    }
    fs::write(
        &file_path,
        serde_json::to_vec_pretty(&downloads).map_err(|error| error.to_string())?,
    )
    .map_err(|error| format!("Could not export downloads: {error}"))?;
    Ok(json!({"success": true, "count": downloads.len()}))
}

#[tauri::command(rename_all = "camelCase")]
fn import_downloads(
    app: AppHandle,
    backend: State<'_, Backend>,
    file_path: String,
) -> Result<Value, String> {
    let bytes = fs::read(file_path).map_err(|error| error.to_string())?;
    let data: Vec<Value> = serde_json::from_slice(strip_utf8_bom(&bytes))
        .map_err(|error| format!("Invalid download export: {error}"))?;
    let mut count = 0;
    for item in data {
        if let Some(url) = item.get("url").and_then(Value::as_str) {
            let filename = item
                .get("filename")
                .and_then(Value::as_str)
                .map(str::to_string);
            let save_dir = item
                .get("saveDir")
                .and_then(Value::as_str)
                .map(str::to_string);
            add_download(
                app.clone(),
                backend.clone(),
                url.to_string(),
                filename,
                save_dir,
                // Restore imported entries without immediately re-downloading
                // them; the user can resume each item explicitly.
                Some(json!({"startImmediate": false})),
            )?;
            count += 1;
        }
    }
    Ok(json!({"success": true, "count": count}))
}

#[tauri::command(rename_all = "camelCase")]
fn schedule_download(
    app: AppHandle,
    backend: State<'_, Backend>,
    id: String,
    start_time: String,
) -> Result<(), String> {
    schedule_download_internal(&app, backend.inner(), &id, &start_time)
}

#[tauri::command(rename_all = "camelCase")]
fn set_speed_limit(
    app: AppHandle,
    backend: State<'_, Backend>,
    bytes_per_second: u64,
) -> Result<Value, String> {
    *backend
        .0
        .speed_limit
        .lock()
        .map_err(|_| "Speed-limit lock was poisoned".to_string())? = bytes_per_second;
    update_settings(app, backend, json!({"speedLimit": bytes_per_second}))?;
    Ok(json!({"success": true}))
}

#[tauri::command]
fn set_bandwidth_schedule(
    app: AppHandle,
    backend: State<'_, Backend>,
    schedule: Value,
) -> Result<Value, String> {
    update_settings(app, backend, json!({"bandwidthSchedule": schedule}))?;
    Ok(json!({"success": true}))
}

#[tauri::command]
fn shutdown_after_complete(
    app: AppHandle,
    backend: State<'_, Backend>,
    enabled: bool,
) -> Result<Value, String> {
    update_settings(app, backend, json!({"shutdownAfterComplete": enabled}))?;
    Ok(json!({"success": true}))
}

fn extension_resource_path(app: &AppHandle, relative_path: &str) -> Result<PathBuf, String> {
    let bundled_path = app
        .path()
        .resource_dir()
        .map_err(|error| error.to_string())?
        .join(relative_path);
    if bundled_path.exists() {
        return Ok(bundled_path);
    }
    let source_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or_else(|| "Project root directory was not found".to_string())?;
    let source_path = source_root.join(relative_path);
    if source_path.exists() {
        return Ok(source_path);
    }
    Err(format!(
        "Application resource was not found: {relative_path}"
    ))
}

#[tauri::command]
fn download_extension(app: AppHandle, backend: State<'_, Backend>) -> Result<Value, String> {
    let source = extension_resource_path(&app, "assets/syntax-downloader-extension.zip")?;
    let destination = backend
        .0
        .settings
        .lock()
        .map_err(|_| "Settings lock was poisoned".to_string())?
        .get("downloadDir")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .unwrap_or_else(default_download_dir);
    fs::create_dir_all(&destination).map_err(|error| error.to_string())?;
    let target = destination.join("syntax-downloader-extension.zip");
    fs::copy(&source, &target)
        .map_err(|error| format!("Could not copy extension archive: {error}"))?;
    Ok(json!({"success": true, "path": target}))
}

#[tauri::command]
fn open_extension_folder(app: AppHandle) -> Result<Value, String> {
    let folder = extension_resource_path(&app, "extension")?;
    open_path(folder.to_string_lossy().to_string())?;
    Ok(json!({"success": true, "path": folder}))
}

#[tauri::command]
fn open_external(url: String) -> Result<(), String> {
    let parsed = reqwest::Url::parse(&url).map_err(|error| error.to_string())?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("Only HTTP and HTTPS links can be opened".to_string());
    }
    #[cfg(target_os = "windows")]
    let result = Command::new("explorer.exe").arg(&url).spawn();
    #[cfg(target_os = "macos")]
    let result = Command::new("open").arg(&url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let result = Command::new("xdg-open").arg(&url).spawn();
    result.map(|_| ()).map_err(|error| error.to_string())
}

fn parse_version_numbers(v: &str) -> Vec<u64> {
    v.trim_start_matches(['v', 'V'])
        .split('.')
        .filter_map(|s| s.split(['-', '+']).next().unwrap_or("").parse::<u64>().ok())
        .collect()
}

fn is_version_newer(remote: &str, current: &str) -> bool {
    let r_parts = parse_version_numbers(remote);
    let c_parts = parse_version_numbers(current);
    let max_len = r_parts.len().max(c_parts.len());
    for i in 0..max_len {
        let r_val = r_parts.get(i).copied().unwrap_or(0);
        let c_val = c_parts.get(i).copied().unwrap_or(0);
        if r_val > c_val {
            return true;
        } else if r_val < c_val {
            return false;
        }
    }
    false
}

#[tauri::command]
async fn check_for_updates(app: AppHandle, backend: State<'_, Backend>) -> Result<Value, String> {
    let feed_url = {
        let settings = backend.0.settings.lock().map_err(|_| "Settings lock poisoned".to_string())?;
        let configured = settings
            .get("updateFeedUrl")
            .and_then(Value::as_str)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "https://gist.githubusercontent.com/Harry2210/d461b829673780c2340d6e54b85d51a9/raw/Syntax%2520downloader%2520version.json".to_string());

        let mut clean = configured.trim().trim_end_matches([',', ';', ' ']).to_string();

        // Strip commit hash if user pasted a commit-pinned raw URL
        // e.g. /raw/0a1965e0d905492245eca53273dce03a03bde71d/Syntax... -> /raw/Syntax...
        if clean.contains("gist.githubusercontent.com") {
            if let Some(raw_idx) = clean.find("/raw/") {
                let after_raw = &clean[raw_idx + 5..];
                if let Some(slash_idx) = after_raw.find('/') {
                    let segment = &after_raw[..slash_idx];
                    if segment.len() == 40 && segment.chars().all(|c| c.is_ascii_hexdigit()) {
                        let remainder = &after_raw[slash_idx + 1..];
                        clean = format!("{}/raw/{}", &clean[..raw_idx], remainder);
                    }
                }
            }
        }

        // Replace any unencoded spaces so reqwest does not fail
        clean = clean.replace(' ', "%20");

        // Fix GitHub Gist raw URL double encoding if needed
        if clean.contains("gist.githubusercontent.com") && clean.contains("Syntax%20downloader") && !clean.contains("Syntax%2520downloader") {
            clean = clean.replace("Syntax%20downloader", "Syntax%2520downloader");
        }

        clean
    };

    let current_version = app.package_info().version.to_string();

    tauri::async_runtime::spawn_blocking(move || {
        use std::net::ToSocketAddrs;

        let mut builder = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(6));

        // When connecting to gist.githubusercontent.com, test resolved IPs to find a reachable one (Happy Eyeballs)
        if feed_url.contains("gist.githubusercontent.com") {
            if let Ok(addrs) = ("gist.githubusercontent.com", 443).to_socket_addrs() {
                for addr in addrs {
                    if std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(1000)).is_ok() {
                        builder = builder.resolve("gist.githubusercontent.com", addr);
                        break;
                    }
                }
            }
        }

        let client = builder
            .build()
            .map_err(|e| format!("Failed to build HTTP client: {e}"))?;

        let cache_buster = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let target_url = if feed_url.contains('?') {
            format!("{feed_url}&_t={cache_buster}")
        } else {
            format!("{feed_url}?_t={cache_buster}")
        };

        let response = client
            .get(&target_url)
            .header("User-Agent", "Syntax-Downloader")
            .header("Cache-Control", "no-cache, no-store, must-revalidate")
            .header("Pragma", "no-cache")
            .header("Expires", "0")
            .send()
            .map_err(|e| format!("Could not reach update server: {e}"))?;

        if !response.status().is_success() {
            return Err(format!("Update server returned HTTP {}", response.status()));
        }

        let payload: Value = response
            .json()
            .map_err(|e| format!("Invalid JSON response from update server: {e}"))?;

        let remote_version = payload
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();

        let has_update = is_version_newer(&remote_version, &current_version);

        Ok(json!({
            "updateAvailable": has_update,
            "currentVersion": current_version,
            "remoteVersion": remote_version,
            "name": payload.get("name").and_then(Value::as_str).unwrap_or(""),
            "pubDate": payload.get("pub_date").or_else(|| payload.get("pubDate")).and_then(Value::as_str).unwrap_or(""),
            "downloadUrl": payload.get("downloadUrl").and_then(Value::as_str).unwrap_or(""),
            "installerUrl": payload.get("installerUrl").and_then(Value::as_str).unwrap_or(""),
            "changelog": payload.get("changelog").cloned().unwrap_or_else(|| json!([])),
            "feedUrl": feed_url
        }))
    })
    .await
    .map_err(|e| e.to_string())?
}

struct ExtensionRequest {
    method: String,
    path: String,
    host: String,
    origin: String,
    content_type: String,
    body: Vec<u8>,
}

fn read_extension_request(stream: &mut TcpStream) -> Result<ExtensionRequest, String> {
    let mut raw = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        if let Some(position) = raw.windows(4).position(|window| window == b"\r\n\r\n") {
            break position;
        }
        if raw.len() >= 16 * 1024 {
            return Err("Request headers exceed the 16 KiB limit".to_string());
        }
        let count = stream
            .read(&mut chunk)
            .map_err(|error| format!("Could not read extension request: {error}"))?;
        if count == 0 {
            return Err("Extension closed the connection before sending headers".to_string());
        }
        raw.extend_from_slice(&chunk[..count]);
    };
    let header_text = std::str::from_utf8(&raw[..header_end])
        .map_err(|error| format!("Request headers are not valid UTF-8: {error}"))?;
    let mut lines = header_text.split("\r\n");
    let mut request_line = lines
        .next()
        .ok_or_else(|| "HTTP request line is missing".to_string())?
        .split_whitespace();
    let method = request_line
        .next()
        .ok_or_else(|| "HTTP method is missing".to_string())?
        .to_string();
    let path = request_line
        .next()
        .ok_or_else(|| "HTTP path is missing".to_string())?
        .split('?')
        .next()
        .unwrap_or("/")
        .to_string();
    let protocol = request_line
        .next()
        .ok_or_else(|| "HTTP version is missing".to_string())?;
    if protocol != "HTTP/1.1" || request_line.next().is_some() {
        return Err("Only HTTP/1.1 requests are supported".to_string());
    }

    let mut headers = HashMap::new();
    for line in lines {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| "Malformed HTTP header".to_string())?;
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim().to_string();
        if headers.insert(name.clone(), value).is_some() {
            return Err(format!("Duplicate HTTP header: {name}"));
        }
    }
    if headers.contains_key("transfer-encoding") {
        return Err("Transfer-encoded request bodies are not supported".to_string());
    }
    let content_length = headers
        .get("content-length")
        .map(|value| {
            value
                .parse::<usize>()
                .map_err(|error| format!("Invalid Content-Length: {error}"))
        })
        .transpose()?
        .unwrap_or(0);
    if content_length as u64 > MAX_EXTENSION_REQUEST_BYTES {
        return Err("Request body exceeds the 1 MiB limit".to_string());
    }
    let mut body = raw[header_end + 4..].to_vec();
    if body.len() > content_length {
        return Err("Unexpected bytes after the request body".to_string());
    }
    while body.len() < content_length {
        let count = stream
            .read(&mut chunk)
            .map_err(|error| format!("Could not read extension request body: {error}"))?;
        if count == 0 {
            return Err("Extension closed the connection before sending its body".to_string());
        }
        body.extend_from_slice(&chunk[..count]);
        if body.len() > content_length {
            return Err("Unexpected bytes after the request body".to_string());
        }
    }
    Ok(ExtensionRequest {
        method,
        path,
        host: headers.remove("host").unwrap_or_default(),
        origin: headers.remove("origin").unwrap_or_default(),
        content_type: headers.remove("content-type").unwrap_or_default(),
        body,
    })
}

fn write_extension_response(
    stream: &mut TcpStream,
    status: u16,
    payload: Value,
    cors_origin: Option<&str>,
) -> Result<(), String> {
    let body = payload.to_string();
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        _ => "Internal Server Error",
    };
    let cors_headers = if let Some(origin) = cors_origin {
        format!(
            "Access-Control-Allow-Origin: {origin}\r\n\
             Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n\
             Access-Control-Allow-Headers: Content-Type\r\n\
             Access-Control-Allow-Private-Network: true\r\n"
        )
    } else {
        String::new()
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: application/json; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         {cors_headers}\r\n\
         {body}",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .map_err(|error| format!("Could not reply to extension request: {error}"))?;
    stream.flush().map_err(|error| error.to_string())
}

fn read_extension_json(request: &ExtensionRequest) -> Result<Value, String> {
    serde_json::from_slice(&request.body).map_err(|error| format!("Invalid JSON request: {error}"))
}

fn bridge_download_options(payload: &Value, max_segments: Option<u64>) -> Value {
    json!({
        "isStream": payload.get("isStream").and_then(Value::as_bool).unwrap_or(false),
        "maxSegments": max_segments
            .or_else(|| payload.get("maxSegments").and_then(Value::as_u64))
            .unwrap_or(16)
            .clamp(1, 32),
        "referrer": payload.get("referrer").and_then(Value::as_str).unwrap_or(""),
        "cookies": payload.get("cookies").and_then(Value::as_str).unwrap_or(""),
        "userAgent": payload.get("userAgent").and_then(Value::as_str).unwrap_or("")
    })
}

fn valid_extension_request(host: &str, origin: &str, method: &str, content_type: &str) -> bool {
    if host != EXTENSION_BRIDGE_ADDRESS {
        return false;
    }
    if origin == EXTENSION_ORIGIN {
        return true;
    }
    if !origin.is_empty() {
        return false;
    }
    method == "GET"
        || (method == "POST"
            && content_type
                .split(';')
                .next()
                .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json")))
}

fn extension_api_request(
    app: &AppHandle,
    stream: &mut TcpStream,
    request: ExtensionRequest,
) -> Result<(), String> {
    if !valid_extension_request(
        &request.host,
        &request.origin,
        &request.method,
        &request.content_type,
    ) {
        return write_extension_response(
            stream,
            403,
            json!({"error": "Only the Syntax Downloader browser extension can access this service"}),
            None,
        );
    }
    if request.method == "OPTIONS" {
        if request.origin != EXTENSION_ORIGIN {
            return write_extension_response(
                stream,
                403,
                json!({"error": "Only the Syntax Downloader browser extension can access this service"}),
                None,
            );
        }
        return write_extension_response(stream, 200, json!({}), Some(EXTENSION_ORIGIN));
    }
    let cors_origin = if request.origin.is_empty() {
        Some("*")
    } else {
        Some(EXTENSION_ORIGIN)
    };

    let backend = app.state::<Backend>();
    let result = (|| -> Result<(u16, Value), String> {
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/api/status") => {
                let active_downloads = backend
                    .0
                    .downloads
                    .lock()
                    .map_err(|_| "Downloads lock was poisoned".to_string())?
                    .iter()
                    .filter(|download| download.status == "downloading")
                    .count();
                Ok((200, json!({"activeDownloads": active_downloads})))
            }
            ("GET", "/api/downloads") => {
                let downloads = backend
                    .0
                    .downloads
                    .lock()
                    .map_err(|_| "Downloads lock was poisoned".to_string())?
                    .clone();
                Ok((200, json!({"downloads": downloads})))
            }
            ("POST", "/api/focus") => {
                let window = app
                    .get_webview_window("main")
                    .ok_or_else(|| "Main application window was not found".to_string())?;
                window
                    .unminimize()
                    .map_err(|error| format!("Could not restore application window: {error}"))?;
                window
                    .show()
                    .map_err(|error| format!("Could not show application window: {error}"))?;
                window
                    .set_focus()
                    .map_err(|error| format!("Could not focus application window: {error}"))?;
                Ok((200, json!({"success": true})))
            }
            ("POST", "/api/download") => {
                let payload = read_extension_json(&request)?;
                let url = payload
                    .get("url")
                    .and_then(Value::as_str)
                    .filter(|url| !url.is_empty())
                    .ok_or_else(|| "Download URL is required".to_string())?
                    .to_string();
                let filename = payload
                    .get("filename")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let download = create_download(
                    app.clone(),
                    backend.inner(),
                    url,
                    filename,
                    None,
                    Some(bridge_download_options(&payload, None)),
                )?;
                Ok((200, json!({"success": true, "download": download})))
            }
            ("POST", "/api/batch-download") => {
                let payload = read_extension_json(&request)?;
                let items = payload
                    .get("items")
                    .and_then(Value::as_array)
                    .ok_or_else(|| "A list of download items is required".to_string())?;
                if items.is_empty() || items.len() > 100 {
                    Err("Batch must contain between 1 and 100 downloads".to_string())
                } else {
                    let max_segments = payload.get("maxSegments").and_then(Value::as_u64);
                    let mut downloads = Vec::new();
                    let mut failures = Vec::new();
                    for item in items {
                        let url = item.get("url").and_then(Value::as_str).unwrap_or("");
                        let filename = item
                            .get("filename")
                            .and_then(Value::as_str)
                            .map(str::to_string);
                        let options = bridge_download_options(item, max_segments);
                        match create_download(
                            app.clone(),
                            backend.inner(),
                            url.to_string(),
                            filename,
                            None,
                            Some(options),
                        ) {
                            Ok(download) => downloads.push(download),
                            Err(error) => failures.push(json!({"url": url, "error": error})),
                        }
                    }
                    if downloads.is_empty() {
                        Err(format!("No downloads could be queued: {failures:?}"))
                    } else {
                        Ok((
                            200,
                            json!({
                                "queued": downloads.len(),
                                "failed": failures,
                                "downloads": downloads
                            }),
                        ))
                    }
                }
            }
            ("POST", path)
                if path.starts_with("/api/pause/")
                    || path.starts_with("/api/resume/")
                    || path.starts_with("/api/cancel/") =>
            {
                let mut parts = path.trim_start_matches("/api/").split('/');
                let action = parts.next().unwrap_or("");
                let id = parts.next().unwrap_or("");
                if id.is_empty() || parts.next().is_some() {
                    Err("A valid download ID is required".to_string())
                } else {
                    let download = match action {
                        "pause" => pause_download_for_app(app, backend.inner(), id),
                        "resume" => resume_download_for_app(app, backend.inner(), id),
                        "cancel" => cancel_download_for_app(app, backend.inner(), id),
                        _ => unreachable!(),
                    }?;
                    Ok((200, json!({"success": true, "download": download})))
                }
            }
            _ => Ok((404, json!({"error": "Endpoint not found"}))),
        }
    })();

    match result {
        Ok((status, payload)) => write_extension_response(stream, status, payload, cors_origin),
        Err(error) => write_extension_response(stream, 400, json!({"error": error}), cors_origin),
    }
}

fn start_extension_server(app: AppHandle) -> Result<(), String> {
    let server = TcpListener::bind(EXTENSION_BRIDGE_ADDRESS)
        .map_err(|error| format!("Could not start browser extension bridge: {error}"))?;
    thread::spawn(move || {
        for incoming in server.incoming() {
            match incoming {
                Ok(mut stream) => {
                    let app = app.clone();
                    thread::spawn(move || {
                        if let Err(error) = stream
                            .set_read_timeout(Some(Duration::from_secs(5)))
                            .and_then(|_| stream.set_write_timeout(Some(Duration::from_secs(5))))
                        {
                            eprintln!("Could not configure extension request timeouts: {error}");
                            return;
                        }
                        let result = read_extension_request(&mut stream)
                            .and_then(|request| extension_api_request(&app, &mut stream, request));
                        if let Err(error) = result {
                            eprintln!("Browser extension bridge request failed: {error}");
                        }
                    });
                }
                Err(error) => {
                    eprintln!("Browser extension bridge listener failed: {error}");
                }
            }
        }
    });
    Ok(())
}

fn extension_bridge_is_running() -> bool {
    let address = match EXTENSION_BRIDGE_ADDRESS.parse() {
        Ok(address) => address,
        Err(_) => return false,
    };
    let Ok(mut stream) = TcpStream::connect_timeout(&address, Duration::from_millis(500)) else {
        return false;
    };
    if stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .is_err()
        || stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .is_err()
    {
        return false;
    }
    if stream
        .write_all(
            b"GET /api/status HTTP/1.1\r\nHost: 127.0.0.1:36855\r\nConnection: close\r\n\r\n",
        )
        .is_err()
    {
        return false;
    }
    let mut response = Vec::new();
    stream.read_to_end(&mut response).is_ok()
        && response.starts_with(b"HTTP/1.1 200")
        && std::str::from_utf8(&response)
            .ok()
            .is_some_and(|text| text.contains("\"activeDownloads\""))
}

fn read_native_message() -> Result<Value, String> {
    let mut stdin = std::io::stdin().lock();
    let mut length = [0u8; 4];
    stdin
        .read_exact(&mut length)
        .map_err(|error| format!("Could not read native message length: {error}"))?;
    let length = u32::from_le_bytes(length) as usize;
    if length == 0 || length > MAX_EXTENSION_REQUEST_BYTES as usize {
        return Err("Native message size is invalid".to_string());
    }
    let mut body = vec![0; length];
    stdin
        .read_exact(&mut body)
        .map_err(|error| format!("Could not read native message: {error}"))?;
    serde_json::from_slice(&body).map_err(|error| format!("Invalid native message: {error}"))
}

fn write_native_message(payload: &Value) -> Result<(), String> {
    let body = payload.to_string();
    let length = u32::try_from(body.len())
        .map_err(|_| "Native response exceeds the message size limit".to_string())?;
    let mut stdout = std::io::stdout().lock();
    stdout
        .write_all(&length.to_le_bytes())
        .and_then(|_| stdout.write_all(body.as_bytes()))
        .and_then(|_| stdout.flush())
        .map_err(|error| format!("Could not write native response: {error}"))
}

pub fn run_native_messaging_host() {
    let response = (|| -> Result<Value, String> {
        let message = read_native_message()?;
        if message.get("command").and_then(Value::as_str) != Some("launch") {
            return Err("Unsupported native messaging command".to_string());
        }
        if !extension_bridge_is_running() {
            let executable = std::env::current_exe()
                .map_err(|error| format!("Could not locate Syntax Downloader: {error}"))?;
            let mut command = Command::new(executable);
            hide_console_window(&mut command);
            command
                .spawn()
                .map_err(|error| format!("Could not launch Syntax Downloader: {error}"))?;
            let ready = (0..100).any(|_| {
                thread::sleep(Duration::from_millis(200));
                extension_bridge_is_running()
            });
            if !ready {
                return Err(
                    "Syntax Downloader started but its browser bridge did not become ready"
                        .to_string(),
                );
            }
        }
        Ok(json!({"ok": true}))
    })()
    .unwrap_or_else(|error| json!({"ok": false, "error": error}));
    if let Err(error) = write_native_message(&response) {
        eprintln!("{error}");
    }
}

/// Apply or revoke Windows autostart based on the `startWithWindows` setting.
fn apply_autostart(settings: &Value) {
    let enabled = settings
        .get("startWithWindows")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    if let Ok(exe_path) = std::env::current_exe() {
        let exe_str = exe_path.to_string_lossy().to_string();
        thread::spawn(move || {
            if enabled {
                let val = format!("\"{}\" --minimized", exe_str);
                let _ = Command::new("reg")
                    .args([
                        "add",
                        r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
                        "/v",
                        "Syntax Downloader",
                        "/t",
                        "REG_SZ",
                        "/d",
                        &val,
                        "/f",
                    ])
                    .output();
            } else {
                let _ = Command::new("reg")
                    .args([
                        "delete",
                        r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
                        "/v",
                        "Syntax Downloader",
                        "/f",
                    ])
                    .output();
            }
        });
    }
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let should_hide = window
                    .app_handle()
                    .try_state::<Backend>()
                    .and_then(|b| b.0.settings.lock().ok().map(|s| {
                        s.get("closeToTray")
                            .and_then(Value::as_bool)
                            .unwrap_or(true)
                    }))
                    .unwrap_or(true);
                if should_hide {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .setup(|app| {
            // Avoid a second instance overwriting the first instance's data
            // files while it is running. The bridge is bound early, so an
            // existing listener means another Syntax Downloader process owns
            // the data directory.
            if extension_bridge_is_running() {
                let _ = reqwest::blocking::Client::builder()
                    .timeout(Duration::from_secs(2))
                    .build()
                    .and_then(|client| {
                        client
                            .post(format!("http://{EXTENSION_BRIDGE_ADDRESS}/api/focus"))
                            .header("Content-Type", "application/json")
                            .body("{}")
                            .send()
                    });
                app.handle().exit(0);
                return Ok(());
            }
            let data_dir = app.path().app_data_dir()?;
            let backend = Backend::new(data_dir)
                .map_err(std::io::Error::other)?;
            let (start_minimized, _close_to_tray, _start_with_windows) = backend
                .0
                .settings
                .lock()
                .map(|settings| {
                    let min = settings
                        .get("startMinimized")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    let close = settings
                        .get("closeToTray")
                        .and_then(Value::as_bool)
                        .unwrap_or(true);
                    let startup = settings
                        .get("startWithWindows")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    (min, close, startup)
                })
                .unwrap_or((false, true, false));

            // Build system tray menu and tray icon.
            let open_i = MenuItem::with_id(app, "open", "Open Syntax Downloader", true, None::<&str>)?;
            let quit_i = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open_i, &quit_i])?;

            let tray_handle = app.handle().clone();
            let mut tray_builder = TrayIconBuilder::new()
                .tooltip("Syntax Downloader")
                .menu(&menu)
                .on_menu_event(|app, event| {
                    match event.id.as_ref() {
                        "open" => {
                            if let Some(window) = app.get_webview_window("main") {
                                let _ = window.show();
                                let _ = window.unminimize();
                                let _ = window.set_focus();
                            }
                        }
                        "quit" => {
                            app.exit(0);
                        }
                        _ => {}
                    }
                })
                .on_tray_icon_event(move |_tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        if let Some(window) = tray_handle.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.unminimize();
                            let _ = window.set_focus();
                        }
                    }
                });

            if let Some(icon) = app.default_window_icon() {
                tray_builder = tray_builder.icon(icon.clone());
            }

            tray_builder.build(app)?;

            // Apply autostart on launch so the registry key stays in sync with settings.
            if let Ok(settings) = backend.0.settings.lock() {
                apply_autostart(&settings);
            }

            let app_handle = app.handle().clone();
            if let Err(error) = refresh_speed_limit(&backend) {
                eprintln!("Could not apply the bandwidth schedule: {error}");
            }
            restore_scheduled_downloads(&app_handle, &backend);
            start_next_queued(&app_handle, &backend);
            start_bandwidth_scheduler(app_handle, backend.clone());
            app.manage(backend);

            let is_minimized_arg = std::env::args_os().any(|arg| {
                let s = arg.to_string_lossy();
                s == "--minimized" || s == "--autostart"
            });
            let should_start_minimized = is_minimized_arg || start_minimized;

            if should_start_minimized {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.hide();
                }
            }

            start_extension_server(app.handle().clone())
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::AddrInUse, error))?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_settings,
            update_settings,
            get_downloads,
            add_download,
            pause_download,
            resume_download,
            cancel_download,
            delete_download,
            pause_all,
            resume_all,
            cancel_all,
            get_categories,
            get_stats,
            open_path,
            set_clipboard_monitoring,
            get_system_info,
            verify_file_hash,
            export_downloads,
            import_downloads,
            schedule_download,
            set_speed_limit,
            set_bandwidth_schedule,
            shutdown_after_complete,
            download_extension,
            open_extension_folder,
            open_external,
            check_for_updates
        ])
        .run(tauri::generate_context!())
        .expect("error while building Tauri application");
}

#[cfg(test)]
mod tests {
    use super::{Download, ProbeInfo, Segment};
    use super::{
        EXTENSION_ORIGIN, PAUSE_REQUESTED, PROGRESS_EVENT_INTERVAL, RUNNING, allocate_segments,
        bridge_download_options, content_range_total, effective_speed_limit,
        filename_from_content_disposition, media_output_filename, media_progress,
        media_video_format, parse_content_range, plausible_media_total, probe_download_metadata,
        progress_event_due,
        read_extension_request, requires_media_extractor, retry_delay, sanitize_filename,
        should_reset_resume, valid_extension_request, valid_media_executable, wait_before_retry,
        is_version_newer,
    };
    use chrono::{Local, TimeZone};
    use serde_json::{Value, json};
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU8, Ordering};
    use std::thread;
    use std::time::{Duration, Instant};

    #[test]
    fn version_comparison_detects_newer_releases() {
        assert!(is_version_newer("1.2.6", "1.2.5"));
        assert!(is_version_newer("v1.3.0", "1.2.5"));
        assert!(is_version_newer("2.0.0", "1.9.9"));
        assert!(!is_version_newer("1.2.5", "1.2.5"));
        assert!(!is_version_newer("1.2.4", "1.2.5"));
        assert!(!is_version_newer("v1.2.5", "1.2.5"));
    }

    #[test]
    fn json_recovery_accepts_a_utf8_bom() {
        let dir = std::env::temp_dir().join(format!("syntax-json-bom-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        std::fs::write(&path, b"\xEF\xBB\xBF{\"theme\":\"light\"}").unwrap();
        let value: Value = super::load_json_recover(&path);
        assert_eq!(value["theme"], "light");
        assert!(path.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn segment_ranges_cover_file_without_gaps_or_overlaps() {
        let segments = allocate_segments(8_000_003, 8);
        assert_eq!(segments.len(), 8);
        assert_eq!(segments.first().unwrap().start, 0);
        assert_eq!(segments.last().unwrap().end, 8_000_002);
        for pair in segments.windows(2) {
            assert_eq!(pair[0].end + 1, pair[1].start);
        }
    }

    #[test]
    fn segment_count_is_capped_by_file_size() {
        let segments = allocate_segments(3, 32);
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].start, 0);
        assert_eq!(segments[0].end, 2);
    }

    #[test]
    fn content_range_parser_handles_ranges_and_unknown_totals() {
        assert_eq!(
            parse_content_range("bytes 5-10/100"),
            Some((5, 10, Some(100)))
        );
        assert_eq!(parse_content_range("bytes */50"), Some((0, 0, Some(50))));
        assert_eq!(parse_content_range("Bytes 5-10/100"), Some((5, 10, Some(100))));
        assert_eq!(parse_content_range("BYTES 0-0/*"), Some((0, 0, None)));
        assert_eq!(parse_content_range("bytes 10-5/100"), None);
        assert_eq!(content_range_total("bytes 0-0/12345"), Some(12345));
        assert_eq!(content_range_total("bytes 1-1/12345"), None);
        assert_eq!(content_range_total("bytes 0-0/*"), None);
        assert_eq!(content_range_total("bytes 0-0/0"), None);
    }

    #[test]
    fn content_disposition_prefers_extended_filename_without_paths() {
        assert_eq!(
            filename_from_content_disposition(
                "attachment; filename=\"fallback.zip\"; filename*=UTF-8''My%20File.zip"
            ),
            Some("My File.zip".to_string())
        );
        assert_eq!(
            filename_from_content_disposition("attachment; filename=\"..\\\\evil/path.txt\""),
            Some("path.txt".to_string())
        );
        assert_eq!(
            filename_from_content_disposition("inline; filename*=UTF-8''%2e%2e%2fescape.bin"),
            Some("escape.bin".to_string())
        );
        assert_eq!(filename_from_content_disposition("attachment"), None);
    }

    #[test]
    fn malformed_partial_probe_falls_back_to_a_regular_download() {
        let malformed = probe_download_metadata(
            reqwest::StatusCode::PARTIAL_CONTENT,
            Some("bytes 5-10/100"),
            Some("6"),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        assert!(!malformed.supports_ranges);
        assert_eq!(malformed.total_size, 100);

        let ranged = probe_download_metadata(
            reqwest::StatusCode::PARTIAL_CONTENT,
            Some("bytes 0-0/100"),
            Some("1"),
            None,
            Some("application/zip; charset=binary"),
            Some("\"abc\""),
            Some("Wed, 21 Oct 2015 07:28:00 GMT"),
        )
        .unwrap();
        assert!(ranged.supports_ranges);
        assert_eq!(ranged.total_size, 100);
        assert_eq!(ranged.mime_type, "application/zip");
        assert_eq!(ranged.etag, "\"abc\"");

        let plain = probe_download_metadata(
            reqwest::StatusCode::OK,
            None,
            Some("2048"),
            Some("attachment; filename=\"setup.exe\""),
            None,
            None,
            None,
        )
        .unwrap();
        assert!(!plain.supports_ranges);
        assert_eq!(plain.total_size, 2048);
        assert_eq!(plain.filename.as_deref(), Some("setup.exe"));
    }

    #[test]
    fn bandwidth_schedule_supports_overnight_windows() {
        let schedule = json!([{
            "start": "22:00",
            "end": "06:00",
            "limitKbps": 512
        }]);
        let settings = json!({"speedLimit": 0, "bandwidthSchedule": schedule});
        let late = Local.with_ymd_and_hms(2026, 3, 10, 23, 30, 0).unwrap();
        let early = Local.with_ymd_and_hms(2026, 3, 11, 5, 59, 0).unwrap();
        let midday = Local.with_ymd_and_hms(2026, 3, 11, 12, 0, 0).unwrap();
        assert_eq!(effective_speed_limit(&settings, late), 512 * 1024);
        assert_eq!(effective_speed_limit(&settings, early), 512 * 1024);
        assert_eq!(effective_speed_limit(&settings, midday), 0);

        let unlimited = json!([{
            "start": "00:00",
            "end": "23:59",
            "unlimited": true
        }]);
        assert_eq!(
            effective_speed_limit(
                &json!({"speedLimit": 4096, "bandwidthSchedule": unlimited}),
                midday
            ),
            0
        );
    }

    #[test]
    fn async_http_download_reads_a_complete_body() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            reader.read_line(&mut request_line).unwrap();
            assert!(request_line.starts_with("GET /file "));
            loop {
                let mut header = String::new();
                if reader.read_line(&mut header).unwrap() == 0 || header == "\r\n" {
                    break;
                }
            }
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\nConnection: close\r\n\r\nhello world",
                )
                .unwrap();
        });

        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(2))
            .read_timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        let request = client
            .get(format!("http://{address}/file"))
            .header(reqwest::header::ACCEPT_ENCODING, "identity");
        let mut response =
            tauri::async_runtime::block_on(async move { request.send().await }).unwrap();
        let mut body = Vec::new();
        while let Some(chunk) =
            tauri::async_runtime::block_on(async { response.chunk().await }).unwrap()
        {
            body.extend_from_slice(&chunk);
        }
        server.join().unwrap();
        assert_eq!(body, b"hello world");
    }

    #[test]
    fn retry_delay_is_bounded_with_exponential_backoff() {
        assert_eq!(retry_delay(0).as_millis(), 500);
        assert_eq!(retry_delay(1).as_millis(), 1000);
        assert_eq!(retry_delay(3).as_millis(), 4000);
        assert_eq!(retry_delay(10).as_millis(), 4000);
    }

    #[test]
    fn progress_events_are_throttled_unless_forced() {
        let start = Instant::now();
        let mut last_emit = start;
        assert!(!progress_event_due(
            &mut last_emit,
            start + PROGRESS_EVENT_INTERVAL - Duration::from_millis(1),
            false
        ));
        assert!(progress_event_due(
            &mut last_emit,
            start + PROGRESS_EVENT_INTERVAL,
            false
        ));
        assert!(progress_event_due(
            &mut last_emit,
            start + PROGRESS_EVENT_INTERVAL,
            true
        ));
    }

    #[test]
    fn retry_wait_stops_promptly_when_download_is_paused() {
        let control = Arc::new(AtomicU8::new(RUNNING));
        let signal = control.clone();
        let setter = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            signal.store(PAUSE_REQUESTED, Ordering::SeqCst);
        });
        let started = Instant::now();
        assert!(!wait_before_retry(&control, 3));
        assert!(started.elapsed() < Duration::from_millis(500));
        setter.join().unwrap();
    }

    #[test]
    fn media_extractor_detects_supported_site_pages_and_manifests() {
        assert!(requires_media_extractor(
            &reqwest::Url::parse("https://www.youtube.com/watch?v=example").unwrap(),
            false
        ));
        assert!(requires_media_extractor(
            &reqwest::Url::parse("https://cdn.example/video.m3u8").unwrap(),
            false
        ));
        assert!(requires_media_extractor(
            &reqwest::Url::parse("https://media.example/watch/123").unwrap(),
            true
        ));
        assert!(!requires_media_extractor(
            &reqwest::Url::parse("https://cdn.example/archive.zip").unwrap(),
            false
        ));
    }

    #[test]
    fn media_progress_uses_only_exact_totals() {
        assert_eq!(
            media_progress("SDL_PROGRESS:1024|4096|NA"),
            Some((1024, 4096))
        );
        assert_eq!(
            media_progress("SDL_PROGRESS:1024|NA|8192"),
            Some((1024, 0))
        );
        assert_eq!(media_progress("other output"), None);
    }

    #[test]
    fn media_totals_smaller_than_downloaded_bytes_are_ignored() {
        assert_eq!(plausible_media_total(9_300_000, 1024), None);
        assert_eq!(plausible_media_total(1024, 4096), Some(4096));
        assert_eq!(plausible_media_total(4096, 4096), Some(4096));
    }

    #[test]
    fn video_download_works_without_ffmpeg_and_keeps_real_extension() {
        assert_eq!(media_video_format(false), "b");
        assert_eq!(media_video_format(true), "bestvideo*+bestaudio/best");
        assert_eq!(
            media_output_filename("video.mp4", Path::new("temporary.webm"), false, false),
            "video.webm"
        );
        assert_eq!(
            media_output_filename("video.mp4", Path::new("temporary.mp4"), false, true),
            "video.mp4"
        );
        assert_eq!(
            media_output_filename("blob-name", Path::new("temporary.webm"), false, true),
            "blob-name.webm"
        );
        assert_eq!(
            media_output_filename("song", Path::new("temporary.mp3"), true, true),
            "song.mp3"
        );
    }

    #[test]
    fn media_executable_validation_rejects_git_lfs_pointer_files() {
        let path = std::env::temp_dir().join(format!("ffmpeg-{}.exe", uuid::Uuid::new_v4()));
        std::fs::write(
            &path,
            b"version https://git-lfs.github.com/spec/v1\noid sha256:placeholder\nsize 113721856\n",
        )
        .unwrap();
        assert!(!valid_media_executable(&path));
        std::fs::remove_file(path).unwrap();

        let path = std::env::temp_dir().join(format!("dos-app-{}.exe", uuid::Uuid::new_v4()));
        let mut file = std::fs::File::create(&path).unwrap();
        file.set_len(1024 * 1024).unwrap();
        file.write_all(b"MZ").unwrap();
        assert!(!valid_media_executable(&path));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn filename_sanitization_keeps_spaces_and_replaces_windows_separators() {
        assert_eq!(sanitize_filename("a file?.zip"), "a file_.zip");
        assert_eq!(sanitize_filename("CON"), "_CON");
        assert_eq!(sanitize_filename("con.txt"), "_con.txt");
        assert_eq!(sanitize_filename("LPT9"), "_LPT9");
        assert_eq!(sanitize_filename("  ..  "), "download");
        assert_eq!(sanitize_filename(&"a".repeat(250)).len(), 200);
    }

    #[test]
    fn extension_bridge_rejects_other_hosts_and_origins() {
        assert!(valid_extension_request(
            "127.0.0.1:36855",
            EXTENSION_ORIGIN,
            "GET",
            ""
        ));
        assert!(valid_extension_request(
            "127.0.0.1:36855",
            "",
            "POST",
            "application/json"
        ));
        assert!(!valid_extension_request(
            "0.0.0.0:36855",
            EXTENSION_ORIGIN,
            "GET",
            ""
        ));
        assert!(!valid_extension_request(
            "127.0.0.1:36855",
            "https://example.com",
            "POST",
            "application/json"
        ));
        assert!(!valid_extension_request(
            "127.0.0.1:36855",
            "",
            "POST",
            "text/plain"
        ));
    }

    #[test]
    fn extension_download_profile_is_clamped() {
        let options = bridge_download_options(&json!({"maxSegments": 100}), None);
        assert_eq!(options["maxSegments"], 32);
    }

    #[test]
    fn extension_http_request_parser_reads_json_body() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let sender = thread::spawn(move || {
            let mut stream = TcpStream::connect(address).unwrap();
            let body = br#"{"url":"https://example.test/file.zip"}"#;
            write!(
                stream,
                "POST /api/download HTTP/1.1\r\nHost: 127.0.0.1:36855\r\nOrigin: {EXTENSION_ORIGIN}\r\nContent-Length: {}\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(body).unwrap();
        });
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_extension_request(&mut stream).unwrap();
        sender.join().unwrap();
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/api/download");
        assert!(valid_extension_request(
            &request.host,
            &request.origin,
            &request.method,
            &request.content_type
        ));
        assert_eq!(
            serde_json::from_slice::<Value>(&request.body).unwrap()["url"],
            "https://example.test/file.zip"
        );
    }

    fn partial_download(total: u64, downloaded: u64, etag: &str) -> Download {
        Download {
            total_size: total,
            downloaded_size: downloaded,
            resumable: true,
            etag: etag.to_string(),
            segments: vec![Segment {
                index: 0,
                start: 0,
                end: total.saturating_sub(1),
                downloaded,
            }],
            ..Download::default()
        }
    }

    #[test]
    fn resume_is_reset_when_the_remote_file_changed() {
        let download = partial_download(1024, 512, "\"v1\"");
        let mut probe = ProbeInfo {
            supports_ranges: true,
            total_size: 1024,
            etag: "\"v1\"".to_string(),
            ..ProbeInfo::default()
        };
        assert!(!should_reset_resume(&download, &probe));

        probe.etag = "\"v2\"".to_string();
        assert!(should_reset_resume(&download, &probe));

        probe.etag = String::new();
        probe.total_size = 2048;
        assert!(should_reset_resume(&download, &probe));
    }

    #[test]
    fn resume_is_reset_for_non_ranged_and_unknown_size_partials() {
        let mut plain = partial_download(1024, 512, "");
        plain.resumable = false;
        let probe = ProbeInfo {
            supports_ranges: false,
            total_size: 1024,
            ..ProbeInfo::default()
        };
        assert!(should_reset_resume(&plain, &probe));

        let mut unknown = partial_download(0, 512, "");
        unknown.resumable = false;
        let unknown_probe = ProbeInfo {
            supports_ranges: false,
            total_size: 0,
            ..ProbeInfo::default()
        };
        assert!(should_reset_resume(&unknown, &unknown_probe));
    }
}
