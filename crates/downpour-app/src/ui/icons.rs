//! Line icons drawn with egui shapes, so they stay sharp at any scale and
//! need no icon font.

use std::f32::consts::{PI, TAU};

use downpour_core::CategoryLook;
use eframe::egui::{self, pos2, vec2, Color32, CornerRadius, Painter, Pos2, Rect, Shape, Stroke};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Plus,
    Play,
    Pause,
    Trash,
    Folder,
    Gear,
    Search,
    Download,
    Check,
    Close,
    Refresh,
    Grid,
    Clock,
    Bolt,
    Alert,
    File,
    Archive,
    Document,
    Music,
    Program,
    Video,
    Image,
    Link,
    Chart,
    Layers,
    Tag,
    Sun,
    Moon,
    Minus,
    ChevronDown,
}

impl Icon {
    pub fn for_look(look: CategoryLook) -> Icon {
        match look {
            CategoryLook::Archive => Icon::Archive,
            CategoryLook::Document => Icon::Document,
            CategoryLook::Music => Icon::Music,
            CategoryLook::Program => Icon::Program,
            CategoryLook::Video => Icon::Video,
            CategoryLook::Image => Icon::Image,
            CategoryLook::Generic => Icon::File,
        }
    }

    /// Paints the icon centered in `rect` (only its shorter side is used).
    pub fn paint(self, painter: &Painter, rect: Rect, color: Color32) {
        let side = rect.width().min(rect.height());
        let c = rect.center();
        let u = side / 16.0; // one unit of a 16x16 design grid
        let p = |x: f32, y: f32| pos2(c.x + (x - 8.0) * u, c.y + (y - 8.0) * u);
        let stroke = Stroke::new((1.6 * u).max(1.2), color);
        let line = |points: Vec<Pos2>| painter.add(Shape::line(points, stroke));
        let closed = |points: Vec<Pos2>| painter.add(Shape::closed_line(points, stroke));

        match self {
            Icon::Plus => {
                line(vec![p(8.0, 3.0), p(8.0, 13.0)]);
                line(vec![p(3.0, 8.0), p(13.0, 8.0)]);
            }
            Icon::Play => {
                painter.add(Shape::convex_polygon(
                    vec![p(5.0, 3.0), p(13.0, 8.0), p(5.0, 13.0)],
                    color,
                    Stroke::NONE,
                ));
            }
            Icon::Pause => {
                let bar = vec2(2.6 * u, 10.0 * u);
                for x in [5.0, 11.0] {
                    painter.rect_filled(
                        Rect::from_center_size(p(x, 8.0), bar),
                        CornerRadius::same(1),
                        color,
                    );
                }
            }
            Icon::Trash => {
                line(vec![p(2.5, 4.5), p(13.5, 4.5)]);
                line(vec![p(6.0, 4.5), p(6.5, 2.5), p(9.5, 2.5), p(10.0, 4.5)]);
                closed(vec![p(4.0, 4.5), p(12.0, 4.5), p(11.2, 14.0), p(4.8, 14.0)]);
                line(vec![p(6.8, 7.0), p(6.8, 11.5)]);
                line(vec![p(9.2, 7.0), p(9.2, 11.5)]);
            }
            Icon::Folder => {
                closed(vec![
                    p(2.0, 4.0),
                    p(6.0, 4.0),
                    p(7.5, 5.5),
                    p(14.0, 5.5),
                    p(14.0, 13.0),
                    p(2.0, 13.0),
                ]);
                line(vec![p(2.0, 7.5), p(14.0, 7.5)]);
            }
            Icon::Gear => {
                painter.circle_stroke(c, 2.2 * u, stroke);
                painter.circle_stroke(c, 4.6 * u, stroke);
                for tooth in 0..8 {
                    let angle = tooth as f32 * TAU / 8.0;
                    let direction = vec2(angle.cos(), angle.sin());
                    line(vec![c + direction * 4.6 * u, c + direction * 6.6 * u]);
                }
            }
            Icon::Search => {
                painter.circle_stroke(p(7.0, 7.0), 4.2 * u, stroke);
                line(vec![p(10.2, 10.2), p(13.5, 13.5)]);
            }
            Icon::Download => {
                line(vec![p(8.0, 2.5), p(8.0, 10.0)]);
                line(vec![p(4.5, 6.8), p(8.0, 10.3), p(11.5, 6.8)]);
                line(vec![p(3.0, 13.5), p(13.0, 13.5)]);
            }
            Icon::Check => {
                line(vec![p(3.0, 8.5), p(6.5, 12.0), p(13.0, 4.5)]);
            }
            Icon::Close => {
                line(vec![p(4.0, 4.0), p(12.0, 12.0)]);
                line(vec![p(12.0, 4.0), p(4.0, 12.0)]);
            }
            Icon::Refresh => {
                line(arc(c, 5.0 * u, -PI * 0.35, PI * 1.45, 24));
                let tip = c + vec2((-PI * 0.35).cos(), (-PI * 0.35).sin()) * 5.0 * u;
                line(vec![
                    tip + vec2(-3.0 * u, -0.4 * u),
                    tip,
                    tip + vec2(-0.2 * u, 3.0 * u),
                ]);
            }
            Icon::Grid => {
                let cell = vec2(4.6 * u, 4.6 * u);
                for (x, y) in [(5.0, 5.0), (11.0, 5.0), (5.0, 11.0), (11.0, 11.0)] {
                    painter.rect_stroke(
                        Rect::from_center_size(p(x, y), cell),
                        CornerRadius::same(2),
                        stroke,
                        egui::StrokeKind::Middle,
                    );
                }
            }
            Icon::Minus => {
                line(vec![p(3.0, 8.0), p(13.0, 8.0)]);
            }
            Icon::ChevronDown => {
                line(vec![p(4.0, 6.0), p(8.0, 10.0), p(12.0, 6.0)]);
            }
            Icon::Sun => {
                painter.circle_stroke(c, 3.0 * u, stroke);
                for ray in 0..8 {
                    let angle = ray as f32 * std::f32::consts::FRAC_PI_4;
                    let (sin, cos) = angle.sin_cos();
                    line(vec![
                        p(8.0 + 5.2 * cos, 8.0 + 5.2 * sin),
                        p(8.0 + 6.8 * cos, 8.0 + 6.8 * sin),
                    ]);
                }
            }
            Icon::Moon => {
                // A crescent: the outer arc of a circle at (8, 8) r 6, back
                // along a circle at (11.5, 4.5) r 5 that bites into it; the
                // angles are where the two circles meet.
                let arc = |cx: f32, cy: f32, r: f32, from: f32, to: f32| -> Vec<Pos2> {
                    (0..=16)
                        .map(|step| {
                            let angle = from + (to - from) * step as f32 / 16.0;
                            p(cx + r * angle.cos(), cy + r * angle.sin())
                        })
                        .collect()
                };
                // Two open lines: joined into one outline, the sharp tips
                // would be drawn as long miter spikes.
                line(arc(8.0, 8.0, 6.0, 0.1448, 4.5676));
                line(arc(11.5, 4.5, 5.0, 3.6508, 1.0616));
            }
            Icon::Clock => {
                painter.circle_stroke(c, 6.0 * u, stroke);
                line(vec![p(8.0, 4.5), p(8.0, 8.0), p(10.8, 9.8)]);
            }
            Icon::Bolt => {
                // Two overlapping triangles: the upper and lower halves of the bolt.
                painter.add(Shape::convex_polygon(
                    vec![p(10.0, 1.0), p(3.5, 9.0), p(9.0, 9.0)],
                    color,
                    Stroke::NONE,
                ));
                painter.add(Shape::convex_polygon(
                    vec![p(7.0, 7.0), p(12.5, 7.0), p(6.0, 15.0)],
                    color,
                    Stroke::NONE,
                ));
            }
            Icon::Alert => {
                closed(vec![p(8.0, 2.0), p(14.5, 13.5), p(1.5, 13.5)]);
                line(vec![p(8.0, 6.0), p(8.0, 9.5)]);
                painter.circle_filled(p(8.0, 11.6), 0.9 * u, color);
            }
            Icon::File => {
                closed(vec![
                    p(3.5, 1.5),
                    p(9.5, 1.5),
                    p(12.5, 4.5),
                    p(12.5, 14.5),
                    p(3.5, 14.5),
                ]);
                line(vec![p(9.5, 1.5), p(9.5, 4.5), p(12.5, 4.5)]);
            }
            Icon::Archive => {
                closed(vec![p(2.5, 3.0), p(13.5, 3.0), p(13.5, 13.5), p(2.5, 13.5)]);
                line(vec![p(2.5, 6.0), p(13.5, 6.0)]);
                for y in [7.5, 9.5, 11.5] {
                    line(vec![p(7.2, y), p(8.8, y)]);
                }
            }
            Icon::Document => {
                Icon::File.paint(painter, rect, color);
                for (y, end) in [(7.5, 10.5), (9.8, 10.5), (12.0, 8.5)] {
                    line(vec![p(5.5, y), p(end, y)]);
                }
            }
            Icon::Music => {
                line(vec![p(6.0, 12.0), p(6.0, 3.0), p(13.0, 1.8), p(13.0, 10.5)]);
                painter.circle_filled(p(4.5, 12.2), 2.0 * u, color);
                painter.circle_filled(p(11.5, 10.7), 2.0 * u, color);
            }
            Icon::Program => {
                closed(vec![p(1.5, 3.0), p(14.5, 3.0), p(14.5, 13.0), p(1.5, 13.0)]);
                line(vec![p(1.5, 6.0), p(14.5, 6.0)]);
                line(vec![p(4.5, 8.5), p(6.5, 10.0), p(4.5, 11.5)]);
            }
            Icon::Image => {
                closed(vec![p(1.5, 3.0), p(14.5, 3.0), p(14.5, 13.0), p(1.5, 13.0)]);
                painter.circle_stroke(p(5.5, 6.5), 1.4 * u, stroke);
                line(vec![
                    p(2.0, 12.0),
                    p(6.5, 8.5),
                    p(9.0, 10.5),
                    p(11.0, 8.5),
                    p(14.0, 11.5),
                ]);
            }
            Icon::Link => {
                // Two chain links at 45 degrees.
                line(vec![p(6.5, 9.5), p(9.5, 6.5)]);
                closed(vec![
                    p(4.5, 8.5),
                    p(2.5, 10.5),
                    p(2.5, 12.0),
                    p(4.0, 13.5),
                    p(5.5, 13.5),
                    p(7.5, 11.5),
                ]);
                closed(vec![
                    p(11.5, 7.5),
                    p(13.5, 5.5),
                    p(13.5, 4.0),
                    p(12.0, 2.5),
                    p(10.5, 2.5),
                    p(8.5, 4.5),
                ]);
            }
            Icon::Chart => {
                line(vec![p(2.0, 2.5), p(2.0, 14.0), p(14.0, 14.0)]);
                for (x, top) in [(5.0, 10.0), (8.0, 6.0), (11.0, 8.0)] {
                    painter.rect_filled(
                        Rect::from_min_max(p(x - 1.0, top), p(x + 1.0, 12.5)),
                        CornerRadius::same(1),
                        color,
                    );
                }
            }
            Icon::Layers => {
                closed(vec![p(8.0, 2.0), p(14.5, 5.5), p(8.0, 9.0), p(1.5, 5.5)]);
                line(vec![p(1.5, 8.5), p(8.0, 12.0), p(14.5, 8.5)]);
                line(vec![p(1.5, 11.0), p(8.0, 14.5), p(14.5, 11.0)]);
            }
            Icon::Tag => {
                closed(vec![
                    p(2.0, 2.0),
                    p(8.5, 2.0),
                    p(14.5, 8.0),
                    p(8.0, 14.5),
                    p(2.0, 8.5),
                ]);
                painter.circle_filled(p(5.2, 5.2), 1.1 * u, color);
            }
            Icon::Video => {
                closed(vec![p(1.5, 3.5), p(14.5, 3.5), p(14.5, 12.5), p(1.5, 12.5)]);
                painter.add(Shape::convex_polygon(
                    vec![p(6.5, 5.8), p(10.8, 8.0), p(6.5, 10.2)],
                    color,
                    Stroke::NONE,
                ));
            }
        }
    }
}

