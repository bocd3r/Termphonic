use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicBool, AtomicU32, Ordering},
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crossterm::ExecutableCommand;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};

use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Borders, List, ListItem, Paragraph};

use rodio::{OutputStream, OutputStreamBuilder, Sink, Source};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use unicode_width::UnicodeWidthChar;

const SEARCH_PAGE_SIZE: usize = 20;
const EMBEDDED_RUNTIME_MAGIC: [u8; 8] = *b"TPKGv1\0\0";
const EMBEDDED_RUNTIME_FOOTER_LEN: usize = 8 + 8 + 32;
static EMBEDDED_RUNTIME_ROOT: OnceLock<Option<PathBuf>> = OnceLock::new();

// =========================================================================
// 1. Audio Streaming backend
// =========================================================================

struct PcmSource {
    receiver: Receiver<Vec<i16>>,
    buffer: std::collections::VecDeque<i16>,
    channels: u16,
    sample_rate: u32,
    is_buffering: bool,
    prebuffer_size: usize,
    played_samples: u64,
    peak_sample: u32,
    shared_playback_level: Arc<AtomicU32>,
    shared_finished: Arc<AtomicBool>,
}

impl Iterator for PcmSource {
    type Item = f32;
    fn next(&mut self) -> Option<Self::Item> {
        // Read all currently available chunks from the channel
        let mut disconnected = false;
        loop {
            match self.receiver.try_recv() {
                Ok(chunk) => {
                    self.buffer.extend(chunk);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }

        // If buffer is empty and channel is disconnected, we are done
        if self.buffer.is_empty() && disconnected {
            self.shared_finished.store(true, Ordering::Release);
            return None;
        }

        // Handle buffering state
        if self.is_buffering {
            // If we have enough samples, stop buffering and play
            if self.buffer.len() >= self.prebuffer_size || disconnected {
                self.is_buffering = false;
            } else {
                // Return silence while buffering
                return Some(0.0);
            }
        }

        // Try to pop a sample
        match self.buffer.pop_front() {
            Some(sample) => {
                self.played_samples += 1;
                self.peak_sample = self.peak_sample.max(sample.unsigned_abs() as u32);
                if self.played_samples % 1024 == 0 {
                    self.shared_playback_level
                        .store(self.peak_sample, Ordering::Relaxed);
                    self.peak_sample = 0;
                }
                Some(sample as f32 / 32768.0)
            }
            None => {
                // Underflow! Re-enter buffering state
                self.is_buffering = true;
                self.shared_playback_level.store(0, Ordering::Relaxed);
                Some(0.0)
            }
        }
    }
}

impl Source for PcmSource {
    fn current_span_len(&self) -> Option<usize> {
        None
    }
    fn channels(&self) -> u16 {
        self.channels
    }
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    fn total_duration(&self) -> Option<Duration> {
        None
    }
}

struct AudioPlayer {
    sink: Sink,
    _stream: OutputStream, // Keep alive
    current_ffmpeg_child: Option<std::process::Child>,
    tx_samples: Option<Sender<Vec<i16>>>,
    current_url: Option<String>,
    playback_offset: Duration,
    playback_level: Arc<AtomicU32>,
    playback_finished: Arc<AtomicBool>,
}

impl AudioPlayer {
    fn new() -> Self {
        let stream = OutputStreamBuilder::open_default_stream()
            .expect("Failed to open default audio output stream");
        let sink = Sink::connect_new(&stream.mixer());
        sink.set_volume(0.5); // Start at 50% volume

        let playback_level = Arc::new(AtomicU32::new(0));
        let playback_finished = Arc::new(AtomicBool::new(false));
        Self {
            sink,
            _stream: stream,
            current_ffmpeg_child: None,
            tx_samples: None,
            current_url: None,
            playback_offset: Duration::ZERO,
            playback_level,
            playback_finished,
        }
    }

    fn play(&mut self, url: &str, start_seconds: u64) {
        self.stop_current_process();

        let (tx, rx) = channel();
        self.tx_samples = Some(tx.clone());
        self.current_url = Some(url.to_string());
        self.playback_offset = Duration::from_secs(start_seconds);
        self.playback_level = Arc::new(AtomicU32::new(0));
        self.playback_finished = Arc::new(AtomicBool::new(false));

        let mut cmd = Command::new("ffmpeg");
        if start_seconds > 0 {
            cmd.arg("-ss").arg(start_seconds.to_string());
        }
        cmd.args([
            "-i",
            url,
            "-f",
            "s16le",
            "-acodec",
            "pcm_s16le",
            "-ar",
            "44100",
            "-ac",
            "2",
            "pipe:1",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(_) => return,
        };

        let mut stdout = child.stdout.take().expect("Failed to take stdout");
        self.current_ffmpeg_child = Some(child);

        // Reader thread
        std::thread::spawn(move || {
            let mut leftover = Vec::new();
            let mut buffer = [0u8; 16384];
            loop {
                match stdout.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(n) => {
                        let mut data = leftover;
                        data.extend_from_slice(&buffer[..n]);

                        let len = data.len();
                        let end = len - (len % 2);

                        if end > 0 {
                            let mut samples = Vec::with_capacity(end / 2);
                            for chunk in data[..end].chunks_exact(2) {
                                samples.push(i16::from_le_bytes([chunk[0], chunk[1]]));
                            }
                            if tx.send(samples).is_err() {
                                break;
                            }
                        }

                        leftover = data[end..].to_vec();
                    }
                    Err(_) => break,
                }
            }
        });

        self.sink.stop();
        let source = PcmSource {
            receiver: rx,
            buffer: std::collections::VecDeque::with_capacity(88200),
            channels: 2,
            sample_rate: 44100,
            is_buffering: true,
            prebuffer_size: 22050, // 0.25 seconds of stereo audio
            played_samples: 0,
            peak_sample: 0,
            shared_playback_level: Arc::clone(&self.playback_level),
            shared_finished: Arc::clone(&self.playback_finished),
        };
        self.sink.append(source);
        self.sink.play();
    }

    fn position(&self) -> Duration {
        self.playback_offset + self.sink.get_pos()
    }

    fn level(&self) -> f64 {
        self.playback_level.load(Ordering::Relaxed) as f64 / i16::MAX as f64
    }

    fn finished(&self) -> bool {
        self.playback_finished.load(Ordering::Acquire) || self.sink.empty()
    }

    fn seek(&mut self, seconds: u64) {
        if let Some(url) = self.current_url.clone() {
            self.play(&url, seconds);
        }
    }

    fn stop_current_process(&mut self) {
        if let Some(mut child) = self.current_ffmpeg_child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.tx_samples = None;
    }
}

// =========================================================================
// 2. State & Models
// =========================================================================

#[derive(Debug, Clone, Deserialize, Serialize)]
struct YtSearchResult {
    id: String,
    title: String,
    duration: Option<f64>,
    channel: Option<String>,
}

#[derive(Debug, Clone)]
struct PlayingSong {
    id: String,
    title: String,
    duration: u64,
}

enum Focus {
    SearchInput,
    SearchResults,
    Queue,
}

enum PlaybackState {
    Stopped,
    Loading,
    Playing,
    Paused,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Serialize)]
enum LoopMode {
    Off,
    Shuffle,
    Single,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Serialize)]
enum Theme {
    Default,
    Midnight,
    HighContrast,
    TerminalGreen,
}

