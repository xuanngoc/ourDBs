//! Vector icons drawn with the painter, so they stay sharp at any scale.
//!
//! Each icon is drawn on a 16 × 16 grid that is scaled to fit the target rect.

use std::f32::consts::PI;

use eframe::egui::{Color32, Painter, Pos2, Rect, Shape, Stroke, vec2};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Icon {
    Plus,
    Close,
    Check,
    ChevronRight,
    ChevronDown,
    ChevronLeft,
    ChevronUp,
    Play,
    Stop,
    Refresh,
    Trash,
    Search,
    Table,
    View,
    Collection,
    Database,
    Export,
    History,
    Columns,
    Key,
    Terminal,
    Warning,
}

/// Paint `icon` centered in `rect` with `color`.
pub fn paint(p: &Painter, rect: Rect, icon: Icon, color: Color32) {
    let s = rect.width().min(rect.height()) / 16.0;
    let origin = rect.center() - vec2(8.0, 8.0) * s;
    let pt = |x: f32, y: f32| origin + vec2(x, y) * s;
    let stroke = Stroke::new((1.35 * s).max(1.0), color);
    let line = |pts: &[(f32, f32)]| {
        let pts: Vec<Pos2> = pts.iter().map(|&(x, y)| pt(x, y)).collect();
        p.add(Shape::line(pts, stroke));
    };
    // Points on an ellipse from angle a0 to a1 (radians, y points down).
    let arc = |cx: f32, cy: f32, rx: f32, ry: f32, a0: f32, a1: f32| -> Vec<Pos2> {
        let n = 24;
        (0..=n)
            .map(|i| {
                let a = a0 + (a1 - a0) * i as f32 / n as f32;
                pt(cx + rx * a.cos(), cy + ry * a.sin())
            })
            .collect()
    };
    let deg = |d: f32| d * PI / 180.0;

    match icon {
        Icon::Plus => {
            line(&[(8.0, 3.0), (8.0, 13.0)]);
            line(&[(3.0, 8.0), (13.0, 8.0)]);
        }
        Icon::Close => {
            line(&[(4.3, 4.3), (11.7, 11.7)]);
            line(&[(11.7, 4.3), (4.3, 11.7)]);
        }
        Icon::Check => line(&[(3.3, 8.4), (6.6, 11.6), (12.8, 4.6)]),
        Icon::ChevronRight => line(&[(6.0, 3.5), (10.5, 8.0), (6.0, 12.5)]),
        Icon::ChevronDown => line(&[(3.5, 6.0), (8.0, 10.5), (12.5, 6.0)]),
        Icon::ChevronLeft => line(&[(10.0, 3.5), (5.5, 8.0), (10.0, 12.5)]),
        Icon::ChevronUp => line(&[(3.5, 10.0), (8.0, 5.5), (12.5, 10.0)]),
        Icon::Play => {
            let pts = vec![pt(4.8, 3.0), pt(13.2, 8.0), pt(4.8, 13.0)];
            p.add(Shape::convex_polygon(pts, color, Stroke::NONE));
        }
        Icon::Stop => {
            let r = Rect::from_min_max(pt(4.0, 4.0), pt(12.0, 12.0));
            p.rect_filled(r, 1.5 * s, color);
        }
        Icon::Refresh => {
            let (cx, cy, r) = (8.0, 8.4, 4.9);
            let (a0, a1) = (deg(-25.0), deg(250.0));
            p.add(Shape::line(arc(cx, cy, r, r, a0, a1), stroke));
            // Arrow head at the end, pointing along the arc.
            let end = (cx + r * a1.cos(), cy + r * a1.sin());
            let d = (-a1.sin(), a1.cos());
            let n = (-d.1, d.0);
            let head = vec![
                pt(end.0 + d.0 * 2.6, end.1 + d.1 * 2.6),
                pt(end.0 + n.0 * 2.3 - d.0 * 0.6, end.1 + n.1 * 2.3 - d.1 * 0.6),
                pt(end.0 - n.0 * 2.3 - d.0 * 0.6, end.1 - n.1 * 2.3 - d.1 * 0.6),
            ];
            p.add(Shape::convex_polygon(head, color, Stroke::NONE));
        }
        Icon::Trash => {
            line(&[(2.8, 4.5), (13.2, 4.5)]);
            line(&[(6.0, 4.5), (6.0, 2.7), (10.0, 2.7), (10.0, 4.5)]);
            line(&[(4.2, 4.5), (4.9, 13.3), (11.1, 13.3), (11.8, 4.5)]);
            line(&[(6.9, 6.9), (6.9, 11.2)]);
            line(&[(9.1, 6.9), (9.1, 11.2)]);
        }
        Icon::Search => {
            p.circle_stroke(pt(7.0, 7.0), 4.4 * s, stroke);
            p.add(Shape::line(
                vec![pt(10.3, 10.3), pt(13.6, 13.6)],
                Stroke::new(stroke.width * 1.2, color),
            ));
        }
        Icon::Table => {
            let r = Rect::from_min_max(pt(2.5, 3.0), pt(13.5, 13.0));
            p.rect_stroke(r, 1.6 * s, stroke, eframe::egui::StrokeKind::Middle);
            line(&[(2.5, 6.6), (13.5, 6.6)]);
            line(&[(2.5, 9.8), (13.5, 9.8)]);
            line(&[(6.6, 6.6), (6.6, 13.0)]);
        }
        Icon::View => {
            let r = Rect::from_min_max(pt(2.5, 3.0), pt(13.5, 13.0));
            p.rect_stroke(r, 1.6 * s, stroke, eframe::egui::StrokeKind::Middle);
            line(&[(2.5, 6.6), (13.5, 6.6)]);
            // An eye in the body.
            let mut eye = arc(8.0, 11.9, 3.6, 2.6, deg(200.0), deg(340.0));
            eye.extend(arc(8.0, 7.9, 3.6, 2.6, deg(20.0), deg(160.0)));
            p.add(Shape::closed_line(
                eye,
                Stroke::new(stroke.width * 0.85, color),
            ));
            p.circle_filled(pt(8.0, 9.9), 0.95 * s, color);
        }
        Icon::Collection => {
            let left = [
                (6.6, 2.6),
                (5.6, 2.8),
                (5.0, 3.6),
                (5.0, 6.8),
                (4.5, 7.6),
                (3.4, 8.0),
                (4.5, 8.4),
                (5.0, 9.2),
                (5.0, 12.4),
                (5.6, 13.2),
                (6.6, 13.4),
            ];
            line(&left);
            let right: Vec<(f32, f32)> = left.iter().map(|&(x, y)| (16.0 - x, y)).collect();
            line(&right);
        }
        Icon::Database => {
            let (rx, ry) = (4.7, 1.8);
            p.add(Shape::closed_line(
                arc(8.0, 4.2, rx, ry, 0.0, 2.0 * PI),
                stroke,
            ));
            line(&[(3.3, 4.2), (3.3, 11.8)]);
            line(&[(12.7, 4.2), (12.7, 11.8)]);
            p.add(Shape::line(arc(8.0, 8.0, rx, ry, 0.0, PI), stroke));
            p.add(Shape::line(arc(8.0, 11.8, rx, ry, 0.0, PI), stroke));
        }
        Icon::Export => {
            line(&[
                (5.2, 6.6),
                (3.5, 6.6),
                (3.5, 13.5),
                (12.5, 13.5),
                (12.5, 6.6),
                (10.8, 6.6),
            ]);
            line(&[(8.0, 9.8), (8.0, 2.3)]);
            line(&[(5.6, 4.7), (8.0, 2.3), (10.4, 4.7)]);
        }
        Icon::History => {
            p.circle_stroke(pt(8.0, 8.0), 5.7 * s, stroke);
            line(&[(8.0, 4.8), (8.0, 8.2), (10.5, 9.6)]);
        }
        Icon::Columns => {
            for y in [4.5, 8.0, 11.5] {
                p.circle_filled(pt(3.6, y), 1.0 * s, color);
                line(&[(6.2, y), (13.2, y)]);
            }
        }
        Icon::Key => {
            p.circle_stroke(pt(5.4, 10.6), 2.8 * s, stroke);
            line(&[(7.4, 8.6), (13.2, 2.8)]);
            line(&[(11.3, 4.7), (12.9, 6.3)]);
            line(&[(9.7, 6.3), (10.9, 7.5)]);
        }
        Icon::Terminal => {
            line(&[(3.5, 4.8), (7.0, 8.0), (3.5, 11.2)]);
            line(&[(8.6, 11.4), (12.8, 11.4)]);
        }
        Icon::Warning => {
            let tri = vec![pt(8.0, 1.8), pt(14.8, 13.8), pt(1.2, 13.8)];
            p.add(Shape::convex_polygon(tri, color, Stroke::NONE));
            let white = Stroke::new(1.6 * s, Color32::WHITE);
            p.add(Shape::line(vec![pt(8.0, 6.0), pt(8.0, 9.8)], white));
            p.circle_filled(pt(8.0, 11.8), 0.95 * s, Color32::WHITE);
        }
    }
}
