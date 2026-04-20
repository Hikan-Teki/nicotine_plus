use crate::config::{CharacterEntry, CharacterHotkey, Config, DisplayMode, LiveSettings};
use crate::tray::{self, Tray, TrayEvent};
use eframe::egui;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;

/// After any config edit we wait this long with no further edits before
/// flushing to disk. 300ms is the sweet spot — saves feel instant when
/// the user taps a checkbox or clicks a binding, but slider drags and
/// text input coalesce into a single write rather than hammering disk
/// on every pixel / keystroke.
const AUTOSAVE_DEBOUNCE: Duration = Duration::from_millis(300);

/// Which config field is currently capturing a live keypress from the
/// panel. Only one can capture at a time; `None` means no capture.
/// `Character` carries the character name so per-character hotkey
/// bindings survive reorders of the characters list.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CaptureTarget {
    ForwardKey,
    BackwardKey,
    ModifierKey,
    Character(String),
}

/// Result of a completed capture poll. Per-character bindings consume
/// `Main` (a non-modifier key plus whichever modifiers were held at the
/// moment of press); the global `modifier_key` slot consumes `Modifier`.
#[derive(Debug, Clone, Copy)]
enum CapturedKey {
    Main {
        vk: u16,
        ctrl: bool,
        shift: bool,
        alt: bool,
    },
    Modifier(u16),
}

/// Per-frame held state of every VK, used to detect rising edges while
/// capturing. Initialized on capture start so modifiers already held
/// (e.g. user still holding Ctrl after pressing the bind button) don't
/// immediately fire as a "new press."
struct CaptureBuffer {
    prev: [bool; 256],
    primed: bool,
}

impl CaptureBuffer {
    fn new() -> Self {
        Self {
            prev: [false; 256],
            primed: false,
        }
    }

    fn reset(&mut self) {
        self.prev = [false; 256];
        self.primed = false;
    }
}

/// Inari Syndicate palette, matching hikanteki.com.
/// Deep navy canvas, vivid orange accent, off-white text — dark theme.
const INARI_BG_PRIMARY: egui::Color32 = egui::Color32::from_rgb(0x0A, 0x0E, 0x1A);
const INARI_BG_SECONDARY: egui::Color32 = egui::Color32::from_rgb(0x0F, 0x15, 0x25);
const INARI_BG_ELEVATED: egui::Color32 = egui::Color32::from_rgb(0x14, 0x1B, 0x2E);
const INARI_ORANGE: egui::Color32 = egui::Color32::from_rgb(0xFF, 0x77, 0x00);
const INARI_GOLD: egui::Color32 = egui::Color32::from_rgb(0xFF, 0xCC, 0x00);
const INARI_TEXT: egui::Color32 = egui::Color32::from_rgb(0xF0, 0xF0, 0xF5);
const INARI_TEXT_MUTED: egui::Color32 = egui::Color32::from_rgb(0x88, 0x92, 0xA8);
const INARI_BORDER: egui::Color32 = egui::Color32::from_rgb(0x2A, 0x32, 0x48);
/// Used only for the "GÜNCEL SÜRÜM" footer badge — the teal from the
/// hikanteki.com utility palette reads cleanly against the dark navy.
const INARI_TEAL: egui::Color32 = egui::Color32::from_rgb(0x4E, 0xCC, 0xA3);

pub struct ConfigPanel {
    config: Config,
    /// Buffer for "add character" text input.
    new_character_buffer: String,
    /// Shared settings watched by the preview manager for live updates
    /// (resize windows while sliders are being dragged).
    live: Arc<Mutex<LiveSettings>>,
    /// When Some(...), the panel is listening for the next keypress /
    /// side-mouse click to bind it to the given field.
    capturing: Option<CaptureTarget>,
    /// Capture state from the previous frame. Used to detect edge
    /// transitions so we can pause the daemon's global hotkeys when
    /// the user enters capture mode (otherwise RegisterHotKey eats the
    /// key before egui can see it) and resume afterwards.
    last_capturing: Option<CaptureTarget>,
    /// Per-VK previous-frame state for the Win32 capture path.
    capture_buf: CaptureBuffer,
    /// Timestamp of the last config edit. When set and `AUTOSAVE_DEBOUNCE`
    /// has elapsed with no further edits, the panel flushes the config
    /// to disk. Kept as an Option so we can skip saving when nothing
    /// has changed since the last flush.
    last_change: Option<Instant>,
    /// Last inner-size we asked the OS viewport to be. Tracked so we
    /// only send a resize command when the measured content height
    /// actually changes — re-sending the same size every frame wastes
    /// work and can cause visual jitter.
    last_applied_height: f32,
    /// Live tray icon. Spawned lazily the first time the user turns on
    /// `minimize_to_tray_on_close` so users who never opt in never see
    /// an extra icon in their notification area. `None` until that
    /// happens (or if the tray failed to spawn).
    tray: Option<Tray>,
    /// Set when the tray menu fires `TrayEvent::Exit`. Checked after
    /// we've drained the event channel so the exit path overrides any
    /// concurrent hide-to-tray request in the same frame.
    exit_requested: bool,
}

