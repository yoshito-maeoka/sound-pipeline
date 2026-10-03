//! Application state and input handling.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use crate::audio::{list_output_devices, OutputDeviceInfo};
use crate::cli::export_pipeline;
use crate::engine::{EngineHandle, RenderJob};
use crate::plugin::{FilterDescriptor, Registry, EQ_BANDS};

use super::editors::{adjust_value, editor_layout, eq_bar_value, hit_editor, is_eq, EditorHit, EQ_FINE_DB};
use super::layout::{self, bar_frac, contains, SlotKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Chain,
    Editor,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PickRow {
    Header(String),
    /// Index into `Registry::all()`.
    Item(usize),
}

/// Picker rows grouped by category (order of first appearance).
pub fn picker_rows(registry: &Registry) -> Vec<PickRow> {
    let mut cats: Vec<&str> = Vec::new();
    for d in registry.all() {
        if !cats.contains(&d.category.as_str()) {
            cats.push(&d.category);
        }
    }
    let mut rows = Vec::new();
    for c in cats {
        rows.push(PickRow::Header(c.to_string()));
        for (i, d) in registry.all().iter().enumerate() {
            if d.category == c {
                rows.push(PickRow::Item(i));
            }
        }
    }
    rows
}

/// Move to the next/previous selectable (non-header) row.
pub fn pick_move(rows: &[PickRow], sel: usize, dir: i32) -> usize {
    let mut i = sel as i32;
    loop {
        i += dir;
        if i < 0 || i >= rows.len() as i32 {
            return sel;
        }
        if matches!(rows[i as usize], PickRow::Item(_)) {
            return i as usize;
        }
    }
}

#[derive(Clone, Debug, Default)]
pub enum Popup {
    #[default]
    None,
    Picker { insert: usize, sel: usize },
    /// `sel` indexes devices, then one extra "Render to WAV" row.
    Output { sel: usize, devices: Vec<OutputDeviceInfo> },
    Export { text: String, copied: bool },
}

#[derive(Clone, Copy, Debug)]
enum Drag {
    Seek,
    Param(usize),
    Band(usize),
}

pub struct App {
    pub registry: Registry,
    pub engine: EngineHandle,
    pub path: PathBuf,
    pub file_name: String,
    /// Item index: 0 = source, 1..=n filters, n+1 = speaker.
    pub selected: usize,
    pub first: usize,
    pub focus: Focus,
    pub param_sel: usize,
    eq_mem: usize,
    pub popup: Popup,
    pub status: Option<(String, bool, Instant)>,
    pub render: Option<RenderJob>,
    pub quit: bool,
    pub size: Rect,
    drag: Option<Drag>,
}

impl App {
    pub fn new(registry: Registry, engine: EngineHandle, path: PathBuf) -> Self {
        let file_name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
        Self {
            registry,
            engine,
            path,
            file_name,
            selected: 0,
            first: 0,
            focus: Focus::Chain,
            param_sel: 0,
            eq_mem: 0,
            popup: Popup::None,
            status: None,
            render: None,
            quit: false,
            size: Rect::new(0, 0, 120, 35),
            drag: None,
        }
    }

    pub fn set_status(&mut self, msg: impl Into<String>, is_err: bool) {
        self.status = Some((msg.into(), is_err, Instant::now()));
    }

    /// Currently selected filter index, if a filter is selected.
    pub fn sel_filter(&self) -> Option<usize> {
        let n = self.engine.chain().len();
        (self.selected >= 1 && self.selected <= n).then(|| self.selected - 1)
    }

    fn select(&mut self, item: usize) {
        let total = self.engine.chain().len() + 2;
        let item = item.min(total - 1);
        if item != self.selected {
            self.selected = item;
            self.param_sel = 0;
            self.eq_mem = 0;
        }
        if self.sel_filter().is_none() {
            self.focus = Focus::Chain;
        }
    }

    /// Periodic work: engine errors, render progress, status expiry.
    pub fn tick(&mut self) {
        if let Some(e) = self.engine.take_error() {
            self.set_status(e, true);
        }
        if let Some(job) = &self.render
            && job.done.load(Ordering::SeqCst)
        {
            let err = job.error.lock().ok().and_then(|mut e| e.take());
            let path = job.path.display().to_string();
            self.render = None;
            match err {
                Some(e) => self.set_status(format!("Render failed: {e}"), true),
                None => self.set_status(format!("Rendered {path}"), false),
            }
        }
        if let Some((_, _, t)) = &self.status
            && t.elapsed() > Duration::from_secs(6)
        {
            self.status = None;
        }
        let total = self.engine.chain().len() + 2;
        if self.selected >= total {
            self.select(total - 1);
        }
    }

    // ---------------------------------------------------------------- keys

    pub fn handle_key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        if !matches!(self.popup, Popup::None) {
            self.popup_key(key);
            return;
        }
        let plain = !key.modifiers.contains(KeyModifiers::CONTROL);
        if plain {
            match key.code {
                KeyCode::Char('q') => return self.quit = true,
                KeyCode::Char(' ') => return self.engine.toggle_play(),
                KeyCode::Char('[') => return self.seek_rel(-5.0),
                KeyCode::Char(']') => return self.seek_rel(5.0),
                KeyCode::Char('l') => {
                    let l = !self.engine.looping();
                    self.engine.set_looping(l);
                    return;
                }
                KeyCode::Char('o') => return self.open_output(),
                KeyCode::Char('x') => return self.open_export(),
                KeyCode::Char('a') => return self.open_picker(self.selected),
                KeyCode::Char('b') => return self.toggle_bypass(),
                _ => {}
            }
        }
        match self.focus {
            Focus::Chain => self.chain_key(key),
            Focus::Editor => self.editor_key(key),
        }
    }

    fn seek_rel(&mut self, d: f64) {
        let p = self.engine.position_secs() + d;
        self.engine.seek(p);
    }

    fn toggle_bypass(&mut self) {
        if let Some(i) = self.sel_filter() {
            let b = self.engine.chain()[i].bypass;
            self.engine.set_bypass(i, !b);
        }
    }

    fn open_picker(&mut self, insert: usize) {
        let n = self.engine.chain().len();
        let rows = picker_rows(&self.registry);
        let sel = rows.iter().position(|r| matches!(r, PickRow::Item(_)));
        match sel {
            Some(sel) => self.popup = Popup::Picker { insert: insert.min(n), sel },
            None => self.set_status("No filters registered", true),
        }
    }

    fn open_output(&mut self) {
        let devices = list_output_devices();
        let cur = self.engine.output_device_name();
        let sel = devices.iter().position(|d| d.name == cur).unwrap_or(0);
        self.popup = Popup::Output { sel, devices };
    }

    fn open_export(&mut self) {
        let cfg = self.engine.output_config();
        let text = export_pipeline(&self.path, self.engine.chain(), cfg.sample_rate, cfg.channels);
        let copied = copy_to_clipboard(&text);
        self.popup = Popup::Export { text, copied };
    }

    pub fn insert_filter(&mut self, at: usize, desc: Arc<FilterDescriptor>) {
        let name = desc.name.clone();
        match self.engine.insert(at, desc) {
            Ok(()) => {
                self.select(at + 1);
                self.param_sel = 0;
            }
            Err(e) => self.set_status(format!("Cannot add {name}: {e:#}"), true),
        }
    }

    fn chain_key(&mut self, key: KeyEvent) {
        let total = self.engine.chain().len() + 2;
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Left if shift => self.move_selected(-1),
            KeyCode::Right if shift => self.move_selected(1),
            KeyCode::Char('<') => self.move_selected(-1),
            KeyCode::Char('>') => self.move_selected(1),
            KeyCode::Left => self.select(self.selected.saturating_sub(1)),
            KeyCode::Right => self.select((self.selected + 1).min(total - 1)),
            KeyCode::Enter | KeyCode::Tab => self.activate_selected(),
            KeyCode::Char('d') | KeyCode::Delete | KeyCode::Backspace => self.delete_selected(),
            _ => {}
        }
    }

    /// Enter on the selected item: edit filter, or open the output picker.
    fn activate_selected(&mut self) {
        if self.sel_filter().is_some() {
            self.focus = Focus::Editor;
        } else if self.selected > 0 {
            self.open_output();
        }
    }

    fn move_selected(&mut self, d: i32) {
        let Some(from) = self.sel_filter() else { return };
        let n = self.engine.chain().len();
        let to = from as i32 + d;
        if to < 0 || to >= n as i32 {
            return;
        }
        self.engine.move_node(from, to as usize);
        self.selected = to as usize + 1;
    }

    fn delete_selected(&mut self) {
        if let Some(i) = self.sel_filter() {
            self.engine.remove(i);
            self.focus = Focus::Chain;
            self.param_sel = 0;
            self.selected = self.selected.min(self.engine.chain().len() + 1);
        }
    }

    fn adjust(&mut self, delta_norm: f32) {
        let Some(i) = self.sel_filter() else { return };
        let node = &self.engine.chain()[i];
        let Some(spec) = node.desc.params.get(self.param_sel) else { return };
        let v = adjust_value(spec, node.params[self.param_sel], delta_norm);
        self.engine.set_param(i, self.param_sel, v);
    }

    fn adjust_db(&mut self, db: f32) {
        let Some(i) = self.sel_filter() else { return };
        let node = &self.engine.chain()[i];
        let Some(spec) = node.desc.params.get(self.param_sel) else { return };
        let v = node.params[self.param_sel] + db;
        self.engine.set_param(i, self.param_sel, v.clamp(spec.min, spec.max));
    }

    fn editor_key(&mut self, key: KeyEvent) {
        let Some(i) = self.sel_filter() else {
            self.focus = Focus::Chain;
            return;
        };
        let desc = self.engine.chain()[i].desc.clone();
        let np = desc.params.len();
        if np == 0 {
            if matches!(key.code, KeyCode::Esc | KeyCode::Tab | KeyCode::Enter) {
                self.focus = Focus::Chain;
            }
            return;
        }
        self.param_sel = self.param_sel.min(np - 1);
        let eq = is_eq(&desc);
        let in_bands = eq && self.param_sel < EQ_BANDS.len();
        let mult: f32 = if key.modifiers.contains(KeyModifiers::SHIFT) { 10.0 } else { 1.0 };
        match key.code {
            KeyCode::Esc => self.focus = Focus::Chain,
            KeyCode::Tab | KeyCode::BackTab => {
                if eq && np > EQ_BANDS.len() {
                    if in_bands {
                        self.eq_mem = self.param_sel;
                        self.param_sel = EQ_BANDS.len();
                    } else {
                        self.param_sel = self.eq_mem;
                    }
                } else {
                    self.focus = Focus::Chain;
                }
            }
            KeyCode::Char('r') => {
                let d = desc.params[self.param_sel].default;
                self.engine.set_param(i, self.param_sel, d);
            }
            KeyCode::Up if in_bands => self.adjust_db(EQ_FINE_DB * mult.min(2.0)),
            KeyCode::Down if in_bands => self.adjust_db(-EQ_FINE_DB * mult.min(2.0)),
            KeyCode::Up => {
                if eq && self.param_sel == EQ_BANDS.len() {
                    self.param_sel = self.eq_mem;
                } else {
                    self.param_sel = self.param_sel.saturating_sub(1);
                }
            }
            KeyCode::Down => self.param_sel = (self.param_sel + 1).min(np - 1),
            KeyCode::Left if in_bands => self.param_sel = self.param_sel.saturating_sub(1),
            KeyCode::Right if in_bands => self.param_sel = (self.param_sel + 1).min(EQ_BANDS.len() - 1),
            KeyCode::Left => self.adjust(-0.01 * mult),
            KeyCode::Right => self.adjust(0.01 * mult),
            KeyCode::Char(',') if in_bands => self.adjust_db(-0.1),
            KeyCode::Char('.') if in_bands => self.adjust_db(0.1),
            KeyCode::Char(',') => self.adjust(-0.001),
            KeyCode::Char('.') => self.adjust(0.001),
            _ => {}
        }
    }

    // --------------------------------------------------------------- popups

    fn popup_key(&mut self, key: KeyEvent) {
        match self.popup.clone() {
            Popup::None => {}
            Popup::Picker { insert, sel } => {
                let rows = picker_rows(&self.registry);
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => self.popup = Popup::None,
                    KeyCode::Up | KeyCode::Char('k') => self.popup = Popup::Picker { insert, sel: pick_move(&rows, sel, -1) },
                    KeyCode::Down | KeyCode::Char('j') => self.popup = Popup::Picker { insert, sel: pick_move(&rows, sel, 1) },
                    KeyCode::Enter => self.pick(insert, sel),
                    _ => {}
                }
            }
            Popup::Output { sel, devices } => {
                let n = devices.len() + 1;
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('o') => self.popup = Popup::None,
                    KeyCode::Up | KeyCode::Char('k') => self.popup = Popup::Output { sel: sel.saturating_sub(1), devices },
                    KeyCode::Down | KeyCode::Char('j') => self.popup = Popup::Output { sel: (sel + 1).min(n - 1), devices },
                    KeyCode::Enter => self.choose_output(sel, &devices),
                    _ => {}
                }
            }
            Popup::Export { text, .. } => match key.code {
                KeyCode::Char('c') => {
                    let copied = copy_to_clipboard(&text);
                    self.popup = Popup::Export { text, copied };
                }
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') | KeyCode::Char('x') => self.popup = Popup::None,
                _ => {}
            },
        }
    }

    fn pick(&mut self, insert: usize, row: usize) {
        let rows = picker_rows(&self.registry);
        self.popup = Popup::None;
        if let Some(PickRow::Item(i)) = rows.get(row)
            && let Some(desc) = self.registry.all().get(*i).cloned()
        {
            self.insert_filter(insert, desc);
        }
    }

    fn choose_output(&mut self, sel: usize, devices: &[OutputDeviceInfo]) {
        self.popup = Popup::None;
        if sel < devices.len() {
            let d = &devices[sel];
            let name = if d.is_default { None } else { Some(d.name.clone()) };
            match self.engine.set_output_device(name) {
                Ok(()) => self.set_status(format!("Output: {}", d.name), false),
                Err(e) => self.set_status(format!("Cannot open {}: {e:#}", d.name), true),
            }
        } else {
            let stem = self.path.file_stem().map_or_else(|| "output".into(), |s| s.to_string_lossy().into_owned());
            let out = self.path.with_file_name(format!("{stem}.sp.wav"));
            self.set_status(format!("Rendering {} ...", out.display()), false);
            self.render = Some(self.engine.render_to_wav(out));
        }
    }

    // ---------------------------------------------------------------- mouse

    pub fn handle_mouse(&mut self, m: MouseEvent) {
        let (x, y) = (m.column, m.row);
        if !matches!(self.popup, Popup::None) {
            return self.popup_mouse(m);
        }
        let areas = layout::main_layout(self.size);
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => self.mouse_down(x, y, areas),
            MouseEventKind::Drag(MouseButton::Left) => self.mouse_drag(x, y, areas),
            MouseEventKind::Up(_) => self.drag = None,
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let up = matches!(m.kind, MouseEventKind::ScrollUp);
                if contains(areas.editor, x, y) {
                    self.wheel_editor(up);
                } else if contains(areas.chain, x, y) {
                    let total = self.engine.chain().len() + 2;
                    let s = if up { self.selected.saturating_sub(1) } else { (self.selected + 1).min(total - 1) };
                    self.select(s);
                }
            }
            _ => {}
        }
    }

    fn wheel_editor(&mut self, up: bool) {
        let Some(i) = self.sel_filter() else { return };
        let desc = self.engine.chain()[i].desc.clone();
        if desc.params.is_empty() {
            return;
        }
        self.focus = Focus::Editor;
        self.param_sel = self.param_sel.min(desc.params.len() - 1);
        let s = if up { 1.0 } else { -1.0 };
        if is_eq(&desc) && self.param_sel < EQ_BANDS.len() {
            self.adjust_db(s * EQ_FINE_DB);
        } else {
            self.adjust(s * 0.01);
        }
    }

    fn mouse_down(&mut self, x: u16, y: u16, areas: layout::Areas) {
        let top = layout::top_layout(layout::inner(areas.top));
        if contains(layout::inner(areas.top), x, y) {
            if contains(top.bar, x, y) {
                self.seek_to_x(top.bar, x);
                self.drag = Some(Drag::Seek);
            } else if contains(top.title, x, y) {
                self.engine.toggle_play();
            } else if contains(top.looping, x, y) {
                let l = !self.engine.looping();
                self.engine.set_looping(l);
            }
            return;
        }
        if contains(areas.chain, x, y) {
            let widths = self.widths();
            let slots = layout::layout_chain(&widths, self.first, layout::inner(areas.chain));
            match layout::hit_slot(&slots, x, y) {
                Some(SlotKind::Node(i)) => {
                    if i == self.selected {
                        self.activate_selected();
                    } else {
                        self.select(i);
                    }
                }
                Some(SlotKind::Arrow(k)) => self.open_picker(k),
                None => {}
            }
            return;
        }
        if contains(areas.editor, x, y) {
            let Some(i) = self.sel_filter() else {
                if self.selected > 0 {
                    self.open_output();
                }
                return;
            };
            let desc = self.engine.chain()[i].desc.clone();
            let l = editor_layout(layout::editor_content(areas.editor), &desc, self.param_sel);
            match hit_editor(&l, x, y) {
                Some(EditorHit::Band(b)) => {
                    self.focus = Focus::Editor;
                    self.param_sel = b;
                    self.drag = Some(Drag::Band(b));
                    self.drag_to(x, y, areas);
                }
                Some(EditorHit::Param(p)) => {
                    self.focus = Focus::Editor;
                    self.param_sel = p;
                    self.drag = Some(Drag::Param(p));
                    self.drag_to(x, y, areas);
                }
                None => self.focus = Focus::Editor,
            }
        }
    }

    fn mouse_drag(&mut self, x: u16, y: u16, areas: layout::Areas) {
        self.drag_to(x, y, areas);
    }

    fn seek_to_x(&mut self, bar: Rect, x: u16) {
        let f = bar_frac(bar, x) as f64;
        self.engine.seek(f * self.engine.duration_secs());
    }

    fn drag_to(&mut self, x: u16, y: u16, areas: layout::Areas) {
        let Some(drag) = self.drag else { return };
        if let Drag::Seek = drag {
            let top = layout::top_layout(layout::inner(areas.top));
            self.seek_to_x(top.bar, x);
            return;
        }
        let Some(i) = self.sel_filter() else { return };
        let desc = self.engine.chain()[i].desc.clone();
        let l = editor_layout(layout::editor_content(areas.editor), &desc, self.param_sel);
        match drag {
            Drag::Param(p) => {
                if let Some(row) = l.rows.iter().find(|r| r.param == p) {
                    let v = desc.params[p].from_norm(bar_frac(row.bar, x));
                    self.engine.set_param(i, p, v);
                }
            }
            Drag::Band(b) => {
                if let Some(bar) = l.eq.as_ref().and_then(|e| e.bars.get(b)) {
                    let spec = &desc.params[b];
                    let n = eq_bar_value(*bar, y);
                    let mid = (spec.min + spec.max) / 2.0;
                    let v = mid + n * (spec.max - spec.min) / 2.0;
                    // snap to 0.5 dB so the zero line is easy to hit
                    let v = (v * 2.0).round() / 2.0;
                    self.engine.set_param(i, b, v);
                }
            }
            Drag::Seek => {}
        }
    }

    fn popup_mouse(&mut self, m: MouseEvent) {
        let (x, y) = (m.column, m.row);
        let size = self.size;
        match self.popup.clone() {
            Popup::None => {}
            Popup::Picker { insert, sel } => {
                let rows = picker_rows(&self.registry);
                let l = layout::list_popup(size, 60, rows.len(), 4);
                match m.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        if contains(l.list, x, y) {
                            let scroll = layout::list_scroll(sel, l.list.height as usize, rows.len());
                            let row = scroll + (y - l.list.y) as usize;
                            if matches!(rows.get(row), Some(PickRow::Item(_))) {
                                self.pick(insert, row);
                            }
                        } else if !contains(l.outer, x, y) {
                            self.popup = Popup::None;
                        }
                    }
                    MouseEventKind::ScrollUp => self.popup = Popup::Picker { insert, sel: pick_move(&rows, sel, -1) },
                    MouseEventKind::ScrollDown => self.popup = Popup::Picker { insert, sel: pick_move(&rows, sel, 1) },
                    _ => {}
                }
            }
            Popup::Output { sel, devices } => {
                let n = devices.len() + 1;
                let l = layout::list_popup(size, 60, n, 2);
                match m.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        if contains(l.list, x, y) {
                            let scroll = layout::list_scroll(sel, l.list.height as usize, n);
                            let row = scroll + (y - l.list.y) as usize;
                            if row < n {
                                self.choose_output(row, &devices);
                            }
                        } else if !contains(l.outer, x, y) {
                            self.popup = Popup::None;
                        }
                    }
                    MouseEventKind::ScrollUp => self.popup = Popup::Output { sel: sel.saturating_sub(1), devices },
                    MouseEventKind::ScrollDown => self.popup = Popup::Output { sel: (sel + 1).min(n - 1), devices },
                    _ => {}
                }
            }
            Popup::Export { .. } => {
                if let MouseEventKind::Down(MouseButton::Left) = m.kind {
                    self.popup = Popup::None;
                }
            }
        }
    }

    /// Box widths for every chain item (source, filters, speaker).
    pub fn widths(&self) -> Vec<u16> {
        self.item_labels()
            .iter()
            .enumerate()
            .map(|(i, (t, s))| {
                let n = self.engine.chain().len();
                let len = t.chars().count().max(s.chars().count());
                if i == 0 || i == n + 1 {
                    layout::node_width(len, 16, 28)
                } else {
                    layout::node_width(len, 12, 20)
                }
            })
            .collect()
    }

    /// (title, subtitle) for each chain item.
    pub fn item_labels(&self) -> Vec<(String, String)> {
        let chain = self.engine.chain();
        let mut v = vec![(self.file_name.clone(), "source".to_string())];
        for n in chain {
            let sub = if n.bypass { "(bypass)".to_string() } else { n.desc.category.clone() };
            v.push((n.desc.name.clone(), sub));
        }
        v.push(("Speaker".to_string(), self.engine.output_device_name()));
        v
    }
}

