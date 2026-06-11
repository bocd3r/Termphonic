use crate::audio::AudioPlayer;
use crate::models::{AppState, LoopMode, PlaybackState, PlayerEvent, YtSearchResult};
use crate::runtime::get_yt_dlp_path;
use crate::search::{spawn_stream_url_fetch, summarize_yt_dlp_error};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub(crate) fn toggle_pause(state: &mut AppState, player: &AudioPlayer) {
    if matches!(state.playback_state, PlaybackState::Playing) {
        player.pause();
        state.playback_state = PlaybackState::Paused;
    } else if matches!(state.playback_state, PlaybackState::Paused) {
        player.resume();
        state.playback_state = PlaybackState::Playing;
    }
}

pub(crate) fn stop_player(state: &mut AppState, player: &mut AudioPlayer) {
    player.stop_current_process();
    player.stop_sink();
    state.playback_state = PlaybackState::Stopped;
    state.playing_song = None;
    state.elapsed = Duration::ZERO;
    state.playback_level = 0.0;
    state.playback_error = None;
}

pub(crate) fn adjust_volume(state: &mut AppState, player: &AudioPlayer, diff: f32) {
    state.volume = (state.volume + diff).clamp(0.0, 1.0);
    player.set_volume(state.volume);
}

pub(crate) fn seek_relative(state: &mut AppState, player: &mut AudioPlayer, diff_seconds: i64) {
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

pub(crate) fn start_song_at_queue_index(
    state: &mut AppState,
    idx: usize,
    player: &mut AudioPlayer,
    tx_event: tokio::sync::mpsc::UnboundedSender<PlayerEvent>,
) {
    start_song_at_queue_index_from(state, idx, 0, false, player, tx_event);
}

pub(crate) fn start_song_at_queue_index_from(
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
    state.playing_song = Some(crate::models::PlayingSong {
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

    player.stop_current_process();
    player.stop_sink();

    spawn_stream_url_fetch(
        song.id.clone(),
        song.title.clone(),
        duration,
        start_seconds,
        start_paused,
        tx_event,
    );
}

fn fetch_shuffle_song(
    seed_title: String,
    last_id: String,
    tx_event: tokio::sync::mpsc::UnboundedSender<PlayerEvent>,
) {
    tokio::spawn(async move {
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

        let pick = (seed as usize) % candidates.len();
        let chosen = candidates.remove(pick);
        let _ = tx_event.send(PlayerEvent::AutoplaySongFetched {
            previous_video_id: last_id,
            song: chosen,
        });
    });
}

pub(crate) fn play_next(
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
