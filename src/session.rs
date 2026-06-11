use crate::models::{AppState, PlaybackState, SavedSession};
use std::path::PathBuf;

fn session_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| {
        PathBuf::from(home)
            .join(".local/share/termphonic")
            .join("session.json")
    })
}

pub(crate) fn load_session() -> Option<SavedSession> {
    let contents = std::fs::read_to_string(session_path()?).ok()?;
    serde_json::from_str(&contents).ok()
}

pub(crate) fn save_session(state: &AppState) -> std::io::Result<()> {
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
