//! Parameter editors: layout (pure), value math (pure) and drawing.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use crate::plugin::{EditorKind, FilterDescriptor, ParamSpec, EQ_BANDS};

use super::layout::contains;

pub const ACCENT: Color = Color::Cyan;
pub const DIM: Color = Color::DarkGray;

const LABEL_W: u16 = 16;
const VALUE_W: u16 = 12;
pub const EQ_FINE_DB: f32 = 0.5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SliderRow {
    pub param: usize,
    pub label: Rect,
    pub bar: Rect,
    pub value: Rect,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EqLayout {
    /// Full column (value row, bars, freq label row) per band.
    pub cols: Vec<Rect>,
    /// Bar area per band.
    pub bars: Vec<Rect>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditorLayout {
    pub eq: Option<EqLayout>,
    pub rows: Vec<SliderRow>,
    pub meter: Option<Rect>,
}

pub fn is_eq(desc: &FilterDescriptor) -> bool {
    desc.editor == EditorKind::Equalizer && desc.params.len() >= EQ_BANDS.len()
}

/// Slider rows for the given parameter indices; windowed so `selected` is visible.
pub fn slider_rows(area: Rect, params: &[usize], selected: usize) -> Vec<SliderRow> {
    let h = area.height as usize;
    if h == 0 || area.width < LABEL_W + VALUE_W + 6 {
        return Vec::new();
    }
    let pos = params.iter().position(|&p| p == selected).unwrap_or(0);
    let start = if params.len() <= h { 0 } else { (pos + 1).saturating_sub(h).min(params.len() - h) };
    let bar_w = area.width - LABEL_W - VALUE_W - 2;
    params
        .iter()
        .enumerate()
        .skip(start)
        .take(h)
        .map(|(i, &p)| {
            let y = area.y + (i - start) as u16;
            SliderRow {
                param: p,
                label: Rect::new(area.x, y, LABEL_W, 1),
                bar: Rect::new(area.x + LABEL_W + 1, y, bar_w, 1),
                value: Rect::new(area.x + LABEL_W + 2 + bar_w, y, VALUE_W, 1),
            }
        })
        .collect()
}

pub fn eq_layout(area: Rect) -> EqLayout {
    let n = EQ_BANDS.len() as u16;
    let col_w = (area.width / n).clamp(1, 9);
    let total = col_w * n;
    let x0 = area.x + (area.width.saturating_sub(total)) / 2;
    let mut cols = Vec::new();
    let mut bars = Vec::new();
    for b in 0..n {
        let col = Rect::new(x0 + b * col_w, area.y, col_w, area.height);
        let bw = 3.min(col_w);
        let bar = Rect::new(col.x + (col_w - bw) / 2, area.y + 1, bw, area.height.saturating_sub(2));
        cols.push(col);
        bars.push(bar);
    }
    EqLayout { cols, bars }
}

pub fn editor_layout(area: Rect, desc: &FilterDescriptor, param_sel: usize) -> EditorLayout {
    let has_meter = desc.meter_label.is_some();
    let meter_rows = u16::from(has_meter);
    if is_eq(desc) {
        let extras: Vec<usize> = (EQ_BANDS.len()..desc.params.len()).collect();
        let k = extras.len() as u16 + meter_rows;
        let eq_h = if k == 0 { area.height } else { area.height.saturating_sub(k + 1).max(5.min(area.height)) };
        let eq = eq_layout(Rect { height: eq_h, ..area });
        let rest = Rect::new(area.x, area.y + (eq_h + 1).min(area.height), area.width, area.height.saturating_sub(eq_h + 1));
        let rows_area = Rect { height: rest.height.saturating_sub(meter_rows), ..rest };
        let rows = slider_rows(rows_area, &extras, param_sel);
        let meter = (has_meter && rest.height > 0)
            .then(|| Rect::new(rest.x, rest.y + rows.len() as u16, rest.width, 1))
            .filter(|m| m.y < area.y + area.height);
        EditorLayout { eq: Some(eq), rows, meter }
    } else {
        let params: Vec<usize> = (0..desc.params.len()).collect();
        let rows_area = Rect { height: area.height.saturating_sub(meter_rows + u16::from(has_meter)), ..area };
        let rows = slider_rows(rows_area, &params, param_sel);
        let meter = (has_meter && !rows.is_empty())
            .then(|| Rect::new(area.x, area.y + rows.len() as u16 + 1, area.width, 1))
            .filter(|m| m.y < area.y + area.height);
        EditorLayout { eq: None, rows, meter }
    }
}

pub enum EditorHit {
    Param(usize),
    Band(usize),
}

pub fn hit_editor(layout: &EditorLayout, x: u16, y: u16) -> Option<EditorHit> {
    if let Some(eq) = &layout.eq
        && let Some(b) = eq.cols.iter().position(|c| contains(*c, x, y))
    {
        return Some(EditorHit::Band(b));
    }
    layout
        .rows
        .iter()
        .find(|r| y == r.label.y && x >= r.label.x && x < r.value.x + r.value.width)
        .map(|r| EditorHit::Param(r.param))
}

/// Map a y coordinate inside an EQ bar to a normalized -1..=1 position.
pub fn eq_bar_value(bar: Rect, y: u16) -> f32 {
    if bar.height == 0 {
        return 0.0;
    }
    let center = bar.y + bar.height / 2;
    let up = (bar.height / 2) as f32;
    let down = (bar.height - bar.height / 2 - 1) as f32;
    if y < center {
        let d = (center - y.max(bar.y)) as f32;
        (d / up.max(1.0)).min(1.0)
    } else if y == center {
        0.0
    } else {
        let d = (y.min(bar.y + bar.height - 1) - center) as f32;
        -(d / down.max(1.0)).min(1.0)
    }
}

/// Apply a relative change (in normalized slider units) to a value. Stepped
/// params always move by at least one step.
pub fn adjust_value(spec: &ParamSpec, v: f32, delta_norm: f32) -> f32 {
    if spec.step > 0.0 {
        let range = spec.max - spec.min;
        let steps = (delta_norm.abs() * range / spec.step).round().max(1.0);
        return spec.sanitize(v + delta_norm.signum() * steps * spec.step);
    }
    spec.from_norm(spec.to_norm(v) + delta_norm)
}

/// dB-scaled meter position (-60 dB .. 0 dB) -> 0..=1.
pub fn peak_to_frac(peak: f32) -> f32 {
    let db = 20.0 * peak.max(1e-6).log10();
    ((db + 60.0) / 60.0).clamp(0.0, 1.0)
}

pub fn meter_color(frac: f32) -> Color {
    if frac > 0.97 {
        Color::Red
    } else if frac > 0.85 {
        Color::Yellow
    } else {
        Color::Green
    }
}

/// Horizontal level meter filling `w` cells starting at (x, y).
pub fn draw_level(buf: &mut Buffer, x: u16, y: u16, w: u16, frac: f32, dimmed: bool) {
    let filled = (frac * w as f32).round() as u16;
    for i in 0..w {
        let (ch, style) = if i < filled {
            let c = if dimmed { DIM } else { meter_color((i + 1) as f32 / w as f32) };
            ("━", Style::default().fg(c))
        } else {
            ("╌", Style::default().fg(DIM))
        };
        buf.set_string(x + i, y, ch, style);
    }
}

pub fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else if n == 0 {
        String::new()
    } else {
        let mut t: String = s.chars().take(n - 1).collect();
        t.push('…');
        t
    }
}

fn draw_slider(buf: &mut Buffer, row: &SliderRow, spec: &ParamSpec, v: f32, selected: bool, focus: bool) {
    let active = selected && focus;
    let label_style = if active {
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
    } else if selected {
        Style::default().fg(Color::White)
    } else {
        Style::default().fg(Color::Gray)
    };
    let mark = if active { "▸ " } else { "  " };
    buf.set_stringn(row.label.x, row.label.y, format!("{mark}{}", spec.label), row.label.width as usize, label_style);
    let w = row.bar.width;
    if w > 0 {
        let pos = (spec.to_norm(v) * (w - 1) as f32).round() as u16;
        for i in 0..w {
            let (ch, st) = if i < pos {
                ("━", Style::default().fg(if active { ACCENT } else { Color::Gray }))
            } else if i == pos {
                ("●", Style::default().fg(Color::White).add_modifier(if active { Modifier::BOLD } else { Modifier::empty() }))
            } else {
                ("─", Style::default().fg(DIM))
            };
            buf.set_string(row.bar.x + i, row.bar.y, ch, st);
        }
    }
    let txt = spec.format_value(v);
    let txt = truncate(&txt, row.value.width as usize);
    let pad = row.value.width as usize - txt.chars().count();
    buf.set_string(
        row.value.x + pad as u16,
        row.value.y,
        &txt,
        if active { Style::default().fg(ACCENT) } else { Style::default().fg(Color::Gray) },
    );
}

fn fmt_freq(f: f32) -> String {
    if f >= 1000.0 {
        let k = f / 1000.0;
        if (k - k.round()).abs() < 1e-3 { format!("{k:.0}k") } else { format!("{k:.1}k") }
    } else {
        format!("{f:.0}")
    }
}

fn draw_eq(buf: &mut Buffer, eq: &EqLayout, desc: &FilterDescriptor, values: &[f32], sel: usize, focus: bool) {
    for (b, (col, bar)) in eq.cols.iter().zip(&eq.bars).enumerate() {
        let spec = &desc.params[b];
        let v = values.get(b).copied().unwrap_or(spec.default);
        let selected = b == sel;
        let active = selected && focus;
        let color = if active { ACCENT } else if selected { Color::White } else { Color::Gray };
        let cx = col.x + col.width / 2;
        // gain readout
        let g = format!("{v:+.1}");
        let gx = cx.saturating_sub(g.chars().count() as u16 / 2).max(col.x);
        buf.set_stringn(gx, col.y, &g, col.width as usize, Style::default().fg(color));
        // bar
        if bar.height > 0 {
            let center = bar.y + bar.height / 2;
            let up = (bar.height / 2) as f32;
            let down = (bar.height - bar.height / 2 - 1) as f32;
            let span = (spec.max - spec.min) / 2.0;
            let n = if span > 0.0 { ((v - (spec.min + span)) / span).clamp(-1.0, 1.0) } else { 0.0 };
            for dx in 0..bar.width {
                buf.set_string(bar.x + dx, center, "─", Style::default().fg(DIM));
            }
            let fill = Style::default().fg(color);
            if n > 0.0 {
                let units = n * up;
                let full = units.floor() as u16;
                for r in 0..full {
                    for dx in 0..bar.width {
                        buf.set_string(bar.x + dx, center - 1 - r, "█", fill);
                    }
                }
                let frac = units - full as f32;
                let tip = ["", "▁", "▂", "▃", "▄", "▅", "▆", "▇"][(frac * 8.0) as usize % 8];
                if !tip.is_empty() && full < up as u16 {
                    for dx in 0..bar.width {
                        buf.set_string(bar.x + dx, center - 1 - full, tip, fill);
                    }
                }
            } else if n < 0.0 {
                let units = -n * down;
                let full = units.floor() as u16;
                for r in 0..full {
                    for dx in 0..bar.width {
                        buf.set_string(bar.x + dx, center + 1 + r, "█", fill);
                    }
                }
                if units - full as f32 >= 0.5 && (full as f32) < down {
                    for dx in 0..bar.width {
                        buf.set_string(bar.x + dx, center + 1 + full, "▀", fill);
                    }
                }
            } else {
                for dx in 0..bar.width {
                    buf.set_string(bar.x + dx, center, "─", Style::default().fg(color));
                }
            }
        }
        // frequency label
        let f = fmt_freq(EQ_BANDS[b]);
        let fx = cx.saturating_sub(f.chars().count() as u16 / 2).max(col.x);
        let ly = col.y + col.height.saturating_sub(1);
        let st = if selected { Style::default().fg(color).add_modifier(Modifier::BOLD) } else { Style::default().fg(DIM) };
        buf.set_stringn(fx, ly, &f, col.width as usize, st);
    }
}

/// Draw the editor for a filter into `area` (content rect).
pub fn draw_editor(
    buf: &mut Buffer,
    area: Rect,
    desc: &FilterDescriptor,
    values: &[f32],
    param_sel: usize,
    focus: bool,
    meter: Option<f32>,
) {
    if desc.params.is_empty() {
        buf.set_string(area.x, area.y, "This filter has no parameters.", Style::default().fg(DIM));
        return;
    }
    let layout = editor_layout(area, desc, param_sel);
    if let Some(eq) = &layout.eq {
        draw_eq(buf, eq, desc, values, param_sel, focus);
    }
    for row in &layout.rows {
        let spec = &desc.params[row.param];
        let v = values.get(row.param).copied().unwrap_or(spec.default);
        draw_slider(buf, row, spec, v, row.param == param_sel, focus);
    }
    if let (Some(m), Some(label)) = (layout.meter, &desc.meter_label) {
        buf.set_stringn(m.x + 2, m.y, label, LABEL_W as usize - 2, Style::default().fg(Color::Gray));
        let bw = m.width.saturating_sub(LABEL_W + VALUE_W + 2);
        let db = meter.unwrap_or(0.0);
        let frac = (db.abs() / 24.0).clamp(0.0, 1.0);
        let filled = (frac * bw as f32).round() as u16;
        for i in 0..bw {
            let (ch, st) = if i < filled { ("▰", Style::default().fg(Color::Yellow)) } else { ("▱", Style::default().fg(DIM)) };
            buf.set_string(m.x + LABEL_W + 1 + i, m.y, ch, st);
        }
        let txt = format!("{db:.1} dB");
        let pad = (VALUE_W as usize).saturating_sub(txt.chars().count());
        buf.set_string(m.x + LABEL_W + 2 + bw + pad as u16, m.y, txt, Style::default().fg(Color::Gray));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::Passthrough;

    fn desc(n: usize, eq: bool) -> FilterDescriptor {
        let params = (0..n).map(|i| ParamSpec::new(&format!("p{i}"), &format!("P{i}"), -12.0, 12.0, 0.0)).collect();
        let mut d = FilterDescriptor::native("t", "T", "Tone", params, |_| Box::new(Passthrough));
        if eq {
            d = d.editor(EditorKind::Equalizer);
        }
        d
    }

    #[test]
    fn slider_rows_hit_test_and_scroll() {
        let area = Rect::new(0, 0, 80, 3);
        let params: Vec<usize> = (0..6).collect();
        let rows = slider_rows(area, &params, 0);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].param, 0);
        let rows = slider_rows(area, &params, 5);
        assert_eq!(rows.last().unwrap().param, 5);
        assert_eq!(rows[0].param, 3);
        let layout = EditorLayout { eq: None, rows: rows.clone(), meter: None };
        match hit_editor(&layout, rows[1].bar.x + 2, rows[1].bar.y) {
            Some(EditorHit::Param(p)) => assert_eq!(p, 4),
            _ => panic!("expected param hit"),
        }
        assert!(hit_editor(&layout, 0, 10).is_none());
    }

    #[test]
    fn eq_layout_has_eight_bands_and_hits() {
        let d = desc(10, true);
        let l = editor_layout(Rect::new(0, 0, 80, 14), &d, 0);
        let eq = l.eq.as_ref().unwrap();
        assert_eq!(eq.cols.len(), 8);
        assert_eq!(l.rows.len(), 2);
        assert!(l.rows[0].label.y > eq.cols[0].y + eq.cols[0].height - 1);
        let b3 = eq.bars[3];
        assert!(matches!(hit_editor(&l, b3.x, b3.y + 1), Some(EditorHit::Band(3))));
        assert!(matches!(hit_editor(&l, l.rows[1].bar.x, l.rows[1].bar.y), Some(EditorHit::Param(9))));
    }

    #[test]
    fn eq_bar_value_mapping() {
        let bar = Rect::new(0, 10, 3, 9); // center row 14
        assert_eq!(eq_bar_value(bar, 14), 0.0);
        assert_eq!(eq_bar_value(bar, 10), 1.0);
        assert_eq!(eq_bar_value(bar, 18), -1.0);
        assert!((eq_bar_value(bar, 12) - 0.5).abs() < 1e-6);
        assert_eq!(eq_bar_value(bar, 0), 1.0);
    }

    #[test]
    fn adjust_continuous_and_stepped() {
        let p = ParamSpec::new("a", "A", 0.0, 100.0, 50.0);
        assert!((adjust_value(&p, 50.0, 0.01) - 51.0).abs() < 1e-3);
        let c = ParamSpec::new("c", "C", 0.0, 0.0, 0.0).choices(&["off", "on"]);
        assert_eq!(adjust_value(&c, 0.0, 0.01), 1.0);
        assert_eq!(adjust_value(&c, 0.0, -0.01), 0.0);
        let lg = ParamSpec::new("f", "F", 20.0, 20000.0, 1000.0).log();
        assert!(adjust_value(&lg, 1000.0, 0.1) > 1000.0);
    }

    #[test]
    fn meter_scale() {
        assert_eq!(peak_to_frac(0.0), 0.0);
        assert_eq!(peak_to_frac(1.0), 1.0);
        assert!((peak_to_frac(0.001) - 0.0).abs() < 1e-6);
        assert!((peak_to_frac(0.0316) - 0.5).abs() < 0.01);
    }

    #[test]
    fn truncation() {
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate("abc", 4), "abc");
    }
}
