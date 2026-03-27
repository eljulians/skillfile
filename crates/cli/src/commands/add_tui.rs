//! TUI for `skillfile add` bulk — split-pane interactive multi-select browser.
//!
//! Architecture: Elm-style Model-View-Update (parallel to `search_tui.rs`).
//!
//! - **Model** ([`App`]): discovered paths + mutable UI state
//!   (filter, selections, preview cache).
//! - **View** ([`draw`]): renders list pane (left) with checkboxes and
//!   preview pane (right) with SKILL.md content.
//! - **Update** ([`handle_key`]): maps key events to state transitions.
//!
//! The public entry point [`run_add_tui`] sets up the terminal, runs the
//! event loop, and returns the user's selections (if any).

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
    Frame, Terminal,
};

use super::skill_preview::{self, parse_skill_frontmatter, PreviewContent};

// ===========================================================================
// Model
// ===========================================================================

#[derive(Debug, Clone)]
pub enum PreviewState {
    Loading,
    Loaded(PreviewContent),
    Failed,
}

pub struct App<'a> {
    /// Discovered entry paths (immutable after construction).
    items: &'a [String],
    /// Indices into `items` that match the current filter.
    filtered: Vec<usize>,
    filter: String,
    /// ListState tracks the currently highlighted row.
    list_state: ListState,
    /// Toggled items (indices into `items`).
    selected: HashSet<usize>,
    /// Whether the user confirmed selections (Enter).
    confirmed: bool,
    /// Whether the user cancelled (Esc / q).
    cancelled: bool,
    /// Cache of fetched SKILL.md previews, keyed by path.
    preview_cache: HashMap<String, PreviewState>,
    /// Receiver for completed preview fetches from background threads.
    preview_rx: mpsc::Receiver<(String, PreviewState)>,
    /// Sender cloned into background fetch threads.
    preview_tx: mpsc::Sender<(String, PreviewState)>,
    owner_repo: String,
    /// Git ref for preview fetches.
    ref_: String,
    preview_scroll: u16,
    /// Previously highlighted index — used to reset scroll on highlight change.
    last_highlighted_idx: Option<usize>,
}

impl<'a> App<'a> {
    pub fn new(items: &'a [String], owner_repo: &str, ref_: &str) -> Self {
        let filtered: Vec<usize> = (0..items.len()).collect();
        let mut list_state = ListState::default();
        if !filtered.is_empty() {
            list_state.select(Some(0));
        }
        let (preview_tx, preview_rx) = mpsc::channel();
        Self {
            items,
            filtered,
            filter: String::new(),
            list_state,
            selected: HashSet::new(),
            confirmed: false,
            cancelled: false,
            preview_cache: HashMap::new(),
            preview_rx,
            preview_tx,
            owner_repo: owner_repo.to_string(),
            ref_: ref_.to_string(),
            preview_scroll: 0,
            last_highlighted_idx: None,
        }
    }

    pub fn highlighted_path(&self) -> Option<&'a str> {
        let idx = self.list_state.selected()?;
        let original_idx = *self.filtered.get(idx)?;
        self.items.get(original_idx).map(String::as_str)
    }

    fn highlighted_index(&self) -> Option<usize> {
        let idx = self.list_state.selected()?;
        self.filtered.get(idx).copied()
    }

    fn refilter(&mut self) {
        let query = self.filter.to_lowercase();
        self.filtered = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, path)| path_matches_query(path, &query))
            .map(|(i, _)| i)
            .collect();

        if self.filtered.is_empty() {
            self.list_state.select(None);
        } else {
            self.list_state.select(Some(0));
        }
    }

    fn should_quit(&self) -> bool {
        self.confirmed || self.cancelled
    }

    fn selection_count(&self) -> usize {
        self.selected.len()
    }

    /// Spawn a background fetch for the currently highlighted item if not cached.
    fn maybe_fetch_preview(&mut self) {
        let Some(path) = self.highlighted_path() else {
            return;
        };
        if self.preview_cache.contains_key(path) {
            return;
        }
        let path_owned = path.to_string();
        self.preview_cache
            .insert(path_owned.clone(), PreviewState::Loading);
        let owner_repo = self.owner_repo.clone();
        let ref_ = self.ref_.clone();
        let tx = self.preview_tx.clone();
        std::thread::spawn(move || {
            let state = fetch_preview(&owner_repo, &ref_, &path_owned);
            let _ = tx.send((path_owned, state));
        });
    }

    fn poll_previews(&mut self) {
        while let Ok((path, state)) = self.preview_rx.try_recv() {
            self.preview_cache.insert(path, state);
        }
    }

    fn reset_scroll_if_changed(&mut self) {
        let current = self.highlighted_index();
        if current != self.last_highlighted_idx {
            self.preview_scroll = 0;
            self.last_highlighted_idx = current;
        }
    }
}

