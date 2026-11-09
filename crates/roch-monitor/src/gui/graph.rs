//! The graph panel: each selected sensor's history over the last few minutes, the way HWiNFO
//! opens a graph from a sensor row. Sensors sharing a unit share a chart; a temperature and a fan
//! speed never share an axis.

use eframe::egui::{self, Align2, Color32, Pos2, Rect, Sense, Shape, Stroke, Ui, pos2, vec2};
use roch_sensors::model::format_value;

use super::theme::{self, Palette};

pub struct Series {
    pub key: String,
    pub label: String,
    pub unit: String,
    pub decimals: u8,
    pub color: Color32,
    pub points: Vec<(f32, f32)>,
    pub current: f64,
}

/// "-5:00" for 300 seconds back.
pub fn ago(seconds: f32) -> String {
    let s = seconds.round() as i64;
    if s == 0 { "now".into() } else { format!("-{}:{:02}", s / 60, s % 60) }
}

/// A y range that shows every point with a little air, and something sensible for a flat line.
pub fn y_range(values: impl Iterator<Item = f32>) -> Option<(f32, f32)> {
    let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
    for v in values.filter(|v| v.is_finite()) {
        lo = lo.min(v);
        hi = hi.max(v);
    }
    if !lo.is_finite() {
        return None;
    }
    let span = hi - lo;
    let pad = if span > 0.0 { span * 0.08 } else { (hi.abs() * 0.05).max(1.0) };
    Some((lo - pad, hi + pad))
}

/// Draw every chart into the space given. Returns the keys whose remove button was clicked.
pub fn draw(ui: &mut Ui, series: &[Series], now: f32, window: f32, p: &Palette) -> Vec<String> {
    let mut removed = Vec::new();
    let mut units: Vec<&str> = Vec::new();
    for s in series {
        if !units.contains(&s.unit.as_str()) {
            units.push(&s.unit);
        }
    }
    if units.is_empty() {
        ui.label(
            egui::RichText::new("Click sensors to graph them; Ctrl+click adds without leaving the list.")
                .color(p.muted),
        );
        return removed;
    }
    // Take exactly the space that is left, once, and divide it by hand. Allocating chart by chart
    // added the item spacing on top of it, the panel grew to fit, and the next frame's charts were
    // sized from the bigger panel: it crept 4 px a frame until it filled the window.
    let area = ui.available_rect_before_wrap();
    ui.allocate_rect(area, Sense::hover());
    let gap = 8.0;
    let n = units.len() as f32;
    let each = ((area.height() - gap * (n - 1.0)) / n).max(40.0);
    for (u_index, unit) in units.iter().enumerate() {
        let top = area.top() + u_index as f32 * (each + gap);
        let rect = Rect::from_min_size(pos2(area.left(), top), vec2(area.width(), each));
        let members: Vec<&Series> = series.iter().filter(|s| s.unit == *unit).collect();
        removed.extend(chart(ui, rect, &members, now, window, p));
    }
    removed
}

