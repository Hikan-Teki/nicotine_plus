use crate::config::{CharacterEntry, CharacterHotkey, Config, DisplayMode, LiveSettings};
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

/// Brand palette matching the existing Linux overlay.
const NICOTINE_RED: egui::Color32 = egui::Color32::from_rgb(196, 30, 58);
const NICOTINE_GOLD: egui::Color32 = egui::Color32::from_rgb(180, 155, 105);
const NICOTINE_CREAM: egui::Color32 = egui::Color32::from_rgb(252, 250, 242);
const NICOTINE_BLACK: egui::Color32 = egui::Color32::from_rgb(30, 30, 30);
/// Used only for the "LATEST VERSION" footer badge — chosen to read
/// clearly against cream while harmonizing with the warm palette.
const NICOTINE_GREEN: egui::Color32 = egui::Color32::from_rgb(60, 140, 70);

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
}

impl ConfigPanel {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        config: Config,
        live: Arc<Mutex<LiveSettings>>,
    ) -> Self {
        // Load Nicotine's brand fonts so the header looks like the overlay.
        let mut fonts = egui::FontDefinitions::default();
        fonts.font_data.insert(
            "jetbrains_mono".to_owned(),
            egui::FontData::from_static(include_bytes!(
                "../assets/fonts/JetBrainsMono-Regular.ttf"
            )),
        );
        fonts.font_data.insert(
            "logo_font".to_owned(),
            egui::FontData::from_static(include_bytes!("../assets/fonts/Marlboro.ttf")),
        );
        fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .insert(0, "jetbrains_mono".to_owned());
        fonts
            .families
            .entry(egui::FontFamily::Name("logo".into()))
            .or_default()
            .push("logo_font".to_owned());
        cc.egui_ctx.set_fonts(fonts);

        // Nicotine-branded light theme. egui's default light visuals use
        // pale grays against our cream background, which makes hover /
        // active state changes basically invisible. Override with a
        // warmer palette so every interactive widget has a visible
        // idle / hover / pressed progression (cream → gold → red).
        cc.egui_ctx.set_visuals(build_visuals());

        Self {
            config,
            new_character_buffer: String::new(),
            live,
            capturing: None,
            last_capturing: None,
            capture_buf: CaptureBuffer::new(),
            last_change: None,
            last_applied_height: 0.0,
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
    let mut v = egui::Visuals::light();

    // Cream page / non-interactive surfaces.
    v.widgets.noninteractive.bg_fill = NICOTINE_CREAM;
    v.widgets.noninteractive.weak_bg_fill = NICOTINE_CREAM;
    v.widgets.noninteractive.fg_stroke.color = NICOTINE_BLACK;

    // Idle: slightly-off-cream so the widget is distinguishable from the
    // surrounding panel, with a gold-ish border.
    v.widgets.inactive.bg_fill = egui::Color32::from_rgb(240, 234, 218);
    v.widgets.inactive.weak_bg_fill = egui::Color32::from_rgb(244, 238, 224);
    v.widgets.inactive.fg_stroke.color = NICOTINE_BLACK;
    v.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, NICOTINE_GOLD);

    // Hover: strong gold — clearly different from idle so moving the
    // mouse over anything shows a visible change.
    v.widgets.hovered.bg_fill = NICOTINE_GOLD;
    v.widgets.hovered.weak_bg_fill = egui::Color32::from_rgb(228, 212, 176);
    v.widgets.hovered.fg_stroke.color = NICOTINE_BLACK;
    v.widgets.hovered.bg_stroke = egui::Stroke::new(1.5, NICOTINE_RED);

    // Pressed / active: Nicotine red with cream text.
    v.widgets.active.bg_fill = NICOTINE_RED;
    v.widgets.active.weak_bg_fill = egui::Color32::from_rgb(230, 176, 186);
    v.widgets.active.fg_stroke.color = NICOTINE_CREAM;
    v.widgets.active.bg_stroke = egui::Stroke::new(1.5, NICOTINE_RED);

    // Open popup / selected — e.g. radio selection, text edit focus.
    v.widgets.open.bg_fill = NICOTINE_GOLD;
    v.widgets.open.weak_bg_fill = egui::Color32::from_rgb(228, 212, 176);
    v.widgets.open.fg_stroke.color = NICOTINE_BLACK;
    v.widgets.open.bg_stroke = egui::Stroke::new(1.5, NICOTINE_RED);

    // Text selection highlight.
    v.selection.bg_fill = NICOTINE_RED.gamma_multiply(0.45);
    v.selection.stroke.color = NICOTINE_BLACK;

    // Hyperlinks / accents (rarely used here but keep the brand colour).
    v.hyperlink_color = NICOTINE_RED;

    v
}