impl Theme {
    fn next(self) -> Self {
        match self {
            Theme::Default => Theme::Midnight,
            Theme::Midnight => Theme::HighContrast,
            Theme::HighContrast => Theme::TerminalGreen,
            Theme::TerminalGreen => Theme::Default,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Theme::Default => "Default",
            Theme::Midnight => "Midnight",
            Theme::HighContrast => "High Contrast",
            Theme::TerminalGreen => "Terminal Green",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ThemePalette {
    accent: Color,
    border: Color,
    selection_bg: Color,
    selection_fg: Color,
    text: Color,
    dim: Color,
    warning: Color,
    good: Color,
    header: Color,
    highlight: Color,
    background: Color,
}

fn theme_palette(theme: Theme) -> ThemePalette {
    match theme {
        Theme::Default => ThemePalette {
            accent: Color::Cyan,
            border: Color::Magenta,
            selection_bg: Color::Rgb(0, 70, 75),
            selection_fg: Color::White,
            text: Color::White,
            dim: Color::DarkGray,
            warning: Color::Yellow,
            good: Color::LightMagenta,
            header: Color::Magenta,
            highlight: Color::LightMagenta,
            background: Color::Black,
        },
        Theme::Midnight => ThemePalette {
            accent: Color::Rgb(125, 218, 214),
            border: Color::Rgb(219, 134, 200),
            selection_bg: Color::Rgb(24, 39, 54),
            selection_fg: Color::White,
            text: Color::Rgb(231, 237, 242),
            dim: Color::Rgb(130, 144, 156),
            warning: Color::Rgb(235, 205, 97),
            good: Color::Rgb(181, 136, 255),
            header: Color::Rgb(219, 134, 200),
            highlight: Color::Rgb(146, 119, 255),
            background: Color::Rgb(8, 12, 20),
        },
        Theme::HighContrast => ThemePalette {
            accent: Color::White,
            border: Color::White,
            selection_bg: Color::White,
            selection_fg: Color::Black,
            text: Color::White,
            dim: Color::Gray,
            warning: Color::Yellow,
            good: Color::Cyan,
            header: Color::White,
            highlight: Color::Yellow,
            background: Color::Black,
        },
        Theme::TerminalGreen => ThemePalette {
            accent: Color::Rgb(90, 255, 138),
            border: Color::Rgb(90, 255, 138),
            selection_bg: Color::Rgb(12, 46, 23),
            selection_fg: Color::Rgb(225, 255, 229),
            text: Color::Rgb(214, 255, 219),
            dim: Color::Rgb(123, 171, 128),
            warning: Color::Rgb(228, 208, 122),
            good: Color::Rgb(90, 255, 138),
            header: Color::Rgb(90, 255, 138),
            highlight: Color::Rgb(160, 255, 186),
            background: Color::Black,
        },
    }
}

struct AppState {
    search_query: String,
    search_results: Vec<YtSearchResult>,
    selected_result: Option<usize>,
    search_page: usize,
    search_has_next_page: bool,
    search_pending_page: Option<usize>,
    queue: Vec<YtSearchResult>,
    selected_queue: Option<usize>,
    current_queue_index: Option<usize>,
    focus: Focus,
    is_searching: bool,
    playback_state: PlaybackState,
    playing_song: Option<PlayingSong>,
    elapsed: Duration,
    playback_level: f64,
    volume: f32,
    loop_mode: LoopMode,
    theme: Theme,
    playback_error: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct SavedSession {
    queue: Vec<YtSearchResult>,
    current_queue_index: usize,
    elapsed_seconds: u64,
    volume: f32,
    loop_mode: LoopMode,
    theme: Theme,
    was_paused: bool,
}

enum PlayerEvent {
    SearchCompleted {
        query: String,
        page: usize,
        result: Result<(Vec<YtSearchResult>, bool), String>,
    },
    UrlFetched {
        video_id: String,
        stream_url: String,
        title: String,
        duration: u64,
        start_seconds: u64,
        start_paused: bool,
    },
    UrlFetchFailed {
        video_id: String,
        error: String,
    },
    AutoplaySongFetched {
        previous_video_id: String,
        song: YtSearchResult,
    },
    AutoplaySongFetchFailed {
        previous_video_id: String,
        error: String,
    },
}

// =========================================================================
// 3. Search & Stream URL Helpers
// =========================================================================

#[derive(Debug)]
struct EmbeddedRuntimePackage {
    payload: Vec<u8>,
}

#[derive(Debug)]
struct EmbeddedRuntimeFooter {
    payload_len: usize,
    digest: [u8; 32],
}

fn embedded_runtime_root() -> Option<PathBuf> {
    EMBEDDED_RUNTIME_ROOT
        .get_or_init(|| resolve_embedded_runtime_root().ok().flatten())
        .clone()
}

fn resolve_embedded_runtime_root() -> std::io::Result<Option<PathBuf>> {
    let Some(footer) = read_embedded_runtime_footer()? else {
        return Ok(None);
    };

    let Some(base_dir) = runtime_cache_base() else {
        return Ok(None);
    };

    let runtime_root = base_dir.join(hex_digest(&footer.digest));
    if runtime_is_ready(&runtime_root) {
        return Ok(Some(runtime_root));
    }

    let package = read_embedded_runtime_package(&footer)?;
    extract_embedded_runtime(&package, &runtime_root)?;
    Ok(Some(runtime_root))
}

fn runtime_cache_base() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| {
        PathBuf::from(home)
            .join(".local/share/termphonic")
            .join("runtime")
    })
}

fn runtime_is_ready(root: &Path) -> bool {
    root.join("libexec/yt-dlp").is_file() && root.join("libexec/deno").is_file()
}

fn read_embedded_runtime_footer() -> std::io::Result<Option<EmbeddedRuntimeFooter>> {
    let executable = match std::env::current_exe() {
        Ok(path) => path,
        Err(_) => return Ok(None),
    };

    let mut file = match std::fs::File::open(&executable) {
        Ok(file) => file,
        Err(_) => return Ok(None),
    };

    let file_size = file.metadata()?.len() as usize;
    if file_size < EMBEDDED_RUNTIME_FOOTER_LEN {
        return Ok(None);
    }

    file.seek(SeekFrom::End(-(EMBEDDED_RUNTIME_FOOTER_LEN as i64)))?;
    let mut footer = [0u8; EMBEDDED_RUNTIME_FOOTER_LEN];
    file.read_exact(&mut footer)?;

    if footer[..8] != EMBEDDED_RUNTIME_MAGIC {
        return Ok(None);
    }

    let payload_len = u64::from_le_bytes(footer[8..16].try_into().unwrap()) as usize;
    if payload_len > file_size.saturating_sub(EMBEDDED_RUNTIME_FOOTER_LEN) {
        return Ok(None);
    }

    let digest = footer[16..48].try_into().unwrap();
    Ok(Some(EmbeddedRuntimeFooter {
        payload_len,
        digest,
    }))
}

fn read_embedded_runtime_package(
    footer: &EmbeddedRuntimeFooter,
) -> std::io::Result<EmbeddedRuntimePackage> {
    let executable = std::env::current_exe()?;
    let mut file = std::fs::File::open(&executable)?;
    let file_size = file.metadata()?.len() as usize;
    let payload_len = footer.payload_len;
    if payload_len > file_size.saturating_sub(EMBEDDED_RUNTIME_FOOTER_LEN) {
        return Err(std::io::Error::other(
            "embedded runtime payload is truncated",
        ));
    }

    let payload_offset = (file_size - EMBEDDED_RUNTIME_FOOTER_LEN - payload_len) as u64;
    file.seek(SeekFrom::Start(payload_offset))?;
    let mut payload = vec![0u8; payload_len];
    file.read_exact(&mut payload)?;

    let digest = Sha256::digest(&payload);
    let digest_bytes: [u8; 32] = digest.into();
    if digest_bytes != footer.digest {
        return Err(std::io::Error::other(
            "embedded runtime payload hash mismatch",
        ));
    }

    Ok(EmbeddedRuntimePackage { payload })
}

fn extract_embedded_runtime(
    package: &EmbeddedRuntimePackage,
    runtime_root: &Path,
) -> std::io::Result<()> {
    if runtime_is_ready(runtime_root) {
        return Ok(());
    }

    let Some(parent) = runtime_root.parent() else {
        return Ok(());
    };
    std::fs::create_dir_all(parent)?;

    let temp_dir = parent.join(format!(
        ".{}.tmp-{}-{}",
        runtime_root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("runtime"),
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    if temp_dir.exists() {
        std::fs::remove_dir_all(&temp_dir)?;
    }
    std::fs::create_dir_all(&temp_dir)?;

    let cursor = Cursor::new(package.payload.as_slice());
    let decoder = flate2::read::GzDecoder::new(cursor);
    let mut archive = tar::Archive::new(decoder);
    archive.unpack(&temp_dir)?;

    if !runtime_is_ready(&temp_dir) {
        let _ = std::fs::remove_dir_all(&temp_dir);
        return Err(std::io::Error::other(
            "embedded runtime payload is incomplete",
        ));
    }

    match std::fs::rename(&temp_dir, runtime_root) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if runtime_is_ready(runtime_root) {
                let _ = std::fs::remove_dir_all(&temp_dir);
                Ok(())
            } else {
                let _ = std::fs::remove_dir_all(runtime_root);
                std::fs::rename(&temp_dir, runtime_root).or_else(|rename_error| {
                    let _ = std::fs::remove_dir_all(&temp_dir);
                    Err(rename_error)
                })
            }
        }
        Err(error) => {
            let _ = std::fs::remove_dir_all(&temp_dir);
            Err(error)
        }
    }
}

fn hex_digest(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn get_yt_dlp_path() -> PathBuf {
    if let Some(path) = std::env::var_os("TERMPHONIC_YT_DLP") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return path;
        }
    }

    if let Some(runtime_root) = embedded_runtime_root() {
        let path = runtime_root.join("libexec/yt-dlp");
        if path.is_file() {
            return path;
        }
    }

    if let Ok(executable) = std::env::current_exe() {
        if let Some(directory) = executable.parent() {
            let portable_path = directory.join("libexec/yt-dlp");
            if portable_path.is_file() {
                return portable_path;
            }

            let installed_path = directory.join("../lib/termphonic/libexec/yt-dlp");
            if installed_path.is_file() {
                return installed_path;
            }
        }
    }

    if let Some(home) = std::env::var_os("HOME") {
        let user_path = PathBuf::from(home).join(".local/lib/termphonic/libexec/yt-dlp");
        if user_path.is_file() {
            return user_path;
        }
    }

    PathBuf::from("yt-dlp")
}

fn find_javascript_runtime() -> Option<(String, String)> {
    if let Some(path) = std::env::var_os("TERMPHONIC_DENO") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(("deno".to_string(), path.to_string_lossy().into_owned()));
        }
    }

    if let Some(runtime_root) = embedded_runtime_root() {
        let path = runtime_root.join("libexec/deno");
        if path.is_file() {
            return Some(("deno".to_string(), path.to_string_lossy().into_owned()));
        }
    }

    if let Ok(executable) = std::env::current_exe() {
        if let Some(directory) = executable.parent() {
            for path in [
                directory.join("libexec/deno"),
                directory.join("../lib/termphonic/libexec/deno"),
            ] {
                if path.is_file() {
                    return Some(("deno".to_string(), path.to_string_lossy().into_owned()));
                }
            }
        }
    }

    if let Some(home) = std::env::var_os("HOME") {
        let path = PathBuf::from(home).join(".local/lib/termphonic/libexec/deno");
        if path.is_file() {
            return Some(("deno".to_string(), path.to_string_lossy().into_owned()));
        }
    }

    for (runtime, binaries) in [
        ("deno", &["deno"][..]),
        ("node", &["node", "nodejs"][..]),
        ("quickjs", &["qjs", "quickjs"][..]),
        ("bun", &["bun"][..]),
    ] {
        for binary in binaries {
            if let Ok(output) = Command::new("which").arg(binary).output() {
                if output.status.success() {
                    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    if !path.is_empty() {
                        return Some((runtime.to_string(), path));
                    }
                }
            }
        }
    }

    None
}

