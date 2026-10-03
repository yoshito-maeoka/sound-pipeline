//! Rendering.

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph, Widget, Wrap};
use ratatui::Frame;

use crate::engine::Meters;

use super::app::{picker_rows, App, Focus, PickRow, Popup};
use super::editors::{draw_editor, draw_level, peak_to_frac, truncate, ACCENT, DIM};
use super::layout::{self, fmt_time, SlotKind, ARROW_W, BOX_H};

fn accent() -> Style {
    Style::default().fg(ACCENT)
}
fn dim() -> Style {
    Style::default().fg(DIM)
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let size = f.area();
    app.size = size;
    let areas = layout::main_layout(size);
    let meters = app.engine.meters();
    let buf = f.buffer_mut();
    draw_top(buf, areas.top, app);
    draw_chain(buf, areas.chain, app, &meters);
    draw_editor_panel(buf, areas.editor, app, &meters);
    draw_status(buf, areas.status, app);
    draw_help(buf, areas.help, app);
    match &app.popup {
        Popup::None => {}
        Popup::Picker { sel, .. } => draw_picker(buf, size, app, *sel),
        Popup::Output { sel, devices } => draw_output(buf, size, app, *sel, devices),
        Popup::Export { text, copied } => draw_export(buf, size, text, *copied),
    }
}

fn panel(title: &str, focused: bool) -> Block<'static> {
    let style = if focused { accent() } else { dim() };
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(style)
        .title(Span::styled(format!(" {title} "), if focused { accent().add_modifier(Modifier::BOLD) } else { Style::default().fg(Color::Gray) }))
}

fn draw_top(buf: &mut Buffer, area: Rect, app: &App) {
    let block = panel("soundpipe", false);
    block.render(area, buf);
    let inn = layout::inner(area);
    let t = layout::top_layout(inn);
    let playing = app.engine.is_playing();
    let (icon, icon_style) = if playing { ("▶", Style::default().fg(Color::Green)) } else { ("▮▮", Style::default().fg(Color::Yellow)) };
    buf.set_string(t.title.x, t.title.y, icon, icon_style);
    buf.set_stringn(
        t.title.x + 3,
        t.title.y,
        &app.file_name,
        (t.title.width as usize).saturating_sub(4),
        Style::default().add_modifier(Modifier::BOLD),
    );
    let pos = app.engine.position_secs();
    let dur = app.engine.duration_secs();
    buf.set_stringn(t.elapsed.x, t.elapsed.y, format!("{:>6} ", fmt_time(pos)), t.elapsed.width as usize, dim());
    buf.set_stringn(t.total.x, t.total.y, format!(" {:<6}", fmt_time(dur)), t.total.width as usize, dim());
    let w = t.bar.width;
    if w > 0 {
        let frac = if dur > 0.0 { (pos / dur).clamp(0.0, 1.0) } else { 0.0 };
        let knob = (frac * (w - 1) as f64).round() as u16;
        for i in 0..w {
            let (ch, st) = if i < knob {
                ("━", accent())
            } else if i == knob {
                ("●", Style::default().fg(Color::White))
            } else {
                ("─", dim())
            };
            buf.set_string(t.bar.x + i, t.bar.y, ch, st);
        }
    }
    let (txt, st) = if app.engine.looping() { ("  ↻ loop ", accent()) } else { ("  ↻ off  ", dim()) };
    buf.set_stringn(t.looping.x, t.looping.y, txt, t.looping.width as usize, st);
}

fn draw_chain(buf: &mut Buffer, area: Rect, app: &mut App, meters: &Meters) {
    let labels = app.item_labels();
    let widths = app.widths();
    let inn = layout::inner(area);
    app.first = layout::ensure_visible(&widths, app.first, app.selected, inn);
    let slots = layout::layout_chain(&widths, app.first, inn);
    let (left, right) = layout::overflow(&slots, widths.len());
    let mut title = String::from("Chain");
    if left {
        title.push_str("  ◀");
    }
    if right {
        title.push_str("  ▶");
    }
    panel(&title, app.focus == Focus::Chain).render(area, buf);

    let n = app.engine.chain().len();
    let y0 = layout::row_y(inn);
    for slot in &slots {
        match slot.kind {
            SlotKind::Arrow(_) => {
                let r = slot.rect;
                buf.set_string(r.x, y0 + 1, " ───▶", Style::default().fg(Color::Gray));
                if r.width >= ARROW_W {
                    buf.set_string(r.x + 1, y0, "[+]", dim());
                }
            }
            SlotKind::Node(i) => {
                let (title, sub) = &labels[i];
                let bypass = i >= 1 && i <= n && app.engine.chain()[i - 1].bypass;
                let peak = if i == 0 {
                    meters.source_peak
                } else if i == n + 1 {
                    meters.output_peak
                } else {
                    meters.node_peaks.get(i - 1).copied().unwrap_or(0.0)
                };
                draw_node(buf, slot.rect, title, sub, i == app.selected, bypass, peak);
            }
        }
    }
    if n == 0 && inn.height > 0 {
        let msg = "press a or click an arrow to add a filter";
        let y = inn.y + inn.height - 1;
        buf.set_stringn(inn.x + 1, y, msg, inn.width.saturating_sub(2) as usize, dim());
    }
}

