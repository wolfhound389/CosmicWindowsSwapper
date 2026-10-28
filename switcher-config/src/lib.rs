use serde::{Deserialize, Serialize};

pub const APP_ID: &str = "io.github.wolfhound389.CosmicWindowsSwapper";
pub const CONFIG_VERSION: u64 = 1;

/// App IDs used before the project was renamed to CosmicWindowsSwapper, newest first.
pub const LEGACY_APP_IDS: [&str; 1] = ["io.github.cosmic-ext-app-switcher"];

/// Copy a config directory from an older app id the first time the new id is used, so a
/// theme picked before the rename survives it. Returns the source directory that was
/// copied, if any. Never overwrites an existing non-empty target.
pub fn migrate_legacy_config(target_dir: &std::path::Path) -> Option<std::path::PathBuf> {
    // Only refuse when the target already holds something *and* it really is a directory:
    // a missing (or unreadable) target is exactly the fresh-install case we want to seed.
    if target_dir.is_dir() && target_dir.read_dir().map(|mut d| d.next().is_some()).unwrap_or(true) {
        return None;
    }
    let cosmic = target_dir.parent()?.parent()?;
    for old in LEGACY_APP_IDS {
        let src = cosmic.join(old).join(format!("v{CONFIG_VERSION}"));
        if src.is_dir() {
            copy_dir(&src, target_dir).ok()?;
            return Some(src);
        }
    }
    None
}

fn copy_dir(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let dst = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &dst)?;
        } else {
            std::fs::copy(entry.path(), &dst)?;
        }
    }
    Ok(())
}

/// How the overlay arranges the windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Layout {
    /// Live window previews wrapped into a grid of several rows.
    #[default]
    Grid,
    /// Live window previews on a single horizontal line.
    Row,
    /// The original macOS-style horizontal strip of app icons.
    Strip,
}

/// How the single-line (`Layout::Row`) representation behaves when the windows do not
/// all fit on screen.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RowMode {
    /// Keep the selected card visible and slide the row along, hiding the cards that
    /// have scrolled past the left edge. The row stays still while every card fits.
    #[default]
    Slide,
    /// A carousel: the leftmost card always leaves the screen and the newly selected one
    /// enters from the right, so there is continuous motion on every focus change.
    Carousel,
}

impl RowMode {
    pub fn label(&self) -> &'static str {
        match self {
            RowMode::Slide    => "Slide",
            RowMode::Carousel => "Carousel",
        }
    }

    /// One-line description used as a tooltip in the settings applet.
    pub fn description(&self) -> &'static str {
        match self {
            RowMode::Slide    => "Slide the row only when the selection leaves the screen",
            RowMode::Carousel => "Every move pushes the leftmost window off the screen",
        }
    }

    pub fn all() -> [RowMode; 2] {
        [RowMode::Slide, RowMode::Carousel]
    }
}

impl Layout {
    pub fn label(&self) -> &'static str {
        match self {
            Layout::Grid  => "Grid",
            Layout::Row   => "Row",
            Layout::Strip => "Icons",
        }
    }

    /// One-line description used as a tooltip in the settings applet.
    pub fn description(&self) -> &'static str {
        match self {
            Layout::Grid  => "Window previews in a grid",
            Layout::Row   => "Window previews in a single line",
            Layout::Strip => "App icons only",
        }
    }

    /// Does this layout draw live window previews (and therefore supports the
    /// per-window close button and preview sizing)?
    pub fn has_previews(&self) -> bool {
        matches!(self, Layout::Grid | Layout::Row)
    }

    pub fn all() -> [Layout; 3] {
        [Layout::Grid, Layout::Row, Layout::Strip]
    }
}

/// Look-and-feel knobs shared by both layouts. Every field is optional in the config
/// file: missing fields fall back to `Default`, so older configs keep working.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Style {
    pub layout:       Layout,
    /// Preferred preview width in logical pixels (Grid only).
    pub thumb_width:  u16,
    /// Preferred preview height in logical pixels (Grid only).
    pub thumb_height: u16,
    /// Gap between cards.
    pub gap:          u16,
    /// Padding inside the overlay panel.
    pub padding:      u16,
    /// Fixed column count for the grid; 0 = pick automatically.
    pub columns:      u8,
    pub show_titles:  bool,
    /// Small app icon next to the title (Grid) — the Strip layout always shows icons.
    pub show_icons:   bool,
    /// Corner radius of the preview cards.
    pub radius:       f32,
    /// Thickness of the selection ring.
    pub border:       f32,
    /// Draw a small "close window" button on every preview (Grid and Row layouts).
    pub show_close:   bool,
    /// How the single-line layout behaves (Row only).
    pub row_mode:     RowMode,
    /// Animation speed multiplier: 0.5 is half speed, 5.0 five times faster.
    pub anim_speed:   f32,
    /// How strongly the desktop behind the overlay is dimmed, 0.0 (untouched) to
    /// 1.0 (opaque). The compositor's blur is fixed; this is the tint on top of it.
    pub blur_strength: f32,
}

