//! CPU-side post-processing for captured window frames.
//!
//! The compositor hands us full-resolution XRGB/ARGB pixels (the protocol requires the
//! buffer to match the source size), so previews have to be shrunk down here before they
//! can be handed to the UI as RGBA textures.

/// Byte layout of the shm pixels we ask the compositor to hand us. Wayland names these
/// formats by their 32-bit value, so on little-endian machines `ARGB8888` is stored as
/// B,G,R,A and `ABGR8888` as R,G,B,A.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceFormat {
    Xrgb8888,
    Argb8888,
    Xbgr8888,
    Abgr8888,
}

impl SourceFormat {
    pub fn has_alpha(self) -> bool {
        matches!(self, SourceFormat::Argb8888 | SourceFormat::Abgr8888)
    }

    /// `true` when the bytes come out blue-first.
    pub fn blue_first(self) -> bool {
        matches!(self, SourceFormat::Xrgb8888 | SourceFormat::Argb8888)
    }

    pub fn rgb_order(self) -> [usize; 3] {
        if self.blue_first() {
            [2, 1, 0]
        } else {
            [0, 1, 2]
        }
    }
}

pub struct Frame {
    pub width:  u32,
    pub height: u32,
    /// Tightly packed RGBA8, top-left origin.
    pub rgba:   Vec<u8>,
}

/// Convert a captured shm buffer into RGBA, scaling it down so that neither side exceeds
/// `max_dim`. Corners are rounded by `corner_radius` pixels so previews blend into the
/// card background instead of showing hard square edges.
pub fn to_preview(
    src: &[u8],
    stride: usize,
    src_w: u32,
    src_h: u32,
    format: SourceFormat,
    max_dim: u32,
    corner_radius: f32,
) -> Frame {
    if src_w == 0 || src_h == 0 {
        return Frame { width: 0, height: 0, rgba: Vec::new() };
    }

    let (dst_w, dst_h) = scaled_size(src_w, src_h, max_dim);
    let alpha_from_src = format.has_alpha();
    let (ri, gi, bi) = { let o = format.rgb_order(); (o[0], o[1], o[2]) };

    let mut rgba = Vec::with_capacity((dst_w * dst_h * 4) as usize);

    for dy in 0..dst_h {
        let y0 = dy * src_h / dst_h;
        let y1 = (((dy + 1) * src_h / dst_h).max(y0 + 1)).min(src_h);
        for dx in 0..dst_w {
            let x0 = dx * src_w / dst_w;
            let x1 = (((dx + 1) * src_w / dst_w).max(x0 + 1)).min(src_w);

            let mut sum_b: u32 = 0;
            let mut sum_g: u32 = 0;
            let mut sum_r: u32 = 0;
            let mut sum_a: u32 = 0;
            let mut count: u32 = 0;

            for sy in y0..y1 {
                let row = sy as usize * stride;
                for sx in x0..x1 {
                    let p = row + sx as usize * 4;
                    if p + 3 >= src.len() {
                        continue;
                    }
                    sum_r += src[p + ri] as u32;
                    sum_g += src[p + gi] as u32;
                    sum_b += src[p + bi] as u32;
                    sum_a += src[p + 3] as u32;
                    count += 1;
                }
            }

            if count == 0 {
                rgba.extend_from_slice(&[0, 0, 0, 255]);
                continue;
            }

            let avg_a = if alpha_from_src { sum_a / count } else { 255 };
            // wl_shm alpha formats are premultiplied; iced wants straight alpha.
            let unpremul = |c: u32| -> u8 {
                if avg_a == 0 || avg_a >= 255 {
                    c.min(255) as u8
                } else {
                    ((c * 255 / avg_a).min(255)) as u8
                }
            };

            let mut alpha = avg_a.min(255) as f32;
            alpha *= corner_coverage(dx, dy, dst_w, dst_h, corner_radius);

            rgba.push(unpremul(sum_r / count));
            rgba.push(unpremul(sum_g / count));
            rgba.push(unpremul(sum_b / count));
            rgba.push(alpha as u8);
        }
    }

    Frame { width: dst_w, height: dst_h, rgba }
}

/// Fit `src_w`x`src_h` into `max_dim` without upscaling (wayland buffers are already
/// larger than the card, so this only ever shrinks).
pub fn scaled_size(src_w: u32, src_h: u32, max_dim: u32) -> (u32, u32) {
    let longest = src_w.max(src_h);
    if longest <= max_dim {
        return (src_w, src_h);
    }
    let dst_w = ((src_w as u64 * max_dim as u64) / longest as u64).max(1) as u32;
    let dst_h = ((src_h as u64 * max_dim as u64) / longest as u64).max(1) as u32;
    (dst_w, dst_h)
}

/// Anti-aliased rounded-rectangle coverage for pixel (x, y), in `0.0..=1.0`.
pub fn corner_coverage(x: u32, y: u32, w: u32, h: u32, radius: f32) -> f32 {
    if radius <= 0.0 {
        return 1.0;
    }
    let r = radius.min(w as f32 / 2.0).min(h as f32 / 2.0);
    let px = x as f32 + 0.5;
    let py = y as f32 + 0.5;

    // Only the four corner squares need the circular test.
    let cx = if px < r {
        r
    } else if px > w as f32 - r {
        w as f32 - r
    } else {
        return 1.0;
    };
    let cy = if py < r {
        r
    } else if py > h as f32 - r {
        h as f32 - r
    } else {
        return 1.0;
    };

    let dx = px - cx;
    let dy = py - cy;
    let dist = (dx * dx + dy * dy).sqrt();
    (r - dist + 0.5).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, b: u8, g: u8, r: u8) -> Vec<u8> {
        let mut v = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..(w * h) {
            v.extend_from_slice(&[b, g, r, 255]);
        }
        v
    }

    #[test]
    fn scales_down_preserving_aspect() {
        assert_eq!(scaled_size(1920, 1080, 600), (600, 337));
        assert_eq!(scaled_size(400, 900, 600), (266, 600));
        assert_eq!(scaled_size(300, 200, 600), (300, 200));
    }

    #[test]
    fn averages_blocks_and_ignores_endianness() {
        let src = solid(8, 8, 10, 20, 30);
        let frame = to_preview(&src, 32, 8, 8, SourceFormat::Xrgb8888, 4, 0.0);
        assert_eq!((frame.width, frame.height), (4, 4));
        assert_eq!(&frame.rgba[0..4], &[30, 20, 10, 255]);
    }

    #[test]
    fn corners_become_transparent() {
        let src = solid(16, 16, 255, 255, 255);
        let frame = to_preview(&src, 64, 16, 16, SourceFormat::Xrgb8888, 16, 6.0);
        assert_eq!(frame.rgba[3], 0, "top-left pixel should be clipped");
        let mid = ((8 * 16) + 8) * 4;
        assert_eq!(frame.rgba[mid + 3], 255, "centre should be opaque");
    }

    #[test]
    fn empty_source_is_safe() {
        let frame = to_preview(&[], 0, 0, 0, SourceFormat::Xrgb8888, 10, 0.0);
        assert!(frame.rgba.is_empty());
    }
}
