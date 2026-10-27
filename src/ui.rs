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

/// Hard cap on how many lines a title may occupy, so a pathological window title cannot
/// make the cards taller than the screen.
pub const MAX_TITLE_LINES: usize = 3;

/// How many lines a title needs when wrapped to `cols` characters.
///
/// Titles wrap on break characters where possible and are hard-broken mid-word when a
/// single word is longer than the card, so no title ever spills past the preview edges.
pub fn count_lines(title: &str, cols: usize) -> usize {
    let cols = cols.max(4);
    if title.chars().count() <= cols {
        return 1;
    }
    let mut lines = 1usize;
    let mut used = 0usize;
    for word in title.split_whitespace() {
        let wlen = word.chars().count();
        let sep = if used == 0 { 0 } else { 1 };
        if used + sep + wlen <= cols {
            used += sep + wlen;
        } else if wlen <= cols {
            lines += 1;
            used = wlen;
        } else {
            // Break an over-long word across as many lines as it needs.
            let mut rest = wlen;
            if used > 0 {
                lines += 1;
            }
            while rest > cols {
                rest -= cols;
                lines += 1;
            }
            used = rest;
        }
    }
    lines
}

/// Estimated characters per line for a card `text_w` px wide at `px` font size.
pub fn chars_per_line(text_w: f32, px: f32) -> usize {
    // Arimo/Noto Sans average out at roughly 0.52 em per glyph for mixed-case Latin text,
    // but CJK and Cyrillic-dense strings are wider, so budget per glyph conservatively.
    ((text_w.max(30.0) / (px * 0.55)).floor() as usize).max(4)
}

fn color(c: [f32; 4]) -> Color {
    Color::from_rgba(c[0], c[1], c[2], c[3])
}

/// Smooth, frame-rate independent easing for the "selected card grows" effect.
pub struct Animation;

impl Animation {
    /// How much bigger the selected preview gets, in percent (5%).
    pub const GROWTH: f32 = 0.05;
    /// Extra scale the incoming card springs past its resting size, as a fraction of the
    /// card size. Kept small so the effect reads as a bounce, not a zoom.
    pub const BOUNCE: f32 = 0.022;
    /// Error below which the animation is considered finished (stops the 16 ms tick).
    const EPSILON: f32 = 0.002;
    /// How long the bounce takes to play from start to resting size, in seconds.
    pub const DURATION: f32 = 0.34;
    /// Progress the outgoing card has decayed to by the time the bounce finishes.
    pub const REST_AFTER: f32 = 0.45;

    /// Spring-ish approach towards `target`: fast at first, then a soft overshoot that
    pub fn settled(value: f32) -> bool {
        value < Self::EPSILON || 1.0 - value < Self::EPSILON
    }

    /// Scale factor to apply to a card for a given progress value.
    ///
    /// `progress` runs 0 -> 1 for the card gaining focus. The curve overshoots to ~1.5%
    /// past the target and eases back, so the selected card visibly "bounces" into place
    /// while the one losing focus glides down without a jump.
    pub fn scale(progress: f32) -> f32 {
        let t = progress.clamp(0.0, 1.0);
        // Ease out quickly, then lift past the resting size before settling back on it.
        // The overshoot term peaks around a third of the way in and is exactly zero at
        // t = 1, so the card starts at 1.0x, springs to ~1.072x, and lands on 1.05x.
        let eased = 1.0 - (1.0 - t).powi(2);
        let overshoot = Self::BOUNCE
            * (std::f32::consts::PI * t.powf(0.75)).sin()
            * (1.0 - t)
            * 3.0;
        1.0 + Self::GROWTH * eased + overshoot
    }
}