impl ConfigPanel {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        config: Config,
        live: Arc<Mutex<LiveSettings>>,
    ) -> Self {
        // Inari brand fonts — matching hikanteki.com:
        //   Inter → body / proportional text
        //   Exo 2 → display / logo face
        // Both are variable TTFs with full latin-ext coverage, so Turkish
        // diacritics (ı, İ, ş, ğ, ç, ü, ö) render correctly.
        let mut fonts = egui::FontDefinitions::default();
        fonts.font_data.insert(
            "inter".to_owned(),
            egui::FontData::from_static(include_bytes!("../assets/fonts/Inter-Variable.ttf")),
        );
        fonts.font_data.insert(
            "exo2".to_owned(),
            egui::FontData::from_static(include_bytes!("../assets/fonts/Exo2-Variable.ttf")),
        );
        fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .insert(0, "inter".to_owned());
        fonts
            .families
            .entry(egui::FontFamily::Name("logo".into()))
            .or_default()
            .push("exo2".to_owned());
        cc.egui_ctx.set_fonts(fonts);

        // Dark theme base: navy canvas, orange accent. egui's default
        // dark visuals are too neutral for a branded app, so every
        // interactive state gets an orange progression (muted → bright →
        // solid) to signal hover / active clearly.
        cc.egui_ctx.set_visuals(build_visuals());

        // Spawn the tray up front when the user has already opted in
        // (via a persisted config). Spawn-on-toggle handles the first-
        // time-enable path from within update().
        let tray = if config.minimize_to_tray_on_close {
            match tray::spawn() {
                Ok(t) => Some(t),
                Err(e) => {
                    eprintln!("sistem tepsisi başlatılamadı: {}", e);
                    None
                }
            }
        } else {
            None
        };

        Self {
            config,
            new_character_buffer: String::new(),
            live,
            capturing: None,
            last_capturing: None,
            capture_buf: CaptureBuffer::new(),
            last_change: None,
            last_applied_height: 0.0,
            tray,
            exit_requested: false,
        }
    }

    /// Mark the config as edited. The next `update()` tick checks this
    /// timestamp and flushes to disk once the user has been idle for
    /// `AUTOSAVE_DEBOUNCE`.
    fn touch(&mut self) {
        self.last_change = Some(Instant::now());
    }
}

fn build_visuals() -> egui::Visuals {
    let mut v = egui::Visuals::dark();

    // Page canvas: deep navy. `noninteractive.bg_fill` is used for panel
    // backgrounds; paired with INARI_TEXT so labels read cleanly.
    v.widgets.noninteractive.bg_fill = INARI_BG_PRIMARY;
    v.widgets.noninteractive.weak_bg_fill = INARI_BG_PRIMARY;
    v.widgets.noninteractive.fg_stroke.color = INARI_TEXT;
    v.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, INARI_BORDER);

    // Idle widget (button / checkbox / slider handle): one notch lighter
    // than the page bg, with a subtle navy border.
    v.widgets.inactive.bg_fill = INARI_BG_SECONDARY;
    v.widgets.inactive.weak_bg_fill = INARI_BG_SECONDARY;
    v.widgets.inactive.fg_stroke.color = INARI_TEXT;
    v.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, INARI_BORDER);

    // Hover: elevated navy + orange border so the cursor's target is
    // unmistakable.
    v.widgets.hovered.bg_fill = INARI_BG_ELEVATED;
    v.widgets.hovered.weak_bg_fill = INARI_BG_ELEVATED;
    v.widgets.hovered.fg_stroke.color = INARI_TEXT;
    v.widgets.hovered.bg_stroke = egui::Stroke::new(1.5, INARI_ORANGE);

    // Pressed / active: solid orange fill with dark text for contrast.
    v.widgets.active.bg_fill = INARI_ORANGE;
    v.widgets.active.weak_bg_fill = INARI_ORANGE;
    v.widgets.active.fg_stroke.color = INARI_BG_PRIMARY;
    v.widgets.active.bg_stroke = egui::Stroke::new(1.5, INARI_ORANGE);

    // Open popup / focused text edit — elevated bg, orange outline.
    v.widgets.open.bg_fill = INARI_BG_ELEVATED;
    v.widgets.open.weak_bg_fill = INARI_BG_ELEVATED;
    v.widgets.open.fg_stroke.color = INARI_TEXT;
    v.widgets.open.bg_stroke = egui::Stroke::new(1.5, INARI_ORANGE);

    // Text selection highlight — orange glow (semi-transparent).
    v.selection.bg_fill = INARI_ORANGE.gamma_multiply(0.35);
    v.selection.stroke.color = INARI_ORANGE;

    // Hyperlinks / accents.
    v.hyperlink_color = INARI_ORANGE;
    v.override_text_color = Some(INARI_TEXT);

    // Window / panel surfaces the core paints for us.
    v.panel_fill = INARI_BG_PRIMARY;
    v.window_fill = INARI_BG_SECONDARY;
    v.window_stroke = egui::Stroke::new(1.0, INARI_BORDER);
    v.extreme_bg_color = INARI_BG_SECONDARY;

    v
}

