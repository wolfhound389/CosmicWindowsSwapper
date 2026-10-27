use cosmic::{
    iced::{
        Alignment, Background, Border, Color, ContentFit, Length, Shadow, Vector,
        widget::container::Style as ContainerStyle,
        widget::text::Wrapping,
    },
    widget::{Space, column, container, image, mouse_area, row, text},
    widget::icon,
    Element,
};
use switcher_config::{Layout, Style};

use crate::app::{AppSwitcher, Message};
use crate::icons::icon_name_for;
use crate::wayland::{Screen, ToplevelEntry};

/// Cheap title clipping: iced's ellipsis needs a height limit we don't want here, and
/// a fixed character budget keeps every row's height identical.
fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn color(c: [f32; 4]) -> Color {
    Color::from_rgba(c[0], c[1], c[2], c[3])
}

/// Smooth, frame-rate independent easing for the "selected card grows" effect.
pub struct Animation;

impl Animation {
    /// How much bigger the selected preview gets, in percent (5%).
    pub const GROWTH: f32 = 0.05;
    /// Error below which the animation is considered finished (stops the 16 ms tick).
    const EPSILON: f32 = 0.002;

    /// Critically-damped-ish approach towards `1.0`: fast at first, gliding to a stop.
    pub fn step(current: f32, _target: f32, dt: f32) -> f32 {
        // Exponential smoothing with a time constant of ~60 ms.
        let k = 1.0 - (-dt / 0.06).exp();
        (current + (1.0 - current) * k).clamp(0.0, 1.0)
    }

    pub fn settled(value: f32) -> bool {
        !(value > 0.0) || 1.0 - value < Self::EPSILON
    }

    /// Scale factor to apply to the selected card for a given progress value.
    pub fn scale(progress: f32) -> f32 {
        // A soft ease-out so even a single Tab press reads as a deliberate "grow".
        let t = 1.0 - (1.0 - progress.clamp(0.0, 1.0)).powi(2);
        1.0 + Self::GROWTH * t
    }
}

/// Geometry of the overlay, computed once at startup from the theme's preferred card size
/// and shrunk when it would not fit the monitor.
#[derive(Debug, Clone, Copy)]
pub struct Metrics {
    pub thumb_w:  u16,
    pub thumb_h:  u16,
    pub card_pad: u16,
    pub title_h:  u16,
    pub gap:      u16,
    pub pad:      u16,
    pub cols:     usize,
    pub rows:     usize,
    pub card_w:   u16,
    pub card_h:   u16,
    pub radius:   f32,
    pub border:   f32,
}

impl Metrics {
    pub fn fit(style: &Style, screen: Screen, n: usize) -> Self {
        let cols = style.columns_for(n);
        let rows = n.div_ceil(cols).max(1);

        let card_pad = 6.0f32;
        let mut thumb_w = style.thumb_width.max(120) as f32;
        let mut thumb_h = style.thumb_height.max(68) as f32;
        let mut gap     = style.gap as f32;
        let mut pad     = style.padding as f32;
        let mut title_h = if style.show_titles { 30.0 } else { 0.0 };
        let mut radius  = style.radius;

        let panel = |tw: f32, th: f32, gap: f32, pad: f32, title_h: f32| {
            let card_w = tw + 2.0 * card_pad;
            let card_h = th + 2.0 * card_pad + title_h;
            (
                cols as f32 * card_w + (cols as f32 - 1.0) * gap + 2.0 * pad,
                rows as f32 * card_h + (rows as f32 - 1.0) * gap + 2.0 * pad,
            )
        };

        // Leave room at the edges for the panel shadow and for the selected card
        // growing by 5%.
        let avail_w = (screen.width as f32 - 96.0).max(280.0);
        let avail_h = (screen.height as f32 - 96.0).max(220.0);

        let (w, h) = panel(thumb_w, thumb_h, gap, pad, title_h);
        if w > avail_w || h > avail_h {
            // A single row of eight windows has to get pretty small to fit; allow it to
            // shrink further than the grid would, down to a legible minimum.
            let floor = if rows == 1 { 0.22 } else { 0.35 };
            let k = (avail_w / w).min(avail_h / h).clamp(floor, 1.0);
            thumb_w *= k;
            thumb_h *= k;
            gap     *= k;
            pad     *= k;
            // Below ~14 px a title is unreadable; drop it and give the space to previews.
            title_h = if title_h * k < 14.0 { 0.0 } else { title_h * k };
            radius  *= k;
        }

        let thumb_w = thumb_w.round() as u16;
        let thumb_h = thumb_h.round() as u16;
        let card_pad = card_pad.round() as u16;
        let title_h = title_h.round() as u16;

        Self {
            thumb_w,
            thumb_h,
            card_pad,
            title_h,
            gap: gap.round() as u16,
            pad: pad.round() as u16,
            cols,
            rows,
            card_w: thumb_w + 2 * card_pad,
            card_h: thumb_h + 2 * card_pad + title_h,
            radius,
            border: style.border,
        }
    }