/// Points along a circular arc from `start` to `end` radians.
pub fn arc(center: Pos2, radius: f32, start: f32, end: f32, segments: usize) -> Vec<Pos2> {
    (0..=segments)
        .map(|step| {
            let angle = start + (end - start) * step as f32 / segments as f32;
            center + vec2(angle.cos(), angle.sin()) * radius
        })
        .collect()
}

/// The app's mark as a texture, for drawing inside the window. Made once
/// and kept in egui's memory.
pub fn brand_mark(ctx: &egui::Context) -> egui::TextureHandle {
    use downpour_protocol::brand::{icon_rgba, ICON_SIZE};
    let id = egui::Id::new("downpour-brand-mark");
    if let Some(texture) = ctx.data(|data| data.get_temp::<egui::TextureHandle>(id)) {
        return texture;
    }
    let image = egui::ColorImage::from_rgba_unmultiplied([ICON_SIZE, ICON_SIZE], &icon_rgba());
    let texture = ctx.load_texture("downpour-brand-mark", image, egui::TextureOptions::LINEAR);
    ctx.data_mut(|data| data.insert_temp(id, texture.clone()));
    texture
}

/// The window icon, shared with the installer.
pub fn app_icon() -> egui::IconData {
    use downpour_protocol::brand::{icon_rgba, ICON_SIZE};
    egui::IconData {
        rgba: icon_rgba(),
        width: ICON_SIZE as u32,
        height: ICON_SIZE as u32,
    }
}