impl eframe::App for ConfigPanel {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
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
        egui::TopBottomPanel::top("nicotine_header")
            .exact_height(72.0)
            .frame(
                egui::Frame::none()
                    .fill(NICOTINE_RED)
                    // Asymmetric vertical margin: the Marlboro font's
                    // glyph box has more descent than ascent, so a
                    // geometrically-centered layout reads as "logo too
                    // high." Bumping the top margin shifts the visual
                    // center down by a few pixels.
                    .inner_margin(egui::Margin {
                        left: 0.0,
                        right: 0.0,
                        top: 6.0,
                        bottom: 0.0,
                    }),
            )
            .show(ctx, |ui| {
                ui.with_layout(
                    egui::Layout::centered_and_justified(egui::Direction::TopDown),
                    |ui| {
                        ui.label(
                            egui::RichText::new("Nicotine")
                                .family(egui::FontFamily::Name("logo".into()))
                                .size(48.0)
                                .color(NICOTINE_CREAM),
                        );
                    },
                );
            });

        // ---- Branded footer with external links ----
        egui::TopBottomPanel::bottom("nicotine_footer")
            .exact_height(40.0)
            .frame(
                egui::Frame::none()
                    .fill(NICOTINE_CREAM)
                    .inner_margin(egui::Margin::symmetric(16.0, 8.0))
                    .stroke(egui::Stroke::new(1.0, NICOTINE_GOLD)),
            )
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    // Explicit .color() on both RichText blocks — egui's
                    // hyperlink color doesn't always propagate through
                    // .strong() in 0.29, leaving the text near-invisible
                    // against the cream background.
                    ui.hyperlink_to(
                        egui::RichText::new("GITHUB").strong().color(NICOTINE_RED),
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
                                        "NEW VERSION AVAILABLE (v{})",
                                        version
                                    ))
                                    .strong()
                                    .color(NICOTINE_RED),
                                    url,
                                );
                            }
                            Some(crate::version_check::UpdateStatus::UpToDate) => {
                                ui.label(
                                    egui::RichText::new("LATEST VERSION")
                                        .strong()
                                        .color(NICOTINE_GREEN),
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
                    .fill(NICOTINE_CREAM)
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
                    eprintln!("config autosave failed: {}", e);
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
                .color(NICOTINE_RED),
        );
        ui.separator();
    }

    fn draw_display_mode_section(&mut self, ui: &mut egui::Ui) {
        Self::draw_section_header(ui, "Display Mode");
        ui.label(
            egui::RichText::new(
                "How Nicotine shows your running clients on screen. \
                 Preview windows mirror each client live; the list view is \
                 a compact always-on-top window of names.",
            )
            .size(11.0)
            .color(NICOTINE_BLACK),
        );
        ui.add_space(4.0);

        let prev = self.config.display_mode;
        ui.horizontal(|ui| {
            ui.radio_value(
                &mut self.config.display_mode,
                DisplayMode::Previews,
                "Preview windows",
            );
            ui.add_space(12.0);
            ui.radio_value(
                &mut self.config.display_mode,
                DisplayMode::List,
                "Client list",
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
            "Lock positions (drag disabled on previews and list)",
        );
        if self.config.positions_locked != prev_lock {
            self.touch();
            // Live-apply so the running preview manager stops honoring
            // drags immediately — no save + restart needed.
            self.live.lock().unwrap().positions_locked = self.config.positions_locked;
        }
    }

    fn draw_characters_section(&mut self, ui: &mut egui::Ui) {
        Self::draw_section_header(ui, "Cycle Order");
        ui.label(
            egui::RichText::new(
                "Characters cycle in the order shown. Uncheck \"in cycle\" to keep a \
                 character visible with its hotkey but skip it during forward/backward \
                 cycling (e.g. a scout). Names must match EVE's window title exactly \
                 (the part after \"EVE - \").",
            )
            .size(11.0)
            .color(NICOTINE_BLACK),
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
                ui.checkbox(&mut self.config.characters[idx].in_cycle, "in cycle");
                if self.config.characters[idx].in_cycle != prev_in_cycle {
                    dirty = true;
                }

                ui.add_space(8.0);
                ui.label("Hotkey:");

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
                    .unwrap_or_else(|| "none".into());
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
            ui.label("Add:");
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
        Self::draw_section_header(ui, "Keyboard Hotkeys");

        let prev_enable = self.config.enable_keyboard_buttons;
        ui.checkbox(
            &mut self.config.enable_keyboard_buttons,
            "Enable keyboard cycling",
        );
        if self.config.enable_keyboard_buttons != prev_enable {
            self.touch();
        }

        ui.add_enabled_ui(self.config.enable_keyboard_buttons, |ui| {
            ui.horizontal(|ui| {
                ui.label("Forward:");
                self.draw_bind_button(
                    ui,
                    &CaptureTarget::ForwardKey,
                    vk_to_label(self.config.forward_key),
                );
            });
            ui.horizontal(|ui| {
                ui.label("Backward:");
                self.draw_bind_button(
                    ui,
                    &CaptureTarget::BackwardKey,
                    vk_to_label(self.config.backward_key),
                );
            });
            ui.horizontal(|ui| {
                ui.label("Modifier:");
                let label = match self.config.modifier_key {
                    Some(vk) => vk_to_label(vk),
                    None => "None".to_string(),
                };
                self.draw_bind_button(ui, &CaptureTarget::ModifierKey, label);
                if self.config.modifier_key.is_some() && ui.button("Clear").clicked() {
                    self.config.modifier_key = None;
                    self.touch();
                }
            });
            ui.label(
                egui::RichText::new(
                    "Click a binding to record the next key you press. Esc cancels. \
                     Set both keys to the same value with a modifier to cycle backward \
                     via modifier+key (e.g. Tab + Shift+Tab).",
                )
                .size(10.0)
                .color(NICOTINE_BLACK),
            );
        });

        ui.add_space(8.0);
        let prev_mouse = self.config.enable_mouse_buttons;
        ui.checkbox(
            &mut self.config.enable_mouse_buttons,
            "Cycle on mouse side buttons (XBUTTON1/XBUTTON2)",
        );
        if self.config.enable_mouse_buttons != prev_mouse {
            self.touch();
        }
        ui.label(
            egui::RichText::new(
                "Off by default. Turn on only if you don't already remap your mouse \
                 side buttons via driver software (Logi Options+, Razer Synapse, etc.) \
                 — otherwise this will hijack the buttons in browsers/games too.",
            )
            .size(10.0)
            .color(NICOTINE_BLACK),
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
            "[press key — Esc]".to_string()
        } else {
            label
        };
        let mut button = egui::Button::new(text).min_size(size);
        if is_capturing {
            button = button
                .fill(NICOTINE_GOLD)
                .stroke(egui::Stroke::new(1.5, NICOTINE_RED));
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
        Self::draw_section_header(ui, "Preview Windows");
        let prev_show = self.config.show_previews;
        ui.checkbox(&mut self.config.show_previews, "Show preview windows");
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
                ui.label("Width:");
                ui.add(
                    egui::Slider::new(&mut self.config.preview_width, 120..=800)
                        .suffix(" px")
                        .smart_aim(false)
                        .step_by(1.0),
                );
            });
            ui.horizontal(|ui| {
                ui.label("Height:");
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
    // Load the Nicotine icon for the window chrome + taskbar + alt-tab.
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
            .with_title("Nicotine")
            .with_icon(icon),
        ..Default::default()
    };

    eframe::run_native(
        "Nicotine",
        options,
        Box::new(move |cc| Ok(Box::new(ConfigPanel::new(cc, config, live)))),
    )
}
