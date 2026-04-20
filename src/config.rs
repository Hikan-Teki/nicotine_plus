use anyhow::{Context, Result};
use serde::de::{Deserializer, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// A single per-character hotkey binding. `vk` is a Win32 Virtual-Key
/// code; `ctrl`/`shift`/`alt` toggle modifier bits, so combos like
/// `Ctrl+Num 1` or `Ctrl+Shift+F11` work. The legacy `modifier`
/// single-VK field is still accepted on deserialize and translated to
/// the matching bool so existing config.toml files keep working.
#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
pub struct CharacterHotkey {
    pub vk: u16,
    #[serde(default)]
    pub ctrl: bool,
    #[serde(default)]
    pub shift: bool,
    #[serde(default)]
    pub alt: bool,
}

impl<'de> Deserialize<'de> for CharacterHotkey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Accept both the modern `{ vk, ctrl, shift, alt }` shape and
        // the legacy `{ vk, modifier }` shape where `modifier` was a VK
        // code for Shift/Ctrl/Alt. Anything unrecognized in `modifier`
        // is silently dropped (user can rebind via the panel).
        #[derive(Deserialize)]
        struct Raw {
            vk: u16,
            #[serde(default)]
            ctrl: bool,
            #[serde(default)]
            shift: bool,
            #[serde(default)]
            alt: bool,
            #[serde(default)]
            modifier: Option<u16>,
        }
        let raw = Raw::deserialize(deserializer)?;
        let (mut ctrl, mut shift, mut alt) = (raw.ctrl, raw.shift, raw.alt);
        if let Some(m) = raw.modifier {
            match m {
                0x10 | 0xA0 | 0xA1 => shift = true,
                0x11 | 0xA2 | 0xA3 => ctrl = true,
                0x12 | 0xA4 | 0xA5 => alt = true,
                _ => {}
            }
        }
        Ok(CharacterHotkey {
            vk: raw.vk,
            ctrl,
            shift,
            alt,
        })
    }
}

/// One character in the cycle list. `in_cycle = false` marks a "scout"
/// entry: it keeps its slot in the list (and any bound hotkey), and
/// still shows up in previews / list view, but forward/backward cycling
/// skips over it.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct CharacterEntry {
    pub name: String,
    #[serde(default = "default_true")]
    pub in_cycle: bool,
}

impl CharacterEntry {
    pub fn new(name: String) -> Self {
        Self {
            name,
            in_cycle: true,
        }
    }
}

fn default_true() -> bool {
    true
}

/// Deserializer for `Config::characters` that accepts three TOML shapes
/// and materializes all three as `Vec<CharacterEntry>` (with
/// `in_cycle = true` defaults for legacy bare-string entries):
///
///   1. `characters = ["Alpha", "Beta"]` — legacy string array
///   2. `[[characters]]` tables with `name` + optional `in_cycle`
///   3. A mix — each element is independently either a string or a table
fn deserialize_characters<'de, D>(deserializer: D) -> Result<Vec<CharacterEntry>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrEntry {
        Name(String),
        Full(CharacterEntry),
    }

    struct Vis;
    impl<'de> Visitor<'de> for Vis {
        type Value = Vec<CharacterEntry>;

        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("a sequence of character names or {name, in_cycle} tables")
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut out = Vec::new();
            while let Some(el) = seq.next_element::<StringOrEntry>()? {
                out.push(match el {
                    StringOrEntry::Name(n) => CharacterEntry::new(n),
                    StringOrEntry::Full(e) => e,
                });
            }
            Ok(out)
        }
    }

    deserializer.deserialize_seq(Vis)
}

/// How the visible-at-a-glance view of clients is rendered.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
pub enum DisplayMode {
    /// One DWM thumbnail window per EVE client (default).
    Previews,
    /// A single always-on-top window listing each character name. Active
    /// character shown in Nicotine red with a 🚬 marker.
    List,
}

/// Settings that components watch for *live* changes — e.g. the preview
/// manager resizes windows as soon as these change, without waiting for a
/// save-to-disk + hot-reload cycle. Shared via Arc<Mutex<>> between the
/// config panel (writer) and the preview manager (reader).
#[derive(Debug, Clone)]
pub struct LiveSettings {
    pub preview_width: u32,
    pub preview_height: u32,
    pub display_mode: DisplayMode,
    /// When true, both preview windows and the client-list window
    /// ignore mouse drags so they can't accidentally be knocked out of
    /// position mid-game. Click-to-activate still works on previews.
    pub positions_locked: bool,
}

