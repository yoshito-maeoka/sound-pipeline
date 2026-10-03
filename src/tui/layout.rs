//! Pure layout and hit-testing helpers (no rendering, no engine access).

use ratatui::layout::{Constraint, Layout, Rect};

/// Height of a node box (border, two text lines, border).
pub const BOX_H: u16 = 4;
/// Node box plus the level-meter row underneath.
pub const ROW_H: u16 = BOX_H + 1;
/// Width of the gap (arrow) between two boxes.
pub const ARROW_W: u16 = 5;
pub const CHAIN_H: u16 = ROW_H + 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Areas {
    pub top: Rect,
    pub chain: Rect,
    pub editor: Rect,
    pub status: Rect,
    pub help: Rect,
}

pub fn main_layout(size: Rect) -> Areas {
    let [top, chain, editor, status, help] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(CHAIN_H),
        Constraint::Min(3),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(size);
    Areas { top, chain, editor, status, help }
}

/// Inside of a bordered panel.
pub fn inner(area: Rect) -> Rect {
    Rect {
        x: area.x.saturating_add(1),
        y: area.y.saturating_add(1),
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    }
}

/// Editor content rect (inside the border, with a 1-column margin).
pub fn editor_content(area: Rect) -> Rect {
    let r = inner(area);
    Rect { x: r.x + 1.min(r.width), width: r.width.saturating_sub(2), ..r }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotKind {
    /// Item index: 0 = source, 1..=n = filters, n+1 = speaker.
    Node(usize),
    /// Insert position for a new filter (0 = right after the source).
    Arrow(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slot {
    pub kind: SlotKind,
    pub rect: Rect,
}

/// Top row of the node boxes inside the chain area.
pub fn row_y(area: Rect) -> u16 {
    area.y + area.height.saturating_sub(ROW_H) / 2
}

/// Box width for a label of `len` characters.
pub fn node_width(len: usize, min: u16, max: u16) -> u16 {
    ((len as u16).saturating_add(4)).clamp(min, max)
}

/// Place items left to right starting at item `first`; items that do not fit
/// are omitted.
pub fn layout_chain(widths: &[u16], first: usize, area: Rect) -> Vec<Slot> {
    let mut out = Vec::new();
    let right = area.x + area.width;
    let y0 = row_y(area);
    let h = ROW_H.min(area.height);
    let mut x = area.x;
    for (i, &w) in widths.iter().enumerate().skip(first) {
        if i > first {
            if x + ARROW_W + w > right {
                break;
            }
            out.push(Slot {
                kind: SlotKind::Arrow(i - 1),
                rect: Rect { x, y: area.y, width: ARROW_W, height: area.height },
            });
            x += ARROW_W;
        }
        let w = w.min(right.saturating_sub(x));
        out.push(Slot { kind: SlotKind::Node(i), rect: Rect { x, y: y0, width: w, height: h } });
        x += w;
    }
    out
}

/// Smallest `first` >= the given one (clamped) such that item `sel` is fully visible.
pub fn ensure_visible(widths: &[u16], first: usize, sel: usize, area: Rect) -> usize {
    if widths.is_empty() {
        return 0;
    }
    let sel = sel.min(widths.len() - 1);
    let mut first = first.min(sel);
    while first < sel {
        let ok = layout_chain(widths, first, area)
            .iter()
            .any(|s| s.kind == SlotKind::Node(sel) && s.rect.width == widths[sel]);
        if ok {
            break;
        }
        first += 1;
    }
    first
}

pub fn contains(r: Rect, x: u16, y: u16) -> bool {
    x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height
}

pub fn hit_slot(slots: &[Slot], x: u16, y: u16) -> Option<SlotKind> {
    slots.iter().find(|s| contains(s.rect, x, y)).map(|s| s.kind)
}

/// Whether items exist left / right of the visible range.
pub fn overflow(slots: &[Slot], total: usize) -> (bool, bool) {
    let nodes: Vec<usize> = slots
        .iter()
        .filter_map(|s| if let SlotKind::Node(i) = s.kind { Some(i) } else { None })
        .collect();
    let (Some(&lo), Some(&hi)) = (nodes.first(), nodes.last()) else {
        return (false, false);
    };
    (lo > 0, hi + 1 < total)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TopAreas {
    pub title: Rect,
    pub elapsed: Rect,
    pub bar: Rect,
    pub total: Rect,
    pub looping: Rect,
}

pub fn top_layout(r: Rect) -> TopAreas {
    let title_w = (r.width / 3).clamp(10, 40).min(r.width);
    let time_w = 7u16.min(r.width.saturating_sub(title_w));
    let loop_w = 9u16.min(r.width.saturating_sub(title_w + 2 * time_w));
    let bar_w = r.width.saturating_sub(title_w + 2 * time_w + loop_w);
    let mut x = r.x;
    let mut take = |w: u16| {
        let rect = Rect { x, y: r.y, width: w, height: r.height };
        x += w;
        rect
    };
    TopAreas {
        title: take(title_w),
        elapsed: take(time_w),
        bar: take(bar_w),
        total: take(time_w),
        looping: take(loop_w),
    }
}

/// Horizontal position within a bar -> 0..=1 (both ends reachable).
pub fn bar_frac(bar: Rect, x: u16) -> f32 {
    if bar.width <= 1 {
        return 0.0;
    }
    let dx = x.saturating_sub(bar.x).min(bar.width - 1);
    dx as f32 / (bar.width - 1) as f32
}

pub fn fmt_time(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// Centered popup rectangle, clamped to the screen.
pub fn popup_rect(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ListLayout {
    pub outer: Rect,
    pub list: Rect,
    pub footer: Rect,
}

/// Popup with a scrolling list and a footer of `footer_h` rows.
pub fn list_popup(area: Rect, w: u16, rows: usize, footer_h: u16) -> ListLayout {
    let max_list = area.height.saturating_sub(4 + footer_h).max(1);
    let list_h = (rows as u16).clamp(1, max_list);
    let outer = popup_rect(area, w, list_h + footer_h + 2);
    let inn = inner(outer);
    let list = Rect { x: inn.x + 1.min(inn.width), y: inn.y, width: inn.width.saturating_sub(2), height: inn.height.saturating_sub(footer_h) };
    let footer = Rect {
        x: list.x,
        y: inn.y + list.height,
        width: list.width,
        height: inn.height.saturating_sub(list.height),
    };
    ListLayout { outer, list, footer }
}

/// First visible row so that `sel` is visible in a window of `h` rows.
pub fn list_scroll(sel: usize, h: usize, total: usize) -> usize {
    if h == 0 || total <= h {
        return 0;
    }
    let s = (sel + 1).saturating_sub(h);
    // show a header row above the first items nicely: keep stable
    s.min(total - h)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(w: u16) -> Rect {
        Rect::new(2, 10, w, 6)
    }

    #[test]
    fn chain_layout_alternates_nodes_and_arrows() {
        let slots = layout_chain(&[10, 12, 10], 0, area(100));
        let kinds: Vec<_> = slots.iter().map(|s| s.kind).collect();
        assert_eq!(
            kinds,
            vec![
                SlotKind::Node(0),
                SlotKind::Arrow(0),
                SlotKind::Node(1),
                SlotKind::Arrow(1),
                SlotKind::Node(2)
            ]
        );
        assert_eq!(slots[0].rect.x, 2);
        assert_eq!(slots[1].rect.x, 12);
        assert_eq!(slots[2].rect.x, 12 + ARROW_W);
    }

    #[test]
    fn hit_arrow_gives_insert_index() {
        let slots = layout_chain(&[10, 12, 10], 0, area(100));
        let ax = slots[3].rect.x + 1;
        assert_eq!(hit_slot(&slots, ax, 10), Some(SlotKind::Arrow(1)));
        assert_eq!(hit_slot(&slots, slots[2].rect.x + 1, slots[2].rect.y + 1), Some(SlotKind::Node(1)));
        assert_eq!(hit_slot(&slots, 0, 0), None);
    }

    #[test]
    fn overflow_omits_items_and_scrolls() {
        let widths = [10u16; 6];
        let a = area(40);
        let slots = layout_chain(&widths, 0, a);
        let nodes = slots.iter().filter(|s| matches!(s.kind, SlotKind::Node(_))).count();
        assert_eq!(nodes, 3);
        assert_eq!(overflow(&slots, 6), (false, true));
        let first = ensure_visible(&widths, 0, 5, a);
        assert_eq!(first, 3);
        let slots = layout_chain(&widths, first, a);
        assert_eq!(overflow(&slots, 6), (true, false));
        assert_eq!(ensure_visible(&widths, 4, 1, a), 1);
    }

    #[test]
    fn top_layout_fits() {
        let r = Rect::new(1, 1, 100, 1);
        let t = top_layout(r);
        assert_eq!(t.title.width + t.elapsed.width + t.bar.width + t.total.width + t.looping.width, 100);
        assert!(t.bar.width > 20);
        assert_eq!(bar_frac(t.bar, t.bar.x), 0.0);
        assert_eq!(bar_frac(t.bar, t.bar.x + t.bar.width - 1), 1.0);
        let tiny = top_layout(Rect::new(0, 0, 5, 1));
        assert_eq!(tiny.bar.width, 0);
    }

    #[test]
    fn time_format() {
        assert_eq!(fmt_time(65.4), "1:05");
        assert_eq!(fmt_time(3725.0), "1:02:05");
    }

    #[test]
    fn list_scroll_keeps_selection_visible() {
        assert_eq!(list_scroll(2, 5, 20), 0);
        assert_eq!(list_scroll(9, 5, 20), 5);
        assert_eq!(list_scroll(19, 5, 20), 15);
    }

    #[test]
    fn popup_is_centered_and_clamped() {
        let p = popup_rect(Rect::new(0, 0, 100, 40), 50, 10);
        assert_eq!((p.x, p.y), (25, 15));
        let p = popup_rect(Rect::new(0, 0, 20, 5), 50, 10);
        assert_eq!((p.width, p.height), (20, 5));
    }
}
