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

use crate::ui::Metrics;
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
}

#[derive(Debug, Clone)]
pub enum Message {
    SelectNext,
    SelectPrev,
    SelectIndex(usize),
    MoveRow(isize),
    Activate,
    Cancel,
    KeyEvent(keyboard::Event),
    Tick,
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

    const APP_ID: &'static str = "io.github.cosmic-ext-app-switcher";

    fn core(&self) -> &Core { &self.core }
    fn core_mut(&mut self) -> &mut Core { &mut self.core }

    fn init(core: Core, flags: Self::Flags) -> (Self, Task<Message>) {
        let (switcher, reverse, theme_preset, style) = flags;
        let Switcher { entries, screen, cmd, thumbs } = switcher;

        let n = entries.len();
        let selected = if reverse { n.saturating_sub(1) } else { 1.min(n.saturating_sub(1)) };

        let metrics = Metrics::fit(&style, screen, n);
        let (surface_w, surface_h) = match style.layout {
            Layout::Grid => {
                let (w, h) = metrics.panel_size();
                (w + 8, h + 8)
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
        let layer_task = get_layer_surface::<cosmic::Action<Message>>(SctkLayerSurfaceSettings {
            id: surface_id,
            layer: Layer::Overlay,
            keyboard_interactivity: interactivity,
            anchor: Anchor::empty(),
            size: Some((Some(surface_w), Some(surface_h))),
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
        };

        (app, layer_task)
    }

    fn update(&mut self, msg: Message) -> Task<Message> {
        let n = self.entries.len();
        if n == 0 {
            return Task::none();
        }
        match msg {
            Message::SelectNext => self.selected = (self.selected + 1) % n,
            Message::SelectPrev => self.selected = (self.selected + n - 1) % n,
            Message::SelectIndex(i) => self.selected = i.min(n - 1),
            Message::MoveRow(delta) => {
                let cols = self.metrics.cols.max(1) as isize;
                let target = self.selected as isize + delta * cols;
                self.selected = target.clamp(0, n as isize - 1) as usize;
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

        Subscription::batch([key_sub, socket_sub, tick_sub])
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
