// Copyright (C) 2026 huangdihd
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! Full-screen task panel. Interactive tasks render their vt100 screen and can
//! receive input; pipe-based tasks render their captured output read-only.
//!
//! Opened with `/terminal [id]` or a `!command`. `Ctrl+O` toggles input grab:
//! while grabbed, every key is translated to terminal bytes and written to the
//! child; while released, the panel handles its own keys (`Esc`/`q` to close).
//! Read-only tasks support scrolling but never forward keyboard or mouse input.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Widget};

use crate::tasks;
use crate::ui::markdown_theme::palette;

/// State for the open terminal panel.
#[derive(Debug)]
pub struct TerminalPane {
    /// The task being shown.
    pub task_id: u64,
    /// Label for the header (the task's name).
    pub name: String,
    /// While true, keystrokes are forwarded to the PTY; while false the panel
    /// consumes them for its own controls.
    pub grabbed: bool,
    /// Whether the task has a PTY and can receive input.
    interactive: bool,
    /// Logical lines scrolled back from the latest captured pipe output.
    read_only_scroll: usize,
    /// Last grid size pushed to the PTY, so we only resize on change.
    last_size: Option<(u16, u16)>,
    /// The vt100 grid's screen area from the last render, for translating mouse
    /// coordinates into cell coordinates.
    pub grid: Option<Rect>,
    /// Consecutive ticks the task has been observed finished. Once this passes
    /// a short grace period (letting the PTY reader flush the output tail),
    /// the panel auto-closes and focus returns to the input.
    pub finished_ticks: u8,
}

impl TerminalPane {
    pub fn new(task_id: u64, name: String) -> Self {
        TerminalPane {
            task_id,
            name,
            grabbed: false,
            interactive: tasks::is_interactive(task_id),
            read_only_scroll: 0,
            last_size: None,
            grid: None,
            finished_ticks: 0,
        }
    }

    /// Whether keyboard and mouse input can be forwarded to this task.
    pub fn accepts_input(&self) -> bool {
        self.interactive
    }

    /// Scroll captured pipe output by logical lines. Positive values move
    /// toward older output; negative values move back toward the live tail.
    pub fn scroll_read_only(&mut self, delta: i32) {
        if self.interactive {
            return;
        }
        let max_scroll = self.read_only_max_scroll();
        let current = self.read_only_scroll.min(max_scroll);
        if delta >= 0 {
            self.read_only_scroll = current.saturating_add(delta as usize).min(max_scroll);
        } else {
            self.read_only_scroll = current.saturating_sub(delta.unsigned_abs() as usize);
        }
    }

    /// Jump to the oldest captured output.
    pub fn scroll_read_only_to_start(&mut self) {
        if !self.interactive {
            self.read_only_scroll = self.read_only_max_scroll();
        }
    }

    /// Jump back to the latest captured output.
    pub fn scroll_read_only_to_end(&mut self) {
        if !self.interactive {
            self.read_only_scroll = 0;
        }
    }

    fn read_only_max_scroll(&self) -> usize {
        let visible_lines = self.grid.map(|grid| grid.height as usize).unwrap_or(1);
        tasks::snapshot(self.task_id)
            .map(|snapshot| {
                read_only_lines(&snapshot)
                    .len()
                    .saturating_sub(visible_lines)
            })
            .unwrap_or(0)
    }

    /// Push the current grid size to the PTY when it changes.
    pub fn maybe_resize(&mut self, rows: u16, cols: u16) {
        if self.interactive && self.last_size != Some((rows, cols)) {
            let _ = tasks::resize(self.task_id, rows, cols);
            self.last_size = Some((rows, cols));
        }
    }
}

/// The vt100 grid area within `area` (everything but the header and hint rows).
pub fn grid_area(area: Rect) -> Rect {
    Rect {
        x: area.x,
        y: area.y.saturating_add(1),
        width: area.width,
        height: area.height.saturating_sub(2),
    }
}

