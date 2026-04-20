// Release builds run under the GUI subsystem so Windows doesn't
// allocate a console (or launch Windows Terminal) when the user
// double-clicks nicotine.exe. Debug builds stay on the default CONSOLE
// subsystem so `cargo run` during dev still prints stdout. The tradeoff:
// release-binary runs from PowerShell no longer show println! output in
// the shell. That's OK — the app is GUI-first; the daemon subcommand
// still works, it just runs headless.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod config_panel;
mod cycle_state;
mod daemon;
mod ipc;
mod lock;
mod paths;
mod preview_windows;
mod tray;
mod version_check;
mod window_manager;
mod windows_input;
mod windows_manager;

use anyhow::Result;
use config::{Config, LiveSettings};
use cycle_state::CycleState;
use daemon::Daemon;
use std::env;
use std::sync::Arc;
use window_manager::WindowManager;

fn create_window_manager() -> Result<Arc<dyn WindowManager>> {
    println!("Windows arka ucu kullanılıyor");
    Ok(Arc::new(windows_manager::WindowsManager::new()?))
}

enum CycleOp {
    Forward,
    Backward,
    Switch(usize),
}

fn start_command(wm: Arc<dyn WindowManager>, config: Config) -> Result<()> {
    let live = LiveSettings::from_config(&config);

    // Kick off the GitHub-release check on a detached thread. The
    // config panel's footer renders a "NEW VERSION AVAILABLE" link
    // once the result lands (~1s after launch on a normal connection).
    version_check::spawn_check();

    // If a separate daemon is already running (e.g. the user invoked
    // `nicotine.exe daemon` in headless mode), don't spawn a duplicate —
    // just open the config panel. Edits propagate via hot-reload.
    if !ipc::daemon_running() {
        let wm_daemon = Arc::clone(&wm);
        let config_daemon = config.clone();
        let live_daemon = Arc::clone(&live);
        std::thread::spawn(move || {
            let mut daemon = Daemon::new(wm_daemon, config_daemon, live_daemon);
            if let Err(e) = daemon.run() {
                eprintln!("Daemon hatası: {}", e);
            }
        });
        // Brief pause so the IPC socket is bound before the panel opens.
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    // The config panel is the visible app — it shows in the taskbar and
    // owns the process's main thread. eframe blocks here until the user
    // closes the window; on close, the process exits and the daemon
    // thread terminates with it. The shared LiveSettings lets the panel
    // push slider changes straight to the preview manager without
    // waiting for a save-to-disk round-trip.
    if let Err(e) = config_panel::run(config, live) {
        eprintln!("Yapılandırma paneli hatası: {}", e);
    }
    Ok(())
}

fn stop_command() {
    // Ask the daemon to quit cleanly; ignore errors (it may not be running).
    let _ = ipc::send_line("quit");
    // Force-kill any stragglers that didn't respond.
    let _ = std::process::Command::new("taskkill")
        .args(["/IM", "Inari.exe", "/F"])
        .output();
    let _ = std::fs::remove_file(paths::lock_file_path());
    let _ = std::fs::remove_file(paths::index_file_path());
}

fn run_cycle_direct(wm: &Arc<dyn WindowManager>, config: &Config, op: CycleOp) -> Result<()> {
    let windows = wm.get_eve_windows()?;
    if windows.is_empty() {
        return Ok(());
    }

    let character_order = if config.characters.is_empty() {
        None
    } else {
        Some(config.characters.clone())
    };

    let mut state = CycleState::new();
    state.set_character_order(character_order.clone());
    state.update_windows(windows);

    if let Ok(active) = wm.get_active_window() {
        state.sync_with_active(active);
    }

    match op {
        CycleOp::Forward => state.cycle_forward(&**wm, config.minimize_inactive)?,
        CycleOp::Backward => state.cycle_backward(&**wm, config.minimize_inactive)?,
        CycleOp::Switch(target) => {
            state.switch_to(
                target,
                &**wm,
                config.minimize_inactive,
                character_order.as_deref(),
            )?;
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    // Declare DPI awareness before any window gets created. SYSTEM_AWARE
    // means Windows reports real monitor DPI instead of bitmap-scaling a
    // virtual 96 DPI canvas — chrome, text, and window dims then need to
    // be scaled manually (see `preview_windows::px`). Without this, users
    // on high-DPI displays get blurry GDI text or weirdly-sized windows
    // depending on launch-order interaction with eframe/winit's own
    // awareness declaration. Ignoring the Result is intentional: the
    // call can fail harmlessly if awareness was already set (e.g. by a
    // hosting process) and we have no recovery.
    unsafe {
        use windows::Win32::UI::HiDpi::{
            SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_SYSTEM_AWARE,
        };
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_SYSTEM_AWARE);
    }

    let args: Vec<String> = env::args().collect();
    let command = args.get(1).map(|s| s.as_str()).unwrap_or("");

    let config = Config::load()?;
    let wm = create_window_manager()?;

    match command {
        "start" => {
            println!("Inari başlatılıyor 🦊");
            start_command(wm, config)?;
        }

        "daemon" => {
            println!("Inari daemon başlatılıyor...");
            let live = LiveSettings::from_config(&config);
            let mut daemon = Daemon::new(wm, config, live);
            daemon.run()?;
        }

        "stack" => {
            println!("EVE pencereleri üst üste diziliyor...");
            let windows = wm.get_eve_windows()?;

            println!(
                "{} EVE istemcisi {}x{} ekrana {}x{} boyutunda ortalanıyor",
                windows.len(),
                config.display_width,
                config.display_height,
                config.eve_width,
                config.eve_height_adjusted(),
            );

            wm.stack_windows(&windows, &config)?;

            println!("✓ {} pencere dizildi", windows.len());
        }

        "cycle-forward" | "forward" | "f" => {
            if daemon::send_command("forward").is_ok() {
                return Ok(());
            }
            lock::with_cycle_lock(|| run_cycle_direct(&wm, &config, CycleOp::Forward))?;
        }

        "cycle-backward" | "backward" | "b" => {
            if daemon::send_command("backward").is_ok() {
                return Ok(());
            }
            lock::with_cycle_lock(|| run_cycle_direct(&wm, &config, CycleOp::Backward))?;
        }

        "stop" => {
            println!("Inari durduruluyor...");
            stop_command();
            println!("✓ Inari durduruldu");
        }

        "init-config" => {
            Config::save_default()?;
        }

        // Double-click: no command arg → go straight to the GUI start
        // path rather than printing help to a hidden console.
        "" => {
            start_command(wm, config)?;
        }

        // Handle switch command or numeric shorthand
        cmd => {
            // Check for "switch N" format
            let target = if cmd == "switch" {
                args.get(2).and_then(|s| s.parse::<usize>().ok())
            } else {
                // Check if it's just a number (shorthand)
                cmd.parse::<usize>().ok()
            };

            if let Some(target) = target {
                if daemon::send_command(&format!("switch:{}", target)).is_ok() {
                    return Ok(());
                }
                lock::with_cycle_lock(|| run_cycle_direct(&wm, &config, CycleOp::Switch(target)))?;
            } else {
                println!();
                println!("🦊  I N A R I  🦊");
                println!("     Inari Syndicate");
                println!();
                println!("Soru veya öneriler için GitHub'da issue açabilirsiniz.");
                println!();
                println!("Kullanım:");
                println!("  inari start         - Her şeyi başlat (daemon + önizlemeler)");
                println!("  inari stop          - Tüm Inari süreçlerini durdur");
                println!("  inari stack         - Tüm EVE pencerelerini üst üste diz");
                println!("  inari forward       - İleri geçiş");
                println!("  inari backward      - Geri geçiş");
                println!("  inari switch N      - N numaralı istemciye geç (hedefli geçiş)");
                println!("  inari N             - switch N için kısa yol");
                println!("  inari init-config   - Varsayılan config.toml oluştur");
                println!();
                println!("Gelişmiş:");
                println!("  inari daemon        - Yalnızca daemon'u başlat");
                println!();
                println!("Hızlı başlangıç:");
                println!("  inari start         # Arka planda otomatik çalışır");
            }
        }
    }

    Ok(())
}
