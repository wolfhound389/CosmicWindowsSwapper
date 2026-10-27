use cosmic::{
    app::{Application, Core, Task},
    iced::{
        Alignment, Background, Border, Color, Length,
        window::Id as WindowId,
        widget::container::Style as ContainerStyle,
    },
    widget::{column, container, mouse_area, row, text},
    Element,
};
use cosmic::iced::platform_specific::shell::commands::popup::{destroy_popup, get_popup};
use cosmic::cosmic_config::{ConfigGet, ConfigSet};
use switcher_config::{Layout, Style, Theme, WorkspaceScope, APP_ID, CONFIG_VERSION};

const APPLET_ID: &str = "io.github.wolfhound389.CosmicWindowsSwapper.applet";
/// Binary name of the overlay, also the marker we look for in the COSMIC shortcut map.
const SWITCHER_BIN: &str = "cosmic-windows-swapper";
/// Pre-rename names, still recognised so an old shortcut binding gets rewritten.
const LEGACY_SWITCHER_BIN: &str = "cosmic-ext-app-switcher";

pub struct AppletApp {
    core:                Core,
    popup:               Option<WindowId>,
    current_theme:       Theme,
    current_scope:       WorkspaceScope,
    current_style:       Style,
    config_handler:      Option<cosmic::cosmic_config::Config>,
    shortcut_configured: bool,
    shortcut_error:      Option<String>,
}

#[derive(Debug, Clone)]
pub enum Message {
    TogglePopup,
    PopupClosed(WindowId),
    SetTheme(Theme),
    SetScope(WorkspaceScope),
    SetLayout(Layout),
    SetPreviewSize(u16, u16),
    ToggleTitles(bool),
    ToggleShortcut(bool),
}

// ---------------------------------------------------------------------------
// Shortcut helpers
// ---------------------------------------------------------------------------

fn shortcuts_config_path() -> Option<std::path::PathBuf> {
    let config_home = std::env::var("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config")
        });

    let shortcuts_dir = config_home
        .join("cosmic")
        .join("com.system76.CosmicSettings.Shortcuts");

    if !shortcuts_dir.exists() {
        return None;
    }

    // Mirror find-config.sh: find existing system_actions in highest version dir.
    if let Ok(entries) = std::fs::read_dir(&shortcuts_dir) {
        let mut version_dirs: Vec<_> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir() && e.file_name().to_string_lossy().starts_with('v'))
            .map(|e| e.path())
            .collect();
        version_dirs.sort_by_key(|p| {
            p.file_name()
                .and_then(|n| {
                    let s = n.to_string_lossy();
                    s.strip_prefix('v').and_then(|v| v.parse::<u32>().ok())
                })
                .unwrap_or(0)
        });

        for dir in version_dirs.iter().rev() {
            let p = dir.join("system_actions");
            if p.exists() {
                return Some(p);
            }
        }

        if let Some(latest) = version_dirs.last() {
            return Some(latest.join("system_actions"));
        }
    }

    Some(shortcuts_dir.join("v1").join("system_actions"))
}

/// Copy a pre-rename config over on first use so the chosen theme/scope survives.
fn migrate_config_once() {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|_| std::env::var("HOME").map(|h| std::path::PathBuf::from(h).join(".config")));
    if let Ok(base) = base {
        let dir = base.join("cosmic").join(APPLET_ID).join(format!("v{CONFIG_VERSION}"));
        switcher_config::migrate_legacy_config(&dir);
    }
}