/// Translate a crossterm key event into the bytes a terminal would send for it.
/// Returns `None` for keys with no terminal encoding. `app_cursor` reflects the
/// child's DECCKM mode: when set, cursor keys use the SS3 (`ESC O`) form the
/// program expects instead of the default CSI (`ESC [`) form.
pub fn key_event_to_bytes(key: KeyEvent, app_cursor: bool) -> Option<Vec<u8>> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    // Cursor keys: `ESC O <c>` in application mode, `ESC [ <c>` otherwise.
    let cursor = |c: u8| -> [u8; 3] { [0x1b, if app_cursor { b'O' } else { b'[' }, c] };
    let mut out: Vec<u8> = Vec::new();
    match key.code {
        KeyCode::Char(c) => {
            if ctrl {
                let b = match c.to_ascii_lowercase() {
                    'a'..='z' => (c.to_ascii_lowercase() as u8 - b'a') + 1,
                    ' ' | '@' => 0,
                    '[' => 0x1b,
                    '\\' => 0x1c,
                    ']' => 0x1d,
                    '^' => 0x1e,
                    '_' => 0x1f,
                    _ => return None,
                };
                if alt {
                    out.push(0x1b);
                }
                out.push(b);
            } else {
                if alt {
                    out.push(0x1b);
                }
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
        }
        KeyCode::Enter => out.push(b'\r'),
        KeyCode::Tab => out.push(b'\t'),
        KeyCode::BackTab => out.extend_from_slice(b"\x1b[Z"),
        KeyCode::Backspace => out.push(0x7f),
        KeyCode::Esc => out.push(0x1b),
        KeyCode::Left => out.extend_from_slice(&cursor(b'D')),
        KeyCode::Right => out.extend_from_slice(&cursor(b'C')),
        KeyCode::Up => out.extend_from_slice(&cursor(b'A')),
        KeyCode::Down => out.extend_from_slice(&cursor(b'B')),
        KeyCode::Home => out.extend_from_slice(&cursor(b'H')),
        KeyCode::End => out.extend_from_slice(&cursor(b'F')),
        KeyCode::PageUp => out.extend_from_slice(b"\x1b[5~"),
        KeyCode::PageDown => out.extend_from_slice(b"\x1b[6~"),
        KeyCode::Delete => out.extend_from_slice(b"\x1b[3~"),
        KeyCode::Insert => out.extend_from_slice(b"\x1b[2~"),
        KeyCode::F(n) => out.extend_from_slice(fkey(n)?),
        _ => return None,
    }
    (!out.is_empty()).then_some(out)
}

/// Translate a crossterm mouse event into an SGR mouse report for the child,
/// or `None` if it shouldn't be forwarded: the pointer is outside the grid, the
/// program hasn't enabled mouse reporting, or the event kind isn't wanted in
/// the program's current tracking mode.
pub fn mouse_event_to_bytes(
    mouse: crossterm::event::MouseEvent,
    grid: Rect,
    mode: vt100::MouseProtocolMode,
) -> Option<Vec<u8>> {
    use crossterm::event::{MouseButton as B, MouseEventKind as K};
    use vt100::MouseProtocolMode as M;

    if mode == M::None {
        return None;
    }
    // Screen coordinates → 0-based cell coordinates within the grid.
    if mouse.column < grid.x || mouse.row < grid.y {
        return None;
    }
    let col = mouse.column - grid.x;
    let row = mouse.row - grid.y;
    if col >= grid.width || row >= grid.height {
        return None;
    }

    let base = |b: B| -> u8 {
        match b {
            B::Left => 0,
            B::Middle => 1,
            B::Right => 2,
        }
    };
    let (mut code, release) = match mouse.kind {
        K::Down(b) => (base(b), false),
        K::Up(b) => {
            if mode == M::Press {
                return None; // press-only tracking doesn't want releases
            }
            (base(b), true)
        }
        K::Drag(b) => {
            if !matches!(mode, M::ButtonMotion | M::AnyMotion) {
                return None;
            }
            (base(b) + 32, false)
        }
        K::Moved => {
            if mode != M::AnyMotion {
                return None;
            }
            (3 + 32, false)
        }
        K::ScrollUp => (64, false),
        K::ScrollDown => (65, false),
        K::ScrollLeft => (66, false),
        K::ScrollRight => (67, false),
    };
    // Modifier bits.
    let m = mouse.modifiers;
    if m.contains(KeyModifiers::SHIFT) {
        code += 4;
    }
    if m.contains(KeyModifiers::ALT) {
        code += 8;
    }
    if m.contains(KeyModifiers::CONTROL) {
        code += 16;
    }
    Some(tasks::sgr_mouse(code, col, row, release))
}