fn draw_node(buf: &mut Buffer, r: Rect, title: &str, sub: &str, selected: bool, bypass: bool, peak: f32) {
    if r.width < 4 || r.height < BOX_H {
        return;
    }
    let boxr = Rect { height: BOX_H, ..r };
    let (bstyle, btype) = if bypass {
        (dim(), BorderType::Rounded)
    } else if selected {
        (accent().add_modifier(Modifier::BOLD), BorderType::Thick)
    } else {
        (Style::default().fg(Color::Gray), BorderType::Rounded)
    };
    let block = Block::bordered().border_type(btype).border_style(bstyle);
    let inn = block.inner(boxr);
    block.render(boxr, buf);
    let w = inn.width as usize;
    let tstyle = if bypass {
        dim()
    } else if selected {
        Style::default().fg(Color::Black).bg(ACCENT).add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::BOLD)
    };
    let sstyle = if bypass { dim().add_modifier(Modifier::ITALIC) } else { Style::default().fg(Color::Gray) };
    let line = |s: &str, st: Style| Paragraph::new(Line::styled(truncate(s, w), st)).alignment(Alignment::Center);
    let r1 = Rect { height: 1, ..inn };
    if selected && !bypass {
        buf.set_style(r1, tstyle);
    }
    line(title, tstyle).render(r1, buf);
    line(sub, sstyle).render(Rect { y: inn.y + 1, height: 1, ..inn }, buf);
    if r.height > BOX_H && r.width > 2 {
        draw_level(buf, r.x + 1, r.y + BOX_H, r.width - 2, peak_to_frac(peak), bypass);
    }
}

fn draw_editor_panel(buf: &mut Buffer, area: Rect, app: &App, meters: &Meters) {
    let focus = app.focus == Focus::Editor;
    let chain = app.engine.chain();
    let content = layout::editor_content(area);
    match app.sel_filter() {
        Some(i) => {
            let node = &chain[i];
            let mut title = node.desc.name.clone();
            if node.bypass {
                title.push_str(" (bypass)");
            }
            panel(&title, focus).render(area, buf);
            if !node.desc.description.is_empty() && area.width > 12 {
                let used = title.chars().count() + 8;
                let d = truncate(&node.desc.description, (area.width as usize).saturating_sub(used + 2));
                buf.set_string(area.x + title.chars().count() as u16 + 4, area.y, format!("─ {d} "), dim());
            }
            let m = meters.node_meters.get(i).copied().flatten();
            draw_editor(buf, content, &node.desc, &node.params, app.param_sel, focus, m);
        }
        None => {
            let cfg = app.engine.output_config();
            let (title, lines): (&str, Vec<String>) = if app.selected == 0 {
                (
                    "Source",
                    vec![
                        format!("File      {}", app.path.display()),
                        format!("Length    {}", fmt_time(app.engine.duration_secs())),
                        format!("Format    {} Hz, {} ch", cfg.sample_rate, cfg.channels),
                        String::new(),
                        "Press a to add a filter after the source.".into(),
                    ],
                )
            } else {
                (
                    "Speaker",
                    vec![
                        format!("Device    {}", app.engine.output_device_name()),
                        format!("Format    {} Hz, {} ch", cfg.sample_rate, cfg.channels),
                        String::new(),
                        "Enter or click: choose output device or render to WAV.".into(),
                        "x: export the chain as a shell pipeline.".into(),
                    ],
                )
            };
            panel(title, false).render(area, buf);
            let lines: Vec<Line> = lines.into_iter().map(|l| Line::styled(l, Style::default().fg(Color::Gray))).collect();
            Paragraph::new(lines).render(content, buf);
        }
    }
}