/// Path of the shared config file the overlay also reads (`style` lives there).
fn config_file() -> Option<std::path::PathBuf> {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|_| std::env::var("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))
        .ok()?;
    Some(
        base.join("cosmic")
            .join(APP_ID)
            .join(format!("v{CONFIG_VERSION}"))
            .join(format!("{APP_ID}.ron")),
    )
}

/// Read the `style` field out of that file, ignoring the other keys.
fn read_style() -> Option<Style> {
    let text = std::fs::read_to_string(config_file()?).ok()?;
    let start = text.find("style:")? + "style:".len();
    let rest = &text[start..];
    // The value ends at the line's trailing comma; anything else is a broken file.
    let end = rest.find(",\n").or_else(|| rest.find(','))?;
    ron::from_str::<Style>(rest[..end].trim()).ok()
}

/// Rewrite just the `style` field, leaving every other key in the file untouched.
///
/// The file is edited as text on purpose: round-tripping it through `ron::Value` loses
/// information (unit enum variants such as `Dark` come back as `()`, so the theme would
/// be silently reset every time the popup is used).
fn write_style(style: &Style) -> Result<(), String> {
    let path = config_file().ok_or("no config directory")?;
    let existing = std::fs::read_to_string(&path).unwrap_or_else(|_| "{\n}\n".to_owned());
    let encoded = ron::ser::to_string(style).map_err(|e| e.to_string())?;

    let mut body = String::new();
    let mut written = false;
    for line in existing.lines() {
        if line.trim_start().starts_with("style:") {
            if !written {
                body.push_str(&format!("    style: {encoded},\n"));
                written = true;
            }
            continue;
        }
        body.push_str(line);
        body.push('\n');
    }
    if !written {
        // First write: put it just before the closing brace of the map.
        let insert_at = body.trim_end().rfind('}').unwrap_or(body.len());
        body.insert_str(insert_at, &format!("    style: {encoded},\n"));
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, body).map_err(|e| e.to_string())
}

fn shortcut_is_configured() -> bool {
    shortcuts_config_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.contains(SWITCHER_BIN) || s.contains(LEGACY_SWITCHER_BIN))
        .unwrap_or(false)
}

fn switcher_exec() -> String {
    if std::env::var("FLATPAK_ID").is_ok() {
        format!("flatpak run --command=cosmic-windows-swapper {APPLET_ID}")
    } else {
        let home = std::env::var("HOME").unwrap_or_default();
        format!("{home}/.local/bin/cosmic-windows-swapper")
    }
}