/// Slowest animation speed the settings applet offers.
pub const ANIM_SPEED_MIN: f32 = 0.5;
/// Fastest animation speed the settings applet offers.
pub const ANIM_SPEED_MAX: f32 = 5.0;
/// Step between the animation speed presets.
pub const ANIM_SPEED_STEP: f32 = 0.5;

/// Speed presets shown in the settings applet: x0.5, x1.0, ... x5.0.
pub fn anim_speed_presets() -> Vec<f32> {
    let count = ((ANIM_SPEED_MAX - ANIM_SPEED_MIN) / ANIM_SPEED_STEP).round() as usize;
    (0..=count)
        .map(|i| ANIM_SPEED_MIN + i as f32 * ANIM_SPEED_STEP)
        .collect()
}

/// Weakest backdrop dimming the settings applet offers.
pub const BLUR_MIN: f32 = 0.0;
/// Strongest backdrop dimming the settings applet offers.
pub const BLUR_MAX: f32 = 1.0;
/// Step between the backdrop dimming presets, in whole percent.
///
/// Whole-percent steps keep every preset exactly representable as `pct / 100.0`, so the
/// pill for the default 22% matches the stored value bit for bit.
pub const BLUR_STEP_PCT: u32 = 10;

/// Backdrop dimming presets shown in the settings applet: 0%, 10%, ... 100%.
pub fn blur_presets() -> Vec<f32> {
    (0..=100 / BLUR_STEP_PCT)
        .map(|i| (i * BLUR_STEP_PCT) as f32 / 100.0)
        .collect()
}

impl Style {
    /// Backdrop dimming, clamped to the supported range. A hand-edited config file
    /// cannot make the overlay fully transparent or hide the desktop entirely.
    pub fn blur_strength(&self) -> f32 {
        if self.blur_strength.is_finite() {
            self.blur_strength.clamp(BLUR_MIN, BLUR_MAX)
        } else {
            // A corrupt value must not wipe the backdrop, so fall back to the default.
            Style::default().blur_strength
        }
    }

    /// Animation speed multiplier, clamped to the supported range so a hand-edited
    /// config file can never stall or teleport the animation.
    pub fn anim_speed(&self) -> f32 {
        if self.anim_speed.is_finite() {
            self.anim_speed.clamp(ANIM_SPEED_MIN, ANIM_SPEED_MAX)
        } else {
            1.0
        }
    }
}

impl Default for Style {
    fn default() -> Self {
        Self {
            layout:       Layout::Grid,
            thumb_width:  300,
            thumb_height: 170,
            gap:          14,
            padding:      16,
            columns:      0,
            show_titles:  true,
            show_icons:   true,
            radius:       10.0,
            border:       2.5,
            show_close:   true,
            row_mode:     RowMode::default(),
            anim_speed:   1.0,
            blur_strength: 0.20,
        }
    }
}