fn draw_status(buf: &mut Buffer, area: Rect, app: &App) {
    let w = area.width as usize;
    let (text, style) = if let Some((m, err, _)) = &app.status {
        let (icon, c) = if *err { ("✖ ", Color::Red) } else { ("● ", Color::Green) };
        (format!("{icon}{m}"), Style::default().fg(c))
    } else if let Some(job) = &app.render {
        let p = job.progress.load(std::sync::atomic::Ordering::Relaxed) as f32 / 10.0;
        (format!("Rendering {} ... {p:.0}%", job.path.display()), Style::default().fg(Color::Yellow))
    } else if !app.registry.warnings.is_empty() {
        (format!("⚠ {}", app.registry.warnings.join(" | ")), Style::default().fg(Color::Yellow))
    } else {
        (String::new(), dim())
    };
    buf.set_stringn(area.x + 1, area.y, text, w.saturating_sub(2), style);
}

fn key_hint(spans: &mut Vec<Span<'static>>, key: &str, what: &str) {
    spans.push(Span::styled(key.to_string(), accent()));
    spans.push(Span::styled(format!(" {what}  "), dim()));
}

fn draw_help(buf: &mut Buffer, area: Rect, app: &App) {
    let hints: &[(&str, &str)] = match (&app.popup, app.focus) {
        (Popup::Picker { .. }, _) => &[("↑↓", "choose"), ("⏎", "add"), ("esc", "cancel")],
        (Popup::Output { .. }, _) => &[("↑↓", "choose"), ("⏎", "select"), ("esc", "cancel")],
        (Popup::Export { .. }, _) => &[("c", "copy"), ("esc", "close")],
        (_, Focus::Editor) => &[
            ("↑↓", "param"),
            ("←→", "adjust"),
            ("shift", "x10"),
            (", .", "fine"),
            ("r", "reset"),
            ("tab", "section"),
            ("esc", "back"),
            ("space", "play"),
            ("q", "quit"),
        ],
        _ => &[
            ("←→", "select"),
            ("a", "add"),
            ("d", "delete"),
            ("b", "bypass"),
            ("< >", "move"),
            ("⏎", "edit"),
            ("space", "play"),
            ("[ ]", "seek"),
            ("l", "loop"),
            ("o", "output"),
            ("x", "export"),
            ("q", "quit"),
        ],
    };
    let mut spans = vec![Span::raw(" ")];
    for (k, w) in hints {
        key_hint(&mut spans, k, w);
    }
    Paragraph::new(Line::from(spans)).render(area, buf);
}

fn popup_block(title: &str) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(accent())
        .title(Span::styled(format!(" {title} "), accent().add_modifier(Modifier::BOLD)))
}

fn sel_style(selected: bool) -> Style {
    if selected { Style::default().fg(Color::Black).bg(ACCENT).add_modifier(Modifier::BOLD) } else { Style::default() }
}

fn draw_picker(buf: &mut Buffer, size: Rect, app: &App, sel: usize) {
    let rows = picker_rows(&app.registry);
    let l = layout::list_popup(size, 60, rows.len(), 4);
    Clear.render(l.outer, buf);
    popup_block("Add filter").render(l.outer, buf);
    let h = l.list.height as usize;
    let scroll = layout::list_scroll(sel, h, rows.len());
    for (k, row) in rows.iter().skip(scroll).take(h).enumerate() {
        let y = l.list.y + k as u16;
        match row {
            PickRow::Header(c) => {
                buf.set_stringn(l.list.x, y, format!("── {c} "), l.list.width as usize, dim().add_modifier(Modifier::BOLD));
            }
            PickRow::Item(i) => {
                let d = &app.registry.all()[*i];
                let st = sel_style(scroll + k == sel);
                buf.set_style(Rect { y, height: 1, ..l.list }, st);
                buf.set_stringn(l.list.x + 2, y, &d.name, (l.list.width as usize).saturating_sub(2), st);
            }
        }
    }
    if let Some(PickRow::Item(i)) = rows.get(sel) {
        let d = &app.registry.all()[*i];
        let mut text = vec![Line::styled(format!("{} params · {}", d.params.len(), d.category), dim())];
        if !d.description.is_empty() {
            text.insert(0, Line::styled(d.description.clone(), Style::default().fg(Color::Gray)));
        }
        if l.footer.height > 0 {
            for x in l.footer.x..l.footer.x + l.footer.width {
                buf.set_string(x, l.footer.y, "─", dim());
            }
        }
        let f = Rect { y: l.footer.y + 1, height: l.footer.height.saturating_sub(1), ..l.footer };
        Paragraph::new(text).wrap(Wrap { trim: true }).render(f, buf);
    }
}