impl eframe::App for ConfigPanel {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // ---- Tray: lazy spawn + event drain ----
        // The tray is kept alive for the rest of the session once
        // spawned, even if the user toggles `minimize_to_tray_on_close`
        // off later. Shell_NotifyIcon's global Sender uses a OnceLock,
        // so re-spawn isn't supported in-process; keeping the icon live
        // also doubles as a "Inari is running" indicator.
        if self.tray.is_none() && self.config.minimize_to_tray_on_close {
            match tray::spawn() {
                Ok(t) => self.tray = Some(t),
                Err(e) => eprintln!("sistem tepsisi başlatılamadı: {}", e),
            }
        }
        if let Some(tray) = &self.tray {
            while let Some(event) = tray.try_recv() {
                match event {
                    TrayEvent::Show => {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                    }
                    TrayEvent::Exit => {
                        self.exit_requested = true;
                    }
                }
            }
        }

        // ---- Close handling ----
        // When the user clicks the X:
        //   * tray-exit just happened → let the close proceed
        //   * setting is on + tray is live → cancel the close and hide
        //     the window (stays accessible through the tray icon)
        //   * otherwise → default behavior, app exits
        if ctx.input(|i| i.viewport().close_requested()) {
            if self.exit_requested {
                // Fall through — viewport closes normally.
            } else if self.config.minimize_to_tray_on_close && self.tray.is_some() {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            }
        }
        if self.exit_requested {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        // ---- Capture mode: listen for the next keypress ----
        // We poll Win32 GetAsyncKeyState rather than egui's key stream
        // because egui doesn't distinguish top-row digits from numpad
        // digits (both fire as `Key::Num1`) and can't expose all the VK
        // codes we need for bindings like Ctrl+Num 1.
        if let Some(target) = self.capturing.clone() {
            match poll_capture(&mut self.capture_buf) {
                Some(CapturedKey::Main {
                    vk,
                    ctrl,
                    shift,
                    alt,
                }) => match &target {
                    CaptureTarget::ForwardKey => {
                        self.config.forward_key = vk;
                        self.capturing = None;
                        self.touch();
                    }
                    CaptureTarget::BackwardKey => {
                        self.config.backward_key = vk;
                        self.capturing = None;
                        self.touch();
                    }
                    CaptureTarget::Character(name) => {
                        self.config.character_hotkeys.insert(
                            name.clone(),
                            CharacterHotkey {
                                vk,
                                ctrl,
                                shift,
                                alt,
                            },
                        );
                        self.capturing = None;
                        self.touch();
                    }
                    // User was aiming to bind a modifier but pressed a
                    // main key instead — ignore; keep capturing so the
                    // next Shift/Ctrl/Alt press lands.
                    CaptureTarget::ModifierKey => {}
                },
                Some(CapturedKey::Modifier(vk)) => {
                    if let CaptureTarget::ModifierKey = &target {
                        self.config.modifier_key = Some(vk);
                        self.capturing = None;
                        self.touch();
                    }
                    // For ForwardKey/BackwardKey/Character targets,
                    // hold the modifier silently — it'll combine with
                    // the next main-key press (above).
                }
                None => {}
            }

            // Escape cancels capture without binding. Checked via egui
            // (the Win32 path skips 0x1B intentionally).
            if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.capturing = None;
            }

            // Keep requesting frames so a key press lands even when the
            // user isn't hovering over the panel.
            ctx.request_repaint();
        }

        // Edge-detect capture start/end so we can pause the daemon's
        // global hotkeys — otherwise RegisterHotKey swallows F10/F11
        // before egui sees them, and binding appears broken.
        if self.last_capturing != self.capturing {
            if self.last_capturing.is_none() && self.capturing.is_some() {
                // Reset per-VK edge-detect state so modifiers already
                // held when the user entered capture mode don't register
                // as a fresh press on the first poll.
                self.capture_buf.reset();
                crate::windows_input::pause_hotkeys();
            } else if self.last_capturing.is_some() && self.capturing.is_none() {
                // Flush config.toml synchronously before resuming so
                // the listener's Config::load() sees the new binding.
                if self.last_change.is_some() {
                    let _ = self.config.save();
                    self.last_change = None;
                }
                crate::windows_input::resume_hotkeys();
            }
            self.last_capturing = self.capturing.clone();
        }

