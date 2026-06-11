use crate::models::{AppState, Focus, LoopMode, PlaybackState, SEARCH_PAGE_SIZE, theme_palette};
use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Borders, List, ListItem, Paragraph};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use unicode_width::UnicodeWidthChar;

pub(crate) fn draw_ui(frame: &mut Frame, state: &AppState) {
    let palette = theme_palette(state.theme);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(10),
            Constraint::Length(2),
        ])
        .split(frame.area());

    let header = Paragraph::new("♫  Termphonic - Music in Your Terminal  ♫")
        .alignment(Alignment::Center)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(palette.border)),
        );
    frame.render_widget(header, chunks[0]);

    let main_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(chunks[1]);

    let search_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(5)])
        .split(main_chunks[0]);

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
                ListItem::new(Line::from(spans))
            })
            .collect();

        frame.render_widget(List::new(items), results_chunks[1]);
    }

    let right_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(9), Constraint::Min(5)])
        .split(main_chunks[1]);

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
                Constraint::Length(4),
                Constraint::Min(0),
                Constraint::Length(1),
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

                ListItem::new(Line::from(vec![
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
                ]))
            })
            .collect();

        let list = List::new(items).block(queue_block);
        frame.render_widget(list, right_chunks[1]);
    }

    let footer = Paragraph::new(vec![
        Line::from(" / Search   ↑/↓ Select   PgUp/PgDn Pages   Enter Play   Tab Focus"),
        Line::from(" Space Play/Pause   ←/→ Seek   +/- Volume   s Stop   r Repeat   q Quit"),
    ])
    .alignment(Alignment::Center)
    .style(Style::default().fg(palette.dim).bg(palette.background));
    frame.render_widget(footer, chunks[2]);
}

pub(crate) fn format_duration(seconds: u64) -> String {
    let hours = seconds / 3600;
    let mins = (seconds % 3600) / 60;
    let secs = seconds % 60;
    if hours > 0 {
        format!("{:02}:{:02}:{:02}", hours, mins, secs)
    } else {
        format!("{:02}:{:02}", mins, secs)
    }
}

pub(crate) fn format_media_duration(duration: Option<f64>) -> String {
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

pub(crate) fn pad_display_width(value: &str, width: usize) -> String {
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

pub(crate) fn truncate_display_width(value: &str, max_width: usize) -> String {
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

pub(crate) fn cava_visualizer(elapsed: Duration, level: f64, width: usize) -> Line<'static> {
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

pub(crate) fn truncate_str(s: &str, max_len: usize) -> String {
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
    fn truncates_emoji_strings_by_display_width() {
        assert_eq!(
            truncate_display_width("AMV - NumB The Pain 🔥", 18),
            "AMV - NumB The ..."
        );
    }
}