fn summarize_yt_dlp_error(stderr: &str) -> String {
    stderr
        .lines()
        .rev()
        .find(|line| line.starts_with("ERROR:"))
        .or_else(|| stderr.lines().rev().find(|line| !line.trim().is_empty()))
        .unwrap_or("Unable to fetch the audio stream")
        .trim_start_matches("ERROR:")
        .trim()
        .to_string()
}

async fn search_youtube(query: &str, page: usize) -> Result<(Vec<YtSearchResult>, bool), String> {
    let first_item = page * SEARCH_PAGE_SIZE + 1;
    let last_item = first_item + SEARCH_PAGE_SIZE;
    let output = Command::new(get_yt_dlp_path())
        .args([
            "--dump-json",
            "--flat-playlist",
            "--playlist-items",
            &format!("{first_item}:{last_item}"),
            &format!("ytsearchall:{query}"),
        ])
        .output()
        .map_err(|e| e.to_string())?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }

    let stdout_str = String::from_utf8_lossy(&output.stdout);
    let mut results = Vec::new();
    for line in stdout_str.lines() {
        if let Ok(dump) = serde_json::from_str::<YtSearchResult>(line) {
            results.push(dump);
        }
    }
    let has_next_page = results.len() > SEARCH_PAGE_SIZE;
    results.truncate(SEARCH_PAGE_SIZE);
    Ok((results, has_next_page))
}

fn start_search(
    state: &mut AppState,
    page: usize,
    tx_event: tokio::sync::mpsc::UnboundedSender<PlayerEvent>,
) {
    if state.search_query.trim().is_empty() || state.is_searching {
        return;
    }

    state.is_searching = true;
    state.search_pending_page = Some(page);
    let query = state.search_query.clone();
    tokio::spawn(async move {
        let result = search_youtube(&query, page).await;
        let _ = tx_event.send(PlayerEvent::SearchCompleted {
            query,
            page,
            result,
        });
    });
}

fn session_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| {
        PathBuf::from(home)
            .join(".local/share/termphonic")
            .join("session.json")
    })
}

fn load_session() -> Option<SavedSession> {
    let contents = std::fs::read_to_string(session_path()?).ok()?;
    serde_json::from_str(&contents).ok()
}

fn save_session(state: &AppState) -> std::io::Result<()> {
    let Some(path) = session_path() else {
        return Ok(());
    };

    let active_index = state.current_queue_index.filter(|index| {
        *index < state.queue.len()
            && matches!(
                state.playback_state,
                PlaybackState::Playing | PlaybackState::Paused | PlaybackState::Loading
            )
    });

    if let Some(index) = active_index {
        let session = SavedSession {
            queue: state.queue.clone(),
            current_queue_index: index,
            elapsed_seconds: state.elapsed.as_secs(),
            volume: state.volume,
            loop_mode: state.loop_mode,
            theme: state.theme,
            was_paused: matches!(state.playback_state, PlaybackState::Paused),
        };
        let contents = serde_json::to_vec_pretty(&session)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temporary_path = path.with_extension("json.tmp");
        std::fs::write(&temporary_path, contents)?;
        std::fs::rename(temporary_path, path)?;
    } else if path.exists() {
        std::fs::remove_file(path)?;
    }

    Ok(())
}