/// Copy text with pbcopy if available. Returns whether it worked.
pub fn copy_to_clipboard(text: &str) -> bool {
    let Ok(mut child) = Command::new("pbcopy").stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() else {
        return false;
    };
    if let Some(mut stdin) = child.stdin.take()
        && stdin.write_all(text.as_bytes()).is_err()
    {
        return false;
    }
    child.wait().map(|s| s.success()).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::Passthrough;

    fn reg() -> Registry {
        let mut r = Registry::new();
        for (id, cat) in [("a", "Tone"), ("b", "Dynamics"), ("c", "Tone")] {
            r.register(FilterDescriptor::native(id, id, cat, vec![], |_| Box::new(Passthrough)));
        }
        r
    }

    #[test]
    fn picker_groups_by_category() {
        let rows = picker_rows(&reg());
        assert_eq!(
            rows,
            vec![
                PickRow::Header("Tone".into()),
                PickRow::Item(0),
                PickRow::Item(2),
                PickRow::Header("Dynamics".into()),
                PickRow::Item(1)
            ]
        );
    }

    #[test]
    fn pick_move_skips_headers() {
        let rows = picker_rows(&reg());
        assert_eq!(pick_move(&rows, 1, 1), 2);
        assert_eq!(pick_move(&rows, 2, 1), 4);
        assert_eq!(pick_move(&rows, 4, 1), 4);
        assert_eq!(pick_move(&rows, 4, -1), 2);
        assert_eq!(pick_move(&rows, 1, -1), 1);
    }
}