impl Style {
    /// Compact size presets offered by the panel applet.
    pub fn preset_sizes() -> [(u16, u16, &'static str); 3] {
        [(220, 125, "S"), (300, 170, "M"), (400, 225, "L")]
    }

    pub fn with_size(mut self, width: u16, height: u16) -> Self {
        self.thumb_width  = width;
        self.thumb_height = height;
        self
    }

    /// Column count for `n` cards, honouring a non-zero `columns` override.
    pub fn columns_for(&self, n: usize) -> usize {
        if n == 0 {
            return 1;
        }
        if !self.layout.has_previews() {
            return n;
        }
        if self.columns > 0 {
            return (self.columns as usize).min(n);
        }
        if self.layout == Layout::Row {
            // Everything on one line; `Metrics::fit` shrinks the previews when they
            // no longer fit across the screen.
            return n;
        }
        // Roughly square grid: a 16:9 screen shows a 16:9 block of 16:9 previews.
        let cols = ((n as f64).sqrt().ceil() as usize).clamp(1, 5);
        cols.min(n)
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub enum Theme {
    #[default]
    Dark,
    Light,
    Frosted,
    Midnight,
}

#[derive(Debug, Clone)]
pub struct ThemeValues {
    /// Overlay panel background.
    pub bg:            [f32; 4],
    /// Unselected card background.
    pub card:          [f32; 4],
    /// Selected card background.
    pub selected_bg:   [f32; 4],
    /// Selection ring / accent colour.
    pub accent:        [f32; 4],
    /// Title text colour.
    pub text:          [f32; 4],
    pub corner_radius: f32,
    pub icon_size:     u16,
}

impl Theme {
    pub fn values(&self) -> ThemeValues {
        match self {
            Theme::Dark => ThemeValues {
                bg:            [0.11, 0.11, 0.12, 0.94],
                card:          [0.17, 0.17, 0.18, 0.95],
                selected_bg:   [0.24, 0.24, 0.26, 0.98],
                accent:        [0.55, 0.72, 1.00, 1.00],
                text:          [0.94, 0.94, 0.95, 1.00],
                corner_radius: 16.0,
                icon_size:     60,
            },
            Theme::Light => ThemeValues {
                bg:            [0.96, 0.96, 0.97, 0.94],
                card:          [0.90, 0.90, 0.92, 0.98],
                selected_bg:   [1.00, 1.00, 1.00, 1.00],
                accent:        [0.13, 0.45, 0.85, 1.00],
                text:          [0.10, 0.10, 0.12, 1.00],
                corner_radius: 16.0,
                icon_size:     60,
            },
            Theme::Frosted => ThemeValues {
                bg:            [0.15, 0.15, 0.16, 0.62],
                card:          [0.22, 0.22, 0.24, 0.72],
                selected_bg:   [0.30, 0.30, 0.33, 0.85],
                accent:        [0.65, 0.80, 1.00, 1.00],
                text:          [0.96, 0.96, 0.97, 1.00],
                corner_radius: 20.0,
                icon_size:     60,
            },
            Theme::Midnight => ThemeValues {
                bg:            [0.05, 0.07, 0.14, 0.96],
                card:          [0.09, 0.12, 0.22, 0.98],
                selected_bg:   [0.16, 0.21, 0.36, 0.98],
                accent:        [0.45, 0.65, 1.00, 1.00],
                text:          [0.88, 0.91, 1.00, 1.00],
                corner_radius: 16.0,
                icon_size:     60,
            },
        }
    }

    pub fn label(&self) -> &str {
        match self {
            Theme::Dark     => "Dark",
            Theme::Light    => "Light",
            Theme::Frosted  => "Frosted",
            Theme::Midnight => "Midnight",
        }
    }

    pub fn all() -> [Theme; 4] {
        [Theme::Dark, Theme::Light, Theme::Frosted, Theme::Midnight]
    }

    pub fn preview_bg(&self) -> [f32; 3] {
        let v = self.values();
        [v.bg[0], v.bg[1], v.bg[2]]
    }

    pub fn accent_rgb(&self) -> [f32; 3] {
        let v = self.values();
        [v.accent[0], v.accent[1], v.accent[2]]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum WorkspaceScope {
    #[default]
    AllWorkspaces,
    CurrentWorkspace,
    CurrentOutput,
}

impl WorkspaceScope {
    pub fn label(&self) -> &str {
        match self {
            WorkspaceScope::AllWorkspaces   => "All workspaces",
            WorkspaceScope::CurrentWorkspace => "Current workspace",
            WorkspaceScope::CurrentOutput    => "Current monitor",
        }
    }

    // CurrentWorkspace is intentionally excluded: cosmic-comp never sends the legacy
    // workspace_enter/workspace_leave events this project's protocol binding relies on,
    // so it's currently a no-op (see src/wayland.rs). Left defined for a future migration
    // to ext_workspace_enter/leave, but hidden from the picker so it doesn't look like a
    // working option that silently does nothing.
    pub fn all() -> [WorkspaceScope; 2] {
        [WorkspaceScope::AllWorkspaces, WorkspaceScope::CurrentOutput]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_columns_are_sane() {
        let s = Style::default();
        assert_eq!(s.columns_for(0), 1);
        assert_eq!(s.columns_for(1), 1);
        assert_eq!(s.columns_for(2), 2);
        assert_eq!(s.columns_for(3), 2);
        assert_eq!(s.columns_for(4), 2);
        assert_eq!(s.columns_for(5), 3);
        assert_eq!(s.columns_for(9), 3);
        assert_eq!(s.columns_for(20), 5);
    }

    #[test]
    fn explicit_columns_are_honoured() {
        let s = Style { columns: 2, ..Default::default() };
        assert_eq!(s.columns_for(9), 2);
        assert_eq!(s.columns_for(1), 1);
    }

    #[test]
    fn blur_strength_is_clamped() {
        assert_eq!(Style::default().blur_strength(), 0.20);
        assert_eq!(Style { blur_strength: -1.0, ..Default::default() }.blur_strength(), BLUR_MIN);
        assert_eq!(Style { blur_strength: 9.0, ..Default::default() }.blur_strength(), BLUR_MAX);
        assert_eq!(Style { blur_strength: f32::NAN, ..Default::default() }.blur_strength(), 0.20);
    }

    #[test]
    fn blur_presets_span_the_whole_range() {
        let presets = blur_presets();
        assert_eq!(presets.first().copied(), Some(BLUR_MIN));
        assert_eq!(presets.last().copied(), Some(BLUR_MAX));
        // The default has to be selectable from the applet, otherwise the pill row
        // would open with nothing highlighted.
        assert_eq!(presets.len(), 11);
        assert!(presets.iter().any(|p| (p - Style::default().blur_strength()).abs() < 1e-3));
    }

    #[test]
    fn anim_speed_presets_span_the_whole_range() {
        let presets = anim_speed_presets();
        assert_eq!(presets, vec![0.5, 1.0, 1.5, 2.0, 2.5, 3.0, 3.5, 4.0, 4.5, 5.0]);
    }

    fn tmpdir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("cws-config-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn legacy_config_is_migrated_once() {
        let root = tmpdir("migrate");
        let cosmic = root.join("cosmic");
        let old = cosmic.join(LEGACY_APP_IDS[0]).join("v1");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("theme"), b"Midnight").unwrap();
        let new = cosmic.join(APP_ID).join("v1");

        // Migration copies the file…
        assert_eq!(migrate_legacy_config(&new).as_deref(), Some(old.as_path()));
        assert_eq!(std::fs::read(new.join("theme")).unwrap(), b"Midnight");

        // …and never runs again, so a later choice is not clobbered by the old value.
        std::fs::write(new.join("theme"), b"Light").unwrap();
        assert_eq!(migrate_legacy_config(&new), None);
        assert_eq!(std::fs::read(new.join("theme")).unwrap(), b"Light");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn migration_is_a_noop_without_a_legacy_config() {
        let root = tmpdir("nomigrate");
        let new = root.join("cosmic").join(APP_ID).join("v1");
        assert_eq!(migrate_legacy_config(&new), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn row_layout_puts_every_window_on_one_line() {
        let style = Style { layout: Layout::Row, ..Style::default() };
        assert_eq!(style.columns_for(7), 7);
        // An explicit column count still wins.
        let style = Style { layout: Layout::Row, columns: 3, ..Style::default() };
        assert_eq!(style.columns_for(7), 3);
        assert!(Layout::Row.has_previews());
        assert!(!Layout::Strip.has_previews());
    }

    #[test]
    fn size_presets_keep_aspect() {
        for (w, h, _) in Style::preset_sizes() {
            let ratio = w as f32 / h as f32;
            assert!((ratio - 16.0 / 9.0).abs() < 0.05, "{w}x{h} is not 16:9");
        }
    }
}

#[cfg(test)]
mod old_config_tests {
    #[test]
    fn config_without_new_fields_still_parses() {
        // A config written before row_mode/anim_speed/blur_strength existed.
        let old = "(layout: Row, thumb_width: 220, thumb_height: 125, gap: 14, padding: 16, \
                   columns: 0, show_titles: true, show_icons: true, radius: 10.0, border: 2.5, \
                   show_close: true)";
        let s: super::Style = ron::from_str(old).expect("old config must keep parsing");
        assert_eq!(s.thumb_width, 220);
        assert_eq!(s.anim_speed(), 1.0);
        assert_eq!(s.blur_strength(), 0.20);
    }
}