fn fkey(n: u8) -> Option<&'static [u8]> {
    Some(match n {
        1 => b"\x1bOP",
        2 => b"\x1bOQ",
        3 => b"\x1bOR",
        4 => b"\x1bOS",
        5 => b"\x1b[15~",
        6 => b"\x1b[17~",
        7 => b"\x1b[18~",
        8 => b"\x1b[19~",
        9 => b"\x1b[20~",
        10 => b"\x1b[21~",
        11 => b"\x1b[23~",
        12 => b"\x1b[24~",
        _ => return None,
    })
}

/// Render the panel: a header line, task output, and a hint line.
pub fn render(pane: &TerminalPane, area: Rect, buf: &mut Buffer) {
    Clear.render(area, buf);

    let snap = tasks::snapshot(pane.task_id);
    let status = snap.as_ref().map(|s| s.status.label()).unwrap_or("gone");

    // Header.
    let accent = if pane.grabbed {
        palette::GREEN
    } else {
        palette::BLUE
    };
    let title = if pane.interactive {
        "\u{1F5A5} terminal"
    } else {
        "\u{1F4CB} task"
    };
    let mode = if !pane.interactive {
        "   ○ READ ONLY"
    } else if pane.grabbed {
        "   ● INPUT GRABBED"
    } else {
        "   ○ view (released)"
    };
    let header = Line::from(vec![
        Span::styled(
            format!(" {title} [{}] ", pane.task_id),
            Style::new().fg(accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{} · {status}", pane.name),
            Style::new().fg(palette::MUTED),
        ),
        Span::styled(mode, Style::new().fg(accent)),
    ]);
    let header_area = Rect { height: 1, ..area };
    header.render(header_area, buf);

    // Grid.
    let grid = grid_area(area);
    if pane.interactive {
        let painted = tasks::with_screen(pane.task_id, |screen| {
            render_screen(screen, pane.grabbed, grid, buf);
        });
        if painted.is_none() {
            Line::from(Span::styled(
                "  (task is no longer available)",
                Style::new().fg(palette::RED_MUTED),
            ))
            .render(grid, buf);
        }
    } else if let Some(snapshot) = snap.as_ref() {
        render_read_only(snapshot, pane.read_only_scroll, grid, buf);
    } else {
        Line::from(Span::styled(
            "  (task is no longer available)",
            Style::new().fg(palette::RED_MUTED),
        ))
        .render(grid, buf);
    }

    // Hint.
    let hint = if !pane.interactive {
        Line::from(Span::styled(
            " ↑/↓ scroll   PgUp/PgDn page   Home/End jump   Esc / q close   input disabled",
            Style::new().fg(palette::FAINT),
        ))
    } else if pane.grabbed {
        Line::from(Span::styled(
            " Ctrl+O release   keys & mouse → program   wheel: scroll back",
            Style::new().fg(palette::FAINT),
        ))
    } else {
        Line::from(Span::styled(
            " Ctrl+O grab input   wheel: scroll   Esc / q close",
            Style::new().fg(palette::FAINT),
        ))
    };
    let hint_area = Rect {
        y: area.y + area.height.saturating_sub(1),
        height: 1,
        ..area
    };
    hint.render(hint_area, buf);
}

fn render_read_only(
    snapshot: &tasks::TaskSnapshot,
    scroll_from_end: usize,
    area: Rect,
    buf: &mut Buffer,
) {
    let lines = read_only_lines(snapshot);
    let max_scroll = lines.len().saturating_sub(area.height as usize);
    let scroll_from_end = scroll_from_end.min(max_scroll);
    let start = max_scroll
        .saturating_sub(scroll_from_end)
        .min(u16::MAX as usize) as u16;
    Paragraph::new(lines).scroll((start, 0)).render(area, buf);
}

fn read_only_lines(snapshot: &tasks::TaskSnapshot) -> Vec<Line<'static>> {
    let mut lines = plain_output_lines(&snapshot.output);
    if !snapshot.stderr.is_empty() {
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        lines.push(Line::from(Span::styled(
            "stderr",
            Style::new()
                .fg(palette::RED_MUTED)
                .add_modifier(Modifier::BOLD),
        )));
        lines.extend(plain_output_lines(&snapshot.stderr));
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "(no output yet)",
            Style::new().fg(palette::FAINT),
        )));
    }
    lines
}