fn chart(ui: &mut Ui, rect: Rect, members: &[&Series], now: f32, window: f32, p: &Palette) -> Vec<String> {
    let mut removed = Vec::new();
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, egui::CornerRadius::same(4), p.panel);
    painter.rect_stroke(rect, egui::CornerRadius::same(4), Stroke::new(1.0, p.border), egui::StrokeKind::Inside);

    // Legend along the top, wrapping onto further lines: swatch, name, current value, remove button.
    const LINE: f32 = 18.0;
    let left = rect.left() + 8.0;
    let mut x = left;
    let mut legend_y = rect.top() + 11.0;
    for s in members {
        let text = format!("{}  {}", s.label, format_value(s.current, &s.unit, s.decimals));
        let galley = painter.layout_no_wrap(text, theme::regular(theme::body() - 1.0), p.text);
        let item = 12.0 + galley.size().x + 6.0 + 14.0 + 14.0;
        if x > left && x + item > rect.right() - 8.0 {
            x = left;
            legend_y += LINE;
        }
        painter.rect_filled(
            Rect::from_center_size(pos2(x + 4.0, legend_y), vec2(8.0, 8.0)),
            egui::CornerRadius::same(2),
            s.color,
        );
        x += 12.0;
        let width = galley.size().x;
        painter.galley(pos2(x, legend_y - galley.size().y / 2.0), galley, p.text);
        x += width + 6.0;
        let close = Rect::from_center_size(pos2(x + 5.0, legend_y), vec2(14.0, 14.0));
        let resp = ui.interact(close, ui.scope_id().with(("graph-remove", &s.key)), Sense::click());
        let color = if resp.hovered() { p.accent } else { p.muted };
        painter.text(close.center(), Align2::CENTER_CENTER, "×", theme::bold(theme::body()), color);
        if resp.clicked() {
            removed.push(s.key.clone());
        }
        resp.on_hover_text("Remove from the graph");
        x += 28.0;
    }

    let plot =
        Rect::from_min_max(pos2(rect.left() + 70.0, legend_y + 13.0), pos2(rect.right() - 12.0, rect.bottom() - 20.0));
    if plot.width() < 40.0 || plot.height() < 20.0 {
        return removed;
    }
    let t0 = now - window;
    let visible = members.iter().flat_map(|s| s.points.iter().filter(|(t, _)| *t >= t0).map(|(_, v)| *v));
    let Some((lo, hi)) = y_range(visible) else {
        painter.text(plot.center(), Align2::CENTER_CENTER, "No readings yet", theme::regular(theme::body()), p.muted);
        return removed;
    };
    let to_screen = |t: f32, v: f32| -> Pos2 {
        pos2(plot.left() + (t - t0) / window * plot.width(), plot.bottom() - (v - lo) / (hi - lo) * plot.height())
    };

    // Grid: four bands up the value axis and along time; two up the axis on a short chart, where
    // five labels would sit on top of each other.
    let decimals = members.first().map_or(0, |s| s.decimals).min(2);
    let bands = if plot.height() < 90.0 { 2 } else { 4 };
    for i in 0..=bands {
        let f = i as f32 / bands as f32;
        let y = plot.bottom() - f * plot.height();
        painter.line_segment([pos2(plot.left(), y), pos2(plot.right(), y)], Stroke::new(1.0, p.grid));
        let v = lo + f * (hi - lo);
        // The unit rides on the top label, where it cannot collide with the legend.
        let label = if i == bands && !members[0].unit.is_empty() {
            format!("{v:.prec$} {}", members[0].unit, prec = decimals as usize)
        } else {
            format!("{v:.prec$}", prec = decimals as usize)
        };
        painter.text(
            pos2(plot.left() - 6.0, y),
            Align2::RIGHT_CENTER,
            label,
            theme::regular(theme::body() - 1.5),
            p.muted,
        );
        let tx = plot.left() + f * plot.width();
        painter.line_segment([pos2(tx, plot.top()), pos2(tx, plot.bottom())], Stroke::new(1.0, p.grid));
        painter.text(
            pos2(tx, plot.bottom() + 3.0),
            Align2::CENTER_TOP,
            ago(window * (1.0 - f)),
            theme::regular(theme::body() - 1.5),
            p.muted,
        );
    }

    // The lines, broken wherever a reading is missing rather than drawn through it.
    for s in members {
        let mut run: Vec<Pos2> = Vec::new();
        let flush = |run: &mut Vec<Pos2>| {
            if run.len() >= 2 {
                painter.add(Shape::line(std::mem::take(run), Stroke::new(1.6, s.color)));
            } else if run.len() == 1 {
                painter.circle_filled(run[0], 1.6, s.color);
                run.clear();
            }
        };
        for &(t, v) in s.points.iter().filter(|(t, _)| *t >= t0) {
            if v.is_finite() {
                run.push(to_screen(t, v));
            } else {
                flush(&mut run);
            }
        }
        flush(&mut run);
    }

    // Hover: the time under the pointer and each series' value there.
    if let Some(at) = ui.ctx().pointer_hover_pos().filter(|at| plot.contains(*at)) {
        painter.line_segment([pos2(at.x, plot.top()), pos2(at.x, plot.bottom())], Stroke::new(1.0, p.muted));
        let t = t0 + (at.x - plot.left()) / plot.width() * window;
        let mut lines = vec![ago(now - t)];
        for s in members {
            if let Some(&(_, v)) = s.points.iter().min_by(|a, b| (a.0 - t).abs().total_cmp(&(b.0 - t).abs())) {
                lines.push(format!("{}: {}", s.label, format_value(v as f64, &s.unit, s.decimals)));
            }
        }
        let text = lines.join("\n");
        let galley = painter.layout_no_wrap(text, theme::regular(theme::body() - 1.0), p.text);
        let mut pos = at + vec2(12.0, 12.0);
        if pos.x + galley.size().x + 12.0 > rect.right() {
            pos.x = at.x - galley.size().x - 18.0;
        }
        if pos.y + galley.size().y + 8.0 > rect.bottom() {
            pos.y = at.y - galley.size().y - 14.0;
        }
        let bg = Rect::from_min_size(pos - vec2(6.0, 4.0), galley.size() + vec2(12.0, 8.0));
        painter.rect_filled(bg, egui::CornerRadius::same(3), p.header);
        painter.rect_stroke(bg, egui::CornerRadius::same(3), Stroke::new(1.0, p.border), egui::StrokeKind::Inside);
        painter.galley(pos, galley, p.text);
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_read_back_from_now() {
        assert_eq!(ago(0.0), "now");
        assert_eq!(ago(300.0), "-5:00");
        assert_eq!(ago(75.0), "-1:15");
    }

    #[test]
    fn a_flat_line_still_gets_a_range() {
        let (lo, hi) = y_range([40.0f32, 40.0].into_iter()).unwrap();
        assert!(lo < 40.0 && hi > 40.0);
        assert!(y_range([f32::NAN].into_iter()).is_none());
        let (lo, hi) = y_range([10.0f32, 20.0].into_iter()).unwrap();
        assert!(lo < 10.0 && hi > 20.0 && hi - lo < 12.0);
    }
}
