//! Requesting a blurred backdrop from the compositor.
//!
//! The switcher draws its own frosted panel on a layer-shell surface, but a tint alone
//! cannot imitate glass: the desktop behind has to be blurred by the compositor. COSMIC
//! (like most wlroots-derived compositors) exposes that through
//! `ext_background_effect_manager_v1`, and libcosmic forwards it as
//! [`iced_runtime::platform_specific::wayland::Action::BlurSurface`].
//!
//! The protocol allows exactly one `ext_background_effect_surface_v1` per `wl_surface`,
//! and iced's backend creates one object *per rectangle* it is given. Passing more than
//! one rectangle therefore raises `background_effect_exists` and kills the event loop, so
//! this module deliberately requests a single rectangle covering the whole surface and
//! lets the compositor clip it.

use cosmic::iced::Rectangle;
use cosmic::iced::core::window::Id as SurfaceId;

/// A rectangle far larger than any screen. The compositor clips the blur region to the
/// surface, so a single oversized rectangle is the portable way to say "everything".
fn whole_surface() -> Rect {
    Rect {
        x:      0.0,
        y:      0.0,
        width:  f32::MAX,
        height: f32::MAX,
    }
}

/// A rectangle in surface-local coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x:      f32,
    pub y:      f32,
    pub width:  f32,
    pub height: f32,
}

/// The blur request for one surface.
///
/// There is always exactly one rectangle, because the compositor rejects a second effect
/// object on the same surface.
pub fn blur_region() -> Vec<Rect> {
    vec![whole_surface()]
}

/// Ask the compositor to blur what is behind `id`.
///
/// An empty rectangle list would *remove* the effect, so the request always carries the
/// full-surface region.
pub fn blur_task<M: 'static>(id: SurfaceId) -> cosmic::Task<M> {
    let rects = blur_region();
    let surface_rects = rects.iter().map(|r| Rectangle {
        x:      r.x,
        y:      r.y,
        width:  r.width,
        height: r.height,
    });
    iced_runtime::task::effect(iced_runtime::Action::PlatformSpecific(
        iced_runtime::platform_specific::Action::Wayland(
            iced_runtime::platform_specific::wayland::Action::BlurSurface(
                id,
                Some(surface_rects.collect()),
            ),
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exactly_one_rectangle_is_requested() {
        // Two rectangles make iced create two effect objects on one surface, which the
        // protocol forbids; the app then exits with a background_effect_exists error.
        assert_eq!(blur_region().len(), 1);
    }

    #[test]
    fn the_rectangle_covers_the_whole_surface() {
        let r = blur_region()[0];
        assert_eq!((r.x, r.y), (0.0, 0.0));
        assert!(r.width > 10_000.0 && r.height > 10_000.0, "must exceed any screen: {r:?}");
    }

    #[test]
    fn the_region_is_never_empty() {
        // An empty list clears the effect instead of applying it.
        assert!(!blur_region().is_empty());
    }
}