fn draw_output(buf: &mut Buffer, size: Rect, app: &App, sel: usize, devices: &[crate::audio::OutputDeviceInfo]) {
    let n = devices.len() + 1;
    let l = layout::list_popup(size, 60, n, 2);
    Clear.render(l.outer, buf);
    popup_block("Output").render(l.outer, buf);
    let cur = app.engine.output_device_name();
    let h = l.list.height as usize;
    let scroll = layout::list_scroll(sel, h, n);
    for k in 0..h.min(n.saturating_sub(scroll)) {
        let idx = scroll + k;
        let y = l.list.y + k as u16;
        let st = sel_style(idx == sel);
        buf.set_style(Rect { y, height: 1, ..l.list }, st);
        let text = if idx < devices.len() {
            let d = &devices[idx];
            let mark = if d.name == cur { "●" } else { " " };
            let def = if d.is_default { " (default)" } else { "" };
            format!("{mark} {}{def}", d.name)
        } else {
            "↳ Render to WAV file…".to_string()
        };
        buf.set_stringn(l.list.x + 1, y, text, (l.list.width as usize).saturating_sub(1), st);
    }
    if l.footer.height >= 2 {
        if devices.is_empty() {
            buf.set_string(l.footer.x, l.footer.y, "No output devices found.", Style::default().fg(Color::Yellow));
        }
        buf.set_stringn(l.footer.x, l.footer.y + 1, "Render writes <name>.sp.wav next to the source.", l.footer.width as usize, dim());
    }
}