        // ---- Branded header strip ----
        // Dark navy background with an orange Exo 2 wordmark. A thin
        // orange bottom stroke marks the header/body boundary.
        egui::TopBottomPanel::top("inari_header")
            .exact_height(80.0)
            .frame(
                egui::Frame::none()
                    .fill(INARI_BG_SECONDARY)
                    .stroke(egui::Stroke::new(1.0, INARI_ORANGE))
                    .inner_margin(egui::Margin::symmetric(0.0, 8.0)),
            )
            .show(ctx, |ui| {
                ui.with_layout(
                    egui::Layout::centered_and_justified(egui::Direction::TopDown),
                    |ui| {
                        ui.vertical_centered(|ui| {
                            ui.label(
                                egui::RichText::new("INARI")
                                    .family(egui::FontFamily::Name("logo".into()))
                                    .size(40.0)
                                    .color(INARI_ORANGE)
                                    .strong(),
                            );
                            ui.label(
                                egui::RichText::new("Inari Syndicate")
                                    .family(egui::FontFamily::Name("logo".into()))
                                    .size(12.0)
                                    .color(INARI_GOLD),
                            );
                        });
                    },
                );
            });

        // ---- Branded footer with external links ----
        egui::TopBottomPanel::bottom("inari_footer")
            .exact_height(40.0)
            .frame(
                egui::Frame::none()
                    .fill(INARI_BG_SECONDARY)
                    .inner_margin(egui::Margin::symmetric(16.0, 8.0))
                    .stroke(egui::Stroke::new(1.0, INARI_BORDER)),
            )
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    // Explicit .color() on both RichText blocks — egui's
                    // hyperlink color doesn't always propagate through
                    // .strong() in 0.29, leaving the text near-invisible
                    // against the dark navy background.
                    ui.hyperlink_to(
                        egui::RichText::new("GITHUB").strong().color(INARI_ORANGE),
                        "https://github.com/Hikan-Teki/nicotine_plus",
                    );

                    // Right-aligned update badge. `right_to_left`
                    // consumes the remaining horizontal space and lays
                    // out items from the right edge so this lands in
                    // the bottom-right corner regardless of panel width.
                    // Three states:
                    //   - `Outdated` → red "NEW VERSION AVAILABLE" link
                    //     to the GitHub release page
                    //   - `UpToDate` → green "LATEST VERSION" label
                    //   - `None` (check pending or failed) → render
                    //     nothing so we don't show stale claims
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        match crate::version_check::get_update_status() {
                            Some(crate::version_check::UpdateStatus::Outdated { version, url }) => {
                                ui.hyperlink_to(
                                    egui::RichText::new(format!(
                                        "YENİ SÜRÜM MEVCUT (v{})",
                                        version
                                    ))
                                    .strong()
                                    .color(INARI_ORANGE),
                                    url,
                                );
                            }
                            Some(crate::version_check::UpdateStatus::UpToDate) => {
                                ui.label(
                                    egui::RichText::new("GÜNCEL SÜRÜM")
                                        .strong()
                                        .color(INARI_TEAL),
                                );
                            }
                            None => {}
                        }
                    });
                });
            });

        // ---- Body ----
        // Capture the central panel's content height from inside its
        // builder so we can size the window to it — `ctx.used_size()`
        // only reports what was *allocated* to the CentralPanel, which
        // is bounded by header+footer, so tall content would clip and
        // paint over the footer without this measurement.
        const HEADER_HEIGHT: f32 = 72.0;
        const FOOTER_HEIGHT: f32 = 40.0;
        const CENTRAL_V_MARGIN: f32 = 12.0;
        let mut central_content_height = 0.0f32;
        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(INARI_BG_PRIMARY)
                    .inner_margin(egui::Margin::symmetric(16.0, CENTRAL_V_MARGIN)),
            )
            .show(ctx, |ui| {
                self.draw_display_mode_section(ui);
                ui.add_space(20.0);
                self.draw_characters_section(ui);
                ui.add_space(20.0);
                self.draw_hotkeys_section(ui);
                ui.add_space(20.0);
                self.draw_previews_section(ui);
                central_content_height = ui.min_rect().height();
            });

        // ---- Auto-size the window to fit the rendered content. ----
        let target_height =
            (HEADER_HEIGHT + FOOTER_HEIGHT + CENTRAL_V_MARGIN * 2.0 + central_content_height)
                .round()
                .clamp(300.0, 1500.0);
        if (target_height - self.last_applied_height).abs() > 1.0 {
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
                600.0,
                target_height,
            )));
            self.last_applied_height = target_height;
        }

        // ---- Debounced auto-save ----
        // After the user has been idle for AUTOSAVE_DEBOUNCE, flush the
        // current config to disk. If they're actively editing (every
        // touch() resets last_change to "now"), we keep deferring; as
        // soon as they stop, the next tick saves. request_repaint_after
        // ensures we get a frame to actually perform the save even
        // when there's no other input.
        if let Some(changed_at) = self.last_change {
            let elapsed = changed_at.elapsed();
            if elapsed >= AUTOSAVE_DEBOUNCE {
                if let Err(e) = self.config.save() {
                    eprintln!("config otomatik kayıt hatası: {}", e);
                }
                self.last_change = None;
            } else {
                ctx.request_repaint_after(AUTOSAVE_DEBOUNCE - elapsed);
            }
        }
    }
}

impl ConfigPanel {
    fn draw_section_header(ui: &mut egui::Ui, label: &str) {
        ui.label(
            egui::RichText::new(label)
                .size(16.0)
                .strong()
                .color(INARI_ORANGE),
        );
        ui.separator();
    }