    /// Extra space reserved so a scaled-up (selected) card plus its ring still fits in
    /// the surface: 5% growth on both axes plus a couple of pixels for the ring.
    pub const GROWTH_PX: u32 = 24;

    /// Card size while animating the selection. The neighbours keep the base size, so the
    /// selected card is exactly `Animation::GROWTH` bigger at the end of the animation.
    pub fn card_size_scaled(&self, scale: f32) -> (f32, f32) {
        (
            (self.card_w as f32 * scale).round(),
            (self.card_h as f32 * scale).round(),
        )
    }

    /// Ground size of a card and its interpolated size for the given progress value.
    pub fn card_size_for(&self, progress: f32) -> (f32, f32) {
        let (w, h) = self.card_size_scaled(Animation::scale(progress));
        ((self.card_w as f32).max(w), (self.card_h as f32).max(h))
    }

    /// Size of the layer surface in logical pixels.
    pub fn panel_size(&self) -> (u32, u32) {
        let (c, r) = (self.cols as u32, self.rows as u32);
        let w = c * self.card_w as u32 + (c - 1) * self.gap as u32 + 2 * self.pad as u32;
        let h = r * self.card_h as u32 + (r - 1) * self.gap as u32 + 2 * self.pad as u32;
        (w.max(64), h.max(48))
    }
}

pub fn view(state: &AppSwitcher) -> Element<'_, Message> {
    match state.style.layout {
        Layout::Grid | Layout::Row => grid(state),
        Layout::Strip              => strip(state),
    }
}

// ---------------------------------------------------------------------------
// Live window previews: wrapped grid or a single row (Windows / KDE style)
// ---------------------------------------------------------------------------

/// Diameter of the material-style close button drawn on each preview.
const CLOSE_D: f32 = 22.0;

fn grid(state: &AppSwitcher) -> Element<'_, Message> {
    let m  = &state.metrics;
    let tv = &state.theme;

    let accent   = color(tv.accent);
    let card_bg  = color(tv.card);
    let sel_bg   = color(tv.selected_bg);
    let text_col = color(tv.text);
    let radius   = m.radius;
    let border_w = m.border;
    let bg       = color(tv.bg);

    let mut rows: Vec<Element<'_, Message>> = Vec::new();
    for (r, chunk) in state.entries.chunks(m.cols).enumerate() {
        let mut cells: Vec<Element<'_, Message>> = Vec::with_capacity(m.cols);
        for (c, entry) in chunk.iter().enumerate() {
            let i = r * m.cols + c;
            cells.push(card(state, entry, i, accent, card_bg, sel_bg, text_col, radius, border_w));
        }
        // Keep short rows aligned with the grid instead of centering them, and fill the
        // leftover width with the selected cell's (larger) footprint.
        let (slot_w, _) = m.card_size_for(state.hover);
        for _ in chunk.len()..m.cols {
            cells.push(Space::new().width(Length::Fixed(slot_w)).height(Length::Fixed(1.0)).into());
        }
        rows.push(row(cells).spacing(m.gap as f32).align_y(Alignment::Center).into());
    }

    let body  = column(rows).spacing(m.gap as f32);
    let panel = container(body).padding(m.pad).style(move |_: &cosmic::Theme| ContainerStyle {
        background: Some(Background::Color(bg)),
        border: Border {
            radius: (radius + 6.0).into(),
            width: 0.0,
            color: Color::TRANSPARENT,
        },
        shadow: Shadow {
            color: Color { r: 0.0, g: 0.0, b: 0.0, a: 0.55 },
            offset: Vector::new(0.0, 10.0),
            blur_radius: 36.0,
        },
        ..Default::default()
    });

    // Centred on both axes: the layer surface is only as large as the panel plus the
    // growth reserve, and the compositor centres the surface on the output.
    container(panel)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Alignment::Center)
        .align_y(Alignment::Center)
        .into()
}

