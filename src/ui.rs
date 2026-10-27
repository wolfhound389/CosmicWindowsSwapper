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

        let avail_w = (screen.width as f32 - 80.0).max(280.0);
        let avail_h = (screen.height as f32 - 80.0).max(220.0);

        let (w, h) = panel(thumb_w, thumb_h, gap, pad, title_h);
        if w > avail_w || h > avail_h {
            let k = (avail_w / w).min(avail_h / h).clamp(0.35, 1.0);
            thumb_w *= k;
            thumb_h *= k;
            gap     *= k;
            pad     *= k;
            title_h *= k;
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
        Layout::Grid  => grid(state),
        Layout::Strip => strip(state),
    }
}

// ---------------------------------------------------------------------------
// Grid of live window previews (Windows / KDE style)
// ---------------------------------------------------------------------------

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
        // Keep short rows aligned with the grid instead of centering them.
        for _ in chunk.len()..m.cols {
            cells.push(Space::new().width(Length::Fixed(m.card_w as f32)).into());
        }
        rows.push(row(cells).spacing(m.gap as f32).into());
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

    container(panel)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into()
}

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

    let thumb: Element<'a, Message> = match &state.thumbs[index] {
        Some(handle) => image(handle.clone())
            .width(Length::Fixed(m.thumb_w as f32))
            .height(Length::Fixed(m.thumb_h as f32))
            .content_fit(ContentFit::Contain)
            .border_radius(8.0)
            .into(),
        None => {
            let app_icon: Element<'a, Message> = icon::from_name(icon_name.as_str())
                .size(46u16)
                .icon()
                .size(46u16)
                .into();
            container(app_icon)
                .width(Length::Fixed(m.thumb_w as f32))
                .height(Length::Fixed(m.thumb_h as f32))
                .align_x(Alignment::Center)
                .align_y(Alignment::Center)
                .style(|_: &cosmic::Theme| ContainerStyle {
                    background: Some(Background::Color(Color::from_rgba(0.0, 0.0, 0.0, 0.22))),
                    border: Border { radius: 8.0.into(), ..Default::default() },
                    ..Default::default()
                })
                .into()
        }
    };

    let title: Element<'a, Message> = if m.title_h == 0 {
        Space::new().height(Length::Fixed(0.0)).into()
    } else {
        let small_icon: Element<'a, Message> = icon::from_name(icon_name.as_str())
            .size(16u16)
            .icon()
            .size(16u16)
            .into();
        row![
            small_icon,
            text(truncate(&entry.title, 42))
                .size(13)
                .width(Length::Fill)
                .wrapping(Wrapping::None)
                .class(cosmic::theme::Text::Color(text_col)),
        ]
        .spacing(6)
        .align_y(Alignment::Center)
        .height(Length::Fixed(m.title_h as f32))
        .into()
    };

    let inner = if m.title_h == 0 {
        column![thumb].spacing(4)
    } else {
        column![thumb, title].spacing(4)
    };

    let cell = container(inner)
        .width(Length::Fixed(m.card_w as f32))
        .height(Length::Fixed(m.card_h as f32))
        .padding(m.card_pad)
        .style(move |_: &cosmic::Theme| ContainerStyle {
            background: Some(Background::Color(if selected { sel_bg } else { card_bg })),
            border: Border {
                radius: (radius + 2.0).into(),
                width: if selected { border_w } else { 1.0 },
                color: if selected {
                    accent
                } else {
                    Color::from_rgba(1.0, 1.0, 1.0, 0.06)
                },
            },
            ..Default::default()
        });

    mouse_area(cell)
        .on_press(Message::SelectIndex(index))
        .on_release(Message::Activate)
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