    fn draw_display_mode_section(&mut self, ui: &mut egui::Ui) {
        Self::draw_section_header(ui, "Görünüm Modu");
        ui.label(
            egui::RichText::new(
                "Inari, çalışan istemcilerinizi ekranda nasıl göstersin. \
                 Önizleme pencereleri her istemciyi canlı yansıtır; liste \
                 görünümü ise her zaman en üstte duran, adları içeren \
                 kompakt bir penceredir.",
            )
            .size(11.0)
            .color(INARI_TEXT_MUTED),
        );
        ui.add_space(4.0);

        let prev = self.config.display_mode;
        ui.horizontal(|ui| {
            ui.radio_value(
                &mut self.config.display_mode,
                DisplayMode::Previews,
                "Önizleme pencereleri",
            );
            ui.add_space(12.0);
            ui.radio_value(
                &mut self.config.display_mode,
                DisplayMode::List,
                "İstemci listesi",
            );
        });
        if self.config.display_mode != prev {
            self.touch();
            // Push immediately to the shared LiveSettings so the preview
            // manager swaps modes within its next reconcile tick.
            self.live.lock().unwrap().display_mode = self.config.display_mode;
        }

        ui.add_space(6.0);
        let prev_lock = self.config.positions_locked;
        ui.checkbox(
            &mut self.config.positions_locked,
            "Konumları kilitle (önizleme ve listede sürükleme devre dışı)",
        );
        if self.config.positions_locked != prev_lock {
            self.touch();
            // Live-apply so the running preview manager stops honoring
            // drags immediately — no save + restart needed.
            self.live.lock().unwrap().positions_locked = self.config.positions_locked;
        }

        ui.add_space(4.0);
        let prev_tray = self.config.minimize_to_tray_on_close;
        ui.checkbox(
            &mut self.config.minimize_to_tray_on_close,
            "Kapatma (X) tuşuna basınca sistem tepsisine küçült",
        );
        if self.config.minimize_to_tray_on_close != prev_tray {
            self.touch();
        }
        ui.label(
            egui::RichText::new(
                "Tepsi ikonuna sol tıkla pencereyi geri getirir; sağ tıkla \
                 açılan menüden \"Çıkış\" ile Inari'yi tamamen kapatabilirsiniz.",
            )
            .size(10.0)
            .color(INARI_TEXT_MUTED),
        );
    }

