use anyhow::Result;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use cosmic::{
    app::{Application, Core, Settings, Task},
    iced::{
        self,
        event,
        keyboard::{self, key::Named},
        widget::image,
        window::Id as WindowId,
        Event, Subscription,
    },
    iced::platform_specific::shell::wayland::commands::layer_surface::{
        Anchor, KeyboardInteractivity, Layer, destroy_layer_surface, get_layer_surface,
    },
    iced::platform_specific::runtime::wayland::layer_surface::SctkLayerSurfaceSettings,
    Element,
};
use switcher_config::{Layout, Style, Theme, ThemeValues};

use crate::ui::{Animation, Metrics};
use crate::wayland::{Command, Switcher, ThumbEvent, ToplevelEntry};

pub struct AppSwitcher {
    core:        Core,
    pub entries: Vec<ToplevelEntry>,
    /// Decoded previews, indexed like `entries`; `None` until the compositor delivers one.
    pub thumbs:  Vec<Option<image::Handle>>,
    pub selected: usize,
    pub metrics:  Metrics,
    pub style:    Style,
    pub theme:    ThemeValues,
    cmd_tx:       mpsc::SyncSender<Command>,
    thumb_rx:     mpsc::Receiver<ThumbEvent>,
    thumbs_done:  bool,
    super_held:   bool,
    alt_held:     bool,
    surface_id:   WindowId,
    test_deadline: Option<Instant>,
    finished:     bool,
    /// Animated scale of the selected card: it grows by `SELECTED_GROWTH` so the
    /// highlight is visible while the modifier key is still held. Values above 1.0 are
    /// interpreted by `ui` as "how far the selection animation has travelled".
    pub hover:    f32,
    /// Timestamp of the previous animation tick (for frame-rate independent easing).
    pub last_tick: Instant,
    /// Keys replayed so far from `COSMIC_SWITCHER_TEST_KEYS`.
    injected:     Vec<String>,
    /// Index whose close button the pointer is over (only for highlights).
    pub hovered:  Option<usize>,
}

#[derive(Debug, Clone)]
pub enum Message {
    SelectNext,
    SelectPrev,
    SelectIndex(usize),
    MoveRow(isize),
    Activate,
    /// Close the window at this index without switching to it.
    CloseIndex(usize),
    HoverCard(usize, bool),
    AnimTick,
    /// Replay the next entry of `COSMIC_SWITCHER_TEST_KEYS` (test hook).
    InjectKey,
    Cancel,
    KeyEvent(keyboard::Event),
    Tick,
}

/// Test hook: `COSMIC_SWITCHER_TEST_KEYS="Tab,Tab,Right,Enter"` replays key presses after
/// a short delay so an automated run exercises the same selection path a human would
/// (including the grow animation), without needing to inject input into the compositor.
fn test_keys() -> Vec<String> {
    std::env::var("COSMIC_SWITCHER_TEST_KEYS")
        .map(|v| v.split(',').map(|k| k.trim().to_string()).collect())
        .unwrap_or_default()
}