// =========================================================================
// 4. Main & Event Loop
// =========================================================================

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Setup terminal
    enable_raw_mode()?;
    std::io::stdout().execute(EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;

    // Player & state
    let mut audio_player = AudioPlayer::new();
    let mut state = AppState {
        search_query: String::new(),
        search_results: Vec::new(),
        selected_result: None,
        search_page: 0,
        search_has_next_page: false,
        search_pending_page: None,
        queue: Vec::new(),
        selected_queue: None,
        current_queue_index: None,
        focus: Focus::SearchInput,
        is_searching: false,
        playback_state: PlaybackState::Stopped,
        playing_song: None,
        elapsed: Duration::ZERO,
        playback_level: 0.0,
        volume: 0.5,
        loop_mode: LoopMode::Off,
        theme: Theme::Default,
        playback_error: None,
    };
    let restored_session = load_session();
    if let Some(session) = restored_session.as_ref() {
        state.queue = session.queue.clone();
        state.current_queue_index = Some(session.current_queue_index);
        state.selected_queue = Some(session.current_queue_index);
        state.volume = session.volume.clamp(0.0, 1.0);
        state.loop_mode = session.loop_mode;
        state.theme = session.theme;
        state.focus = Focus::Queue;
    }
    audio_player.sink.set_volume(state.volume);

    // Event channels
    let (tx_event, mut rx_event) = tokio::sync::mpsc::unbounded_channel::<PlayerEvent>();
    if let Some(session) = restored_session {
        if session.current_queue_index < state.queue.len() {
            start_song_at_queue_index_from(
                &mut state,
                session.current_queue_index,
                session.elapsed_seconds,
                session.was_paused,
                &mut audio_player,
                tx_event.clone(),
            );
        }
    }

    let mut last_tick = std::time::Instant::now();
    let mut last_session_save = std::time::Instant::now();
    let tick_rate = Duration::from_millis(200);

    loop {
        // Render
        terminal.draw(|f| draw_ui(f, &state))?;

        // Handle async backend events
        while let Ok(event) = rx_event.try_recv() {
            match event {
                PlayerEvent::SearchCompleted {
                    query,
                    page,
                    result,
                } => {
                    if query != state.search_query {
                        continue;
                    }
                    state.is_searching = false;
                    state.search_pending_page = None;
                    if let Ok((results, has_next_page)) = result {
                        state.search_results = results;
                        state.search_page = page;
                        state.search_has_next_page = has_next_page;
                        if !state.search_results.is_empty() {
                            state.selected_result = Some(0);
                            state.focus = Focus::SearchResults;
                        } else {
                            state.selected_result = None;
                        }
                    }
                }
                PlayerEvent::UrlFetched {
                    video_id,
                    stream_url,
                    title,
                    duration,
                    start_seconds,
                    start_paused,
                } => {
                    if let Some(ref active) = state.playing_song {
                        if active.id == video_id
                            && matches!(state.playback_state, PlaybackState::Loading)
                        {
                            state.playing_song = Some(PlayingSong {
                                id: video_id,
                                title,
                                duration,
                            });
                            audio_player.play(&stream_url, start_seconds);
                            state.elapsed = Duration::from_secs(start_seconds);
                            if start_paused {
                                audio_player.sink.pause();
                                state.playback_state = PlaybackState::Paused;
                            } else {
                                state.playback_state = PlaybackState::Playing;
                            }
                            state.playback_error = None;
                        }
                    }
                }
                PlayerEvent::UrlFetchFailed { video_id, error } => {
                    if let Some(ref active) = state.playing_song {
                        if active.id == video_id {
                            state.playing_song = None;
                            state.playback_state = PlaybackState::Stopped;
                            state.playback_error = Some(error.trim().to_string());
                        }
                    }
                }
                PlayerEvent::AutoplaySongFetched {
                    previous_video_id,
                    song,
                } => {
                    let still_waiting = state
                        .playing_song
                        .as_ref()
                        .is_some_and(|active| active.id == previous_video_id)
                        && matches!(state.playback_state, PlaybackState::Loading);
                    if still_waiting {
                        state.queue.push(song);
                        let new_idx = state.queue.len() - 1;
                        start_song_at_queue_index(
                            &mut state,
                            new_idx,
                            &mut audio_player,
                            tx_event.clone(),
                        );
                    }
                }
                PlayerEvent::AutoplaySongFetchFailed {
                    previous_video_id,
                    error,
                } => {
                    let still_waiting = state
                        .playing_song
                        .as_ref()
                        .is_some_and(|active| active.id == previous_video_id)
                        && matches!(state.playback_state, PlaybackState::Loading);
                    if still_waiting {
                        state.playback_state = PlaybackState::Stopped;
                        state.playback_error = Some(error);
                    }
                }
            }
        }

        // Handle auto-advance to next song
        if matches!(state.playback_state, PlaybackState::Playing) && audio_player.finished() {
            play_next(&mut state, &mut audio_player, tx_event.clone());
        }

        // Follow samples actually consumed by the audio output, including seeks.
        if matches!(
            state.playback_state,
            PlaybackState::Playing | PlaybackState::Paused
        ) {
            state.elapsed = audio_player.position();
            if let Some(ref song) = state.playing_song {
                state.elapsed = state.elapsed.min(Duration::from_secs(song.duration));
            }
            if matches!(state.playback_state, PlaybackState::Playing) {
                state.playback_level = audio_player.level();
            }
        }

        // Poll events
        let timeout = tick_rate
            .checked_sub(last_tick.elapsed())
            .unwrap_or(Duration::from_secs(0));

        if crossterm::event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Release {
                    match state.focus {
                        Focus::SearchInput => match key.code {
                            KeyCode::Char(c) => {
                                state.search_query.push(c);
                            }
                            KeyCode::Backspace => {
                                state.search_query.pop();
                            }
                            KeyCode::Esc => {
                                state.focus = Focus::SearchResults;
                            }
                            KeyCode::Enter => {
                                state.search_page = 0;
                                state.search_has_next_page = false;
                                start_search(&mut state, 0, tx_event.clone());
                            }
                            _ => {}
                        },
                        Focus::SearchResults => match key.code {
                            KeyCode::Char('q') => {
                                break;
                            }
                            KeyCode::Char('/') | KeyCode::Char('i') => {
                                state.focus = Focus::SearchInput;
                            }
                            KeyCode::Char(' ') => {
                                toggle_pause(&mut state, &audio_player);
                            }
                            KeyCode::Char('s') => {
                                stop_player(&mut state, &mut audio_player);
                            }
                            KeyCode::Char('r') => {
                                state.loop_mode = match state.loop_mode {
                                    LoopMode::Off => LoopMode::Shuffle,
                                    LoopMode::Shuffle => LoopMode::Single,
                                    LoopMode::Single => LoopMode::Off,
                                };
                            }
                            KeyCode::Char('t') => {
                                state.theme = state.theme.next();
                            }
                            KeyCode::Char('T') => {
                                state.theme = Theme::Default;
                            }
                            KeyCode::Up => {
                                if let Some(sel) = state.selected_result {
                                    if sel > 0 {
                                        state.selected_result = Some(sel - 1);
                                    }
                                }
                            }
                            KeyCode::Down => {
                                if let Some(sel) = state.selected_result {
                                    if sel < state.search_results.len() - 1 {
                                        state.selected_result = Some(sel + 1);
                                    }
                                }
                            }
                            KeyCode::PageUp => {
                                if state.search_page > 0 {
                                    let page = state.search_page - 1;
                                    start_search(&mut state, page, tx_event.clone());
                                }
                            }
                            KeyCode::PageDown => {
                                if state.search_has_next_page {
                                    let page = state.search_page + 1;
                                    start_search(&mut state, page, tx_event.clone());
                                }
                            }
                            KeyCode::Enter => {
                                if let Some(sel) = state.selected_result {
                                    let song = state.search_results[sel].clone();
                                    // Add to queue and play
                                    state.queue.push(song.clone());
                                    let queue_idx = state.queue.len() - 1;
                                    state.selected_queue = Some(queue_idx);

                                    start_song_at_queue_index(
                                        &mut state,
                                        queue_idx,
                                        &mut audio_player,
                                        tx_event.clone(),
                                    );
                                }
                            }
                            KeyCode::Tab => {
                                if !state.queue.is_empty() {
                                    state.selected_queue = Some(0);
                                    state.focus = Focus::Queue;
                                }
                            }
                            KeyCode::Char('+') | KeyCode::Char('=') => {
                                adjust_volume(&mut state, &audio_player, 0.05);
                            }
                            KeyCode::Char('-') | KeyCode::Char('_') => {
                                adjust_volume(&mut state, &audio_player, -0.05);
                            }
                            KeyCode::Left => {
                                seek_relative(&mut state, &mut audio_player, -10);
                            }
                            KeyCode::Right => {
                                seek_relative(&mut state, &mut audio_player, 10);
                            }
                            _ => {}
                        },
                        Focus::Queue => match key.code {
                            KeyCode::Char('q') => {
                                break;
                            }
                            KeyCode::Char('/') | KeyCode::Char('i') => {
                                state.focus = Focus::SearchInput;
                            }
                            KeyCode::Char(' ') => {
                                toggle_pause(&mut state, &audio_player);
                            }
                            KeyCode::Char('s') => {
                                stop_player(&mut state, &mut audio_player);
                            }
                            KeyCode::Char('r') => {
                                state.loop_mode = match state.loop_mode {
                                    LoopMode::Off => LoopMode::Shuffle,
                                    LoopMode::Shuffle => LoopMode::Single,
                                    LoopMode::Single => LoopMode::Off,
                                };
                            }
                            KeyCode::Char('t') => {
                                state.theme = state.theme.next();
                            }
                            KeyCode::Char('T') => {
                                state.theme = Theme::Default;
                            }
                            KeyCode::Up => {
                                if let Some(sel) = state.selected_queue {
                                    if sel > 0 {
                                        state.selected_queue = Some(sel - 1);
                                    }
                                }
                            }
                            KeyCode::Down => {
                                if let Some(sel) = state.selected_queue {
                                    if sel < state.queue.len() - 1 {
                                        state.selected_queue = Some(sel + 1);
                                    }
                                }
                            }
                            KeyCode::Enter => {
                                if let Some(sel) = state.selected_queue {
                                    start_song_at_queue_index(
                                        &mut state,
                                        sel,
                                        &mut audio_player,
                                        tx_event.clone(),
                                    );
                                }
                            }
                            KeyCode::Tab => {
                                state.focus = Focus::SearchResults;
                            }
                            KeyCode::Char('d') | KeyCode::Delete => {
                                if let Some(sel) = state.selected_queue {
                                    state.queue.remove(sel);

                                    // Adjust current playing index if needed
                                    if let Some(curr) = state.current_queue_index {
                                        if curr == sel {
                                            stop_player(&mut state, &mut audio_player);
                                            state.current_queue_index = None;
                                        } else if curr > sel {
                                            state.current_queue_index = Some(curr - 1);
                                        }
                                    }

                                    // Adjust selected queue index
                                    if state.queue.is_empty() {
                                        state.selected_queue = None;
                                        state.focus = Focus::SearchResults;
                                    } else {
                                        state.selected_queue = Some(sel.min(state.queue.len() - 1));
                                    }
                                }
                            }
                            KeyCode::Char('+') | KeyCode::Char('=') => {
                                adjust_volume(&mut state, &audio_player, 0.05);
                            }
                            KeyCode::Char('-') | KeyCode::Char('_') => {
                                adjust_volume(&mut state, &audio_player, -0.05);
                            }
                            KeyCode::Left => {
                                seek_relative(&mut state, &mut audio_player, -10);
                            }
                            KeyCode::Right => {
                                seek_relative(&mut state, &mut audio_player, 10);
                            }
                            _ => {}
                        },
                    }
                }
            }
        }

        if last_tick.elapsed() >= tick_rate {
            last_tick = std::time::Instant::now();
        }
        if last_session_save.elapsed() >= Duration::from_secs(1) {
            let _ = save_session(&state);
            last_session_save = std::time::Instant::now();
        }
    }

    let _ = save_session(&state);
    // Restore terminal
    disable_raw_mode()?;
    std::io::stdout().execute(LeaveAlternateScreen)?;
    Ok(())
}

