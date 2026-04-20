use crate::config::Config;
use anyhow::Result;

#[derive(Debug, Clone)]
pub struct EveWindow {
    pub id: u32,
    pub title: String,
}

/// Trait for window management. Kept as a trait (rather than a concrete
/// struct) so the rest of the codebase still goes through a polymorphic
/// boundary — leaves room for future alternate backends (e.g. a
/// headless/mock implementation for tests).
pub trait WindowManager: Send + Sync {
    /// Get all EVE Online client windows
    fn get_eve_windows(&self) -> Result<Vec<EveWindow>>;

    /// Activate/focus a specific window by ID
    fn activate_window(&self, window_id: u32) -> Result<()>;

    /// Stack all EVE windows at the same position (centered)
    fn stack_windows(&self, windows: &[EveWindow], config: &Config) -> Result<()>;

    /// Get the currently active window ID
    fn get_active_window(&self) -> Result<u32>;

    /// Minimize a window
    fn minimize_window(&self, window_id: u32) -> Result<()>;

    /// Restore a minimized window
    fn restore_window(&self, window_id: u32) -> Result<()>;
}