fn path_matches_query(path: &str, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let haystack = path.to_lowercase();
    query.split_whitespace().all(|w| haystack.contains(w))
}

fn has_md_extension(path: &str) -> bool {
    Path::new(path)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
}

/// Determine the raw GitHub path for fetching SKILL.md preview content.
pub fn resolve_preview_path(path: &str) -> String {
    if has_md_extension(path) {
        path.to_string()
    } else if path == "." {
        "SKILL.md".to_string()
    } else {
        format!("{path}/SKILL.md")
    }
}

fn fetch_preview(owner_repo: &str, ref_: &str, path: &str) -> PreviewState {
    let client = skillfile_sources::http::UreqClient::new();
    let gh = skillfile_sources::resolver::GithubFetch {
        client: &client,
        owner_repo,
        ref_,
    };
    let fetch_path = resolve_preview_path(path);
    match skillfile_sources::resolver::fetch_github_file(&gh, &fetch_path) {
        Ok(bytes) => {
            let text = String::from_utf8_lossy(&bytes);
            PreviewState::Loaded(parse_skill_frontmatter(&text))
        }
        Err(_) => PreviewState::Failed,
    }
}

fn is_dir_entry(path: &str) -> bool {
    !has_md_extension(path) && path != "."
}

fn display_label(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

// ===========================================================================
// Update
// ===========================================================================

const SCROLL_STEP: u16 = 3;

pub fn handle_key(app: &mut App<'_>, key: event::KeyEvent) {
    match key.code {
        KeyCode::Esc => app.cancelled = true,
        KeyCode::Char('q') if app.filter.is_empty() => app.cancelled = true,
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.cancelled = true;
        }
        KeyCode::Enter => handle_key_enter(app),
        KeyCode::Char(' ') => handle_key_space(app),
        KeyCode::Char('a') if app.filter.is_empty() => handle_key_toggle_all(app),
        KeyCode::Up | KeyCode::Char('k') if app.filter.is_empty() => {
            move_selection(app, -1);
        }
        KeyCode::Down | KeyCode::Char('j') if app.filter.is_empty() => {
            move_selection(app, 1);
        }
        KeyCode::Home | KeyCode::Char('g') if app.filter.is_empty() => {
            handle_key_jump_top(app);
        }
        KeyCode::End | KeyCode::Char('G') if app.filter.is_empty() => {
            handle_key_jump_bottom(app);
        }
        // Preview scroll: Tab down, Shift+Tab up
        KeyCode::Tab => {
            app.preview_scroll = app.preview_scroll.saturating_add(SCROLL_STEP);
        }
        KeyCode::BackTab => {
            app.preview_scroll = app.preview_scroll.saturating_sub(SCROLL_STEP);
        }
        KeyCode::Char(c) => {
            app.filter.push(c);
            app.refilter();
        }
        KeyCode::Backspace => {
            app.filter.pop();
            app.refilter();
        }
        _ => {}
    }
}

/// Confirm current selections (Enter key). No-op if nothing selected.
fn handle_key_enter(app: &mut App<'_>) {
    if !app.selected.is_empty() {
        app.confirmed = true;
    }
}

fn handle_key_space(app: &mut App<'_>) {
    if let Some(idx) = app.highlighted_index() {
        if app.selected.contains(&idx) {
            app.selected.remove(&idx);
        } else {
            app.selected.insert(idx);
        }
    }
}

fn handle_key_toggle_all(app: &mut App<'_>) {
    let all_selected = app.filtered.iter().all(|idx| app.selected.contains(idx));
    if all_selected {
        for &idx in &app.filtered {
            app.selected.remove(&idx);
        }
    } else {
        for &idx in &app.filtered {
            app.selected.insert(idx);
        }
    }
}

fn handle_key_jump_top(app: &mut App<'_>) {
    if !app.filtered.is_empty() {
        app.list_state.select(Some(0));
    }
}

fn handle_key_jump_bottom(app: &mut App<'_>) {
    if !app.filtered.is_empty() {
        app.list_state.select(Some(app.filtered.len() - 1));
    }
}