/// Delay before the first replayed key, in milliseconds.
fn test_key_delay() -> u64 {
    std::env::var("COSMIC_SWITCHER_TEST_KEY_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(400)
}

/// Test hook: `COSMIC_SWITCHER_TEST_MS=1500` opens the overlay without grabbing the
/// keyboard and closes it again after the given delay. Used for screenshots in CI/dev.
fn test_mode() -> Option<Duration> {
    std::env::var("COSMIC_SWITCHER_TEST_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_millis)
}

impl Application for AppSwitcher {
    type Executor = cosmic::executor::Default;
    type Flags    = (Switcher, bool, Theme, Style);
    type Message  = Message;

    const APP_ID: &'static str = "io.github.wolfhound389.CosmicWindowsSwapper";

    fn core(&self) -> &Core { &self.core }
    fn core_mut(&mut self) -> &mut Core { &mut self.core }

    fn init(core: Core, flags: Self::Flags) -> (Self, Task<Message>) {
        let (switcher, reverse, theme_preset, style) = flags;
        let Switcher { entries, screen, cmd, thumbs } = switcher;

        let n = entries.len();
        let selected = if reverse { n.saturating_sub(1) } else { 1.min(n.saturating_sub(1)) };

        let metrics = Metrics::fit(&style, screen, n);

        // The surface is sized to the panel (plus room for the selection growing), and
        // the content inside it is centred on both axes, which is what puts the panel in
        // the middle of the screen.
        let (surface_w, surface_h) = match style.layout {
            Layout::Grid | Layout::Row => {
                let (w, h) = metrics.panel_size();
                (w + Metrics::GROWTH_PX, h + Metrics::GROWTH_PX)
            }
            Layout::Strip => {
                let icon = theme_preset.values().icon_size as u32;
                let n32 = n as u32;
                (n32 * (icon + 20) + (n32.saturating_sub(1)) * 4 + 36 + 40, 160)
            }
        };

        let test = test_mode();
        let interactivity = if test.is_some() {
            KeyboardInteractivity::None
        } else {
            KeyboardInteractivity::Exclusive
        };

        let surface_id = WindowId::unique();
        // Anchor every edge and let the surface span the whole output. A layer surface
        // that is smaller than the output and not anchored on the opposite edge is
        // pinned to the top-left corner by the compositor, which is what used to leave
        // the panel hugging the corner. Spanning the output makes the position
        // unambiguous, and the content centres itself inside the surface instead.
        let _ = (surface_w, surface_h);
        let (full_w, full_h) = (screen.width as u32, screen.height as u32);
        let layer_task = get_layer_surface::<cosmic::Action<Message>>(SctkLayerSurfaceSettings {
            id: surface_id,
            layer: Layer::Overlay,
            keyboard_interactivity: interactivity,
            anchor: Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
            size: Some((Some(full_w), Some(full_h))),
            // The default limits (1920x1080) would clamp a wide grid on a big output.
            size_limits: iced::Limits::NONE
                .min_width(1.0)
                .min_height(1.0)
                .max_width(full_w as f32)
                .max_height(full_h as f32),
            ..Default::default()
        });

        let app = AppSwitcher {
            core,
            thumbs: vec![None; n],
            entries,
            selected,
            metrics,
            style,
            theme: theme_preset.values(),
            cmd_tx: cmd,
            thumb_rx: thumbs,
            thumbs_done: false,
            super_held: false,
            alt_held: false,
            surface_id,
            test_deadline: test.map(|d| Instant::now() + d),
            finished: false,
            hover: 0.0,
            last_tick: Instant::now(),
            injected: Vec::new(),
            hovered: None,
        };

        (app, layer_task)
    }

    fn update(&mut self, msg: Message) -> Task<Message> {
        let n = self.entries.len();
        if n == 0 {
            return Task::none();
        }
        match msg {
            Message::SelectNext => {
                self.selected = (self.selected + 1) % n;
                self.hover = 0.0;
            }
            Message::SelectPrev => {
                self.selected = (self.selected + n - 1) % n;
                self.hover = 0.0;
            }
            Message::SelectIndex(i) => {
                if i.min(n - 1) != self.selected {
                    self.hover = 0.0;
                }
                self.selected = i.min(n - 1);
            }
            Message::MoveRow(delta) => {
                let cols = self.metrics.cols.max(1) as isize;
                let target = self.selected as isize + delta * cols;
                let next = target.clamp(0, n as isize - 1) as usize;
                if next != self.selected {
                    self.hover = 0.0;
                }
                self.selected = next;
            }
            Message::InjectKey => {
                let keys = test_keys();
                let mut queue = self.injected.clone();
                if queue.len() < keys.len() {
                    let next = keys[queue.len()].clone();
                    queue.push(next.clone());
                    self.injected = queue;
                    return self.update(match next.as_str() {
                        "Tab"        => Message::SelectNext,
                        "ShiftTab"   => Message::SelectPrev,
                        "Tab2"       => Message::SelectNext,
                        "Right"      => Message::SelectNext,
                        "Left"       => Message::SelectPrev,
                        "Down"       => Message::MoveRow(1),
                        "Up"         => Message::MoveRow(-1),
                        "Settle"     => Message::AnimTick,
                        "Activate"   => Message::Activate,
                        "Cancel"     => Message::Cancel,
                        _            => Message::AnimTick,
                    });
                }
                return Task::none();
            }
            Message::HoverCard(i, on) => {
                self.hovered = if on { Some(i) } else { self.hovered.filter(|h| *h != i) };
            }
            Message::AnimTick => {
                let dt = (Instant::now() - self.last_tick).as_secs_f32().clamp(0.0, 0.1);
                self.last_tick = Instant::now();
                self.hover = Animation::step(self.hover, 1.0, dt);
            }
            Message::CloseIndex(i) => {
                if self.finished || i >= n {
                    return Task::none();
                }
                // Windows-style: closing from the switcher dismisses the overlay. The
                // wayland thread lingers a beat so the panel is gone before the
                // compositor animates the window away.
                self.finished = true;
                self.cmd_tx.send(Command::Close(self.entries[i].key)).ok();
                return Task::batch([
                    destroy_layer_surface(self.surface_id),
                    iced::exit(),
                ]);
            }
            Message::Tick => {
                loop {
                    match self.thumb_rx.try_recv() {
                        Ok(ThumbEvent::Ready(t)) => {
                            if t.index < self.thumbs.len() {
                                self.thumbs[t.index] = Some(image::Handle::from_rgba(
                                    t.width, t.height, t.rgba,
                                ));
                            }
                        }
                        Ok(ThumbEvent::Finished) => self.thumbs_done = true,
                        Err(_) => break,
                    }
                }
                if let Some(deadline) = self.test_deadline {
                    if Instant::now() >= deadline {
                        return self.update(Message::Cancel);
                    }
                }
            }
            Message::Activate => {
                if self.finished {
                    return Task::none();
                }
                self.finished = true;
                self.cmd_tx
                    .send(Command::Activate(self.entries[self.selected].key))
                    .ok();
                return Task::batch([
                    destroy_layer_surface(self.surface_id),
                    iced::exit(),
                ]);
            }
            Message::Cancel => {
                if self.finished {
                    return Task::none();
                }
                self.finished = true;
                self.cmd_tx.send(Command::Cancel).ok();
                return Task::batch([
                    destroy_layer_surface(self.surface_id),
                    iced::exit(),
                ]);
            }
            Message::KeyEvent(ke) => match ke {
                keyboard::Event::KeyPressed { key, modifiers, .. } => match key {
                    iced::keyboard::Key::Named(Named::Tab) => {
                        return self.update(if modifiers.shift() {
                            Message::SelectPrev
                        } else {
                            Message::SelectNext
                        });
                    }
                    iced::keyboard::Key::Named(Named::ArrowRight) => {
                        return self.update(Message::SelectNext);
                    }
                    iced::keyboard::Key::Named(Named::ArrowLeft) => {
                        return self.update(Message::SelectPrev);
                    }
                    iced::keyboard::Key::Named(Named::ArrowDown) => {
                        return self.update(Message::MoveRow(1));
                    }
                    iced::keyboard::Key::Named(Named::ArrowUp) => {
                        return self.update(Message::MoveRow(-1));
                    }
                    iced::keyboard::Key::Named(Named::Escape) => {
                        return self.update(Message::Cancel);
                    }
                    iced::keyboard::Key::Named(Named::Enter) => {
                        return self.update(Message::Activate);
                    }
                    _ => {}
                },
                keyboard::Event::ModifiersChanged(mods) => {
                    let activate = (self.super_held && !mods.logo())
                        || (self.alt_held && !mods.alt());
                    self.super_held = mods.logo();
                    self.alt_held = mods.alt();
                    if activate {
                        return self.update(Message::Activate);
                    }
                }
                _ => {}
            },
        }
        Task::none()
    }

    fn subscription(&self) -> Subscription<Message> {
        let key_sub = event::listen_with(|event, _status, _window| match event {
            Event::Keyboard(ke) => Some(Message::KeyEvent(ke)),
            Event::Window(iced::window::Event::Unfocused) => {
                if test_mode().is_some() {
                    None
                } else {
                    Some(Message::Cancel)
                }
            }
            _ => None,
        });

        // Listen on the Unix socket for cycle commands from subsequent invocations.
        let socket_sub = {
            use std::any::TypeId;
            use cosmic::iced::futures::SinkExt;
            struct SocketSub;
            Subscription::run_with(TypeId::of::<SocketSub>(), |_| {
                cosmic::iced::stream::channel(16, |mut tx: cosmic::iced::futures::channel::mpsc::Sender<Message>| async move {
                    use tokio::io::AsyncReadExt;
                    let listener = match tokio::net::UnixListener::bind(crate::socket_path()) {
                        Ok(l) => l,
                        Err(_) => {
                            std::future::pending::<()>().await;
                            unreachable!()
                        }
                    };
                    loop {
                        if let Ok((mut stream, _)) = listener.accept().await {
                            let mut buf = Vec::new();
                            let _ = stream.read_to_end(&mut buf).await;
                            let msg = match buf.as_slice() {
                                b"next" => Message::SelectNext,
                                b"prev" => Message::SelectPrev,
                                _ => continue,
                            };
                            let _ = tx.send(msg).await;
                        }
                    }
                })
            })
        };

        // Collect previews as they arrive from the capture thread.
        let tick_sub = if self.thumbs_done && self.test_deadline.is_none() {
            Subscription::none()
        } else {
            cosmic::iced::time::every(Duration::from_millis(25)).map(|_| Message::Tick)
        };

        // Drive the selection "pop" animation until it settles.
        let anim_sub = if Animation::settled(self.hover) {
            Subscription::none()
        } else {
            cosmic::iced::time::every(Duration::from_millis(16)).map(|_| Message::AnimTick)
        };

        let keys = test_keys();
        let inject_sub = if keys.is_empty() || self.finished {
            Subscription::none()
        } else {
            cosmic::iced::time::every(Duration::from_millis(test_key_delay()))
                .map(|_| Message::InjectKey)
        };

        Subscription::batch([key_sub, socket_sub, tick_sub, anim_sub, inject_sub])
    }

    fn view(&self) -> Element<'_, Message> {
        crate::ui::view(self)
    }

    fn view_window(&self, _id: WindowId) -> Element<'_, Message> {
        crate::ui::view(self)
    }
}

pub fn run(switcher: Switcher, reverse: bool, theme: Theme, style: Style) -> Result<()> {
    let settings = Settings::default().no_main_window(true);
    cosmic::app::run::<AppSwitcher>(settings, (switcher, reverse, theme, style))
        .map_err(|e| anyhow::anyhow!("{e:?}"))
}
