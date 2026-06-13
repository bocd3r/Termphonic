use crate::application::actions::{
    play_next, start_song_at_queue_index, start_song_at_queue_index_from, stop_player, toggle_pause,
};
use crate::domain::{AppState, Focus, LoopMode, PlaybackState, PlayerEvent, YtSearchResult};
use crate::infrastructure::audio::AudioPlayer;
use crate::infrastructure::search::search_youtube;
use crate::infrastructure::session::{load_session, save_session};
use eframe::egui::{
    self, Align, Color32, FontId, Layout, RichText, ScrollArea, Sense, Stroke, Vec2,
};
use std::time::{Duration, Instant};
use tokio::runtime::Runtime;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

const BACKGROUND: Color32 = Color32::from_rgb(18, 18, 18);
const SURFACE: Color32 = Color32::from_rgb(28, 28, 28);
const SURFACE_HOVER: Color32 = Color32::from_rgb(42, 42, 42);
const SIDEBAR: Color32 = Color32::from_rgb(10, 10, 10);
const TEXT: Color32 = Color32::from_rgb(245, 245, 245);
const MUTED: Color32 = Color32::from_rgb(179, 179, 179);
const ACCENT: Color32 = Color32::from_rgb(30, 215, 96);

pub fn run() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Termphonic")
            .with_inner_size([1_200.0, 760.0])
            .with_min_inner_size([900.0, 600.0]),
        centered: true,
        ..Default::default()
    };

    eframe::run_native(
        "Termphonic",
        options,
        Box::new(|creation_context| Ok(Box::new(TermphonicGui::new(creation_context)))),
    )
}

struct TermphonicGui {
    state: AppState,
    player: AudioPlayer,
    runtime: Runtime,
    tx_event: UnboundedSender<PlayerEvent>,
    rx_event: UnboundedReceiver<PlayerEvent>,
    last_session_save: Instant,
    pending_seek: Option<f32>,
}

impl TermphonicGui {
    fn new(creation_context: &eframe::CreationContext<'_>) -> Self {
        configure_style(&creation_context.egui_ctx);

        let runtime = Runtime::new().expect("failed to create async runtime");
        let (tx_event, rx_event) = unbounded_channel();
        let mut player = AudioPlayer::new();
        let mut state = AppState::default();

        if let Some(session) = load_session() {
            state.queue = session.queue;
            state.current_queue_index = Some(session.current_queue_index);
            state.selected_queue = Some(session.current_queue_index);
            state.volume = session.volume.clamp(0.0, 1.0);
            state.loop_mode = session.loop_mode;
            state.theme = session.theme;
            state.focus = Focus::Queue;
            player.set_volume(state.volume);

            if session.current_queue_index < state.queue.len() {
                let _guard = runtime.enter();
                start_song_at_queue_index_from(
                    &mut state,
                    session.current_queue_index,
                    session.elapsed_seconds,
                    session.was_paused,
                    &mut player,
                    tx_event.clone(),
                );
            }
        } else {
            player.set_volume(state.volume);
        }

        Self {
            state,
            player,
            runtime,
            tx_event,
            rx_event,
            last_session_save: Instant::now(),
            pending_seek: None,
        }
    }

    fn start_search(&mut self, page: usize) {
        if self.state.search_query.trim().is_empty() || self.state.is_searching {
            return;
        }

        self.state.is_searching = true;
        self.state.search_pending_page = Some(page);
        let query = self.state.search_query.clone();
        let tx_event = self.tx_event.clone();
        let _guard = self.runtime.enter();
        tokio::spawn(async move {
            let result = search_youtube(&query, page).await;
            let _ = tx_event.send(PlayerEvent::SearchCompleted {
                query,
                page,
                result,
            });
        });
    }

    fn play_queue_index(&mut self, index: usize) {
        let _guard = self.runtime.enter();
        start_song_at_queue_index(
            &mut self.state,
            index,
            &mut self.player,
            self.tx_event.clone(),
        );
    }