/// Move selection by `delta` rows (negative = up, positive = down), wrapping.
fn move_selection(app: &mut App<'_>, delta: i32) {
    let len = app.filtered.len();
    if len == 0 {
        return;
    }
    let current = app.list_state.selected().unwrap_or(0);
    #[allow(clippy::cast_possible_wrap)]
    let next = (current as isize + delta as isize).rem_euclid(len as isize) as usize;
    app.list_state.select(Some(next));
}

// ===========================================================================
// View
// ===========================================================================

/// Render the full TUI frame: status bar + split pane (list | preview).
pub fn draw(frame: &mut Frame, app: &mut App<'_>) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(frame.area());

    draw_status_bar(frame, chunks[0], app);

    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(chunks[1]);

    draw_list(frame, panes[0], app);
    draw_preview(frame, panes[1], app);
}

fn draw_status_bar(frame: &mut Frame, area: Rect, app: &App<'_>) {
    let sel_count = app.selection_count();
    let count = format!("{}/{}", app.filtered.len(), app.items.len());

    let mut spans = if app.filter.is_empty() {
        vec![
            Span::styled(" Space", Style::default().fg(Color::Cyan)),
            Span::styled(" toggle  ", Style::default().fg(Color::DarkGray)),
            Span::styled("a", Style::default().fg(Color::Cyan)),
            Span::styled(" all  ", Style::default().fg(Color::DarkGray)),
            Span::styled("Tab", Style::default().fg(Color::Cyan)),
            Span::styled(" scroll  ", Style::default().fg(Color::DarkGray)),
            Span::styled("Enter", Style::default().fg(Color::Cyan)),
            Span::styled(" confirm  ", Style::default().fg(Color::DarkGray)),
            Span::styled("Esc", Style::default().fg(Color::Cyan)),
            Span::styled(" cancel", Style::default().fg(Color::DarkGray)),
        ]
    } else {
        vec![
            Span::styled(" filter: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{}_", app.filter),
                Style::default().fg(Color::Yellow),
            ),
        ]
    };

    if sel_count > 0 {
        spans.push(Span::styled(
            format!("  \u{2713} {sel_count} selected"),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ));
    }

    spans.push(Span::raw("  "));
    spans.push(Span::styled(count, Style::default().fg(Color::DarkGray)));

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Extract the parent directory portion of a path for display (e.g. "skills/" from "skills/browser").
fn parent_hint(path: &str) -> &str {
    match path.rfind('/') {
        Some(pos) => &path[..=pos],
        None => "",
    }
}