impl LiveSettings {
    pub fn from_config(config: &Config) -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Self {
            preview_width: config.preview_width,
            preview_height: config.preview_height,
            display_mode: config.display_mode,
            positions_locked: config.positions_locked,
        }))
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Config {
    pub display_width: u32,
    pub display_height: u32,
    pub panel_height: u32,
    pub eve_width: u32,
    pub eve_height: u32,
    #[serde(default = "default_enable_mouse")]
    pub enable_mouse_buttons: bool,
    #[serde(default = "default_forward_button")]
    pub forward_button: u16, // XBUTTON2 (forward side button)
    #[serde(default = "default_backward_button")]
    pub backward_button: u16, // XBUTTON1 (backward side button)
    #[serde(default = "default_enable_keyboard")]
    pub enable_keyboard_buttons: bool,
    #[serde(default = "default_forward_key")]
    pub forward_key: u16, // VK_F11
    #[serde(default = "default_backward_key")]
    pub backward_key: u16, // VK_F10
    #[serde(default = "default_minimize_inactive")]
    pub minimize_inactive: bool,
    #[serde(default = "default_modifier_key")]
    pub modifier_key: Option<u16>,
    /// Width of preview windows in pixels. Single global value — every
    /// preview gets the same size. Aspect ratio is preserved on the
    /// thumbnail; the window is sized exactly as configured.
    #[serde(default = "default_preview_width")]
    pub preview_width: u32,
    /// Height of preview windows in pixels.
    #[serde(default = "default_preview_height")]
    pub preview_height: u32,
    /// Whether DWM preview windows are spawned at all. When false, the
    /// daemon runs headless and you cycle via hotkeys / CLI only.
    #[serde(default = "default_show_previews")]
    pub show_previews: bool,
    /// Ordered list of EVE character entries. Forward/backward cycling
    /// traverses this order (skipping `in_cycle = false` entries);
    /// `switch N` maps target N to the N-th entry regardless of cycle
    /// membership. Empty list = cycle through whatever order the window
    /// manager reports (no stable ordering).
    #[serde(default, deserialize_with = "deserialize_characters")]
    pub characters: Vec<CharacterEntry>,
    /// Which on-screen representation of running clients Nicotine shows.
    #[serde(default = "default_display_mode")]
    pub display_mode: DisplayMode,
    /// When true, drag is disabled on preview windows and the client
    /// list so they can't accidentally move during gameplay.
    #[serde(default)]
    pub positions_locked: bool,
    /// Map of character name → hotkey for jump-to-character. When the
    /// configured key (plus optional modifier) fires, Nicotine activates
    /// that EVE client directly — independent of the forward/backward
    /// cycle. Keyed by name so bindings follow reorders and renames
    /// without reassigning keys.
    #[serde(default)]
    pub character_hotkeys: HashMap<String, CharacterHotkey>,
}

// Off by default — most users remap side buttons at the driver level
// (Logi Options+, etc.) and use keyboard hotkeys instead. When the
// native hook is on, it intercepts XBUTTON1/2 from games and browsers
// (back/forward) which surprises users who didn't ask for cycling there.
fn default_enable_mouse() -> bool {
    false
}

fn default_forward_button() -> u16 {
    2 // XBUTTON2 (forward side button)
}

fn default_backward_button() -> u16 {
    1 // XBUTTON1 (backward side button)
}

fn default_enable_keyboard() -> bool {
    true // F10/F11 are uncommon enough to enable by default for cycling
}

fn default_forward_key() -> u16 {
    0x7A // VK_F11
}

fn default_backward_key() -> u16 {
    0x79 // VK_F10
}

fn default_minimize_inactive() -> bool {
    false
}

fn default_modifier_key() -> Option<u16> {
    None // No modifier for backward shifting by default
}

fn default_preview_width() -> u32 {
    320
}

fn default_preview_height() -> u32 {
    180
}

fn default_show_previews() -> bool {
    true
}

fn default_display_mode() -> DisplayMode {
    DisplayMode::Previews
}

impl Config {
    fn config_dir() -> PathBuf {
        let mut path = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
        path.push("nicotine");
        path
    }

    fn config_path() -> PathBuf {
        let mut path = Self::config_dir();
        path.push("config.toml");
        path
    }

    /// Persist the current Config back to disk. Used by the config panel
    /// to commit user edits.
    pub fn save(&self) -> Result<()> {
        let config_path = Self::config_path();
        if let Some(parent) = config_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let contents = toml::to_string_pretty(self).context("Yapılandırma serileştirilemedi")?;
        fs::write(&config_path, contents).context("config.toml yazılamadı")?;
        Ok(())
    }