    fn play_result(&mut self, result: YtSearchResult) {
        self.state.queue.push(result);
        let index = self.state.queue.len() - 1;
        self.state.selected_queue = Some(index);
        self.play_queue_index(index);
    }

    fn poll_backend(&mut self) {
        while let Ok(event) = self.rx_event.try_recv() {
            match event {
                PlayerEvent::SearchCompleted {
                    query,
                    page,
                    result,
                } => {
                    if query != self.state.search_query {
                        continue;
                    }
                    self.state.is_searching = false;
                    self.state.search_pending_page = None;
                    match result {
                        Ok((results, has_next_page)) => {
                            self.state.search_results = results;
                            self.state.search_page = page;
                            self.state.search_has_next_page = has_next_page;
                            self.state.selected_result =
                                (!self.state.search_results.is_empty()).then_some(0);
                            self.state.playback_error = None;
                        }
                        Err(error) => {
                            self.state.search_results.clear();
                            self.state.selected_result = None;
                            self.state.playback_error = Some(error);
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
                    let is_current = self
                        .state
                        .playing_song
                        .as_ref()
                        .is_some_and(|song| song.id == video_id);
                    if is_current && matches!(self.state.playback_state, PlaybackState::Loading) {
                        self.state.playing_song = Some(crate::domain::PlayingSong {
                            id: video_id,
                            title,
                            duration,
                        });
                        self.player.play(&stream_url, start_seconds);
                        self.state.elapsed = Duration::from_secs(start_seconds);
                        if start_paused {
                            self.player.pause();
                            self.state.playback_state = PlaybackState::Paused;
                        } else {
                            self.state.playback_state = PlaybackState::Playing;
                        }
                        self.state.playback_error = None;
                    }
                }
                PlayerEvent::UrlFetchFailed { video_id, error } => {
                    let is_current = self
                        .state
                        .playing_song
                        .as_ref()
                        .is_some_and(|song| song.id == video_id);
                    if is_current {
                        self.state.playing_song = None;
                        self.state.playback_state = PlaybackState::Stopped;
                        self.state.playback_error = Some(error);
                    }
                }
                PlayerEvent::AutoplaySongFetched {
                    previous_video_id,
                    song,
                } => {
                    let still_waiting = self
                        .state
                        .playing_song
                        .as_ref()
                        .is_some_and(|active| active.id == previous_video_id)
                        && matches!(self.state.playback_state, PlaybackState::Loading);
                    if still_waiting {
                        self.state.queue.push(song);
                        let index = self.state.queue.len() - 1;
                        self.play_queue_index(index);
                    }
                }
                PlayerEvent::AutoplaySongFetchFailed {
                    previous_video_id,
                    error,
                } => {
                    let still_waiting = self
                        .state
                        .playing_song
                        .as_ref()
                        .is_some_and(|active| active.id == previous_video_id)
                        && matches!(self.state.playback_state, PlaybackState::Loading);
                    if still_waiting {
                        self.state.playback_state = PlaybackState::Stopped;
                        self.state.playback_error = Some(error);
                    }
                }
            }
        }

        if matches!(self.state.playback_state, PlaybackState::Playing) && self.player.finished() {
            let _guard = self.runtime.enter();
            play_next(&mut self.state, &mut self.player, self.tx_event.clone());
        }

        if matches!(
            self.state.playback_state,
            PlaybackState::Playing | PlaybackState::Paused
        ) {
            self.state.elapsed = self.player.position();
            if let Some(song) = self.state.playing_song.as_ref() {
                self.state.elapsed = self.state.elapsed.min(Duration::from_secs(song.duration));
            }
            if matches!(self.state.playback_state, PlaybackState::Playing) {
                self.state.playback_level = self.player.level();
            }
        }

        if self.last_session_save.elapsed() >= Duration::from_secs(1) {
            let _ = save_session(&self.state);
            self.last_session_save = Instant::now();
        }
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(18.0);
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(Vec2::splat(34.0), Sense::hover());
            ui.painter().circle_filled(rect.center(), 17.0, ACCENT);
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "T",
                FontId::proportional(20.0),
                Color32::BLACK,
            );
            ui.label(RichText::new("Termphonic").size(21.0).strong().color(TEXT));
        });
        ui.add_space(28.0);

