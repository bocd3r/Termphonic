use crate::actions::{
    adjust_volume, play_next, seek_relative, start_song_at_queue_index,
    start_song_at_queue_index_from, stop_player, toggle_pause,
};
use crate::audio::AudioPlayer;
use crate::models::{AppState, Focus, LoopMode, PlaybackState, PlayerEvent, Theme};
use crate::search::search_youtube;
use crate::session::{load_session, save_session};
use crate::ui::draw_ui;
use crossterm::ExecutableCommand;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::error::Error;
use std::time::Duration;
use tokio::sync::mpsc::unbounded_channel;

pub(crate) async fn run() -> Result<(), Box<dyn Error>> {
    enable_raw_mode()?;
    std::io::stdout().execute(EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;

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
    audio_player.set_volume(state.volume);

    let (tx_event, mut rx_event) = unbounded_channel::<PlayerEvent>();
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
        terminal.draw(|f| draw_ui(f, &state))?;

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
                            state.playing_song = Some(crate::models::PlayingSong {
                                id: video_id,
                                title,
                                duration,
                            });
                            audio_player.play(&stream_url, start_seconds);
                            state.elapsed = Duration::from_secs(start_seconds);
                            if start_paused {
                                audio_player.pause();
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

        if matches!(state.playback_state, PlaybackState::Playing) && audio_player.finished() {
            play_next(&mut state, &mut audio_player, tx_event.clone());
        }

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

        let timeout = tick_rate
            .checked_sub(last_tick.elapsed())
            .unwrap_or(Duration::from_secs(0));

        if crossterm::event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Release {
                    match state.focus {
                        Focus::SearchInput => match key.code {
                            KeyCode::Char(c) => state.search_query.push(c),
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
                            KeyCode::Char('q') => break,
                            KeyCode::Char('/') | KeyCode::Char('i') => {
                                state.focus = Focus::SearchInput;
                            }
                            KeyCode::Char(' ') => toggle_pause(&mut state, &audio_player),
                            KeyCode::Char('s') => stop_player(&mut state, &mut audio_player),
                            KeyCode::Char('r') => {
                                state.loop_mode = match state.loop_mode {
                                    LoopMode::Off => LoopMode::Shuffle,
                                    LoopMode::Shuffle => LoopMode::Single,
                                    LoopMode::Single => LoopMode::Off,
                                };
                            }
                            KeyCode::Char('t') => state.theme = state.theme.next(),
                            KeyCode::Char('T') => state.theme = Theme::Default,
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
                            KeyCode::Left => seek_relative(&mut state, &mut audio_player, -10),
                            KeyCode::Right => seek_relative(&mut state, &mut audio_player, 10),
                            _ => {}
                        },
                        Focus::Queue => match key.code {
                            KeyCode::Char('q') => break,
                            KeyCode::Char('/') | KeyCode::Char('i') => {
                                state.focus = Focus::SearchInput;
                            }
                            KeyCode::Char(' ') => toggle_pause(&mut state, &audio_player),
                            KeyCode::Char('s') => stop_player(&mut state, &mut audio_player),
                            KeyCode::Char('r') => {
                                state.loop_mode = match state.loop_mode {
                                    LoopMode::Off => LoopMode::Shuffle,
                                    LoopMode::Shuffle => LoopMode::Single,
                                    LoopMode::Single => LoopMode::Off,
                                };
                            }
                            KeyCode::Char('t') => state.theme = state.theme.next(),
                            KeyCode::Char('T') => state.theme = Theme::Default,
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

                                    if let Some(curr) = state.current_queue_index {
                                        if curr == sel {
                                            stop_player(&mut state, &mut audio_player);
                                            state.current_queue_index = None;
                                        } else if curr > sel {
                                            state.current_queue_index = Some(curr - 1);
                                        }
                                    }

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
                            KeyCode::Left => seek_relative(&mut state, &mut audio_player, -10),
                            KeyCode::Right => seek_relative(&mut state, &mut audio_player, 10),
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
    disable_raw_mode()?;
    std::io::stdout().execute(LeaveAlternateScreen)?;
    Ok(())
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