    fn draw_characters_section(&mut self, ui: &mut egui::Ui) {
        Self::draw_section_header(ui, "Geçiş Sırası");
        ui.label(
            egui::RichText::new(
                "Karakterler burada gösterilen sırada döner. \"Döngüde\" kutucuğunu kapatırsanız \
                 karakter listede ve kısayolunda görünmeye devam eder, ama ileri/geri döngüde \
                 atlanır (örneğin bir scout için). İsimler, EVE'in pencere başlığıyla birebir \
                 aynı olmalı (\"EVE - \" kısmından sonraki ad).",
            )
            .size(11.0)
            .color(INARI_TEXT_MUTED),
        );
        ui.add_space(6.0);

        let mut swap: Option<(usize, usize)> = None;
        let mut remove: Option<usize> = None;
        // Track edits locally; `self.touch()` can't be called from
        // inside the closures below because the closures borrow self.
        let mut dirty = false;

        let len = self.config.characters.len();
        // Cap the visible list height so large rosters don't blow the
        // window past the screen — each entry is two rows, and ~6–7
        // entries fit in ~440px. Anything beyond that scrolls
        // internally. auto_shrink lets the scroll area stay compact
        // when the roster is small.
        const CHARACTER_LIST_MAX_HEIGHT: f32 = 440.0;
        egui::ScrollArea::vertical()
            .id_salt("characters_scroll")
            .max_height(CHARACTER_LIST_MAX_HEIGHT)
            .auto_shrink([false, true])
            .show(ui, |ui| {
        for idx in 0..len {
            // Row 1 — name + reorder + delete.
            ui.horizontal(|ui| {
                ui.label(format!("{}.", idx + 1));
                if ui
                    .text_edit_singleline(&mut self.config.characters[idx].name)
                    .changed()
                {
                    dirty = true;
                }
                if ui.button("↑").clicked() && idx > 0 {
                    swap = Some((idx, idx - 1));
                }
                if ui.button("↓").clicked() {
                    swap = Some((idx, idx + 1));
                }
                if ui.button("✕").clicked() {
                    remove = Some(idx);
                }
            });

            // Row 2 — per-character jump hotkey + cycle flag.
            let name = self.config.characters[idx].name.clone();
            ui.horizontal(|ui| {
                ui.add_space(22.0);

                // In-cycle toggle — scout characters get their hotkey
                // and their list row, but are skipped by forward/back.
                let prev_in_cycle = self.config.characters[idx].in_cycle;
                ui.checkbox(&mut self.config.characters[idx].in_cycle, "döngüde");
                if self.config.characters[idx].in_cycle != prev_in_cycle {
                    dirty = true;
                }

                ui.add_space(8.0);
                ui.label("Kısayol:");

                // Modifier checkboxes (Ctrl / Shift / Alt). Creating a
                // placeholder entry with vk=0 is how we preserve a
                // modifier-only selection until the user captures a
                // main key — register_hotkeys skips vk=0 entries.
                let current = self
                    .config
                    .character_hotkeys
                    .get(&name)
                    .cloned()
                    .unwrap_or(CharacterHotkey {
                        vk: 0,
                        ctrl: false,
                        shift: false,
                        alt: false,
                    });
                let mut next = current.clone();
                ui.checkbox(&mut next.ctrl, "Ctrl");
                ui.checkbox(&mut next.shift, "Shift");
                ui.checkbox(&mut next.alt, "Alt");
                if next != current {
                    self.config
                        .character_hotkeys
                        .insert(name.clone(), next.clone());
                    dirty = true;
                }

                // Bind button — shows the full combo label. vk == 0
                // means "only the modifiers are set so far," so we
                // display that as "none" until a real key is captured.
                let binding_label = self
                    .config
                    .character_hotkeys
                    .get(&name)
                    .filter(|h| h.vk != 0)
                    .map(hotkey_label)
                    .unwrap_or_else(|| "yok".into());
                self.draw_bind_button_sized(
                    ui,
                    &CaptureTarget::Character(name.clone()),
                    binding_label,
                    egui::vec2(140.0, 20.0),
                );

                // Clear the binding entirely.
                if self.config.character_hotkeys.contains_key(&name) && ui.button("✕").clicked() {
                    self.config.character_hotkeys.remove(&name);
                    dirty = true;
                }
            });

            ui.add_space(2.0);
        }
            });

        if dirty {
            self.touch();
        }
        if let Some((a, b)) = swap {
            if b < self.config.characters.len() {
                self.config.characters.swap(a, b);
                self.touch();
            }
        }
        if let Some(idx) = remove {
            // Drop the per-character hotkey for the removed name too.
            let removed = self.config.characters.remove(idx);
            self.config.character_hotkeys.remove(&removed.name);
            self.touch();
        }

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label("Ekle:");
            let response = ui.text_edit_singleline(&mut self.new_character_buffer);
            let add_clicked = ui.button("+").clicked();
            let enter_pressed =
                response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if (add_clicked || enter_pressed) && !self.new_character_buffer.trim().is_empty() {
                self.config.characters.push(CharacterEntry::new(
                    self.new_character_buffer.trim().to_string(),
                ));
                self.new_character_buffer.clear();
                self.touch();
            }
        });
    }

    fn draw_hotkeys_section(&mut self, ui: &mut egui::Ui) {
        Self::draw_section_header(ui, "Klavye Kısayolları");

        let prev_enable = self.config.enable_keyboard_buttons;
        ui.checkbox(
            &mut self.config.enable_keyboard_buttons,
            "Klavye ile geçişi etkinleştir",
        );
        if self.config.enable_keyboard_buttons != prev_enable {
            self.touch();
        }

        ui.add_enabled_ui(self.config.enable_keyboard_buttons, |ui| {
            ui.horizontal(|ui| {
                ui.label("İleri:");
                self.draw_bind_button(
                    ui,
                    &CaptureTarget::ForwardKey,
                    vk_to_label(self.config.forward_key),
                );
            });
            ui.horizontal(|ui| {
                ui.label("Geri:");
                self.draw_bind_button(
                    ui,
                    &CaptureTarget::BackwardKey,
                    vk_to_label(self.config.backward_key),
                );
            });
            ui.horizontal(|ui| {
                ui.label("Değiştirici:");
                let label = match self.config.modifier_key {
                    Some(vk) => vk_to_label(vk),
                    None => "Yok".to_string(),
                };
                self.draw_bind_button(ui, &CaptureTarget::ModifierKey, label);
                if self.config.modifier_key.is_some() && ui.button("Temizle").clicked() {
                    self.config.modifier_key = None;
                    self.touch();
                }
            });
            ui.label(
                egui::RichText::new(
                    "Bir kısayolu değiştirmek için üzerine tıklayın ve bir sonraki tuşa basın. \
                     Esc iptal eder. İleri ve geri tuşlarını aynı yapıp bir değiştirici \
                     atayarak tek tuşla çift yönlü geçiş kurabilirsiniz (örn. Tab + Shift+Tab).",
                )
                .size(10.0)
                .color(INARI_TEXT_MUTED),
            );
        });

        ui.add_space(8.0);
        let prev_mouse = self.config.enable_mouse_buttons;
        ui.checkbox(
            &mut self.config.enable_mouse_buttons,
            "Fare yan tuşlarıyla geçiş yap (XBUTTON1/XBUTTON2)",
        );
        if self.config.enable_mouse_buttons != prev_mouse {
            self.touch();
        }
        ui.label(
            egui::RichText::new(
                "Varsayılan olarak kapalıdır. Yalnızca fare yan tuşlarınızı driver yazılımıyla \
                 (Logi Options+, Razer Synapse vb.) zaten yeniden atamadıysanız açın — aksi \
                 hâlde bu ayar tarayıcı/oyun içindeki ileri-geri tuşlarını da ele geçirir.",
            )
            .size(10.0)
            .color(INARI_TEXT_MUTED),
        );
    }

    /// Button that toggles capture for a given config field. When
    /// capturing, shows a hint; otherwise shows the current binding's
    /// label. Click while already capturing to cancel.
    fn draw_bind_button(&mut self, ui: &mut egui::Ui, target: &CaptureTarget, label: String) {
        self.draw_bind_button_sized(ui, target, label, egui::vec2(200.0, 22.0));
    }

    fn draw_bind_button_sized(
        &mut self,
        ui: &mut egui::Ui,
        target: &CaptureTarget,
        label: String,
        size: egui::Vec2,
    ) {
        let is_capturing = self.capturing.as_ref() == Some(target);
        let text = if is_capturing {
            "[tuşa bas — Esc]".to_string()
        } else {
            label
        };
        let mut button = egui::Button::new(text).min_size(size);
        if is_capturing {
            button = button
                .fill(INARI_ORANGE)
                .stroke(egui::Stroke::new(1.5, INARI_GOLD));
        }
        if ui.add(button).clicked() {
            self.capturing = if is_capturing {
                None
            } else {
                Some(target.clone())
            };
        }
    }

    fn draw_previews_section(&mut self, ui: &mut egui::Ui) {
        Self::draw_section_header(ui, "Önizleme Pencereleri");
        let prev_show = self.config.show_previews;
        ui.checkbox(&mut self.config.show_previews, "Önizleme pencerelerini göster");
        if self.config.show_previews != prev_show {
            self.touch();
        }
        ui.add_enabled_ui(self.config.show_previews, |ui| {
            // Widen sliders so a 1px step is actually reachable without
            // sub-pixel cursor precision. 3× the egui default width.
            ui.spacing_mut().slider_width = ui.spacing().slider_width * 3.0;

            let prev_w = self.config.preview_width;
            let prev_h = self.config.preview_height;
            ui.horizontal(|ui| {
                ui.label("Genişlik:");
                ui.add(
                    egui::Slider::new(&mut self.config.preview_width, 120..=800)
                        .suffix(" px")
                        .smart_aim(false)
                        .step_by(1.0),
                );
            });
            ui.horizontal(|ui| {
                ui.label("Yükseklik:");
                ui.add(
                    egui::Slider::new(&mut self.config.preview_height, 80..=600)
                        .suffix(" px")
                        .smart_aim(false)
                        .step_by(1.0),
                );
            });
            if self.config.preview_width != prev_w || self.config.preview_height != prev_h {
                self.touch();
                // Push the new size to the shared LiveSettings so the
                // preview manager resizes its windows on the next tick —
                // no need to wait for Save + hot-reload.
                let mut live = self.live.lock().unwrap();
                live.preview_width = self.config.preview_width;
                live.preview_height = self.config.preview_height;
            }
        });
    }
}