        navigation_button(ui, "⌂", "Início", false);
        navigation_button(ui, "⌕", "Buscar", true);
        navigation_button(ui, "≡", "Sua fila", false);

        ui.add_space(30.0);
        ui.label(
            RichText::new("SUA BIBLIOTECA")
                .size(11.0)
                .strong()
                .color(MUTED),
        );
        ui.add_space(10.0);
        ui.label(RichText::new(format!("{} músicas na fila", self.state.queue.len())).color(MUTED));

        ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
            ui.add_space(18.0);
            ui.label(RichText::new("Termphonic nightly").size(11.0).color(MUTED));
        });
    }

    fn queue_panel(&mut self, ui: &mut egui::Ui) {
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.heading(RichText::new("Fila").color(TEXT));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new(self.state.queue.len().to_string()).color(MUTED));
            });
        });
        ui.add_space(8.0);

        let mut play_index = None;
        let mut remove_index = None;
        ScrollArea::vertical().show(ui, |ui| {
            for (index, song) in self.state.queue.iter().enumerate() {
                let is_current = self.state.current_queue_index == Some(index);
                let fill = if is_current { SURFACE_HOVER } else { SURFACE };
                egui::Frame::new()
                    .fill(fill)
                    .corner_radius(8)
                    .inner_margin(10)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let marker = if is_current { "▶" } else { "♫" };
                            ui.label(RichText::new(marker).size(14.0).color(if is_current {
                                ACCENT
                            } else {
                                MUTED
                            }));
                            ui.vertical(|ui| {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(&song.title)
                                            .color(if is_current { ACCENT } else { TEXT })
                                            .strong(),
                                    )
                                    .truncate(),
                                );
                                ui.label(
                                    RichText::new(song.channel.as_deref().unwrap_or("YouTube"))
                                        .size(12.0)
                                        .color(MUTED),
                                );
                            });
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui
                                    .button(RichText::new("×").size(17.0).color(MUTED))
                                    .on_hover_text("Remover da fila")
                                    .clicked()
                                {
                                    remove_index = Some(index);
                                }
                                if ui
                                    .button(RichText::new("▶").size(13.0).color(TEXT))
                                    .on_hover_text("Reproduzir")
                                    .clicked()
                                {
                                    play_index = Some(index);
                                }
                            });
                        });
                    });
                ui.add_space(5.0);
            }
        });

        if let Some(index) = remove_index {
            self.state.queue.remove(index);
            match self.state.current_queue_index {
                Some(current) if current == index => {
                    stop_player(&mut self.state, &mut self.player);
                    self.state.current_queue_index = None;
                }
                Some(current) if current > index => {
                    self.state.current_queue_index = Some(current - 1);
                }
                _ => {}
            }
        }
        if let Some(index) = play_index {
            self.play_queue_index(index);
        }
    }

    fn content(&mut self, ui: &mut egui::Ui) {
        ui.add_space(18.0);
        let search_response = egui::Frame::new()
            .fill(Color32::WHITE)
            .corner_radius(24)
            .inner_margin(egui::Margin::symmetric(16, 7))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("⌕").size(22.0).color(Color32::BLACK));
                    ui.add(
                        egui::TextEdit::singleline(&mut self.state.search_query)
                            .hint_text("O que você quer ouvir?")
                            .desired_width(f32::INFINITY)
                            .text_color(Color32::BLACK),
                    )
                })
                .inner
            })
            .inner;

        if search_response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
            self.start_search(0);
        }

        ui.add_space(30.0);
        ui.horizontal(|ui| {
            ui.heading(RichText::new("Resultados").size(28.0).color(TEXT));
            if self.state.is_searching {
                ui.spinner();
                ui.label(RichText::new("Buscando...").color(MUTED));
            }
        });
        ui.add_space(12.0);

        if let Some(error) = self.state.playback_error.as_deref() {
            ui.colored_label(Color32::from_rgb(248, 113, 113), error);
            ui.add_space(10.0);
        }

        let mut selected_result = None;
        ScrollArea::vertical().show(ui, |ui| {
            if self.state.search_results.is_empty() && !self.state.is_searching {
                ui.add_space(60.0);
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new("♫").size(52.0).color(ACCENT));
                    ui.add_space(10.0);
                    ui.label(
                        RichText::new("Busque uma música, artista ou playlist")
                            .size(20.0)
                            .color(TEXT),
                    );
                    ui.label(
                        RichText::new("Os resultados do YouTube aparecerão aqui.").color(MUTED),
                    );
                });
            }

            for (index, result) in self.state.search_results.iter().enumerate() {
                let response = egui::Frame::new()
                    .fill(SURFACE)
                    .corner_radius(8)
                    .inner_margin(12)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let (cover, _) =
                                ui.allocate_exact_size(Vec2::splat(48.0), Sense::hover());
                            ui.painter().rect_filled(cover, 6.0, SURFACE_HOVER);
                            ui.painter().text(
                                cover.center(),
                                egui::Align2::CENTER_CENTER,
                                "♫",
                                FontId::proportional(22.0),
                                ACCENT,
                            );
                            ui.add_space(4.0);
                            ui.vertical(|ui| {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(&result.title)
                                            .size(15.0)
                                            .strong()
                                            .color(TEXT),
                                    )
                                    .truncate(),
                                );
                                ui.label(
                                    RichText::new(result.channel.as_deref().unwrap_or("YouTube"))
                                        .size(13.0)
                                        .color(MUTED),
                                );
                            });
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui
                                    .add(
                                        egui::Button::new(
                                            RichText::new("▶")
                                                .size(15.0)
                                                .strong()
                                                .color(Color32::BLACK),
                                        )
                                        .fill(ACCENT)
                                        .corner_radius(22)
                                        .min_size(Vec2::splat(42.0)),
                                    )
                                    .clicked()
                                {
                                    selected_result = Some(index);
                                }
                                ui.label(
                                    RichText::new(format_media_duration(result.duration))
                                        .color(MUTED),
                                );
                            });
                        });
                    })
                    .response
                    .interact(Sense::click());
                if response.double_clicked() {
                    selected_result = Some(index);
                }
                ui.add_space(6.0);
            }
        });

        if let Some(index) = selected_result
            && let Some(result) = self.state.search_results.get(index).cloned()
        {
            self.play_result(result);
        }

        ui.with_layout(Layout::bottom_up(Align::Center), |ui| {
            ui.horizontal(|ui| {
                let previous_enabled = self.state.search_page > 0 && !self.state.is_searching;
                if ui
                    .add_enabled(previous_enabled, egui::Button::new("← Anterior"))
                    .clicked()
                {
                    self.start_search(self.state.search_page - 1);
                }
                ui.label(
                    RichText::new(format!("Página {}", self.state.search_page + 1)).color(MUTED),
                );
                let next_enabled = self.state.search_has_next_page && !self.state.is_searching;
                if ui
                    .add_enabled(next_enabled, egui::Button::new("Próxima →"))
                    .clicked()
                {
                    self.start_search(self.state.search_page + 1);
                }
            });
        });
    }

    fn player_bar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        ui.columns(3, |columns| {
            columns[0].horizontal(|ui| {
                let (cover, _) = ui.allocate_exact_size(Vec2::splat(58.0), Sense::hover());
                ui.painter().rect_filled(cover, 7.0, SURFACE_HOVER);
                ui.painter().text(
                    cover.center(),
                    egui::Align2::CENTER_CENTER,
                    "♫",
                    FontId::proportional(25.0),
                    ACCENT,
                );
                ui.vertical(|ui| {
                    let title = self
                        .state
                        .playing_song
                        .as_ref()
                        .map(|song| song.title.as_str())
                        .unwrap_or("Nenhuma música");
                    ui.add(egui::Label::new(RichText::new(title).strong().color(TEXT)).truncate());
                    ui.label(status_label(self.state.playback_state));
                });
            });

            columns[1].vertical_centered(|ui| {
                ui.horizontal(|ui| {
                    ui.add_space(ui.available_width() * 0.5 - 100.0);
                    if ui
                        .button(RichText::new(loop_icon(self.state.loop_mode)).color(
                            if self.state.loop_mode == LoopMode::Off {
                                MUTED
                            } else {
                                ACCENT
                            },
                        ))
                        .clicked()
                    {
                        self.state.loop_mode = next_loop_mode(self.state.loop_mode);
                    }
                    if ui.button(RichText::new("−10").color(MUTED)).clicked() {
                        self.seek_to(self.state.elapsed.as_secs().saturating_sub(10));
                    }
                    let play_icon = match self.state.playback_state {
                        PlaybackState::Playing => "Ⅱ",
                        PlaybackState::Loading => "…",
                        PlaybackState::Stopped | PlaybackState::Paused => "▶",
                    };
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new(play_icon)
                                    .size(18.0)
                                    .strong()
                                    .color(Color32::BLACK),
                            )
                            .fill(Color32::WHITE)
                            .corner_radius(22)
                            .min_size(Vec2::splat(42.0)),
                        )
                        .clicked()
                    {
                        toggle_pause(&mut self.state, &self.player);
                    }
                    if ui.button(RichText::new("+10").color(MUTED)).clicked() {
                        self.seek_to(self.state.elapsed.as_secs().saturating_add(10));
                    }
                    if ui.button(RichText::new("■").color(MUTED)).clicked() {
                        stop_player(&mut self.state, &mut self.player);
                    }
                });

                let total = self
                    .state
                    .playing_song
                    .as_ref()
                    .map(|song| song.duration)
                    .unwrap_or(0);
                let mut progress = self
                    .pending_seek
                    .unwrap_or(self.state.elapsed.as_secs_f32())
                    .min(total as f32);
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format_duration(progress as u64))
                            .size(11.0)
                            .color(MUTED),
                    );
                    let response = ui.add(
                        egui::Slider::new(&mut progress, 0.0..=total.max(1) as f32)
                            .show_value(false),
                    );
                    if response.changed() {
                        self.pending_seek = Some(progress);
                    }
                    if response.drag_stopped() {
                        self.pending_seek = None;
                        self.seek_to(progress as u64);
                    }
                    ui.label(
                        RichText::new(format_duration(total))
                            .size(11.0)
                            .color(MUTED),
                    );
                });
            });

            columns[2].with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(14.0);
                let mut volume = self.state.volume;
                if ui
                    .add(
                        egui::Slider::new(&mut volume, 0.0..=1.0)
                            .show_value(false)
                            .max_decimals(2),
                    )
                    .changed()
                {
                    self.state.volume = volume;
                    self.player.set_volume(volume);
                }
                ui.label(RichText::new("VOL").size(11.0).color(MUTED));
            });
        });
    }

    fn seek_to(&mut self, seconds: u64) {
        let Some(song) = self.state.playing_song.as_ref() else {
            return;
        };
        let target = seconds.min(song.duration);
        self.player.seek(target);
        self.state.elapsed = Duration::from_secs(target);
        self.state.playback_state = PlaybackState::Playing;
    }
}