// =========================================================================
// 5. Actions / Functions
// =========================================================================

fn toggle_pause(state: &mut AppState, player: &AudioPlayer) {
    if matches!(state.playback_state, PlaybackState::Playing) {
        player.sink.pause();
        state.playback_state = PlaybackState::Paused;
    } else if matches!(state.playback_state, PlaybackState::Paused) {
        player.sink.play();
        state.playback_state = PlaybackState::Playing;
    }
}

fn stop_player(state: &mut AppState, player: &mut AudioPlayer) {
    player.stop_current_process();
    player.sink.stop();
    state.playback_state = PlaybackState::Stopped;
    state.playing_song = None;
    state.elapsed = Duration::ZERO;
    state.playback_level = 0.0;
    state.playback_error = None;
}

fn adjust_volume(state: &mut AppState, player: &AudioPlayer, diff: f32) {
    state.volume = (state.volume + diff).clamp(0.0, 1.0);
    player.sink.set_volume(state.volume);
}

fn seek_relative(state: &mut AppState, player: &mut AudioPlayer, diff_seconds: i64) {
    if let Some(ref song) = state.playing_song {
        if matches!(
            state.playback_state,
            PlaybackState::Playing | PlaybackState::Paused
        ) {
            let current = state.elapsed.as_secs() as i64;
            let target = (current + diff_seconds).clamp(0, song.duration as i64) as u64;
            player.seek(target);
            state.elapsed = Duration::from_secs(target);
            state.playback_state = PlaybackState::Playing;
        }
    }
}

fn start_song_at_queue_index(
    state: &mut AppState,
    idx: usize,
    player: &mut AudioPlayer,
    tx_event: tokio::sync::mpsc::UnboundedSender<PlayerEvent>,
) {
    start_song_at_queue_index_from(state, idx, 0, false, player, tx_event);
}

fn start_song_at_queue_index_from(
    state: &mut AppState,
    idx: usize,
    start_seconds: u64,
    start_paused: bool,
    player: &mut AudioPlayer,
    tx_event: tokio::sync::mpsc::UnboundedSender<PlayerEvent>,
) {
    if idx >= state.queue.len() {
        return;
    }

    let song = state.queue[idx].clone();
    state.current_queue_index = Some(idx);
    state.playback_state = PlaybackState::Loading;
    state.playing_song = Some(PlayingSong {
        id: song.id.clone(),
        title: song.title.clone(),
        duration: song.duration.unwrap_or(0.0) as u64,
    });
    let duration = song.duration.unwrap_or(0.0) as u64;
    let start_seconds = if duration > 0 {
        start_seconds.min(duration.saturating_sub(1))
    } else {
        0
    };
    state.elapsed = Duration::from_secs(start_seconds);
    state.playback_level = 0.0;
    state.playback_error = None;

    // Reset player process/sink
    player.stop_current_process();
    player.sink.stop();

    // Fetch stream url
    let video_id = song.id.clone();
    let title = song.title.clone();

    let yt_dlp_bin = get_yt_dlp_path();
    let javascript_runtime = find_javascript_runtime();
    tokio::spawn(async move {
        let mut command = Command::new(yt_dlp_bin);
        // Prefer audio-only streams, but support videos that only expose
        // combined HLS formats. FFmpeg discards the video track downstream.
        command.args(["--no-playlist", "-g", "-f", "bestaudio/best"]);
        if let Some((runtime, path)) = javascript_runtime {
            command
                .arg("--js-runtimes")
                .arg(format!("{runtime}:{path}"));
        }
        command.arg(format!("https://www.youtube.com/watch?v={video_id}"));

        let yt_output = command.output();

        match yt_output {
            Ok(output) if output.status.success() => {
                let stream_url = String::from_utf8_lossy(&output.stdout).trim().to_string();
                let _ = tx_event.send(PlayerEvent::UrlFetched {
                    video_id,
                    stream_url,
                    title,
                    duration,
                    start_seconds,
                    start_paused,
                });
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let error = summarize_yt_dlp_error(&stderr);
                let _ = tx_event.send(PlayerEvent::UrlFetchFailed { video_id, error });
            }
            Err(e) => {
                let _ = tx_event.send(PlayerEvent::UrlFetchFailed {
                    video_id,
                    error: e.to_string(),
                });
            }
        }
    });
}