/// Poll Win32 `GetAsyncKeyState` and return the first rising-edge press
/// observed this frame. The CaptureBuffer tracks last-frame state per
/// VK so modifiers already held when capture began (prev=true) never
/// fire as "new."
fn poll_capture(buf: &mut CaptureBuffer) -> Option<CapturedKey> {
    let mut cur = [false; 256];
    for vk in 0u16..=255 {
        let s = unsafe { GetAsyncKeyState(vk as i32) } as u16;
        cur[vk as usize] = (s & 0x8000) != 0;
    }

    // First poll after capture starts: snapshot whatever's already held
    // as the baseline and return nothing. Only keys pressed AFTER this
    // moment will register as edges on subsequent polls.
    if !buf.primed {
        buf.prev = cur;
        buf.primed = true;
        return None;
    }

    let mut result: Option<CapturedKey> = None;

    for vk in 0u16..=255 {
        let was_down = buf.prev[vk as usize];
        let is_down = cur[vk as usize];
        if was_down || !is_down {
            continue;
        }

        if is_modifier_vk(vk) {
            // Canonicalize left/right variants to the plain VK the
            // config stores (0x10 / 0x11 / 0x12).
            let canonical = canonical_modifier(vk);
            if result.is_none() {
                result = Some(CapturedKey::Modifier(canonical));
            }
            continue;
        }
        if !is_bindable_main_key(vk) {
            continue;
        }

        // Main key edge — emit with a fresh read of all three modifier
        // groups (using `cur`, not prev, so a modifier being held at
        // the moment of press flips the flag to true).
        let ctrl = cur[0x11] || cur[0xA2] || cur[0xA3];
        let shift = cur[0x10] || cur[0xA0] || cur[0xA1];
        let alt = cur[0x12] || cur[0xA4] || cur[0xA5];
        result = Some(CapturedKey::Main {
            vk,
            ctrl,
            shift,
            alt,
        });
        break;
    }

    buf.prev = cur;
    result
}