impl eframe::App for TermphonicGui {
    fn logic(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_backend();
        context.request_repaint_after(Duration::from_millis(100));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::bottom("player")
            .exact_size(105.0)
            .frame(
                egui::Frame::new()
                    .fill(SURFACE)
                    .inner_margin(egui::Margin::symmetric(16, 8))
                    .stroke(Stroke::new(1.0, Color32::from_rgb(48, 48, 48))),
            )
            .show_inside(ui, |ui| self.player_bar(ui));

        egui::Panel::left("navigation")
            .exact_size(210.0)
            .frame(
                egui::Frame::new()
                    .fill(SIDEBAR)
                    .inner_margin(egui::Margin::same(14)),
            )
            .show_inside(ui, |ui| self.sidebar(ui));

        egui::Panel::right("queue")
            .default_size(310.0)
            .size_range(260.0..=380.0)
            .frame(
                egui::Frame::new()
                    .fill(Color32::from_rgb(16, 16, 16))
                    .inner_margin(egui::Margin::same(14)),
            )
            .show_inside(ui, |ui| self.queue_panel(ui));

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(BACKGROUND)
                    .inner_margin(egui::Margin::symmetric(24, 12)),
            )
            .show_inside(ui, |ui| self.content(ui));
    }
}

impl Drop for TermphonicGui {
    fn drop(&mut self) {
        let _ = save_session(&self.state);
        self.player.stop_current_process();
    }
}

