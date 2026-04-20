<div align="center">
  <img src="assets/ghlogo.png" alt="Nicotine Logo" width="600">
</div>

# Nicotine 🚬

High-performance EVE Online multiboxing tool for Windows, inspired by EVE-O Preview.

## Features

- **Instant client cycling** with mouse side buttons or keyboard hotkeys (F10/F11 by default)
- **DWM preview windows** - one live thumbnail per EVE client, click to activate
- **List view** - compact always-on-top panel listing characters with active indicator
- **Per-character hotkeys** - jump straight to a specific client
- **Auto-stack windows** to perfectly center multiple EVE clients
- **Auto-detects display resolution** - works on any monitor setup
- **Minimize inactive clients** - Optional feature to reduce resource usage by minimizing unfocused clients
- **Hot-reload config** - slider changes in the config panel take effect live

## Quick Install

Grab the latest `Nicotine.exe` from the [GitHub Releases page](https://github.com/Hikan-Teki/nicotine_plus/releases) and double-click to launch. The config panel opens on first run and a default `config.toml` is generated at `%APPDATA%\nicotine\config.toml`.

## Usage

Double-clicking `Nicotine.exe` is equivalent to `nicotine start` — it spawns the daemon and opens the config panel.

### Basic Commands

```
nicotine start          # Start everything (daemon + previews)
nicotine stop           # Stop all Nicotine processes
nicotine stack          # Stack all EVE windows
nicotine forward        # Cycle to next client
nicotine backward       # Cycle to previous client
nicotine 1              # Jump to client 1
nicotine 2              # Jump to client 2
```

### Targeted Cycling

By default, `nicotine 1`, `nicotine 2`, etc. use window detection order. To define your own order, list character names in `config.toml` under `characters`:

```toml
characters = [
  "Main Character",
  "Alt One",
  "Alt Two",
]
```

Line 1 = target 1, line 2 = target 2, etc.

### Hotkeys

Edit keys via the config panel or directly in `config.toml`:

```toml
enable_keyboard_buttons = true
forward_key  = 0x7A  # VK_F11
backward_key = 0x79  # VK_F10
modifier_key = 0     # Optional extra key that must be held for backward
```

Mouse side buttons are off by default (they clash with browser back/forward); toggle with `enable_mouse_buttons = true`.

## Configuration

Config file: `%APPDATA%\nicotine\config.toml`

Auto-generated on first run. Key settings:

```toml
display_width = 1920
display_height = 1080
panel_height = 0            # Set this if you have a taskbar/panel
eve_width = 1037            # ~54% of display width
eve_height = 1080
enable_mouse_buttons = false
forward_button = 2          # XBUTTON2 (forward side button)
backward_button = 1         # XBUTTON1 (backward side button)
enable_keyboard_buttons = true
forward_key = 0x7A          # VK_F11
backward_key = 0x79         # VK_F10
minimize_inactive = false   # Minimize clients when cycling away
preview_width = 320
preview_height = 180
show_previews = true        # Set false for headless daemon (hotkeys only)
positions_locked = false
```

## Architecture

- **Daemon mode**: Maintains window state in memory for instant cycling
- **Named-pipe IPC**: ~2ms command latency (vs ~50-100ms process spawning)
- **Native input hooks**: Low-level keyboard + mouse hooks for hotkeys
- **DWM thumbnails**: Live preview windows via the Desktop Window Manager API

## Building from Source

```
# Install Rust (https://rustup.rs)
cargo build --release

# Binary at: target\release\Nicotine.exe
```

## License

See [LICENSE](LICENSE.md)
