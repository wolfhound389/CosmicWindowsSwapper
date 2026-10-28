mod app;
mod blur;
mod icons;
mod shot;
mod thumbs;
mod ui;
mod wayland;

use anyhow::Result;
use clap::Parser;
use cosmic::cosmic_config::{Config, ConfigGet};
use std::io::Write;
use std::os::unix::net::UnixStream;
use switcher_config::{Style, Theme, WorkspaceScope, APP_ID, CONFIG_VERSION};

pub fn socket_path() -> std::path::PathBuf {
    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    std::path::PathBuf::from(dir).join("cosmic-windows-swapper.sock")
}

#[derive(Parser, Debug)]
#[command(name = "cosmic-windows-swapper", version)]
pub struct Args {
    /// Move the selection backwards (Shift+Tab).
    #[arg(long, default_value_t = false)]
    pub reverse: bool,

    /// Development aid: capture the whole screen to a PPM file and exit.
    #[arg(long, value_name = "PATH")]
    pub screenshot: Option<std::path::PathBuf>,
}

/// Where cosmic-config keeps this app's files (mirrors its own path scheme).
fn config_dir() -> Option<std::path::PathBuf> {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|_| std::env::var("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))
        .ok()?;
    Some(base.join("cosmic").join(APP_ID).join(format!("v{CONFIG_VERSION}")))
}

fn migrate_config_once() {
    let Some(dir) = config_dir() else { return };
    if let Some(from) = switcher_config::migrate_legacy_config(&dir) {
        log::info!("migrated config from {} to {}", from.display(), dir.display());
    }
}

fn load_config() -> (Theme, WorkspaceScope, Style) {
    migrate_config_once();
    let Ok(config) = Config::new(APP_ID, CONFIG_VERSION) else {
        return (Theme::default(), WorkspaceScope::default(), Style::default());
    };
    let theme = config.get::<Theme>("theme").unwrap_or_default();
    let scope = config.get::<WorkspaceScope>("workspace_scope").unwrap_or_default();
    let style = config.get::<Style>("style").unwrap_or_default();
    (theme, scope, style)
}

/// Preferred preview resolution: twice the on-screen size keeps text crisp on HiDPI and
/// still downscales cleanly on 1x displays.
fn preview_resolution(style: &Style) -> u32 {
    (style.thumb_width as u32).saturating_mul(2).max(320)
}

fn main() -> Result<()> {
    env_logger::init();
    let args = Args::parse();

    if let Some(path) = &args.screenshot {
        return shot::capture_screen(path);
    }

    // If a switcher is already running, signal it to cycle and exit.
    let cmd = if args.reverse { b"prev" as &[u8] } else { b"next" as &[u8] };
    if let Ok(mut s) = UnixStream::connect(socket_path()) {
        let _ = s.write_all(cmd);
        return Ok(());
    }

    // We are the first instance. Clean up any stale socket from a crash.
    let _ = std::fs::remove_file(socket_path());

    let (theme, scope, style) = load_config();

    let switcher = wayland::spawn(scope, preview_resolution(&style), style.radius * 2.0)?;

    if switcher.entries.is_empty() {
        return Ok(());
    }

    app::run(switcher, args.reverse, theme, style)
}