fn configure_style(context: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = BACKGROUND;
    visuals.window_fill = SURFACE;
    visuals.extreme_bg_color = Color32::from_rgb(36, 36, 36);
    visuals.selection.bg_fill = ACCENT;
    visuals.selection.stroke = Stroke::new(1.0, Color32::BLACK);
    visuals.widgets.inactive.bg_fill = SURFACE;
    visuals.widgets.hovered.bg_fill = SURFACE_HOVER;
    visuals.widgets.active.bg_fill = Color32::from_rgb(56, 56, 56);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    context.set_visuals(visuals);

    let mut style = (*context.global_style()).clone();
    style.spacing.item_spacing = Vec2::new(10.0, 8.0);
    style.spacing.button_padding = Vec2::new(12.0, 7.0);
    context.set_global_style(style);
}

fn navigation_button(ui: &mut egui::Ui, icon: &str, label: &str, selected: bool) {
    let color = if selected { TEXT } else { MUTED };
    let fill = if selected {
        Color32::from_rgb(36, 36, 36)
    } else {
        Color32::TRANSPARENT
    };
    let response = ui.add(
        egui::Button::new(
            RichText::new(format!("{icon}   {label}"))
                .size(15.0)
                .strong()
                .color(color),
        )
        .fill(fill)
        .corner_radius(7)
        .min_size(Vec2::new(ui.available_width(), 42.0)),
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand);
}