fn draw_export(buf: &mut Buffer, size: Rect, text: &str, copied: bool) {
    let w = size.width.saturating_sub(6).clamp(20, 100);
    let lines = (text.chars().count() as u16 / w.saturating_sub(4).max(1)) + 1;
    let r = layout::popup_rect(size, w, (lines + 5).min(size.height));
    Clear.render(r, buf);
    popup_block("Export as shell pipeline").render(r, buf);
    let inn = layout::inner(r);
    let body = Rect { x: inn.x + 1, y: inn.y, width: inn.width.saturating_sub(2), height: inn.height.saturating_sub(2) };
    Paragraph::new(text.to_string()).wrap(Wrap { trim: false }).render(body, buf);
    let note = if copied { "Copied to clipboard." } else { "Clipboard unavailable (pbcopy not found); select the text above." };
    let st = if copied { Style::default().fg(Color::Green) } else { Style::default().fg(Color::Yellow) };
    if inn.height > 0 {
        buf.set_stringn(body.x, inn.y + inn.height - 1, note, body.width as usize, st);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::AudioData;
    use crate::engine::EngineHandle;
    use crate::plugin::{EditorKind, FilterDescriptor, ParamSpec, Passthrough, Registry};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::path::PathBuf;

    fn registry() -> Registry {
        let mut r = Registry::new();
        let eq_params: Vec<ParamSpec> = ["60", "150", "400", "1k", "2.5k", "6k", "12k", "16k"]
            .iter()
            .map(|f| ParamSpec::new(f, f, -12.0, 12.0, 0.0).unit("dB"))
            .chain([ParamSpec::new("out", "Output", -12.0, 12.0, 0.0).unit("dB")])
            .collect();
        r.register(
            FilterDescriptor::native("eq", "Equalizer", "Tone", eq_params, |_| Box::new(Passthrough))
                .editor(EditorKind::Equalizer)
                .description("8-band graphic equalizer"),
        );
        r.register(
            FilterDescriptor::native(
                "lowpass",
                "Low-Pass",
                "Tone",
                vec![ParamSpec::new("cutoff", "Cutoff", 20.0, 20000.0, 1000.0).unit("Hz").log(), ParamSpec::new("q", "Q", 0.1, 10.0, 0.7)],
                |_| Box::new(Passthrough),
            )
            .description("Removes highs"),
        );
        r.register(
            FilterDescriptor::native("comp", "Compressor", "Dynamics", vec![ParamSpec::new("t", "Threshold", -60.0, 0.0, -18.0).unit("dB")], |_| {
                Box::new(Passthrough)
            })
            .meter("Gain reduction"),
        );
        r
    }

    fn make_app() -> Option<App> {
        let data = AudioData { samples: vec![0.0; 44100 * 2 * 3], sample_rate: 44100, channels: 2 };
        let engine = EngineHandle::start(data, None).ok()?;
        Some(App::new(registry(), engine, PathBuf::from("/tmp/song.mp3")))
    }

    fn render(app: &mut App, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw(f, app)).unwrap();
        let buf = term.backend().buffer();
        let mut s = String::new();
        for y in 0..h {
            for x in 0..w {
                s.push_str(buf[(x, y)].symbol());
            }
            s.push('\n');
        }
        s
    }

    #[test]
    fn empty_chain_snapshot() {
        let Some(mut app) = make_app() else { return };
        let s = render(&mut app, 100, 30);
        println!("{s}");
        assert!(s.contains("song.mp3"));
        assert!(s.contains("Speaker"));
        assert!(s.contains("───▶"));
        assert!(s.contains("[+]"));
    }

    #[test]
    fn chain_with_filters_and_popups() {
        let Some(mut app) = make_app() else { return };
        let reg = app.registry.clone();
        app.insert_filter(0, reg.get("eq").unwrap());
        app.insert_filter(1, reg.get("comp").unwrap());
        app.engine.set_bypass(1, true);
        app.selected = 1;
        app.param_sel = 3;
        app.engine.set_param(0, 3, 6.0);
        let s = render(&mut app, 100, 32);
        println!("{s}");
        assert!(s.contains("Equalizer"));
        assert!(s.contains("(bypass)"));
        assert!(s.contains("+6.0"));
        assert!(s.contains("2.5k"));
        app.handle_key(ratatui::crossterm::event::KeyEvent::from(ratatui::crossterm::event::KeyCode::Char('a')));
        let s = render(&mut app, 100, 32);
        println!("{s}");
        assert!(s.contains("Add filter"));
        assert!(s.contains("Dynamics"));
        assert!(s.contains("8-band graphic equalizer"));
    }

    #[test]
    fn narrow_terminal_does_not_panic() {
        let Some(mut app) = make_app() else { return };
        let reg = app.registry.clone();
        for i in 0..5 {
            app.insert_filter(i, reg.get("lowpass").unwrap());
        }
        for (w, h) in [(40, 12), (20, 6), (80, 24), (3, 3)] {
            let _ = render(&mut app, w, h);
        }
        app.selected = 6;
        let s = render(&mut app, 60, 24);
        assert!(s.contains("Speaker"));
    }

    #[test]
    fn mouse_interactions() {
        use ratatui::crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        let Some(mut app) = make_app() else { return };
        let click = |app: &mut App, x: u16, y: u16| {
            app.handle_mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: x, row: y, modifiers: KeyModifiers::NONE });
            app.handle_mouse(MouseEvent { kind: MouseEventKind::Up(MouseButton::Left), column: x, row: y, modifiers: KeyModifiers::NONE });
        };
        let reg = app.registry.clone();
        app.insert_filter(0, reg.get("lowpass").unwrap());
        let _ = render(&mut app, 100, 30);
        // click the arrow after the source -> picker inserting at 0
        let areas = layout::main_layout(app.size);
        let inn = layout::inner(areas.chain);
        let slots = layout::layout_chain(&app.widths(), app.first, inn);
        let arrow = slots.iter().find(|s| s.kind == SlotKind::Arrow(0)).unwrap();
        click(&mut app, arrow.rect.x + 1, arrow.rect.y + 2);
        assert!(matches!(app.popup, Popup::Picker { insert: 0, .. }));
        // click first picker item (row 1) -> inserted at 0
        let l = layout::list_popup(app.size, 60, picker_rows(&app.registry).len(), 4);
        click(&mut app, l.list.x + 3, l.list.y + 1);
        assert!(matches!(app.popup, Popup::None));
        assert_eq!(app.engine.chain().len(), 2);
        assert_eq!(app.engine.chain()[0].desc.id, "eq");
        assert_eq!(app.selected, 1);
        // click the selected node -> focus editor
        let _ = render(&mut app, 100, 30);
        let slots = layout::layout_chain(&app.widths(), app.first, inn);
        let node = slots.iter().find(|s| s.kind == SlotKind::Node(2)).unwrap();
        click(&mut app, node.rect.x + 2, node.rect.y + 1);
        assert_eq!(app.selected, 2);
        click(&mut app, node.rect.x + 2, node.rect.y + 1);
        assert_eq!(app.focus, Focus::Editor);
        // click on slider bar's right end sets the cutoff to max
        let desc = app.engine.chain()[1].desc.clone();
        let l = crate::tui::editors::editor_layout(layout::editor_content(areas.editor), &desc, 0);
        let bar = l.rows[0].bar;
        click(&mut app, bar.x + bar.width - 1, bar.y);
        assert_eq!(app.engine.chain()[1].params[0], 20000.0);
        // click progress bar middle seeks
        let t = layout::top_layout(layout::inner(areas.top));
        click(&mut app, t.bar.x + t.bar.width / 2, t.bar.y);
        let p = app.engine.position_secs();
        assert!(p > 1.0 && p < 2.0, "pos {p}");
    }
}
