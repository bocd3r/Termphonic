use crate::domain::{PlayerEvent, SEARCH_PAGE_SIZE, YtSearchResult};
use crate::infrastructure::runtime::{find_javascript_runtime, get_yt_dlp_path};
use std::process::Command;

pub(crate) fn summarize_yt_dlp_error(stderr: &str) -> String {
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

pub(crate) async fn search_youtube(
    query: &str,
    page: usize,
) -> Result<(Vec<YtSearchResult>, bool), String> {
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

pub(crate) fn spawn_stream_url_fetch(
    video_id: String,
    title: String,
    duration: u64,
    start_seconds: u64,
    start_paused: bool,
    tx_event: tokio::sync::mpsc::UnboundedSender<PlayerEvent>,
) {
    let yt_dlp_bin = get_yt_dlp_path();
    let javascript_runtime = find_javascript_runtime();
    tokio::spawn(async move {
        let mut command = Command::new(yt_dlp_bin);
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
