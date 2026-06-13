use ratatui::prelude::Color;
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub const SEARCH_PAGE_SIZE: usize = 20;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct YtSearchResult {
    pub id: String,
    pub title: String,
    pub duration: Option<f64>,
    pub channel: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PlayingSong {
    pub id: String,
    pub title: String,
    pub duration: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    SearchInput,
    SearchResults,
    Queue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackState {
    Stopped,
    Loading,
    Playing,
    Paused,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Serialize)]
pub enum LoopMode {
    Off,
    Shuffle,
    Single,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Serialize)]
pub enum Theme {
    Default,
    Midnight,
    HighContrast,
    TerminalGreen,
}

impl Theme {
    pub fn next(self) -> Self {
        match self {
            Theme::Default => Theme::Midnight,
            Theme::Midnight => Theme::HighContrast,
            Theme::HighContrast => Theme::TerminalGreen,
            Theme::TerminalGreen => Theme::Default,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Theme::Default => "Default",
            Theme::Midnight => "Midnight",
            Theme::HighContrast => "High Contrast",
            Theme::TerminalGreen => "Terminal Green",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ThemePalette {
    pub accent: Color,
    pub border: Color,
    pub selection_bg: Color,
    pub selection_fg: Color,
    pub text: Color,
    pub dim: Color,
    pub warning: Color,
    pub good: Color,
    pub header: Color,
    pub highlight: Color,
    pub background: Color,
}

pub fn theme_palette(theme: Theme) -> ThemePalette {
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

#[derive(Debug)]
pub struct AppState {
    pub search_query: String,
    pub search_results: Vec<YtSearchResult>,
    pub selected_result: Option<usize>,
    pub search_page: usize,
    pub search_has_next_page: bool,
    pub search_pending_page: Option<usize>,
    pub queue: Vec<YtSearchResult>,
    pub selected_queue: Option<usize>,
    pub current_queue_index: Option<usize>,
    pub focus: Focus,
    pub is_searching: bool,
    pub playback_state: PlaybackState,
    pub playing_song: Option<PlayingSong>,
    pub elapsed: Duration,
    pub playback_level: f64,
    pub volume: f32,
    pub loop_mode: LoopMode,
    pub theme: Theme,
    pub playback_error: Option<String>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
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
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SavedSession {
    pub queue: Vec<YtSearchResult>,
    pub current_queue_index: usize,
    pub elapsed_seconds: u64,
    pub volume: f32,
    pub loop_mode: LoopMode,
    pub theme: Theme,
    pub was_paused: bool,
}

#[derive(Debug)]
pub enum PlayerEvent {
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
