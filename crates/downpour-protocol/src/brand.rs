//! The Downpour mark (a raindrop split white and coral, with a download
//! chevron, on a slate rounded square), rasterized in code so the app and
//! the installer share one icon without shipping image files. Keep in step
//! with `scripts/generate_icons.py`.

/// Window icon size; the window manager scales it as needed.
pub const ICON_SIZE: usize = 64;
/// The mark's background, also the app's own slate.
pub const SLATE: [u8; 3] = [0x40, 0x44, 0x4B];
/// The right half of the drop, also the app's own coral.
pub const CORAL: [u8; 3] = [0xF8, 0x61, 0x5A];
const WHITE: [u8; 3] = [255, 255, 255];
/// Samples per pixel along each axis, for smooth edges.
const SUBSAMPLES: usize = 4;

const CORNER_RADIUS: f32 = 16.0;
const DROP_CENTER: (f32, f32) = (32.0, 38.0);
const DROP_RADIUS: f32 = 17.0;
const DROP_TIP_Y: f32 = 8.0;
const CHEVRON_HALF_STROKE: f32 = 2.5;

/// RGBA pixels of the mark, `ICON_SIZE` × `ICON_SIZE`, row by row.
pub fn icon_rgba() -> Vec<u8> {
    let mut rgba = vec![0u8; ICON_SIZE * ICON_SIZE * 4];
    for y in 0..ICON_SIZE {
        for x in 0..ICON_SIZE {
            let (mut sum, mut covered) = ([0f32; 3], 0usize);
            for sy in 0..SUBSAMPLES {
                for sx in 0..SUBSAMPLES {
                    let fx = x as f32 + (sx as f32 + 0.5) / SUBSAMPLES as f32;
                    let fy = y as f32 + (sy as f32 + 0.5) / SUBSAMPLES as f32;
                    let Some(color) = color_at(fx, fy) else {
                        continue;
                    };
                    covered += 1;
                    for (total, channel) in sum.iter_mut().zip(color) {
                        *total += f32::from(channel);
                    }
                }
            }
            if covered == 0 {
                continue;
            }
            let pixel = &mut rgba[(y * ICON_SIZE + x) * 4..][..4];
            for (out, total) in pixel.iter_mut().zip(sum) {
                *out = (total / covered as f32).round() as u8;
            }
            pixel[3] = (255 * covered / (SUBSAMPLES * SUBSAMPLES)) as u8;
        }
    }
    rgba
}

/// The mark's color at a point of the 64-unit grid, or `None` outside it.
fn color_at(x: f32, y: f32) -> Option<[u8; 3]> {
    if !inside_rounded_square(x, y) {
        return None;
    }
    if on_chevron(x, y) || !inside_drop(x, y) {
        return Some(SLATE);
    }
    Some(if x < 32.0 { WHITE } else { CORAL })
}

fn inside_rounded_square(x: f32, y: f32) -> bool {
    let half = ICON_SIZE as f32 / 2.0;
    let dx = ((x - half).abs() - (half - CORNER_RADIUS)).max(0.0);
    let dy = ((y - half).abs() - (half - CORNER_RADIUS)).max(0.0);
    dx * dx + dy * dy <= CORNER_RADIUS * CORNER_RADIUS
}

/// A round bottom and a pointed top whose sides touch the circle.
fn inside_drop(x: f32, y: f32) -> bool {
    let (cx, cy) = DROP_CENTER;
    if (x - cx).powi(2) + (y - cy).powi(2) <= DROP_RADIUS * DROP_RADIUS {
        return true;
    }
    // Where the sides meet the circle, seen from the circle's center.
    let cos = DROP_RADIUS / (cy - DROP_TIP_Y);
    let sin = (1.0 - cos * cos).sqrt();
    let (touch_dx, touch_y) = (DROP_RADIUS * sin, cy - DROP_RADIUS * cos);
    if !(DROP_TIP_Y..=touch_y).contains(&y) {
        return false;
    }
    (x - cx).abs() <= touch_dx * (y - DROP_TIP_Y) / (touch_y - DROP_TIP_Y)
}

/// The download chevron from (24, 37) down to (32, 45) and up to (40, 37).
fn on_chevron(x: f32, y: f32) -> bool {
    let near = |(ax, ay): (f32, f32), (bx, by): (f32, f32)| {
        let (dx, dy) = (bx - ax, by - ay);
        let along = (((x - ax) * dx + (y - ay) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
        let (px, py) = (ax + along * dx - x, ay + along * dy - y);
        px * px + py * py <= CHEVRON_HALF_STROKE * CHEVRON_HALF_STROKE
    };
    near((24.0, 37.0), (32.0, 45.0)) || near((32.0, 45.0), (40.0, 37.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mark_has_transparent_corners_split_drop_and_slate_chevron() {
        let rgba = icon_rgba();
        let pixel = |x: usize, y: usize| &rgba[(y * ICON_SIZE + x) * 4..][..4];
        assert_eq!(pixel(0, 0), [0, 0, 0, 0]);
        assert_eq!(pixel(22, 30)[..3], WHITE);
        assert_eq!(pixel(42, 30)[..3], CORAL);
        assert_eq!(pixel(32, 44)[..3], SLATE);
        assert_eq!(pixel(6, 32)[..3], SLATE);
    }
}
