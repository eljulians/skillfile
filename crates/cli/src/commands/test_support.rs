//! Shared test utilities for TUI rendering tests.
//!
//! `#[cfg(test)]`-gated module — only compiled during `cargo test`.

use ratatui::backend::TestBackend;
use ratatui::layout::Position;
use ratatui::Terminal;

/// Standard terminal dimensions for snapshot tests.
pub const TERM_WIDTH: u16 = 80;
pub const TERM_HEIGHT: u16 = 24;

/// Extract text from a single buffer row, trimming trailing whitespace.
fn row_text(buf: &ratatui::buffer::Buffer, y: u16) -> String {
    (0..buf.area.width)
        .map(|x| {
            buf.cell(Position::new(x, y))
                .map_or(" ", ratatui::buffer::Cell::symbol)
        })
        .collect::<String>()
        .trim_end()
        .to_string()
}

/// Render the terminal buffer to text — one trimmed line per row.
fn buffer_text(buf: &ratatui::buffer::Buffer) -> String {
    (0..buf.area.height)
        .map(|y| row_text(buf, y))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Render a TUI frame via `draw_fn` and return the buffer as text.
///
/// Creates a `TestBackend` terminal at the standard 80x24 size,
/// draws one frame, and extracts the buffer content. This eliminates
/// the 4-line boilerplate from every render test.
pub fn render_to_text(draw_fn: impl FnOnce(&mut ratatui::Frame)) -> String {
    let backend = TestBackend::new(TERM_WIDTH, TERM_HEIGHT);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| draw_fn(f)).unwrap();
    buffer_text(terminal.backend().buffer())
}