/// Searches YouTube using the current song's title as a seed query,
/// picks a random result that hasn't just been played, then sends it
/// back to the main loop via AutoplaySongFetched.
fn fetch_shuffle_song(
    seed_title: String,
    last_id: String,
    tx_event: tokio::sync::mpsc::UnboundedSender<PlayerEvent>,
) {
    tokio::spawn(async move {
        // Use a simple LCG seeded from current time for randomness (no extra dep)
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos() as u64;

        let query = format!("{} mix", seed_title);
        let output = Command::new(get_yt_dlp_path())
            .args([
                "--dump-json",
                "--flat-playlist",
                &format!("ytsearch10:{}", query),
            ])
            .output();

        let output = match output {
            Ok(output) if output.status.success() => output,
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let _ = tx_event.send(PlayerEvent::AutoplaySongFetchFailed {
                    previous_video_id: last_id,
                    error: summarize_yt_dlp_error(&stderr),
                });
                return;
            }
            Err(error) => {
                let _ = tx_event.send(PlayerEvent::AutoplaySongFetchFailed {
                    previous_video_id: last_id,
                    error: error.to_string(),
                });
                return;
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut candidates: Vec<YtSearchResult> = stdout
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .filter(|r: &YtSearchResult| r.id != last_id)
            .collect();

        if candidates.is_empty() {
            let _ = tx_event.send(PlayerEvent::AutoplaySongFetchFailed {
                previous_video_id: last_id,
                error: "No related songs found for autoplay".to_string(),
            });
            return;
        }

        // Pick a pseudo-random candidate using the seed
        let pick = (seed as usize) % candidates.len();
        let chosen = candidates.remove(pick);
        let _ = tx_event.send(PlayerEvent::AutoplaySongFetched {
            previous_video_id: last_id,
            song: chosen,
        });
    });
}

fn play_next(
    state: &mut AppState,
    player: &mut AudioPlayer,
    tx_event: tokio::sync::mpsc::UnboundedSender<PlayerEvent>,
) {
    if let Some(curr) = state.current_queue_index {
        match state.loop_mode {
            LoopMode::Single => {
                start_song_at_queue_index(state, curr, player, tx_event);
            }
            LoopMode::Shuffle => {
                let seed = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .subsec_nanos() as usize;
                let available_indices: Vec<usize> = (0..state.queue.len())
                    .filter(|index| *index != curr)
                    .collect();
                if !available_indices.is_empty() {
                    let next_index = available_indices[seed % available_indices.len()];
                    start_song_at_queue_index(state, next_index, player, tx_event);
                    return;
                }

                let current_id = state
                    .playing_song
                    .as_ref()
                    .map(|song| song.id.as_str())
                    .unwrap_or_default();
                let candidates: Vec<YtSearchResult> = state
                    .search_results
                    .iter()
                    .filter(|song| song.id != current_id)
                    .cloned()
                    .collect();
                if !candidates.is_empty() {
                    let next_song = candidates[seed % candidates.len()].clone();
                    state.queue.push(next_song);
                    let next_index = state.queue.len() - 1;
                    start_song_at_queue_index(state, next_index, player, tx_event);
                    return;
                }

                // Fetch a new song based on the current song's title
                let seed_title = state
                    .playing_song
                    .as_ref()
                    .map(|s| s.title.clone())
                    .unwrap_or_default();
                let last_id = state
                    .playing_song
                    .as_ref()
                    .map(|s| s.id.clone())
                    .unwrap_or_default();

                // Mark as loading while we fetch
                state.playback_state = PlaybackState::Loading;
                fetch_shuffle_song(seed_title, last_id, tx_event);
            }
            LoopMode::Off => {
                if curr + 1 < state.queue.len() {
                    start_song_at_queue_index(state, curr + 1, player, tx_event);
                } else {
                    stop_player(state, player);
                    state.current_queue_index = None;
                }
            }
        }
    }
}

// =========================================================================
// 6. UI Drawing
// =========================================================================

fn draw_ui(frame: &mut Frame, state: &AppState) {
    let palette = theme_palette(state.theme);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Header
            Constraint::Min(10),   // Main area
            Constraint::Length(2), // Footer / Help
        ])
        .split(frame.area());

    // 1. Header
    let header = Paragraph::new("♫  Termphonic - Music in Your Terminal  ♫")
        .alignment(Alignment::Center)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(palette.border)),
        );
    frame.render_widget(header, chunks[0]);

    // Split main area horizontally
    let main_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(55), // Left: Search
            Constraint::Percentage(45), // Right: Player & Queue
        ])
        .split(chunks[1]);

    // Left Panel: Search Query & Search Results
    let search_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Input
            Constraint::Min(5),    // Results
        ])
        .split(main_chunks[0]);

    // Draw Input box
    let input_border_color = if matches!(state.focus, Focus::SearchInput) {
        palette.accent
    } else {
        palette.dim
    };
    let input_title = if matches!(state.focus, Focus::SearchInput) {
        " Search (Typing...) "
    } else {
        " Search (Press / to search) "
    };
    let input_box = Paragraph::new(state.search_query.as_str()).block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(input_title)
            .border_style(Style::default().fg(input_border_color)),
    );
    frame.render_widget(input_box, search_chunks[0]);

    // Draw Results List
    let results_border_color = if matches!(state.focus, Focus::SearchResults) {
        palette.accent
    } else {
        palette.dim
    };
    let visible_result_rows = search_chunks[1].height.saturating_sub(3).max(1) as usize;
    let selected_result = state.selected_result.unwrap_or(0);
    let result_start = selected_result
        .saturating_sub(visible_result_rows.saturating_sub(1))
        .min(
            state
                .search_results
                .len()
                .saturating_sub(visible_result_rows),
        );
    let results_title = if state.is_searching {
        format!(
            " Search Results · Loading page {}... ",
            state.search_pending_page.unwrap_or(state.search_page) + 1
        )
    } else if state.search_results.is_empty() {
        " Search Results ".to_string()
    } else {
        let page_start = state.search_page * SEARCH_PAGE_SIZE + 1;
        let page_end = page_start + state.search_results.len() - 1;
        let next_hint = if state.search_has_next_page {
            " · PgDn next"
        } else {
            ""
        };
        format!(
            " Search Results · Page {} · {}-{}{} ",
            state.search_page + 1,
            page_start,
            page_end,
            next_hint
        )
    };
    let results_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(results_title)
        .border_style(Style::default().fg(results_border_color));

    if state.is_searching {
        let loading = Paragraph::new("\n\n🔍 Searching YouTube...")
            .alignment(Alignment::Center)
            .block(results_block);
        frame.render_widget(loading, search_chunks[1]);
    } else if state.search_results.is_empty() {
        let empty =
            Paragraph::new("\n\nNo results. Press / and type a query above, then press Enter.")
                .alignment(Alignment::Center)
                .block(results_block);
        frame.render_widget(empty, search_chunks[1]);
    } else {
        let results_inner = results_block.inner(search_chunks[1]);
        frame.render_widget(results_block, search_chunks[1]);

        let results_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(1)])
            .split(results_inner);

        let results_width = results_inner.width as usize;
        let show_channel = results_width >= 62;
        let fixed_width = if show_channel { 32 } else { 16 };
        let title_width = results_width.saturating_sub(fixed_width).max(8);
        let channel_width = 14;
        let duration_width = 8;
        let header_style = Style::default()
            .fg(palette.dim)
            .add_modifier(Modifier::BOLD);

        let mut header_spans = vec![
            Span::styled("  ", header_style),
            Span::styled("#   ", header_style),
            Span::styled(pad_display_width("Title", title_width), header_style),
        ];
        if show_channel {
            header_spans.extend([
                Span::styled("  ", header_style),
                Span::styled(pad_display_width("Channel", channel_width), header_style),
            ]);
        }
        header_spans.extend([
            Span::styled("  ", header_style),
            Span::styled(
                format!("{:>width$}", "Duration", width = duration_width),
                header_style,
            ),
        ]);
        frame.render_widget(Paragraph::new(Line::from(header_spans)), results_chunks[0]);

        let visible_rows = results_chunks[1].height.max(1) as usize;
        let start = result_start.min(state.search_results.len().saturating_sub(visible_rows));
        let items: Vec<ListItem> = state
            .search_results
            .iter()
            .enumerate()
            .skip(start)
            .take(visible_rows)
            .map(|(idx, res)| {
                let is_selected = Some(idx) == state.selected_result;
                let is_active = state
                    .playing_song
                    .as_ref()
                    .is_some_and(|song| song.id == res.id);
                let row_style = if is_selected {
                    Style::default()
                        .fg(palette.selection_fg)
                        .bg(palette.selection_bg)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(palette.text)
                };
                let marker = if is_active {
                    match state.playback_state {
                        PlaybackState::Loading => "… ",
                        PlaybackState::Playing | PlaybackState::Paused => "▶ ",
                        PlaybackState::Stopped => "  ",
                    }
                } else if is_selected {
                    "› "
                } else {
                    "  "
                };
                let marker_style = if is_active {
                    row_style.fg(palette.good)
                } else {
                    row_style
                };
                let duration_str = format_media_duration(res.duration);
                let channel_str = res.channel.as_deref().unwrap_or("Unknown");

                let mut spans = vec![
                    Span::styled(marker, marker_style),
                    Span::styled(
                        format!("{:02}. ", state.search_page * SEARCH_PAGE_SIZE + idx + 1),
                        if is_selected {
                            row_style
                        } else {
                            Style::default().fg(palette.dim)
                        },
                    ),
                    Span::styled(
                        pad_display_width(&res.title, title_width),
                        if is_active {
                            marker_style.add_modifier(Modifier::BOLD)
                        } else {
                            row_style
                        },
                    ),
                ];
                if show_channel {
                    spans.extend([
                        Span::styled("  ", row_style),
                        Span::styled(
                            pad_display_width(channel_str, channel_width),
                            if is_selected {
                                row_style
                            } else {
                                Style::default().fg(palette.dim)
                            },
                        ),
                    ]);
                }
                spans.extend([
                    Span::styled("  ", row_style),
                    Span::styled(
                        format!("{duration_str:>duration_width$}"),
                        if is_selected {
                            row_style
                        } else {
                            Style::default().fg(palette.warning)
                        },
                    ),
                ]);
                let line = Line::from(spans);
                ListItem::new(line)
            })
            .collect();

        frame.render_widget(List::new(items), results_chunks[1]);
    }

    // Right Panel: Player Status & Queue
    let right_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(9), // Player Status
            Constraint::Min(5),    // Queue
        ])
        .split(main_chunks[1]);

    // Player Status Box
    let player_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(" Now Playing ")
        .border_style(Style::default().fg(palette.header));

    if let Some(ref song) = state.playing_song {
        let title_width = right_chunks[0].width.saturating_sub(10) as usize;
        let status_str = match state.playback_state {
            PlaybackState::Stopped => "Stopped".to_string(),
            PlaybackState::Loading => {
                const SPINNER: [char; 8] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧'];
                let frame = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
                    / 200;
                format!(
                    "Preparing audio {}",
                    SPINNER[frame as usize % SPINNER.len()]
                )
            }
            PlaybackState::Playing => "Playing 🔊".to_string(),
            PlaybackState::Paused => "Paused ⏸".to_string(),
        };

        let elapsed = state.elapsed;
        let total = song.duration;
        let elapsed_str = format_duration(elapsed.as_secs());
        let total_str = format_duration(total);
        let timer_str = format!("{} / {}", elapsed_str, total_str);

        let vol_pct = (state.volume * 100.0) as u32;
        let vol_blocks = (state.volume * 10.0) as usize;
        let vol_bar = "━".repeat(vol_blocks) + &"─".repeat(10 - vol_blocks);
        let vol_str = format!("[{}] {}%", vol_bar, vol_pct);
        let repeat_str = match state.loop_mode {
            LoopMode::Off => "Off",
            LoopMode::Shuffle => "Shuffle 🔀",
            LoopMode::Single => "Single 🔂",
        };

        let mut status_line = vec![
            Span::styled(
                "Status: ",
                Style::default()
                    .fg(palette.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                status_str,
                Style::default().fg(if matches!(state.playback_state, PlaybackState::Loading) {
                    palette.accent
                } else {
                    palette.warning
                }),
            ),
        ];
        if !matches!(state.playback_state, PlaybackState::Loading) {
            status_line.extend([
                Span::styled("  ", Style::default()),
                Span::styled(
                    timer_str,
                    Style::default()
                        .fg(palette.text)
                        .add_modifier(Modifier::BOLD),
                ),
            ]);
        }

        let info = vec![
            Line::from(vec![
                Span::styled(
                    "Title:  ",
                    Style::default()
                        .fg(palette.accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    truncate_str(&song.title, title_width.max(4)),
                    Style::default().fg(palette.text),
                ),
            ]),
            Line::from(status_line),
            Line::from(vec![
                Span::styled(
                    "Volume: ",
                    Style::default()
                        .fg(palette.accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(vol_str),
            ]),
            Line::from(vec![
                Span::styled(
                    "Repeat: ",
                    Style::default()
                        .fg(palette.accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(repeat_str, Style::default().fg(palette.warning)),
            ]),
            Line::from(vec![
                Span::styled(
                    "Theme:  ",
                    Style::default()
                        .fg(palette.accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(state.theme.name(), Style::default().fg(palette.highlight)),
            ]),
        ];

        let player_inner_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(4), // Text details
                Constraint::Min(0),    // Flexible spacing
                Constraint::Length(1), // CAVA-style visualizer
            ])
            .split(player_block.inner(right_chunks[0]));

        let visualizer_width = player_inner_chunks[2].width as usize;
        let visualizer = Paragraph::new(cava_visualizer(
            state.elapsed,
            state.playback_level,
            visualizer_width,
        ));

        frame.render_widget(player_block, right_chunks[0]);
        frame.render_widget(Paragraph::new(info), player_inner_chunks[0]);
        frame.render_widget(visualizer, player_inner_chunks[2]);
    } else {
        let message = if let Some(error) = state.playback_error.as_deref() {
            format!(
                "\nPlayback error:\n{}",
                truncate_str(error.lines().next().unwrap_or(error), 48)
            )
        } else {
            "\n\nNo song playing.\nSelect a search result and press Enter to play.".to_string()
        };
        let no_song = Paragraph::new(message)
            .alignment(Alignment::Center)
            .style(Style::default().fg(if state.playback_error.is_some() {
                palette.warning
            } else {
                palette.text
            }))
            .block(player_block);
        frame.render_widget(no_song, right_chunks[0]);
    }

    // Draw Queue
    let queue_border_color = if matches!(state.focus, Focus::Queue) {
        palette.accent
    } else {
        palette.dim
    };
    let queue_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(" Playback Queue ")
        .border_style(Style::default().fg(queue_border_color));

    if state.queue.is_empty() {
        let empty_queue = Paragraph::new("\n\nQueue is empty. Select search results to add them.")
            .alignment(Alignment::Center)
            .block(queue_block);
        frame.render_widget(empty_queue, right_chunks[1]);
    } else {
        let queue_width = right_chunks[1].width.saturating_sub(2) as usize;
        let title_width = queue_width.saturating_sub(13).max(8);
        let items: Vec<ListItem> = state
            .queue
            .iter()
            .enumerate()
            .map(|(idx, res)| {
                let is_selected = Some(idx) == state.selected_queue;
                let is_currently_playing = Some(idx) == state.current_queue_index;

                let row_style = if is_selected {
                    Style::default()
                        .fg(palette.selection_fg)
                        .bg(palette.selection_bg)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(palette.text)
                };
                let active_style = if is_selected {
                    row_style.fg(palette.good)
                } else if is_currently_playing {
                    Style::default()
                        .fg(palette.good)
                        .add_modifier(Modifier::BOLD)
                } else {
                    row_style
                };
                let prefix = if is_currently_playing { "▶ " } else { "  " };
                let duration = format_media_duration(res.duration);

                let line = Line::from(vec![
                    Span::styled(prefix, active_style),
                    Span::styled(format!("{:02}. ", idx + 1), row_style),
                    Span::styled(
                        format!(
                            "{:<width$}",
                            truncate_str(&res.title, title_width),
                            width = title_width
                        ),
                        if is_currently_playing {
                            active_style
                        } else {
                            row_style
                        },
                    ),
                    Span::styled("  ", row_style),
                    Span::styled(duration, row_style.fg(palette.warning)),
                ]);
                ListItem::new(line)
            })
            .collect();

        let list = List::new(items).block(queue_block);
        frame.render_widget(list, right_chunks[1]);
    }

    // 3. Footer / Help Bar
    let footer = Paragraph::new(vec![
        Line::from(" / Search   ↑/↓ Select   PgUp/PgDn Pages   Enter Play   Tab Focus"),
        Line::from(" Space Play/Pause   ←/→ Seek   +/- Volume   s Stop   r Repeat   q Quit"),
    ])
    .alignment(Alignment::Center)
    .style(Style::default().fg(palette.dim).bg(palette.background));
    frame.render_widget(footer, chunks[2]);
}

fn format_duration(seconds: u64) -> String {
    let hours = seconds / 3600;
    let mins = (seconds % 3600) / 60;
    let secs = seconds % 60;
    if hours > 0 {
        format!("{:02}:{:02}:{:02}", hours, mins, secs)
    } else {
        format!("{:02}:{:02}", mins, secs)
    }
}

fn format_media_duration(duration: Option<f64>) -> String {
    match duration {
        Some(seconds) if seconds >= 3600.0 => {
            let total_seconds = seconds as u64;
            let hours = total_seconds / 3600;
            let minutes = (total_seconds % 3600) / 60;
            format!("{hours}h{minutes:02}m")
        }
        Some(seconds) if seconds > 0.0 => format_duration(seconds as u64),
        Some(_) | None => "AO VIVO".to_string(),
    }
}

fn pad_display_width(value: &str, width: usize) -> String {
    let truncated = truncate_display_width(value, width);
    let display_width = truncated
        .chars()
        .map(|character| character.width().unwrap_or(0))
        .sum::<usize>();
    format!(
        "{truncated}{}",
        " ".repeat(width.saturating_sub(display_width))
    )
}

fn truncate_display_width(value: &str, max_width: usize) -> String {
    let width = value
        .chars()
        .map(|character| character.width().unwrap_or(0))
        .sum::<usize>();
    if width <= max_width {
        return value.to_string();
    }

    let ellipsis = "...";
    let target_width = max_width.saturating_sub(ellipsis.len());
    let mut current_width = 0;
    let mut truncated = String::new();
    for character in value.chars() {
        let character_width = character.width().unwrap_or(0);
        if current_width + character_width > target_width {
            break;
        }
        truncated.push(character);
        current_width += character_width;
    }
    truncated.push_str(ellipsis);
    truncated
}

fn cava_visualizer(elapsed: Duration, level: f64, width: usize) -> Line<'static> {
    const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

    let phase = elapsed.as_secs_f64() * 8.0;
    let strength = level.clamp(0.0, 1.0).sqrt();

    let spans = (0..width)
        .map(|index| {
            let x = index as f64;
            let movement =
                ((phase + x * 0.83).sin().abs() + ((phase * 0.61) - x * 0.47).sin().abs()) / 2.0;
            let height = (movement * strength * (BARS.len() - 1) as f64).round() as usize;
            let mix = if width > 1 {
                index as f64 / (width - 1) as f64
            } else {
                0.0
            };
            let color = Color::Rgb(
                (70.0 + 180.0 * mix) as u8,
                (210.0 - 100.0 * mix) as u8,
                (210.0 + 35.0 * mix) as u8,
            );
            Span::styled(
                BARS[height.min(BARS.len() - 1)].to_string(),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            )
        })
        .collect::<Vec<_>>();

    Line::from(spans)
}

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.chars().count() <= max_len {
        s.to_string()
    } else {
        let mut truncated = s.chars().take(max_len - 3).collect::<String>();
        truncated.push_str("...");
        truncated
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::fs;

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ))
    }

    #[test]
    fn formats_durations_with_hours() {
        assert_eq!(format_duration(302 * 60), "05:02:00");
        assert_eq!(format_duration(71), "01:11");
    }

    #[test]
    fn formats_live_and_unknown_media() {
        assert_eq!(format_media_duration(Some(42_825.0)), "11h53m");
        assert_eq!(format_media_duration(Some(211.0)), "03:31");
        assert_eq!(format_media_duration(Some(0.0)), "AO VIVO");
        assert_eq!(format_media_duration(None), "AO VIVO");
    }

    #[test]
    fn truncates_using_terminal_display_width() {
        assert_eq!(pad_display_width("Lofi 💤 radio", 10), "Lofi 💤...");
        assert_eq!(pad_display_width("Rise", 8), "Rise    ");
    }

    #[test]
    fn marks_pcm_source_as_finished_when_input_disconnects() {
        let (sender, receiver) = channel();
        drop(sender);
        let finished = Arc::new(AtomicBool::new(false));
        let mut source = PcmSource {
            receiver,
            buffer: std::collections::VecDeque::new(),
            channels: 2,
            sample_rate: 44_100,
            is_buffering: true,
            prebuffer_size: 1,
            played_samples: 0,
            peak_sample: 0,
            shared_playback_level: Arc::new(AtomicU32::new(0)),
            shared_finished: Arc::clone(&finished),
        };

        assert_eq!(source.next(), None);
        assert!(finished.load(Ordering::Acquire));
    }

    #[test]
    fn serializes_and_restores_playback_session() {
        let session = SavedSession {
            queue: vec![YtSearchResult {
                id: "video-id".to_string(),
                title: "Track".to_string(),
                duration: Some(240.0),
                channel: Some("Channel".to_string()),
            }],
            current_queue_index: 0,
            elapsed_seconds: 75,
            volume: 0.45,
            loop_mode: LoopMode::Shuffle,
            theme: Theme::Midnight,
            was_paused: false,
        };

        let json = serde_json::to_string(&session).unwrap();
        let restored: SavedSession = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.elapsed_seconds, 75);
        assert_eq!(restored.loop_mode, LoopMode::Shuffle);
        assert_eq!(restored.queue[0].id, "video-id");
    }

    #[test]
    fn extracts_embedded_runtime_payload() {
        let payload_root = unique_temp_dir("termphonic-payload");
        let extract_root = unique_temp_dir("termphonic-runtime");
        let _ = fs::remove_dir_all(&payload_root);
        let _ = fs::remove_dir_all(&extract_root);

        fs::create_dir_all(payload_root.join("libexec")).unwrap();
        fs::write(payload_root.join("libexec/yt-dlp"), b"yt-dlp").unwrap();
        fs::write(payload_root.join("libexec/deno"), b"deno").unwrap();

        let archive = {
            let encoder = GzEncoder::new(Vec::new(), Compression::default());
            let mut builder = tar::Builder::new(encoder);
            builder
                .append_path_with_name(payload_root.join("libexec/yt-dlp"), "libexec/yt-dlp")
                .unwrap();
            builder
                .append_path_with_name(payload_root.join("libexec/deno"), "libexec/deno")
                .unwrap();
            let encoder = builder.into_inner().unwrap();
            encoder.finish().unwrap()
        };

        let package = EmbeddedRuntimePackage { payload: archive };

        extract_embedded_runtime(&package, &extract_root).unwrap();
        assert!(extract_root.join("libexec/yt-dlp").is_file());
        assert!(extract_root.join("libexec/deno").is_file());

        let _ = fs::remove_dir_all(&payload_root);
        let _ = fs::remove_dir_all(&extract_root);
    }
}