fn is_modifier_vk(vk: u16) -> bool {
    matches!(vk, 0x10 | 0x11 | 0x12 | 0xA0..=0xA5)
}

fn canonical_modifier(vk: u16) -> u16 {
    match vk {
        0x10 | 0xA0 | 0xA1 => 0x10,
        0x11 | 0xA2 | 0xA3 => 0x11,
        0x12 | 0xA4 | 0xA5 => 0x12,
        other => other,
    }
}

fn is_bindable_main_key(vk: u16) -> bool {
    // Escape is reserved for cancel. Mouse buttons (0x01–0x06) and
    // null-ish low codes aren't useful as keyboard hotkeys.
    if vk == 0x1B || vk < 0x08 {
        return false;
    }
    // Mouse VKs — not a keyboard hotkey.
    if matches!(vk, 0x01 | 0x02 | 0x04 | 0x05 | 0x06) {
        return false;
    }
    // Caps/Num/Scroll Lock, IME keys, and other oddities that
    // RegisterHotKey rejects or that fire spuriously during typing.
    if matches!(
        vk,
        0x14 | 0x90 | 0x91 | 0x15..=0x1A | 0x1C..=0x1F | 0x5B..=0x5F | 0xA6..=0xB7 | 0xE0..=0xFE
    ) {
        return false;
    }
    true
}

/// Human label for a Win32 VK code, used on the bind button.
fn vk_to_label(vk: u16) -> String {
    match vk {
        0x70..=0x87 => format!("F{}", vk - 0x6F),
        0x09 => "Tab".into(),
        0x20 => "Space".into(),
        0x0D => "Enter".into(),
        0x08 => "Backspace".into(),
        0x2D => "Insert".into(),
        0x2E => "Delete".into(),
        0x24 => "Home".into(),
        0x23 => "End".into(),
        0x21 => "PgUp".into(),
        0x22 => "PgDown".into(),
        0x1B => "Escape".into(),
        0x10 | 0xA0 | 0xA1 => "Shift".into(),
        0x11 | 0xA2 | 0xA3 => "Ctrl".into(),
        0x12 | 0xA4 | 0xA5 => "Alt".into(),
        0xC0 => "`".into(),
        0x30..=0x39 => format!("{}", (vk - 0x30) as u8 as char),
        0x41..=0x5A => format!("{}", vk as u8 as char),
        0x60..=0x69 => format!("Num {}", vk - 0x60),
        0x6A => "Num *".into(),
        0x6B => "Num +".into(),
        0x6D => "Num -".into(),
        0x6E => "Num .".into(),
        0x6F => "Num /".into(),
        0x26 => "Up".into(),
        0x28 => "Down".into(),
        0x25 => "Left".into(),
        0x27 => "Right".into(),
        0xBA => ";".into(),
        0xBB => "=".into(),
        0xBC => ",".into(),
        0xBD => "-".into(),
        0xBE => ".".into(),
        0xBF => "/".into(),
        0xDB => "[".into(),
        0xDC => "\\".into(),
        0xDD => "]".into(),
        0xDE => "'".into(),
        other => format!("VK 0x{:02X}", other),
    }
}

/// Human label for a character hotkey, combining its modifier flags
/// with the main key (e.g. `Ctrl+Shift+Num 1`).
fn hotkey_label(hk: &CharacterHotkey) -> String {
    let mut parts: Vec<String> = Vec::new();
    if hk.ctrl {
        parts.push("Ctrl".into());
    }
    if hk.shift {
        parts.push("Shift".into());
    }
    if hk.alt {
        parts.push("Alt".into());
    }
    parts.push(vk_to_label(hk.vk));
    parts.join("+")
}

/// Open the config panel as a top-level window. Blocks until the user
/// closes the window. Takes a shared LiveSettings so slider changes can
/// be applied to the running preview manager instantly.
pub fn run(config: Config, live: Arc<Mutex<LiveSettings>>) -> Result<(), eframe::Error> {
    // Load the Inari icon for the window chrome + taskbar + alt-tab.
    // Baked into the binary via include_bytes so there's no external
    // asset to lose on install. from_png_bytes goes through eframe's
    // bundled `image` crate (already pulled in with the png feature).
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png"))
        .expect("failed to decode embedded icon.png");

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            // Open at the empty-config size; the per-frame auto-resize
            // grows the window as the user adds characters. Starting at
            // a tall fixed value (e.g. 1000pt) caused huge dead space on
            // first launch on machines where the OS ignores
            // ViewportCommand::InnerSize *shrinks* on a non-resizable
            // window — the window would never shrink back from the
            // initial size to fit the (much shorter) empty content.
            // Growing reliably works everywhere, so we start small.
            .with_inner_size([600.0, 640.0])
            .with_resizable(false)
            .with_title("Inari")
            .with_icon(icon),
        ..Default::default()
    };

    eframe::run_native(
        "Inari",
        options,
        Box::new(move |cc| Ok(Box::new(ConfigPanel::new(cc, config, live)))),
    )
}