fn plain_output_lines(output: &str) -> Vec<Line<'static>> {
    tasks::strip_ansi(output)
        .lines()
        .map(|line| Line::raw(line.to_owned()))
        .collect()
}

/// Paint the vt100 screen cell-by-cell into `area`.
fn render_screen(screen: &vt100::Screen, grabbed: bool, area: Rect, buf: &mut Buffer) {
    let (cur_row, cur_col) = screen.cursor_position();
    let show_cursor = grabbed && !screen.hide_cursor();
    for row in 0..area.height {
        for col in 0..area.width {
            let Some(src) = screen.cell(row, col) else {
                continue;
            };
            let Some(dst) = buf.cell_mut((area.x + col, area.y + row)) else {
                continue;
            };
            let contents = src.contents();
            if contents.is_empty() {
                dst.set_char(' ');
            } else {
                dst.set_symbol(&contents);
            }
            let mut style = Style::new();
            if let Some(fg) = conv_color(src.fgcolor()) {
                style = style.fg(fg);
            }
            if let Some(bg) = conv_color(src.bgcolor()) {
                style = style.bg(bg);
            }
            let mut mods = Modifier::empty();
            if src.bold() {
                mods |= Modifier::BOLD;
            }
            if src.italic() {
                mods |= Modifier::ITALIC;
            }
            if src.underline() {
                mods |= Modifier::UNDERLINED;
            }
            if src.inverse() {
                mods |= Modifier::REVERSED;
            }
            if show_cursor && row == cur_row && col == cur_col {
                mods |= Modifier::REVERSED;
            }
            dst.set_style(style.add_modifier(mods));
        }
    }
}