/// One preview card. The selected card is laid out `Animation::scale(progress)` larger
/// than its neighbours and scales its contents to match, so the highlight grows smoothly
/// while the switcher key is held.
#[allow(clippy::too_many_arguments)]
fn card<'a>(
    state: &'a AppSwitcher,
    entry: &'a ToplevelEntry,
    index: usize,
    accent: Color,
    card_bg: Color,
    sel_bg: Color,
    text_col: Color,
    radius: f32,
    border_w: f32,
) -> Element<'a, Message> {
    let m = &state.metrics;
    let selected = index == state.selected;
    let icon_name = icon_name_for(&entry.app_id);

    let progress = if selected { state.hover } else { 0.0 };
    let scale    = Animation::scale(progress);
    let (card_w, card_h) = m.card_size_for(progress);

    // Everything inside the card is scaled by the same factor, which is what makes the
    // extra 5% read as a real "zoom" rather than just a bigger empty box.
    let thumb_w = (m.thumb_w as f32 * scale).round().max(1.0);
    let thumb_h = (m.thumb_h as f32 * scale).round().max(1.0);
    let title_h = (m.title_h as f32 * scale).round();

    let icon_radius = 8.0 * scale;
    let thumb: Element<'a, Message> = match &state.thumbs[index] {
        Some(handle) => image(handle.clone())
            .width(Length::Fixed(thumb_w))
            .height(Length::Fixed(thumb_h))
            .content_fit(ContentFit::Contain)
            .border_radius(8.0 * scale)
            .into(),
        None => {
            let icon_px = (46.0 * scale).round().max(16.0) as u16;
            let app_icon: Element<'a, Message> = icon::from_name(icon_name.as_str())
                .size(icon_px)
                .icon()
                .size(icon_px)
                .into();
            container(app_icon)
                .width(Length::Fixed(thumb_w))
                .height(Length::Fixed(thumb_h))
                .align_x(Alignment::Center)
                .align_y(Alignment::Center)
                .style(move |_: &cosmic::Theme| ContainerStyle {
                    background: Some(Background::Color(Color::from_rgba(0.0, 0.0, 0.0, 0.22))),
                    border: Border { radius: icon_radius.into(), ..Default::default() },
                    ..Default::default()
                })
                .into()
        }
    };

    let title: Element<'a, Message> = if title_h < 8.0 {
        Space::new().height(Length::Fixed(0.0)).into()
    } else {
        let icon_px = (16.0 * scale).round().max(12.0) as u16;
        let small_icon: Element<'a, Message> = icon::from_name(icon_name.as_str())
            .size(icon_px)
            .icon()
            .size(icon_px)
            .into();
        let text_px = (13.0 * scale).round().max(10.0) as u16;
        row![
            small_icon,
            text(truncate(&entry.title, 42))
                .size(text_px)
                .width(Length::Fill)
                .wrapping(Wrapping::None)
                .class(cosmic::theme::Text::Color(text_col)),
        ]
        .spacing((6.0 * scale).round())
        .align_y(Alignment::Center)
        .height(Length::Fixed(title_h))
        .into()
    };

    let inner = column![thumb, title].spacing((4.0 * scale).round());

    let pad = (m.card_pad as f32 * scale).round();
    let cell = container(inner)
        .width(Length::Fixed(card_w))
        .height(Length::Fixed(card_h))
        .padding(pad)
        .style(move |_: &cosmic::Theme| ContainerStyle {
            background: Some(Background::Color(if selected { sel_bg } else { card_bg })),
            border: Border {
                radius: (radius * scale + 2.0).into(),
                width: if selected { border_w } else { 1.0 },
                color: if selected {
                    accent
                } else {
                    Color::from_rgba(1.0, 1.0, 1.0, 0.06)
                },
            },
            ..Default::default()
        });

    // Fix the card footprint, then pin the close button to its top-right corner. The
    // slot keeps every card the same size while the selected one grows.
    let body: Element<'a, Message> = container(cell)
        .width(Length::Fixed(card_w))
        .height(Length::Fixed(card_h))
        .into();

    let slotted: Element<'a, Message> = if state.style.show_close {
        let inset = (pad + radius * 0.35).round();
        let btn = close_button(index, scale, state.hovered == Some(index));
        cosmic::iced::widget::stack![
            body,
            container(btn)
                .width(Length::Fixed(card_w))
                .height(Length::Fixed(card_h))
                .padding(inset)
                .align_x(Alignment::End)
                .align_y(Alignment::Start),
        ]
        .into()
    } else {
        body
    };

    mouse_area(slotted)
        .on_press(Message::SelectIndex(index))
        .on_release(Message::Activate)
        .into()
}