/// Rebuild the `system_actions` RON map: every entry except our two keys, wrapped in a
/// fresh pair of braces, with our entries appended when `cmd` is `Some`.
///
/// Rebuilding rather than splicing is what keeps degenerate inputs safe — an empty file
/// or a single-line `{}` used to produce a body with no opening brace. That doesn't fail
/// loudly: cosmic-settings-daemon logs a parse error and falls back to the packaged
/// defaults, so the user's other overrides quietly stop working (issue #10). Mirrors
/// scripts/shortcut-config.sh.
fn rebuild_shortcut_map(existing: Option<&str>, cmd: Option<&str>) -> String {
    let mut out = String::from("{\n");

    for line in existing.unwrap_or_default().lines() {
        let t = line.trim();
        if t.is_empty() || t == "{" || t == "}" || t == "{}" {
            continue;
        }
        if t.starts_with("WindowSwitcher:") || t.starts_with("WindowSwitcherPrevious:") {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }

    if let Some(cmd) = cmd {
        out.push_str(&format!("    WindowSwitcher: \"{cmd}\",\n"));
        out.push_str(&format!("    WindowSwitcherPrevious: \"{cmd} --reverse\",\n"));
    }

    out.push('}');
    out.push('\n');
    out
}

fn write_shortcut_map(path: &std::path::Path, content: String) -> Result<(), String> {
    if !content.starts_with('{') || !content.trim_end().ends_with('}') {
        return Err("refusing to write malformed shortcut config".to_string());
    }
    std::fs::write(path, content).map_err(|e| e.to_string())?;
    // cosmic-config writes these 0600; keep that when we create the file ourselves.
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    Ok(())
}

fn do_unregister_shortcut() -> Result<(), String> {
    let path = match shortcuts_config_path() {
        Some(p) => p,
        None => return Ok(()),
    };
    if !path.exists() {
        return Ok(());
    }
    let content = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    write_shortcut_map(&path, rebuild_shortcut_map(Some(&content), None))
}

fn do_register_shortcut() -> Result<(), String> {
    let path = shortcuts_config_path()
        .ok_or_else(|| "COSMIC shortcuts directory not found — is COSMIC installed?".to_string())?;

    let existing = std::fs::read_to_string(&path).ok();
    if existing
        .as_deref()
        .is_some_and(|s| s.contains(SWITCHER_BIN))
    {
        return Ok(());
    }

    std::fs::create_dir_all(path.parent().unwrap())
        .map_err(|e| e.to_string())?;

    let cmd = switcher_exec();
    write_shortcut_map(&path, rebuild_shortcut_map(existing.as_deref(), Some(&cmd)))
}

// ---------------------------------------------------------------------------
// Application impl
// ---------------------------------------------------------------------------

impl AppletApp {
    /// Persist the edited `Style` so the next overlay launch picks it up.
    fn save_style(&self) {
        if let Some(handler) = &self.config_handler {
            let _ = handler.set("style", &self.current_style);
        } else {
            let _ = write_style(&self.current_style);
        }
    }

}

impl Application for AppletApp {
    type Executor = cosmic::executor::Default;
    type Flags    = ();
    type Message  = Message;

    const APP_ID: &'static str = APPLET_ID;

    fn core(&self) -> &Core { &self.core }
    fn core_mut(&mut self) -> &mut Core { &mut self.core }

    fn init(core: Core, _flags: ()) -> (Self, Task<Message>) {
        migrate_config_once();
        let config_handler = cosmic::cosmic_config::Config::new(APP_ID, CONFIG_VERSION).ok();
        let current_theme  = config_handler
            .as_ref()
            .and_then(|c| c.get::<Theme>("theme").ok())
            .unwrap_or_default();
        let current_scope  = config_handler
            .as_ref()
            .and_then(|c| c.get::<WorkspaceScope>("workspace_scope").ok())
            .unwrap_or_default();
        // The overlay owns `style`; the applet only edits it. Read it from disk rather
        // than from our own config handle: the handle caches the keys present when the
        // applet started, so a `style` written by the overlay later would look absent.
        let current_style  = read_style().unwrap_or_default();

        (
            Self {
                core,
                popup: None,
                current_theme,
                current_scope,
                current_style,
                config_handler,
                shortcut_configured: shortcut_is_configured(),
                shortcut_error: None,
            },
            Task::none(),
        )
    }

    fn style(&self) -> Option<cosmic::iced::theme::Style> {
        Some(cosmic::applet::style())
    }

    fn on_close_requested(&self, id: WindowId) -> Option<Message> {
        Some(Message::PopupClosed(id))
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::TogglePopup => {
                if let Some(popup_id) = self.popup.take() {
                    return destroy_popup(popup_id);
                }
                let new_id = WindowId::unique();
                self.popup = Some(new_id);
                let popup_settings = self.core.applet.get_popup_settings(
                    self.core.main_window_id().unwrap(),
                    new_id,
                    None,
                    None,
                    None,
                );
                return get_popup(popup_settings);
            }
            Message::PopupClosed(id) => {
                if self.popup == Some(id) {
                    self.popup = None;
                }
            }
            Message::SetTheme(theme) => {
                if let Some(handler) = &self.config_handler {
                    let _ = handler.set("theme", &theme);
                }
                self.current_theme = theme;
            }
            Message::SetScope(scope) => {
                if let Some(handler) = &self.config_handler {
                    let _ = handler.set("workspace_scope", scope);
                }
                self.current_scope = scope;
            }
            Message::SetLayout(layout) => {
                self.current_style.layout = layout;
                self.save_style();
            }
            Message::SetPreviewSize(w, h) => {
                self.current_style.thumb_width = w;
                self.current_style.thumb_height = h;
                self.save_style();
            }
            Message::ToggleTitles(enable) => {
                self.current_style.show_titles = enable;
                if !enable {
                    // Icons identify the window once the titles are gone.
                    self.current_style.show_icons = true;
                }
                self.save_style();
            }
            Message::ToggleShortcut(enable) => {
                let result = if enable {
                    do_register_shortcut()
                } else {
                    do_unregister_shortcut()
                };
                match result {
                    Ok(()) => {
                        self.shortcut_configured = enable;
                        self.shortcut_error = None;
                    }
                    Err(e) => self.shortcut_error = Some(e),
                }
            }
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Message> {
        let mut handle = cosmic::widget::icon::from_svg_bytes(
            include_bytes!("../data/io.github.wolfhound389.CosmicWindowsSwapper.applet-symbolic.svg")
                as &'static [u8],
        );
        handle.symbolic = true;
        self.core
            .applet
            .icon_button_from_handle(handle)
            .on_press(Message::TogglePopup)
            .into()
    }

    fn view_window(&self, _id: WindowId) -> Element<'_, Message> {
        let swatches: Vec<Element<Message>> = Theme::all()
            .into_iter()
            .map(|t| {
                let preview_bg = {
                    let p = t.preview_bg();
                    Color::from_rgb(p[0], p[1], p[2])
                };
                let selected = t == self.current_theme;
                let label = t.label().to_string();
                let t_clone = t.clone();

                let swatch = container(
                    cosmic::widget::Space::new()
                )
                .width(Length::Fixed(68.0))
                .height(Length::Fixed(40.0))
                .style(move |_: &cosmic::Theme| ContainerStyle {
                    background: Some(Background::Color(preview_bg)),
                    border: Border {
                        radius: 6.0.into(),
                        width: if selected { 2.5 } else { 0.0 },
                        color: Color::from_rgb(0.38, 0.58, 1.0),
                    },
                    ..Default::default()
                });

                let card = column![
                    swatch,
                    text(label).size(11),
                ]
                .spacing(4)
                .align_x(Alignment::Center);

                mouse_area(card)
                    .on_press(Message::SetTheme(t_clone))
                    .into()
            })
            .collect();

        let scope_options: Vec<Element<Message>> = WorkspaceScope::all()
            .into_iter()
            .map(|s| {
                let selected = s == self.current_scope;
                let label = s.label().to_string();

                let card = container(text(label).size(12))
                    .padding([6, 10])
                    .style(move |_: &cosmic::Theme| ContainerStyle {
                        border: Border {
                            radius: 6.0.into(),
                            width: if selected { 2.0 } else { 1.0 },
                            color: if selected {
                                Color::from_rgb(0.38, 0.58, 1.0)
                            } else {
                                Color::from_rgba(1.0, 1.0, 1.0, 0.15)
                            },
                        },
                        ..Default::default()
                    });

                mouse_area(card).on_press(Message::SetScope(s)).into()
            })
            .collect();

        let flatpak_note: Option<Element<Message>> = std::env::var("FLATPAK_ID").ok().map(|_| {
            // Nothing of ours runs at `flatpak uninstall`, so a shortcut left registered
            // keeps Super+Tab pointing at a command that no longer exists — and COSMIC
            // does not fall back to its built-in switcher (issue #10).
            text("Flatpak: turn this off before uninstalling, or Super+Tab stays bound to a removed app.")
                .size(10)
                .into()
        });

        let shortcut_row = row![
            text("Super+Tab shortcut").size(14),
            cosmic::widget::Space::new().width(Length::Fill),
            cosmic::widget::toggler(self.shortcut_configured)
                .on_toggle(Message::ToggleShortcut),
        ]
        .align_y(Alignment::Center);

        let mut shortcut_section = column![shortcut_row].spacing(6);
        if let Some(note) = flatpak_note {
            shortcut_section = shortcut_section.push(note);
        }

        let layout_options: Vec<Element<Message>> = Layout::all()
            .iter()
            .map(|l| {
                let l = *l;
                let chosen = l == self.current_style.layout;
                let card = container(text(l.label()).size(12))
                    .padding([6, 12])
                    .style(move |_: &cosmic::Theme| ContainerStyle {
                        border: Border {
                            radius: 6.0.into(),
                            width: if chosen { 2.0 } else { 1.0 },
                            color: if chosen {
                                Color::from_rgb(0.38, 0.58, 1.0)
                            } else {
                                Color::from_rgba(1.0, 1.0, 1.0, 0.15)
                            },
                        },
                        ..Default::default()
                    });
                mouse_area(card).on_press(Message::SetLayout(l)).into()
            })
            .collect();

        let size_options: Vec<Element<Message>> = Style::preset_sizes()
            .iter()
            .map(|(w, h, label)| {
                let (w, h) = (*w, *h);
                let chosen = self.current_style.thumb_width == w;
                let card = container(text(*label).size(12))
                    .padding([6, 16])
                    .style(move |_: &cosmic::Theme| ContainerStyle {
                        border: Border {
                            radius: 6.0.into(),
                            width: if chosen { 2.0 } else { 1.0 },
                            color: if chosen {
                                Color::from_rgb(0.38, 0.58, 1.0)
                            } else {
                                Color::from_rgba(1.0, 1.0, 1.0, 0.15)
                            },
                        },
                        ..Default::default()
                    });
                mouse_area(card)
                    .on_press(Message::SetPreviewSize(w, h))
                    .into()
            })
            .collect();

        // Preview size only affects the grid; the icon strip is sized by the icons.
        let mut size_section: Vec<Element<Message>> =
            vec![text("Preview size").size(14).into()];
        if self.current_style.layout == Layout::Grid {
            size_section.push(row(size_options).spacing(8).into());
        } else {
            size_section.push(
                text("Only used by the preview grid").size(10).into(),
            );
        }

        let mut content = column![
            shortcut_section,
            cosmic::widget::divider::horizontal::default(),
            text("Theme").size(14),
            row(swatches).spacing(12),
            cosmic::widget::divider::horizontal::default(),
            text("Layout").size(14),
            row(layout_options).spacing(8),
            column(size_section).spacing(8),
            row![
                cosmic::widget::checkbox(self.current_style.show_titles)
                    .on_toggle(Message::ToggleTitles),
                text("Show window titles").size(12),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
            cosmic::widget::divider::horizontal::default(),
            text("Switch scope").size(14),
            row(scope_options).spacing(8),
            // Shown so bug reports can name a version — see the release checklist in
            // .claude/CLAUDE.md.
            text(format!("v{}", env!("CARGO_PKG_VERSION"))).size(10),
        ]
        .spacing(12)
        .padding(16)
        .align_x(Alignment::Center);

        if let Some(err) = &self.shortcut_error {
            content = column![content, text(format!("Error: {err}")).size(10)]
                .spacing(4)
                .align_x(Alignment::Center);
        }

        self.core.applet.popup_container(content).into()
    }
}

#[cfg(test)]
mod tests {
    use super::{rebuild_shortcut_map, Style};

    const CMD: &str = "/home/u/.local/bin/cosmic-windows-swapper";

        #[test]
    fn style_write_keeps_the_rest_of_the_file() {
        let dir = std::env::temp_dir().join(format!("cws-style-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cfg.ron");

        // A file as COSMIC writes it: bare enum variant, plus keys we must not disturb.
        std::fs::write(&path, "{\n    theme: Dark,\n    workspace_scope: AllWorkspaces,\n}\n").unwrap();
        let style = Style::default().with_size(220, 125);
        let encoded = ron::ser::to_string(&style).unwrap();

        let existing = std::fs::read_to_string(&path).unwrap();
        let mut body = String::new();
        for line in existing.lines() {
            body.push_str(line);
            body.push('\n');
        }
        let insert_at = body.trim_end().rfind('}').unwrap();
        body.insert_str(insert_at, &format!("    style: {encoded},\n"));
        std::fs::write(&path, &body).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("theme: Dark"));
        assert!(text.contains("workspace_scope: AllWorkspaces"));
        assert_eq!(text.matches("style:").count(), 1, "{text}");
        // The whole file must still parse as a RON map.
        let parsed = ron::from_str::<ron::Value>(&text).expect("file parses");
        assert!(matches!(parsed, ron::Value::Map(_)));

        std::fs::remove_dir_all(&dir).ok();
    }

    fn assert_well_formed(s: &str) {
        assert!(s.starts_with("{\n"), "missing opening brace: {s:?}");
        assert!(s.ends_with("}\n"), "missing closing brace: {s:?}");
    }

    /// The inputs that used to yield a body with no opening brace.
    #[test]
    fn degenerate_inputs_stay_well_formed() {
        for existing in [None, Some(""), Some("{}"), Some("{}\n"), Some("{\n}\n"), Some("\n\n")] {
            let out = rebuild_shortcut_map(existing, Some(CMD));
            assert_well_formed(&out);
            assert_eq!(out.matches("WindowSwitcher:").count(), 1, "for {existing:?}");
        }
    }

    #[test]
    fn other_entries_are_preserved() {
        let existing = "{\n    Terminal: \"ghostty\",\n    WebBrowser: \"firefox\",\n}\n";
        let out = rebuild_shortcut_map(Some(existing), Some(CMD));
        assert_well_formed(&out);
        assert!(out.contains("Terminal: \"ghostty\""));
        assert!(out.contains("WebBrowser: \"firefox\""));
        assert!(out.contains(&format!("WindowSwitcher: \"{CMD}\"")));
        assert!(out.contains(&format!("WindowSwitcherPrevious: \"{CMD} --reverse\"")));
    }

    #[test]
    fn existing_bindings_are_replaced_not_duplicated() {
        let existing = "{\n    WindowSwitcher: \"/old\",\n    WindowSwitcherPrevious: \"/old --reverse\",\n}\n";
        let out = rebuild_shortcut_map(Some(existing), Some(CMD));
        assert_eq!(out.matches("WindowSwitcher:").count(), 1);
        assert_eq!(out.matches("WindowSwitcherPrevious:").count(), 1);
        assert!(!out.contains("/old"));
    }

    #[test]
    fn unregister_leaves_a_valid_map() {
        let existing = "{\n    Terminal: \"ghostty\",\n    WindowSwitcher: \"x\",\n    WindowSwitcherPrevious: \"x --reverse\",\n}\n";
        let out = rebuild_shortcut_map(Some(existing), None);
        assert_well_formed(&out);
        assert!(!out.contains("WindowSwitcher"));
        assert!(out.contains("Terminal: \"ghostty\""));

        // …and an empty map when we were the only entry.
        let only_ours = "{\n    WindowSwitcher: \"x\",\n}\n";
        assert_eq!(rebuild_shortcut_map(Some(only_ours), None), "{\n}\n");
    }
}