fn conv_color(color: vt100::Color) -> Option<Color> {
    match color {
        vt100::Color::Default => None,
        vt100::Color::Idx(i) => Some(Color::Indexed(i)),
        vt100::Color::Rgb(r, g, b) => Some(Color::Rgb(r, g, b)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn translates_plain_and_control_keys() {
        assert_eq!(
            key_event_to_bytes(key(KeyCode::Char('a'), KeyModifiers::NONE), false),
            Some(vec![b'a'])
        );
        assert_eq!(
            key_event_to_bytes(key(KeyCode::Char('c'), KeyModifiers::CONTROL), false),
            Some(vec![0x03])
        );
        assert_eq!(
            key_event_to_bytes(key(KeyCode::Enter, KeyModifiers::NONE), false),
            Some(vec![b'\r'])
        );
        // Alt prefixes ESC.
        assert_eq!(
            key_event_to_bytes(key(KeyCode::Char('x'), KeyModifiers::ALT), false),
            Some(vec![0x1b, b'x'])
        );
        assert_eq!(
            key_event_to_bytes(key(KeyCode::F(1), KeyModifiers::NONE), false),
            Some(b"\x1bOP".to_vec())
        );
    }

    #[test]
    fn cursor_keys_switch_between_csi_and_ss3() {
        // Normal mode → CSI (ESC [), application-cursor mode → SS3 (ESC O).
        assert_eq!(
            key_event_to_bytes(key(KeyCode::Up, KeyModifiers::NONE), false),
            Some(b"\x1b[A".to_vec())
        );
        assert_eq!(
            key_event_to_bytes(key(KeyCode::Up, KeyModifiers::NONE), true),
            Some(b"\x1bOA".to_vec())
        );
        assert_eq!(
            key_event_to_bytes(key(KeyCode::Left, KeyModifiers::NONE), true),
            Some(b"\x1bOD".to_vec())
        );
    }

    #[test]
    fn grid_area_reserves_header_and_hint() {
        let area = Rect::new(0, 0, 80, 24);
        let g = grid_area(area);
        assert_eq!(g.y, 1);
        assert_eq!(g.height, 22);
        assert_eq!(g.width, 80);
    }

    fn mev(
        kind: crossterm::event::MouseEventKind,
        col: u16,
        row: u16,
    ) -> crossterm::event::MouseEvent {
        crossterm::event::MouseEvent {
            kind,
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn mouse_click_encodes_sgr_with_local_coords() {
        use crossterm::event::{MouseButton, MouseEventKind};
        // Grid starts at y=1 (header above). A click at screen (5,3) is local
        // (5,2) → SGR 1-based (6,3).
        let grid = Rect::new(0, 1, 80, 22);
        let m = mev(MouseEventKind::Down(MouseButton::Left), 5, 3);
        let bytes = mouse_event_to_bytes(m, grid, vt100::MouseProtocolMode::PressRelease).unwrap();
        assert_eq!(bytes, b"\x1b[<0;6;3M".to_vec());
    }

    #[test]
    fn mouse_gated_off_when_disabled_or_outside_grid() {
        use crossterm::event::{MouseButton, MouseEventKind};
        let grid = Rect::new(0, 1, 80, 22);
        let inside = mev(MouseEventKind::Down(MouseButton::Left), 5, 3);
        assert!(mouse_event_to_bytes(inside, grid, vt100::MouseProtocolMode::None).is_none());
        // Screen row 0 is the header, above the grid.
        let above = mev(MouseEventKind::Down(MouseButton::Left), 5, 0);
        assert!(
            mouse_event_to_bytes(above, grid, vt100::MouseProtocolMode::PressRelease).is_none()
        );
    }

    #[test]
    fn drag_and_scroll_respect_tracking_mode() {
        use crossterm::event::{MouseButton, MouseEventKind};
        let grid = Rect::new(0, 0, 80, 24);
        let drag = mev(MouseEventKind::Drag(MouseButton::Left), 5, 3);
        assert!(mouse_event_to_bytes(drag, grid, vt100::MouseProtocolMode::PressRelease).is_none());
        assert!(mouse_event_to_bytes(drag, grid, vt100::MouseProtocolMode::ButtonMotion).is_some());
        let scroll = mev(MouseEventKind::ScrollUp, 2, 2);
        let bytes = mouse_event_to_bytes(scroll, grid, vt100::MouseProtocolMode::Press).unwrap();
        assert_eq!(bytes, b"\x1b[<64;3;3M".to_vec());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn renders_live_task_screen_into_buffer() {
        // Drive a real PTY task and confirm its echoed output lands in the
        // rendered ratatui buffer (exercises the whole cell-paint path).
        let id = tasks::spawn_interactive("cat", None, Some("cat"), 10, 40).expect("spawn");
        tasks::write_bytes(id, b"hello-term\r").expect("write");
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        let pane = TerminalPane::new(id, "cat".to_string());
        let area = Rect::new(0, 0, 40, 12);
        let mut buf = Buffer::empty(area);
        render(&pane, area, &mut buf);

        let text: String = (0..area.height)
            .flat_map(|y| (0..area.width).map(move |x| (x, y)))
            .map(|(x, y)| buf[(x, y)].symbol().to_string())
            .collect();
        assert!(text.contains("hello-term"), "buffer text: {text}");

        tasks::kill(id).ok();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn renders_pipe_task_output_read_only() {
        let id = tasks::spawn(
            "printf 'pipe-out-%s\\n' 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; \
             printf 'pipe-err\\n' >&2",
            None,
            Some("pipe"),
        )
        .expect("spawn");
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        let mut pane = TerminalPane::new(id, "pipe".to_string());
        assert!(!pane.accepts_input());
        let area = Rect::new(0, 0, 50, 12);
        pane.grid = Some(grid_area(area));
        let mut buf = Buffer::empty(area);
        render(&pane, area, &mut buf);

        let text: String = (0..area.height)
            .flat_map(|y| (0..area.width).map(move |x| (x, y)))
            .map(|(x, y)| buf[(x, y)].symbol().to_string())
            .collect();
        assert!(text.contains("READ ONLY"), "buffer text: {text}");
        assert!(text.contains("pipe-out-15"), "buffer text: {text}");
        assert!(text.contains("pipe-err"), "buffer text: {text}");

        pane.scroll_read_only_to_start();
        let oldest = pane.read_only_scroll;
        assert!(oldest > 0);
        pane.scroll_read_only(-1);
        assert_eq!(pane.read_only_scroll, oldest - 1);
    }
}