    fn detect_display_size() -> (u32, u32) {
        use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};
        let w = unsafe { GetSystemMetrics(SM_CXSCREEN) };
        let h = unsafe { GetSystemMetrics(SM_CYSCREEN) };
        if w > 0 && h > 0 {
            (w as u32, h as u32)
        } else {
            (1920, 1080)
        }
    }

    fn build_default(display_width: u32, display_height: u32) -> Self {
        Self {
            display_width,
            display_height,
            panel_height: 0,
            eve_width: (display_width as f32 * 0.54) as u32, // ~54% of width
            eve_height: display_height,
            enable_mouse_buttons: default_enable_mouse(),
            forward_button: default_forward_button(),
            backward_button: default_backward_button(),
            enable_keyboard_buttons: default_enable_keyboard(),
            forward_key: default_forward_key(),
            backward_key: default_backward_key(),
            minimize_inactive: default_minimize_inactive(),
            modifier_key: default_modifier_key(),
            preview_width: default_preview_width(),
            preview_height: default_preview_height(),
            show_previews: default_show_previews(),
            characters: Vec::new(),
            display_mode: default_display_mode(),
            positions_locked: false,
            character_hotkeys: HashMap::new(),
        }
    }

    pub fn load() -> Result<Self> {
        let config_path = Self::config_path();

        if let Ok(contents) = fs::read_to_string(&config_path) {
            return toml::from_str(&contents).context("config.toml ayrıştırılamadı");
        }

        println!("Ekranınıza göre yapılandırma oluşturuluyor...");
        let (display_width, display_height) = Self::detect_display_size();
        println!("Algılanan ekran: {}x{}", display_width, display_height);

        let config = Self::build_default(display_width, display_height);

        if let Some(parent) = config_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let contents = toml::to_string_pretty(&config)?;
        fs::write(&config_path, contents)?;
        println!("Yapılandırma oluşturuldu: {}", config_path.display());
        println!("Pencere boyutlarını ve konumlarını özelleştirmek için düzenleyebilirsiniz");

        Ok(config)
    }

    pub fn save_default() -> Result<()> {
        let config_path = Self::config_path();
        let (display_width, display_height) = Self::detect_display_size();

        let config = Self::build_default(display_width, display_height);

        if let Some(parent) = config_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let contents = toml::to_string_pretty(&config)?;
        fs::write(&config_path, contents)?;
        println!("Yapılandırma oluşturuldu: {}", config_path.display());
        Ok(())
    }

    pub fn eve_height_adjusted(&self) -> u32 {
        self.display_height - self.panel_height
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_config() -> Config {
        Config {
            display_width: 1920,
            display_height: 1080,
            panel_height: 40,
            eve_width: 1000,
            eve_height: 1080,
            enable_mouse_buttons: false,
            forward_button: 2,
            backward_button: 1,
            enable_keyboard_buttons: true,
            forward_key: 0x7A,
            backward_key: 0x79,
            minimize_inactive: false,
            modifier_key: None,
            preview_width: 320,
            preview_height: 180,
            show_previews: true,
            characters: Vec::new(),
            display_mode: DisplayMode::Previews,
            positions_locked: false,
            character_hotkeys: HashMap::new(),
        }
    }

    #[test]
    fn test_eve_height_adjusted_with_panel() {
        let config = sample_config();
        // Height should be: 1080 - 40 = 1040
        assert_eq!(config.eve_height_adjusted(), 1040);
    }

    #[test]
    fn test_eve_height_adjusted_without_panel() {
        let mut config = sample_config();
        config.panel_height = 0;
        assert_eq!(config.eve_height_adjusted(), 1080);
    }

    #[test]
    fn test_config_serialization() {
        let mut config = sample_config();
        config.display_width = 7680;
        config.display_height = 2160;
        config.panel_height = 0;
        config.eve_width = 4147;
        config.eve_height = 2160;

        let toml_str = toml::to_string(&config).unwrap();
        let deserialized: Config = toml::from_str(&toml_str).unwrap();

        assert_eq!(deserialized.display_width, 7680);
        assert_eq!(deserialized.display_height, 2160);
        assert_eq!(deserialized.eve_width, 4147);
    }

    #[test]
    fn legacy_character_strings_deserialize_as_in_cycle() {
        let toml_src = "\
display_width = 1920\n\
display_height = 1080\n\
panel_height = 0\n\
eve_width = 1000\n\
eve_height = 1080\n\
characters = [\"Alpha\", \"Beta\"]\n";
        let c: Config = toml::from_str(toml_src).unwrap();
        assert_eq!(c.characters.len(), 2);
        assert_eq!(c.characters[0].name, "Alpha");
        assert!(c.characters[0].in_cycle);
        assert!(c.characters[1].in_cycle);
    }

    #[test]
    fn legacy_hotkey_modifier_translates_to_bool() {
        let toml_src = "\
display_width = 1920\n\
display_height = 1080\n\
panel_height = 0\n\
eve_width = 1000\n\
eve_height = 1080\n\
[character_hotkeys.Alpha]\n\
vk = 0x70\n\
modifier = 0x11\n";
        let c: Config = toml::from_str(toml_src).unwrap();
        let hk = &c.character_hotkeys["Alpha"];
        assert_eq!(hk.vk, 0x70);
        assert!(hk.ctrl);
        assert!(!hk.shift);
        assert!(!hk.alt);
    }

    #[test]
    fn scout_entry_round_trips() {
        let mut config = sample_config();
        config.characters = vec![
            CharacterEntry::new("Cycler".into()),
            CharacterEntry {
                name: "Scout".into(),
                in_cycle: false,
            },
        ];
        let toml_str = toml::to_string(&config).unwrap();
        let back: Config = toml::from_str(&toml_str).unwrap();
        assert_eq!(back.characters[0].name, "Cycler");
        assert!(back.characters[0].in_cycle);
        assert_eq!(back.characters[1].name, "Scout");
        assert!(!back.characters[1].in_cycle);
    }
}
