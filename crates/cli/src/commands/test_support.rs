//! Shared test utilities for TUI rendering tests.
//!
//! `#[cfg(test)]`-gated module — only compiled during `cargo test`.

use ratatui::layout::Position;

/// Extract text from a single buffer row, trimming trailing whitespace.
pub fn row_text(buf: &ratatui::buffer::Buffer, y: u16) -> String {
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
pub fn buffer_text(buf: &ratatui::buffer::Buffer) -> String {
    (0..buf.area.height)
        .map(|y| row_text(buf, y))
        .collect::<Vec<_>>()
        .join("\n")
}