fn draw_list(frame: &mut Frame, area: Rect, app: &mut App<'_>) {
    let items: Vec<ListItem<'_>> = app
        .filtered
        .iter()
        .map(|&idx| {
            let path = &app.items[idx];
            let label = display_label(path);
            let checked = app.selected.contains(&idx);

            // Unicode checkmarks instead of boring brackets
            let (icon, icon_color) = if checked {
                ("\u{25c9} ", Color::Green) // ◉
            } else {
                ("\u{25cb} ", Color::DarkGray) // ○
            };

            let mut spans = vec![
                Span::styled(icon, Style::default().fg(icon_color)),
                Span::styled(label, Style::default().add_modifier(Modifier::BOLD)),
            ];

            if is_dir_entry(path) {
                spans.push(Span::styled(
                    "  \u{1f4c1}",
                    Style::default().fg(Color::Yellow),
                ));
            }

            // Show parent path as a dim hint
            let hint = parent_hint(path);
            if !hint.is_empty() {
                spans.push(Span::styled(
                    format!("  {hint}"),
                    Style::default().fg(Color::DarkGray),
                ));
            }

            Line::from(spans).into()
        })
        .collect();

    let title = Line::from(vec![
        Span::raw(" "),
        Span::styled(
            app.owner_repo.as_str(),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
    ]);

    let block = Block::default().borders(Borders::ALL).title(title);

    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("\u{25b6} "); // ▶

    frame.render_stateful_widget(list, area, &mut app.list_state);
}

fn draw_preview(frame: &mut Frame, area: Rect, app: &App<'_>) {
    let scroll_hint = if app.preview_scroll > 0 {
        format!(" Preview (scroll: {}) ", app.preview_scroll)
    } else {
        " Preview \u{2191}\u{2193}Tab ".to_string()
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(scroll_hint)
        .border_style(Style::default().fg(Color::DarkGray));

    let content = match app.highlighted_path() {
        Some(path) => {
            let url = build_github_url(&app.owner_repo, &app.ref_, path);
            build_preview_lines(path, app.preview_cache.get(path), &url)
        }
        None => vec![Line::from(Span::styled(
            "No entries match the filter.",
            Style::default().fg(Color::DarkGray),
        ))],
    };

    let para = Paragraph::new(content)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((app.preview_scroll, 0));
    frame.render_widget(para, area);
}

fn build_loaded_preview_lines(content: &PreviewContent, url: &str) -> Vec<Line<'static>> {
    let label_style = Style::default().fg(Color::DarkGray);
    let mut lines: Vec<Line<'static>> = Vec::new();

    lines.push(Line::from(vec![
        Span::styled("URL:         ", label_style),
        Span::styled(
            url.to_string(),
            Style::default()
                .fg(Color::Blue)
                .add_modifier(Modifier::UNDERLINED),
        ),
    ]));
    lines.push(Line::from(""));
    lines.extend(skill_preview::build_skill_content_lines(content));
    lines
}

fn build_github_url(owner_repo: &str, ref_: &str, path: &str) -> String {
    if path == "." {
        return format!("https://github.com/{owner_repo}/tree/{ref_}");
    }
    let kind = if is_dir_entry(path) { "tree" } else { "blob" };
    format!("https://github.com/{owner_repo}/{kind}/{ref_}/{path}")
}

fn build_preview_lines(path: &str, state: Option<&PreviewState>, url: &str) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(16);

    lines.push(Line::from(Span::styled(
        path.to_string(),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));

    match state {
        Some(PreviewState::Loaded(content)) => {
            lines.extend(build_loaded_preview_lines(content, url));
        }
        Some(PreviewState::Failed) => {
            lines.push(Line::from(Span::styled(
                "\u{2717} Preview not available".to_string(),
                Style::default().fg(Color::Red),
            )));
        }
        Some(PreviewState::Loading) | None => {
            lines.push(Line::from(Span::styled(
                "\u{25cb} Loading preview...".to_string(),
                Style::default().fg(Color::Yellow),
            )));
        }
    }

    lines
}

// ===========================================================================
// Terminal lifecycle
// ===========================================================================

fn resolve_selections(app: &App<'_>) -> Vec<String> {
    let mut selected: Vec<usize> = app.selected.iter().copied().collect();
    selected.sort_unstable();
    selected
        .iter()
        .filter_map(|&idx| app.items.get(idx).cloned())
        .collect()
}

fn process_terminal_event(app: &mut App<'_>) -> Result<(), std::io::Error> {
    if let Event::Key(key) = event::read()? {
        handle_key(app, key);
    }
    Ok(())
}

fn resolve_result(app: &App<'_>) -> Vec<String> {
    if app.confirmed {
        resolve_selections(app)
    } else {
        Vec::new()
    }
}

/// Run the add TUI event loop. Returns selected paths (empty = cancelled).
pub fn run_add_tui(
    items: &[String],
    owner_repo: &str,
    ref_: &str,
) -> Result<Vec<String>, std::io::Error> {
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = std::io::stderr();
    crossterm::execute!(
        stdout,
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::EnableMouseCapture
    )?;

    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(
            std::io::stderr(),
            crossterm::terminal::LeaveAlternateScreen,
            crossterm::event::DisableMouseCapture
        );
        prev_hook(info);
    }));

    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(items, owner_repo, ref_);

    let result = loop {
        app.reset_scroll_if_changed();
        app.maybe_fetch_preview();
        app.poll_previews();

        terminal.draw(|f| draw(f, &mut app))?;

        if event::poll(Duration::from_millis(100))? {
            process_terminal_event(&mut app)?;
        }

        if app.should_quit() {
            break resolve_result(&app);
        }
    };

    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::event::DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    let _ = std::panic::take_hook();

    Ok(result)
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_items() -> Vec<String> {
        vec![
            "skills/browser".to_string(),
            "skills/code-review".to_string(),
            "skills/commit".to_string(),
            "skills/debugging/SKILL.md".to_string(),
            "skills/testing".to_string(),
        ]
    }

    // -- App state tests -------------------------------------------------------

    #[test]
    fn app_new_selects_first_item() {
        let items = sample_items();
        let app = App::new(&items, "owner/repo", "main");
        assert_eq!(app.list_state.selected(), Some(0));
        assert_eq!(app.filtered.len(), 5);
    }

    #[test]
    fn app_new_empty_items() {
        let items: Vec<String> = vec![];
        let app = App::new(&items, "owner/repo", "main");
        assert_eq!(app.list_state.selected(), None);
        assert!(app.filtered.is_empty());
    }

    #[test]
    fn app_new_selection_empty() {
        let items = sample_items();
        let app = App::new(&items, "owner/repo", "main");
        assert!(app.selected.is_empty());
    }

    #[test]
    fn highlighted_path_returns_first() {
        let items = sample_items();
        let app = App::new(&items, "owner/repo", "main");
        assert_eq!(app.highlighted_path(), Some("skills/browser"));
    }

    #[test]
    fn highlighted_path_empty_list() {
        let items: Vec<String> = vec![];
        let app = App::new(&items, "owner/repo", "main");
        assert_eq!(app.highlighted_path(), None);
    }

    // -- Multi-select tests ----------------------------------------------------

    #[test]
    fn toggle_selection_on_off() {
        let items = sample_items();
        let mut app = App::new(&items, "owner/repo", "main");
        handle_key_space(&mut app);
        assert!(app.selected.contains(&0));
        assert_eq!(app.selection_count(), 1);

        handle_key_space(&mut app);
        assert!(!app.selected.contains(&0));
        assert_eq!(app.selection_count(), 0);
    }

    #[test]
    fn toggle_all_selects_all_filtered() {
        let items = sample_items();
        let mut app = App::new(&items, "owner/repo", "main");
        handle_key_toggle_all(&mut app);
        assert_eq!(app.selection_count(), 5);
    }

    #[test]
    fn toggle_all_deselects_when_all_selected() {
        let items = sample_items();
        let mut app = App::new(&items, "owner/repo", "main");
        handle_key_toggle_all(&mut app);
        assert_eq!(app.selection_count(), 5);
        handle_key_toggle_all(&mut app);
        assert_eq!(app.selection_count(), 0);
    }

    #[test]
    fn selections_persist_across_filter() {
        let items = sample_items();
        let mut app = App::new(&items, "owner/repo", "main");
        handle_key_space(&mut app);
        assert!(app.selected.contains(&0));

        app.filter = "commit".to_string();
        app.refilter();
        assert_eq!(app.filtered.len(), 1);
        assert!(app.selected.contains(&0));

        app.filter.clear();
        app.refilter();
        assert!(app.selected.contains(&0));
    }

    #[test]
    fn toggle_all_only_affects_filtered() {
        let items = sample_items();
        let mut app = App::new(&items, "owner/repo", "main");
        app.filter = "code".to_string();
        app.refilter();
        assert_eq!(app.filtered.len(), 1);

        handle_key_toggle_all(&mut app);
        assert_eq!(app.selection_count(), 1);
        assert!(app.selected.contains(&1));
    }

    #[test]
    fn resolve_selections_returns_correct_paths() {
        let items = sample_items();
        let mut app = App::new(&items, "owner/repo", "main");
        app.selected.insert(0);
        app.selected.insert(2);
        let paths = resolve_selections(&app);
        assert_eq!(paths, vec!["skills/browser", "skills/commit"]);
    }

    #[test]
    fn resolve_selections_empty_when_none_selected() {
        let items = sample_items();
        let app = App::new(&items, "owner/repo", "main");
        let paths = resolve_selections(&app);
        assert!(paths.is_empty());
    }

    // -- Filter tests ----------------------------------------------------------

    #[test]
    fn filter_narrows_results() {
        let items = sample_items();
        let mut app = App::new(&items, "owner/repo", "main");
        app.filter = "browser".to_string();
        app.refilter();
        assert_eq!(app.filtered.len(), 1);
        assert_eq!(app.highlighted_path(), Some("skills/browser"));
    }

    #[test]
    fn filter_case_insensitive() {
        let items = sample_items();
        let mut app = App::new(&items, "owner/repo", "main");
        app.filter = "BROWSER".to_string();
        app.refilter();
        assert_eq!(app.filtered.len(), 1);
    }

    #[test]
    fn filter_no_match_clears_selection() {
        let items = sample_items();
        let mut app = App::new(&items, "owner/repo", "main");
        app.filter = "zzz_nonexistent".to_string();
        app.refilter();
        assert!(app.filtered.is_empty());
        assert!(app.highlighted_path().is_none());
    }

    #[test]
    fn clear_filter_restores_all() {
        let items = sample_items();
        let mut app = App::new(&items, "owner/repo", "main");
        app.filter = "browser".to_string();
        app.refilter();
        assert_eq!(app.filtered.len(), 1);

        app.filter.clear();
        app.refilter();
        assert_eq!(app.filtered.len(), 5);
    }

    #[test]
    fn filter_multi_word() {
        let items = sample_items();
        let mut app = App::new(&items, "owner/repo", "main");
        app.filter = "skills commit".to_string();
        app.refilter();
        assert_eq!(app.filtered.len(), 1);
        assert_eq!(app.highlighted_path(), Some("skills/commit"));
    }

    // -- Navigation tests -------------------------------------------------------

    #[test]
    fn move_selection_down() {
        let items = sample_items();
        let mut app = App::new(&items, "owner/repo", "main");
        move_selection(&mut app, 1);
        assert_eq!(app.list_state.selected(), Some(1));
    }

    #[test]
    fn move_selection_wraps_bottom_to_top() {
        let items = sample_items();
        let mut app = App::new(&items, "owner/repo", "main");
        app.list_state.select(Some(4));
        move_selection(&mut app, 1);
        assert_eq!(app.list_state.selected(), Some(0));
    }

    #[test]
    fn move_selection_wraps_top_to_bottom() {
        let items = sample_items();
        let mut app = App::new(&items, "owner/repo", "main");
        move_selection(&mut app, -1);
        assert_eq!(app.list_state.selected(), Some(4));
    }

    #[test]
    fn move_selection_empty_list() {
        let items: Vec<String> = vec![];
        let mut app = App::new(&items, "owner/repo", "main");
        move_selection(&mut app, 1);
        assert!(app.list_state.selected().is_none());
    }

    #[test]
    fn home_jumps_to_first() {
        let items = sample_items();
        let mut app = App::new(&items, "owner/repo", "main");
        app.list_state.select(Some(3));
        handle_key_jump_top(&mut app);
        assert_eq!(app.list_state.selected(), Some(0));
    }

    #[test]
    fn end_jumps_to_last() {
        let items = sample_items();
        let mut app = App::new(&items, "owner/repo", "main");
        handle_key_jump_bottom(&mut app);
        assert_eq!(app.list_state.selected(), Some(4));
    }

    // -- Key handling tests ----------------------------------------------------

    #[test]
    fn space_toggles_selection() {
        let items = sample_items();
        let mut app = App::new(&items, "owner/repo", "main");
        let key = event::KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE);
        handle_key(&mut app, key);
        assert!(app.selected.contains(&0));
    }

    #[test]
    fn enter_confirms_when_selected() {
        let items = sample_items();
        let mut app = App::new(&items, "owner/repo", "main");
        app.selected.insert(0);
        let key = event::KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        handle_key(&mut app, key);
        assert!(app.confirmed);
        assert!(app.should_quit());
    }

    #[test]
    fn enter_noop_when_nothing_selected() {
        let items = sample_items();
        let mut app = App::new(&items, "owner/repo", "main");
        let key = event::KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        handle_key(&mut app, key);
        assert!(!app.confirmed);
        assert!(!app.should_quit());
    }

    #[test]
    fn esc_cancels() {
        let items = sample_items();
        let mut app = App::new(&items, "owner/repo", "main");
        let key = event::KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        handle_key(&mut app, key);
        assert!(app.cancelled);
        assert!(app.should_quit());
    }

    #[test]
    fn q_cancels_when_filter_empty() {
        let items = sample_items();
        let mut app = App::new(&items, "owner/repo", "main");
        let key = event::KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        handle_key(&mut app, key);
        assert!(app.cancelled);
    }

    #[test]
    fn q_types_into_filter_when_filter_has_text() {
        let items = sample_items();
        let mut app = App::new(&items, "owner/repo", "main");
        app.filter = "bro".to_string();
        let key = event::KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        handle_key(&mut app, key);
        assert!(!app.cancelled);
        assert_eq!(app.filter, "broq");
    }

    #[test]
    fn ctrl_c_cancels() {
        let items = sample_items();
        let mut app = App::new(&items, "owner/repo", "main");
        let key = event::KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        handle_key(&mut app, key);
        assert!(app.cancelled);
    }

    #[test]
    fn typing_updates_filter() {
        let items = sample_items();
        let mut app = App::new(&items, "owner/repo", "main");
        for c in "bro".chars() {
            let key = event::KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
            handle_key(&mut app, key);
        }
        assert_eq!(app.filter, "bro");
        assert_eq!(app.filtered.len(), 1);
    }

    #[test]
    fn backspace_removes_from_filter() {
        let items = sample_items();
        let mut app = App::new(&items, "owner/repo", "main");
        app.filter = "bro".to_string();
        app.refilter();
        let key = event::KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE);
        handle_key(&mut app, key);
        assert_eq!(app.filter, "br");
    }

    #[test]
    fn j_k_navigate_when_filter_empty() {
        let items = sample_items();
        let mut app = App::new(&items, "owner/repo", "main");
        let j = event::KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE);
        handle_key(&mut app, j);
        assert_eq!(app.list_state.selected(), Some(1));

        let k = event::KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE);
        handle_key(&mut app, k);
        assert_eq!(app.list_state.selected(), Some(0));
    }

    #[test]
    fn g_jumps_to_top() {
        let items = sample_items();
        let mut app = App::new(&items, "owner/repo", "main");
        app.list_state.select(Some(3));
        let key = event::KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE);
        handle_key(&mut app, key);
        assert_eq!(app.list_state.selected(), Some(0));
    }

    #[test]
    fn shift_g_jumps_to_bottom() {
        let items = sample_items();
        let mut app = App::new(&items, "owner/repo", "main");
        let key = event::KeyEvent::new(KeyCode::Char('G'), KeyModifiers::NONE);
        handle_key(&mut app, key);
        assert_eq!(app.list_state.selected(), Some(4));
    }

    #[test]
    fn a_toggles_all_when_filter_empty() {
        let items = sample_items();
        let mut app = App::new(&items, "owner/repo", "main");
        let key = event::KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        handle_key(&mut app, key);
        assert_eq!(app.selection_count(), 5);
    }

    #[test]
    fn a_types_into_filter_when_filter_has_text() {
        let items = sample_items();
        let mut app = App::new(&items, "owner/repo", "main");
        app.filter = "x".to_string();
        let key = event::KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        handle_key(&mut app, key);
        assert_eq!(app.filter, "xa");
        assert_eq!(app.selection_count(), 0);
    }

    // -- resolve_preview_path tests --------------------------------------------

    #[test]
    fn resolve_preview_path_dir() {
        assert_eq!(
            resolve_preview_path("skills/browser"),
            "skills/browser/SKILL.md"
        );
    }

    #[test]
    fn resolve_preview_path_file() {
        assert_eq!(
            resolve_preview_path("skills/debugging/SKILL.md"),
            "skills/debugging/SKILL.md"
        );
    }

    #[test]
    fn resolve_preview_path_root() {
        assert_eq!(resolve_preview_path("."), "SKILL.md");
    }

    // -- Display helper tests --------------------------------------------------

    #[test]
    fn is_dir_entry_true_for_dirs() {
        assert!(is_dir_entry("skills/browser"));
        assert!(is_dir_entry("skills/code-review"));
    }

    #[test]
    fn is_dir_entry_false_for_files() {
        assert!(!is_dir_entry("skills/debugging/SKILL.md"));
    }

    #[test]
    fn display_label_extracts_last_segment() {
        assert_eq!(display_label("skills/browser"), "browser");
        assert_eq!(display_label("skills/debugging/SKILL.md"), "SKILL.md");
    }

    #[test]
    fn display_label_no_slash() {
        assert_eq!(display_label("browser"), "browser");
    }

    // -- path_matches_query tests ----------------------------------------------

    #[test]
    fn path_matches_empty_query() {
        assert!(path_matches_query("skills/browser", ""));
    }

    #[test]
    fn path_matches_substring() {
        assert!(path_matches_query("skills/browser", "brow"));
    }

    #[test]
    fn path_no_match() {
        assert!(!path_matches_query("skills/browser", "docker"));
    }

    // -- GitHub URL tests ------------------------------------------------------

    #[test]
    fn github_url_for_dir() {
        assert_eq!(
            build_github_url("owner/repo", "main", "skills/browser"),
            "https://github.com/owner/repo/tree/main/skills/browser"
        );
    }

    #[test]
    fn github_url_for_file() {
        assert_eq!(
            build_github_url("owner/repo", "main", "skills/foo/SKILL.md"),
            "https://github.com/owner/repo/blob/main/skills/foo/SKILL.md"
        );
    }

    #[test]
    fn github_url_with_ref() {
        assert_eq!(
            build_github_url("org/repo", "v1.0", "skills/bar"),
            "https://github.com/org/repo/tree/v1.0/skills/bar"
        );
    }

    #[test]
    fn github_url_root_path() {
        assert_eq!(
            build_github_url("owner/repo", "main", "."),
            "https://github.com/owner/repo/tree/main"
        );
    }

    // -- Scroll tests ----------------------------------------------------------

    #[test]
    fn tab_scrolls_preview_down() {
        let items = sample_items();
        let mut app = App::new(&items, "owner/repo", "main");
        assert_eq!(app.preview_scroll, 0);

        let key = event::KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);
        handle_key(&mut app, key);
        assert_eq!(app.preview_scroll, SCROLL_STEP);
    }

    #[test]
    fn shift_tab_scrolls_preview_up() {
        let items = sample_items();
        let mut app = App::new(&items, "owner/repo", "main");
        app.preview_scroll = 6;

        let key = event::KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT);
        handle_key(&mut app, key);
        assert_eq!(app.preview_scroll, 6 - SCROLL_STEP);
    }

    #[test]
    fn scroll_does_not_underflow() {
        let items = sample_items();
        let mut app = App::new(&items, "owner/repo", "main");
        let key = event::KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT);
        handle_key(&mut app, key);
        assert_eq!(app.preview_scroll, 0);
    }

    #[test]
    fn scroll_resets_on_highlight_change() {
        let items = sample_items();
        let mut app = App::new(&items, "owner/repo", "main");
        app.last_highlighted_idx = Some(0);
        app.preview_scroll = 10;

        // Move to next item
        move_selection(&mut app, 1);
        app.reset_scroll_if_changed();
        assert_eq!(app.preview_scroll, 0);
    }

    #[test]
    fn scroll_persists_when_highlight_unchanged() {
        let items = sample_items();
        let mut app = App::new(&items, "owner/repo", "main");
        app.last_highlighted_idx = Some(0);
        app.preview_scroll = 10;

        app.reset_scroll_if_changed();
        assert_eq!(app.preview_scroll, 10);
    }

    // -- parent_hint tests -----------------------------------------------------

    #[test]
    fn parent_hint_with_slash() {
        assert_eq!(parent_hint("skills/browser"), "skills/");
    }

    #[test]
    fn parent_hint_no_slash() {
        assert_eq!(parent_hint("browser"), "");
    }

    #[test]
    fn parent_hint_nested() {
        assert_eq!(
            parent_hint("skills/debugging/SKILL.md"),
            "skills/debugging/"
        );
    }

    // -- Rendering tests (TestBackend) -----------------------------------------

    use crate::commands::test_support::render_to_text;

    #[test]
    fn render_initial_state() {
        let items = sample_items();
        let mut app = App::new(&items, "owner/repo", "main");
        insta::assert_snapshot!(render_to_text(|f| draw(f, &mut app)));
    }

    #[test]
    fn render_selected_items() {
        let items = sample_items();
        let mut app = App::new(&items, "owner/repo", "main");
        app.selected.insert(0);
        app.selected.insert(2);
        insta::assert_snapshot!(render_to_text(|f| draw(f, &mut app)));
    }

    #[test]
    fn render_filtered_with_selections() {
        let items = sample_items();
        let mut app = App::new(&items, "owner/repo", "main");
        app.selected.insert(0);
        app.selected.insert(2);
        app.filter = "code".to_string();
        app.refilter();
        insta::assert_snapshot!(render_to_text(|f| draw(f, &mut app)));
    }

    /// Preview title shows scroll offset when `preview_scroll > 0`.
    #[test]
    fn render_with_preview_scroll() {
        let items = sample_items();
        let mut app = App::new(&items, "owner/repo", "main");
        app.preview_scroll = 5;
        insta::assert_snapshot!(render_to_text(|f| draw(f, &mut app)));
    }

    /// Preview shows "No entries match" when filter produces empty list.
    #[test]
    fn render_empty_filter() {
        let items = sample_items();
        let mut app = App::new(&items, "owner/repo", "main");
        app.filter = "zzz_nonexistent".to_string();
        app.refilter();
        insta::assert_snapshot!(render_to_text(|f| draw(f, &mut app)));
    }

    /// Preview pane shows loaded SKILL.md content from cache.
    #[test]
    fn render_with_loaded_preview() {
        let items = sample_items();
        let mut app = App::new(&items, "owner/repo", "main");
        app.preview_cache.insert(
            "skills/browser".to_string(),
            PreviewState::Loaded(PreviewContent {
                name: Some("Browser Automation".into()),
                description: Some("Automate browsing".into()),
                risk: Some("medium".into()),
                source: Some("community".into()),
                body_excerpt: Some("## Usage\n- Navigate pages".into()),
            }),
        );
        insta::assert_snapshot!(render_to_text(|f| draw(f, &mut app)));
    }

    /// Preview pane shows failure state when fetch failed.
    #[test]
    fn render_with_failed_preview() {
        let items = sample_items();
        let mut app = App::new(&items, "owner/repo", "main");
        app.preview_cache
            .insert("skills/browser".to_string(), PreviewState::Failed);
        insta::assert_snapshot!(render_to_text(|f| draw(f, &mut app)));
    }
}