/// A material "close" button: a translucent dark circle with an × drawn from two thin
/// bars. Built from primitives so it scales with the card and always matches the theme.
fn close_button<'a>(index: usize, scale: f32, hovered: bool) -> Element<'a, Message> {
    let d  = (CLOSE_D * scale).round().max(16.0);
    let arm = d * 0.34;
    let fill = if hovered {
        Color::from_rgba(0.95, 0.36, 0.36, 0.95)
    } else {
        Color::from_rgba(0.0, 0.0, 0.0, 0.46)
    };

    let circle = container(close_glyph(arm, hovered))
        .width(Length::Fixed(d))
        .height(Length::Fixed(d))
        .align_x(Alignment::Center)
        .align_y(Alignment::Center)
        .style(move |_: &cosmic::Theme| ContainerStyle {
            background: Some(Background::Color(fill)),
            border: Border {
                radius: (d / 2.0).into(),
                width: 1.0,
                color: Color::from_rgba(1.0, 1.0, 1.0, 0.30),
            },
            ..Default::default()
        });

    mouse_area(circle)
        .on_press(Message::CloseIndex(index))
        .on_enter(Message::HoverCard(index, true))
        .on_exit(Message::HoverCard(index, false))
        .into()
}

/// The × glyph. A canvas is used on purpose: iced has no rotation transform, so two
/// crossing rectangles would only ever render a "+". Painting the glyph gives a proper
/// material-style close cross that stays crisp at any card scale.
struct Cross {
    arm:   f32,
    alpha: f32,
}

impl<Theme> cosmic::iced::widget::canvas::Program<Message, Theme> for Cross {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &cosmic::iced::Renderer,
        _theme: &Theme,
        bounds: cosmic::iced::Rectangle,
        _cursor: cosmic::iced::mouse::Cursor,
    ) -> Vec<cosmic::iced::widget::canvas::Geometry> {
        use cosmic::iced::widget::canvas::{Frame, Path, Stroke};
        let mut frame = Frame::new(renderer, bounds.size());

        let c = cosmic::iced::Point::new(bounds.width / 2.0, bounds.height / 2.0);
        let h = self.arm / 2.0;
        let col = Color::from_rgba(1.0, 1.0, 1.0, self.alpha);
        let stroke = Stroke::default().with_width(self.arm * 0.17).with_color(col);

        for (dx, dy) in [(-1.0f32, -1.0f32), (-1.0, 1.0)] {
            let p = Path::line(
                cosmic::iced::Point::new(c.x + dx * h, c.y + dy * h),
                cosmic::iced::Point::new(c.x - dx * h, c.y - dy * h),
            );
            frame.stroke(&p, stroke);
        }
        vec![frame.into_geometry()]
    }
}

fn close_glyph<'a>(arm: f32, hovered: bool) -> Element<'a, Message> {
    let alpha = if hovered { 0.98 } else { 0.88 };
    cosmic::widget::canvas(Cross { arm, alpha })
        .width(Length::Fixed(arm))
        .height(Length::Fixed(arm))
        .into()
}

// ---------------------------------------------------------------------------
// Classic horizontal icon strip
// ---------------------------------------------------------------------------

fn strip(state: &AppSwitcher) -> Element<'_, Message> {
    let tv     = &state.theme;
    let bg     = color(tv.bg);
    let sel_bg = color(tv.selected_bg);
    let accent = color(tv.accent);
    let corner = tv.corner_radius;
    let icon_sz = tv.icon_size;

    let cells: Vec<Element<'_, Message>> = state
        .entries
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            let icon_name = icon_name_for(&entry.app_id);
            let selected  = i == state.selected;

            let app_icon: Element<'_, Message> = icon::from_name(icon_name.as_str())
                .size(icon_sz)
                .icon()
                .size(icon_sz)
                .into();

            let cell = container(app_icon)
                .padding(10)
                .style(move |_: &cosmic::Theme| {
                    if selected {
                        ContainerStyle {
                            background: Some(Background::Color(sel_bg)),
                            border: Border {
                                radius: (corner - 2.0).into(),
                                width: 1.5,
                                color: accent,
                            },
                            ..Default::default()
                        }
                    } else {
                        ContainerStyle::default()
                    }
                });

            mouse_area(cell)
                .on_press(Message::SelectIndex(i))
                .on_release(Message::Activate)
                .into()
        })
        .collect();

    let strip = container(row(cells).spacing(4).align_y(Alignment::Center))
        .style(move |_: &cosmic::Theme| ContainerStyle {
            background: Some(Background::Color(bg)),
            border: Border { radius: corner.into(), width: 0.0, color: Color::TRANSPARENT },
            shadow: Shadow {
                color: Color { r: 0.0, g: 0.0, b: 0.0, a: 0.6 },
                offset: Vector::new(0.0, 8.0),
                blur_radius: 32.0,
            },
            ..Default::default()
        })
        .padding([14, 18]);

    container(strip)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into()
}