fn status_label(state: PlaybackState) -> RichText {
    let (label, color) = match state {
        PlaybackState::Stopped => ("Parado", MUTED),
        PlaybackState::Loading => ("Preparando áudio...", ACCENT),
        PlaybackState::Playing => ("Reproduzindo", MUTED),
        PlaybackState::Paused => ("Pausado", MUTED),
    };
    RichText::new(label).size(12.0).color(color)
}

fn next_loop_mode(mode: LoopMode) -> LoopMode {
    match mode {
        LoopMode::Off => LoopMode::Shuffle,
        LoopMode::Shuffle => LoopMode::Single,
        LoopMode::Single => LoopMode::Off,
    }
}

fn loop_icon(mode: LoopMode) -> &'static str {
    match mode {
        LoopMode::Off => "⇄",
        LoopMode::Shuffle => "⤨",
        LoopMode::Single => "↻1",
    }
}

fn format_duration(seconds: u64) -> String {
    let hours = seconds / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let seconds = seconds % 60;
    if hours > 0 {
        format!("{hours:02}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

fn format_media_duration(duration: Option<f64>) -> String {
    duration
        .filter(|seconds| *seconds > 0.0)
        .map(|seconds| format_duration(seconds as u64))
        .unwrap_or_else(|| "AO VIVO".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cycles_loop_modes() {
        assert_eq!(next_loop_mode(LoopMode::Off), LoopMode::Shuffle);
        assert_eq!(next_loop_mode(LoopMode::Shuffle), LoopMode::Single);
        assert_eq!(next_loop_mode(LoopMode::Single), LoopMode::Off);
    }

    #[test]
    fn formats_gui_duration() {
        assert_eq!(format_duration(71), "01:11");
        assert_eq!(format_duration(3_661), "01:01:01");
    }

    #[test]
    fn keeps_search_page_size_consistent() {
        assert_eq!(crate::domain::SEARCH_PAGE_SIZE, 20);
    }
}