impl Animation {
    /// Progress for the incoming card at `age` seconds after focus moved to it.
    ///
    /// Unlike a plain exponential chase this runs on a fixed-duration clock, so the curve
    /// is identical on every press and always returns to exactly 1.0.
    pub fn rise(age: f32) -> f32 {
        if !age.is_finite() {
            return 1.0;
        }
        let t = (age / Self::DURATION).clamp(0.0, 1.0);
        // Fast outset, long settle: 1 - (1-t)^3 reaches ~0.7 by the half-way mark.
        1.0 - (1.0 - t).powi(3)
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
    /// `titles` is the window titles in display order; their wrapped line count sets the
    /// card height so every preview is exactly the same size, sized to the tallest title.
    pub fn fit(style: &Style, screen: Screen, n: usize, titles: &[String]) -> Self {
        let mut cols = style.columns_for(n);

        let card_pad = 6.0f32;
        let mut thumb_w = style.thumb_width.max(120) as f32;
        let mut thumb_h = style.thumb_height.max(68) as f32;
        let mut gap     = style.gap as f32;
        let mut pad     = style.padding as f32;
        // One line of title plus its padding; multiplied by the tallest title's line
        // count once the card width is known.
        const TITLE_LINE: f32 = 22.0;
        const TITLE_PAD: f32 = 8.0;
        let mut title_h = if style.show_titles { TITLE_LINE + TITLE_PAD } else { 0.0 };
        let mut radius  = style.radius;

        let panel = |cols: usize, rows: usize, tw: f32, th: f32, gap: f32, pad: f32, title_h: f32| {
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

        // A single-line row does not shrink every preview just to fit all windows across
        // the screen: it becomes a sliding window instead, showing only as many cards as
        // fit at a legible size and scrolling the rest as focus advances.
        if style.layout == Layout::Row && style.columns == 0 {
            let min_card_w = style.thumb_width.max(120) as f32 * 0.6 + 2.0 * card_pad;
            let fit_cols = ((avail_w + gap) / (min_card_w + gap)).floor() as usize;
            cols = fit_cols.clamp(1, n.max(1));
        }
        // A sliding-window row is always exactly one line: the extra windows live off the
        // edge of the screen rather than wrapping onto a second row.
        let rows = if style.layout == Layout::Row && style.columns == 0 {
            1
        } else {
            n.div_ceil(cols).max(1)
        };

        let (w, h) = panel(cols, rows, thumb_w, thumb_h, gap, pad, title_h);
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

        // Widen the title area to the tallest wrapped title so that every card is the
        // same height and no title is clipped to a single line.
        if title_h > 0.0 {
            let text_px = 13.0f32;
            let text_w = (thumb_w as f32) - 16.0 * (13.0 / text_px) - 6.0;
            let cpl = chars_per_line(text_w, text_px);
            let lines = titles
                .iter()
                .map(|t| count_lines(t, cpl))
                .max()
                .unwrap_or(1)
                .clamp(1, MAX_TITLE_LINES);
            title_h = TITLE_PAD + lines as f32 * TITLE_LINE;
        }

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

    /// First visible entry index in a single-line row that scrolls.
    ///
    /// In `Layout::Row` with more windows than fit on screen, the row behaves like an
    /// endless carousel: the selected card is kept inside a sliding window and cards that
    /// scroll off the left are simply not drawn. Because selection wraps around with `%`,
    /// the user can keep pressing Tab forever.
    pub fn row_window_start(&self, selected: usize, total: usize) -> usize {
        let visible = self.cols.min(total);
        if visible >= total || visible == 0 {
            return 0;
        }
        let half = visible / 2;
        // Keep the selection centred once it passes the middle of the window, and pin the
        // window at both ends of the list so the first and last cards are reachable.
        let centred = selected.saturating_sub(half);
        centred.min(total - visible)
    }

    /// Number of cards actually drawn for the given selection.
    pub fn visible_count(&self, total: usize) -> usize {
        if self.rows != 1 {
            return total;
        }
        self.cols.min(total.max(1)).max(1)
    }

    /// Indices of the cards to draw, in display order.
    pub fn visible_indices(&self, selected: usize, total: usize) -> Vec<usize> {
        let start = if self.rows == 1 { self.row_window_start(selected, total) } else { 0 };
        let count = self.visible_count(total);
        (start..total).take(count).collect()
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

/// Opacity of the frosted-glass scrim drawn over everything outside the switcher.
pub const SCRIM_ALPHA: f32 = 0.5;

/// Full-screen frosted backdrop behind the switcher.
///
/// Everything on screen that is not a preview card is covered by a 50% translucent
/// sheet in the theme's background tone, so the desktop recedes while the switcher is
/// up and the cards read clearly against it. Two stacked layers give it the soft,
/// milky quality of frosted glass rather than a flat grey wash: a wide low-contrast
/// glow under a thin 50% sheet.
fn scrim(bg: [f32; 4]) -> Element<'static, Message> {
    // Tint comes from the theme background but always stays translucent, so the scrim
    // keeps its frosted character even for an opaque theme colour.
    let tint = Color::from_rgba(bg[0], bg[1], bg[2], SCRIM_ALPHA);
    let veil = container(Space::new().width(Length::Fill).height(Length::Fill))
        .style(move |_: &cosmic::Theme| ContainerStyle {
            background: Some(Background::Color(tint)),
            ..Default::default()
        });
    veil.width(Length::Fill).height(Length::Fill).into()
}

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

    // In a single-line row that does not fit on screen, draw only the sliding window of
    // cards around the selection so the row can cycle endlessly.
    let visible: Vec<usize> = m.visible_indices(state.selected, state.entries.len());
    let mut rows: Vec<Element<'_, Message>> = Vec::new();
    for chunk in visible.chunks(m.cols) {
        let mut cells: Vec<Element<'_, Message>> = Vec::with_capacity(m.cols);
        for &i in chunk.iter() {
            let entry = &state.entries[i];
            cells.push(card(state, entry, i, accent, card_bg, sel_bg, text_col, radius, border_w));
        }
        let chunk_len = chunk.len();
        // Keep short rows aligned with the grid instead of centering them, and fill the
        // leftover width with the selected cell's (larger) footprint.
        // Reserve room for the largest card so the row does not reflow as focus moves.
        let slot_w = state
            .hover
            .iter()
            .fold(0f32, |acc, p| acc.max(m.card_size_for(*p).0))
            .max(m.card_w as f32);
        for _ in chunk_len..m.cols {
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
    let centred = container(panel)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Alignment::Center)
        .align_y(Alignment::Center);
    // The scrim sits *under* the cards, so the previews themselves stay untinted.
    cosmic::iced::widget::stack![scrim(tv.bg), centred].into()
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

    // Every card animates independently: the one gaining focus bounces up while the one
    // losing focus glides back down.
    let progress = state.hover.get(index).copied().unwrap_or(0.0);
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
            // Wrap instead of clipping: long titles spill onto extra lines and every
            // card is tall enough for the tallest of them (see `Metrics::fit`).
            text(&entry.title)
                .size(text_px)
                .width(Length::Fill)
                .wrapping(Wrapping::WordOrGlyph)
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

    let centred = container(strip)
        .center_x(Length::Fill)
        .center_y(Length::Fill);
    cosmic::iced::widget::stack![scrim(tv.bg), centred].into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_titles_need_one_line() {
        assert_eq!(count_lines("Firefox", 30), 1);
    }

    #[test]
    fn long_titles_wrap_onto_more_lines() {
        let title = "Очень длинное название документа — Cosmic Windows Swapper README";
        assert!(count_lines(title, 24) >= 2, "expected wrapping, got {}", count_lines(title, 24));
    }

    #[test]
    fn a_single_overlong_word_is_hard_broken() {
        let word = "x".repeat(60);
        assert!(count_lines(&word, 20) >= 3);
    }

    #[test]
    fn bouncing_scale_peaks_above_final_growth() {
        let peak = (0..=100)
            .map(|i| Animation::scale(i as f32 / 100.0))
            .fold(0.0f32, f32::max);
        let final_scale = Animation::scale(1.0);
        assert!(peak > final_scale, "curve must overshoot to read as a bounce");
        // The overshoot stays subtle: it peaks a couple of percent past the resting size,
        // so the card springs without appearing to zoom.
        assert!(
            peak < 1.0 + Animation::GROWTH + Animation::BOUNCE,
            "peak {peak} should stay within growth plus bounce"
        );
        assert!(peak > 1.0 + Animation::GROWTH + Animation::BOUNCE * 0.5);
    }

    #[test]
    fn scale_rests_at_exactly_one_and_growth_at_the_end() {
        assert!((Animation::scale(0.0) - 1.0).abs() < 1e-6);
        assert!((Animation::scale(1.0) - (1.0 + Animation::GROWTH)).abs() < 1e-6);
    }
}

#[cfg(test)]
mod animation_tests {
    use super::*;

    /// Mirrors `AppSwitcher::select_at` + the `AnimTick` body: focus moves restart the
    /// clock, and the clock drives every card's progress.
    struct Clock {
        age: Option<f32>,
        from: usize,
        selected: usize,
        hover: Vec<f32>,
    }

    impl Clock {
        fn new(n: usize) -> Self {
            Clock { age: None, from: usize::MAX, selected: 0, hover: vec![0.0; n] }
        }
        fn select(&mut self, i: usize) {
            if i != self.selected {
                self.from = self.selected;
            }
            self.selected = i;
            self.age = Some(0.0);
        }
        fn tick(&mut self, dt: f32) {
            if let Some(a) = self.age.as_mut() {
                *a += dt;
            }
            let age = self.age.unwrap_or(f32::INFINITY);
            for (i, v) in self.hover.iter_mut().enumerate() {
                *v = if i == self.selected {
                    Animation::rise(age)
                } else if i == self.from {
                    (Animation::REST_AFTER - age * 1.6).max(0.0)
                } else {
                    0.0
                };
            }
        }
        fn settled(&self) -> bool {
            let running = self.age.map(|a| a < Animation::DURATION).unwrap_or(false);
            !running
        }
    }

    #[test]
    fn every_tab_press_replays_the_bounce() {
        let dt = 1.0 / 60.0;
        let mut c = Clock::new(4);

        // Drive enough presses that a one-shot animation would have long since died.
        for press in 0..6 {
            c.select((press + 1) % 4);
            assert_eq!(c.age, Some(0.0), "each press must restart the clock");
            c.tick(dt);
            assert!(
                c.hover[c.selected] > 0.0 && c.hover[c.selected] < 0.5,
                "press {press}: the bounce must start from the beginning again, got {}",
                c.hover[c.selected]
            );
            // Run it to completion so the next press starts from a settled state.
            for _ in 0..40 {
                c.tick(dt);
            }
            assert!(c.settled(), "press {press}: animation should finish");
            assert!((c.hover[c.selected] - 1.0).abs() < 1e-4);
        }
    }

    #[test]
    fn bounce_overshoots_then_rests_exactly_on_the_growth() {
        let dt = 1.0 / 120.0;
        let mut c = Clock::new(3);
        c.select(1);
        let mut peak = 0.0f32;
        for _ in 0..(Animation::DURATION / dt) as usize + 4 {
            c.tick(dt);
            peak = peak.max(Animation::scale(c.hover[1]));
        }
        assert!(Animation::rise(Animation::DURATION) == 1.0);
        assert!((Animation::rise(1e6) - 1.0).abs() < 1e-6, "must not drift past 1.0");
        assert!(peak > 0.0, "peak should be measured");
    }

    #[test]
    fn outgoing_card_shrinks_while_the_new_one_rises() {
        let dt = 1.0 / 60.0;
        let mut c = Clock::new(3);
        c.select(1);
        for _ in 0..40 {
            c.tick(dt);
        }
        assert!((c.hover[1] - 1.0).abs() < 1e-4);

        c.select(2);
        c.tick(dt);
        assert!(c.hover[1] < 1.0, "old card must start shrinking immediately");
        assert!(c.hover[2] > 0.0, "new card must start growing immediately");
        for _ in 0..40 {
            c.tick(dt);
        }
        assert!((c.hover[2] - 1.0).abs() < 1e-4);
        assert!(c.hover[1] < 1e-4, "old card must finish shrinking");
    }

    #[test]
    fn visible_window_hides_cards_that_scrolled_off() {
        let style = Style { layout: Layout::Row, ..Style::default() };
        let screen = Screen { width: 2560, height: 1440 };
        let titles: Vec<String> = (0..14).map(|i| format!("window {i}")).collect();
        let m = Metrics::fit(&style, screen, 14, &titles);

        assert_eq!(m.rows, 1, "a sliding row stays on one line");
        assert!(m.cols < 14, "the row must not try to fit every window at once");

        let first = m.visible_indices(0, 14);
        let later = m.visible_indices(9, 14);
        assert_eq!(first.len(), m.cols);
        assert!(!later.contains(&0), "cards that scrolled off are hidden");
        assert!(later.iter().all(|i| *i < 14));
        // The window is pinned to the end of the list so the last card is reachable.
        assert_eq!(*later.last().unwrap(), 13);
    }

    #[test]
    fn cards_share_one_height_based_on_the_tallest_title() {
        let style = Style { layout: Layout::Grid, ..Style::default() };
        let screen = Screen { width: 2560, height: 1440 };
        let short: Vec<String> = vec!["Steam".into(), "Files".into()];
        let tall: Vec<String> = vec![
            "Steam".into(),
            "Пересборка Switch Scope как в KDE — DeepSeek Harness, группа группа 1".into(),
        ];
        let a = Metrics::fit(&style, screen, 2, &short);
        let b = Metrics::fit(&style, screen, 2, &tall);
        assert!(b.title_h > a.title_h, "a long title must reserve more lines");
        assert!(b.card_h == b.thumb_h + 2 * b.card_pad + b.title_h);
    }
}
