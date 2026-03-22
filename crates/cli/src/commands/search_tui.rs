//! TUI for `skillfile search` — split-pane interactive result browser.
//!
//! Architecture: Elm-style Model-View-Update.
//!
//! - **Model** ([`App`]): immutable search results + mutable UI state
//!   (filter text, selected index, scroll offset).
//! - **View** ([`draw`]): renders the list pane (left) and detail preview (right).
//! - **Update** ([`handle_key`]): maps key events to state transitions.
//!
//! The public entry point [`run_tui`] sets up the terminal, runs the event loop,
//! and returns the user's selection (if any).

use std::collections::HashMap;
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
use skillfile_sources::registry::{RegistryId, SearchResult};

use super::skill_preview::{
    build_skill_content_lines, parse_skill_frontmatter, PreviewContent, PREVIEW_HR,
};

// ===========================================================================
// Model
// ===========================================================================

/// A single security audit result (provider + pass/fail).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecurityAudit {
    pub provider: String,
    pub passed: bool,
}

/// State of an audit fetch for a given skill URL.
#[derive(Debug, Clone)]
pub enum AuditState {
    Loading,
    Loaded(Vec<SecurityAudit>),
    Failed,
}

/// State of a SKILL.md preview fetch for a given search result URL.
#[derive(Debug, Clone)]
pub enum SkillPreviewState {
    /// Fetch in progress.
    Loading,
    /// Successfully fetched and parsed.
    Loaded(PreviewContent),
    /// No GitHub coordinates available — nothing to fetch.
    NotAvailable,
    /// Fetch attempted but failed.
    Failed,
}

/// Application state for the TUI event loop.
pub struct App<'a> {
    /// All search results (immutable after construction).
    items: &'a [SearchResult],
    /// Indices into `items` that match the current filter.
    filtered: Vec<usize>,
    /// Current filter text typed by the user.
    filter: String,
    /// ListState tracks the currently highlighted row.
    list_state: ListState,
    /// Whether the user confirmed a selection (Enter).
    confirmed: bool,
    /// Whether the user cancelled (Esc / q).
    cancelled: bool,
    /// Total results across all registries (for status line).
    total: usize,
    /// Cache of fetched security audit results, keyed by skill URL.
    audit_cache: HashMap<String, AuditState>,
    /// Receiver for completed audit fetches from background threads.
    audit_rx: mpsc::Receiver<(String, AuditState)>,
    /// Sender cloned into background fetch threads.
    audit_tx: mpsc::Sender<(String, AuditState)>,
    /// Cache of fetched SKILL.md previews, keyed by skill URL.
    skill_preview_cache: HashMap<String, SkillPreviewState>,
    /// Receiver for completed SKILL.md preview fetches from background threads.
    skill_preview_rx: mpsc::Receiver<(String, SkillPreviewState)>,
    /// Sender cloned into background SKILL.md fetch threads.
    skill_preview_tx: mpsc::Sender<(String, SkillPreviewState)>,
}

impl<'a> App<'a> {
    /// Create a new App from search results.
    pub fn new(items: &'a [SearchResult], total: usize) -> Self {
        let filtered: Vec<usize> = (0..items.len()).collect();
        let mut list_state = ListState::default();
        if !filtered.is_empty() {
            list_state.select(Some(0));
        }
        let (audit_tx, audit_rx) = mpsc::channel();
        let (skill_preview_tx, skill_preview_rx) = mpsc::channel();
        Self {
            items,
            filtered,
            filter: String::new(),
            list_state,
            confirmed: false,
            cancelled: false,
            total,
            audit_cache: HashMap::new(),
            audit_rx,
            audit_tx,
            skill_preview_cache: HashMap::new(),
            skill_preview_rx,
            skill_preview_tx,
        }
    }

    /// The currently selected SearchResult, if any.
    pub fn selected(&self) -> Option<&'a SearchResult> {
        let idx = self.list_state.selected()?;
        let original_idx = *self.filtered.get(idx)?;
        self.items.get(original_idx)
    }

    /// Recompute filtered indices from the current filter text.
    fn refilter(&mut self) {
        let query = self.filter.to_lowercase();
        self.filtered = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| item_matches_query(item, &query))
            .map(|(i, _)| i)
            .collect();

        // Reset selection to first match (or none).
        if self.filtered.is_empty() {
            self.list_state.select(None);
        } else {
            self.list_state.select(Some(0));
        }
    }

    /// Returns true when the event loop should exit.
    fn should_quit(&self) -> bool {
        self.confirmed || self.cancelled
    }

    /// Spawn background fetches for all visible skills.sh entries that
    /// don't have cached audit results yet.
    fn maybe_fetch_audits(&mut self) {
        let urls_to_fetch: Vec<String> = self
            .filtered
            .iter()
            .map(|&idx| &self.items[idx])
            .filter(|item| {
                item.registry.has_security_audits() && !self.audit_cache.contains_key(&item.url)
            })
            .map(|item| item.url.clone())
            .collect();

        for url in urls_to_fetch {
            self.audit_cache.insert(url.clone(), AuditState::Loading);
            spawn_audit_fetch(url, self.audit_tx.clone());
        }
    }

    /// Drain any completed audit fetches from the channel into the cache.
    fn poll_audits(&mut self) {
        while let Ok((url, state)) = self.audit_rx.try_recv() {
            self.audit_cache.insert(url, state);
        }
    }

    /// Spawn a background SKILL.md fetch for the currently highlighted item if not cached.
    ///
    /// Delegates content extraction to each registry's `fetch_skill_content`
    /// implementation (Strategy pattern). Only the highlighted item is fetched.
    fn maybe_fetch_skill_preview(&mut self) {
        let Some(item) = self.selected() else {
            return;
        };
        let url = item.url.clone();
        if self.skill_preview_cache.contains_key(&url) {
            return;
        }
        self.skill_preview_cache
            .insert(url.clone(), SkillPreviewState::Loading);
        let item = item.clone();
        let tx = self.skill_preview_tx.clone();
        std::thread::spawn(move || {
            let state = fetch_skill_preview_for_item(&item);
            let _ = tx.send((url, state));
        });
    }

    /// Drain any completed SKILL.md preview fetches from the channel into the cache.
    fn poll_skill_previews(&mut self) {
        while let Ok((url, state)) = self.skill_preview_rx.try_recv() {
            self.skill_preview_cache.insert(url, state);
        }
    }
}

/// Returns true if `item` matches `query` (empty query matches all).
fn item_matches_query(item: &SearchResult, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let haystack = format!(
        "{} {} {} {}",
        item.name,
        item.owner,
        item.description.as_deref().unwrap_or(""),
        item.registry
    )
    .to_lowercase();
    // Simple substring match — sufficient for a filter list.
    query.split_whitespace().all(|w| haystack.contains(w))
}

/// Fetch SKILL.md content via the per-registry Strategy dispatch.
fn fetch_skill_preview_for_item(item: &SearchResult) -> SkillPreviewState {
    match skillfile_sources::registry::fetch_skill_content_for(item) {
        Some(content) => SkillPreviewState::Loaded(parse_skill_frontmatter(&content)),
        None => SkillPreviewState::NotAvailable,
    }
}

/// Spawn a background thread to fetch audit results for `url`.
fn spawn_audit_fetch(url: String, tx: mpsc::Sender<(String, AuditState)>) {
    std::thread::spawn(move || {
        let state = match fetch_skillssh_audits(&url) {
            Ok(audits) => AuditState::Loaded(audits),
            Err(_) => AuditState::Failed,
        };
        let _ = tx.send((url, state));
    });
}

// ===========================================================================
// Update
// ===========================================================================

/// Process a single key event and mutate app state.
pub fn handle_key(app: &mut App<'_>, key: event::KeyEvent) {
    match key.code {
        KeyCode::Esc => app.cancelled = true,
        KeyCode::Char('q') if app.filter.is_empty() => app.cancelled = true,
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.cancelled = true;
        }
        KeyCode::Enter => handle_key_enter(app),
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

/// Confirm the current selection (Enter key).
fn handle_key_enter(app: &mut App<'_>) {
    if app.selected().is_some() {
        app.confirmed = true;
    }
}

/// Jump to the first item (Home / g key).
fn handle_key_jump_top(app: &mut App<'_>) {
    if !app.filtered.is_empty() {
        app.list_state.select(Some(0));
    }
}

/// Jump to the last item (End / G key).
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
    #[allow(clippy::cast_possible_wrap)] // list length is always small
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

    // Split main area: 40% list, 60% preview
    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(chunks[1]);

    draw_list(frame, panes[0], app);
    draw_preview(frame, panes[1], app);
}

/// Top status bar: filter input + result count.
fn draw_status_bar(frame: &mut Frame, area: Rect, app: &App<'_>) {
    let filter_display = if app.filter.is_empty() {
        String::from(" type to filter, Enter to select, Esc to cancel")
    } else {
        format!(" filter: {}_", app.filter)
    };

    let count = if app.total > app.items.len() {
        format!(
            " {}/{} (of {} total) ",
            app.filtered.len(),
            app.items.len(),
            app.total
        )
    } else {
        format!(" {}/{} ", app.filtered.len(), app.items.len())
    };

    let bar = Line::from(vec![
        Span::styled(filter_display, Style::default().fg(Color::Yellow)),
        Span::raw("  "),
        Span::styled(count, Style::default().fg(Color::DarkGray)),
    ]);
    frame.render_widget(Paragraph::new(bar), area);
}

/// Build the spans for a single list item's audit/security indicator.
fn build_list_item_audit_spans<'a>(
    item: &'a SearchResult,
    audit_cache: &HashMap<String, AuditState>,
) -> Vec<Span<'a>> {
    let mut spans = Vec::new();
    if let Some(score) = item.security_score {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            format!("\u{1f6e1} {score}"),
            Style::default().fg(score_color(score)),
        ));
    } else if let Some(AuditState::Loaded(audits)) = audit_cache.get(&item.url) {
        let all_pass = !audits.is_empty() && audits.iter().all(|a| a.passed);
        let (icon, color) = audit_pass_fail_icon(all_pass);
        spans.push(Span::raw("  "));
        spans.push(Span::styled(icon, Style::default().fg(color)));
    }
    spans
}

/// Returns the icon and color for a pass/fail audit result (shield variant).
fn audit_pass_fail_icon(passed: bool) -> (&'static str, Color) {
    if passed {
        ("\u{1f6e1} \u{2713}", Color::Green)
    } else {
        ("\u{1f6e1} \u{2717}", Color::Red)
    }
}

/// Returns the icon and color for a per-provider audit result (check/cross variant).
fn audit_provider_icon(passed: bool) -> (&'static str, Color) {
    if passed {
        ("\u{2713} ", Color::Green)
    } else {
        ("\u{2717} ", Color::Red)
    }
}

/// Left pane: filterable list of results.
fn draw_list(frame: &mut Frame, area: Rect, app: &mut App<'_>) {
    let items: Vec<ListItem<'_>> = app
        .filtered
        .iter()
        .map(|&idx| {
            let item = &app.items[idx];
            let stars_text = item
                .stars
                .map(|s| format!("  \u{2605}{s}"))
                .unwrap_or_default();

            let mut spans = vec![
                Span::styled(&item.name, Style::default().add_modifier(Modifier::BOLD)),
                Span::styled(stars_text, Style::default().fg(Color::Yellow)),
            ];
            spans.extend(build_list_item_audit_spans(item, &app.audit_cache));
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                item.registry.as_str(),
                Style::default().fg(registry_color(item.registry)),
            ));

            Line::from(spans).into()
        })
        .collect();

    let block = Block::default().borders(Borders::ALL).title(" Results ");

    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");

    frame.render_stateful_widget(list, area, &mut app.list_state);
}

/// Right pane: detail preview for the highlighted result.
fn draw_preview(frame: &mut Frame, area: Rect, app: &App<'_>) {
    let block = Block::default().borders(Borders::ALL).title(" Preview ");

    let content = match app.selected() {
        Some(item) => {
            let audit_state = app.audit_cache.get(&item.url);
            let skill_preview = app.skill_preview_cache.get(&item.url);
            build_preview_lines(item, audit_state, skill_preview)
        }
        None => vec![Line::from("No results match the filter.")],
    };

    let para = Paragraph::new(content)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(para, area);
}

/// Build the spans for a single audit entry in the preview pane.
fn build_single_audit_spans(
    audit: &SecurityAudit,
    label_style: Style,
    is_first: bool,
) -> Vec<Span<'_>> {
    let mut spans = Vec::new();
    if !is_first {
        spans.push(Span::styled(" | ", label_style));
    }
    let (icon, color) = audit_provider_icon(audit.passed);
    spans.push(Span::styled(
        format!("{icon}{}", audit.provider),
        Style::default().fg(color),
    ));
    spans
}

/// Render the security audit section for the preview.
fn build_audit_lines(registry: RegistryId, audit_state: Option<&AuditState>) -> Vec<Line<'_>> {
    if !registry.has_security_audits() {
        return Vec::new();
    }
    let label_style = Style::default().fg(Color::DarkGray);
    match audit_state {
        Some(AuditState::Loaded(audits)) if !audits.is_empty() => {
            let mut spans = vec![Span::styled("Audits:         ", label_style)];
            for (i, audit) in audits.iter().enumerate() {
                spans.extend(build_single_audit_spans(audit, label_style, i == 0));
            }
            vec![Line::from(spans)]
        }
        Some(AuditState::Loading) => {
            vec![Line::from(vec![
                Span::styled("Audits:         ", label_style),
                Span::styled("loading...", Style::default().fg(Color::DarkGray)),
            ])]
        }
        Some(AuditState::Failed) => {
            vec![Line::from(vec![
                Span::styled("Audits:         ", label_style),
                Span::styled("fetch failed", Style::default().fg(Color::Red)),
            ])]
        }
        _ => Vec::new(),
    }
}

/// Render the description section for the preview.
fn build_description_lines<'a>(description: Option<&'a str>) -> Vec<Line<'a>> {
    let mut lines: Vec<Line<'a>> = vec![Line::from("")];
    if let Some(desc) = description {
        lines.push(Line::from(Span::styled(
            "Description:",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(""));
        // Show the full, untruncated description.
        for line in desc.lines() {
            lines.push(Line::from(line.to_string()));
        }
    } else {
        lines.push(Line::from(Span::styled(
            "No description available.",
            Style::default().fg(Color::DarkGray),
        )));
    }
    lines
}

/// Render the SKILL.md preview section for the preview pane.
fn build_skill_preview_section(state: Option<&SkillPreviewState>) -> Vec<Line<'static>> {
    let header_style = Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::BOLD);
    match state {
        Some(SkillPreviewState::Loaded(content)) => {
            let mut lines: Vec<Line<'static>> = vec![
                Line::from(""),
                Line::from(Span::styled(
                    PREVIEW_HR,
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(""),
                Line::from(Span::styled("SKILL.md:", header_style)),
                Line::from(""),
            ];
            lines.extend(build_skill_content_lines(content));
            lines
        }
        Some(SkillPreviewState::Loading) => vec![
            Line::from(""),
            Line::from(Span::styled(
                PREVIEW_HR,
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "Loading SKILL.md...",
                Style::default().fg(Color::Yellow),
            )),
        ],
        Some(SkillPreviewState::Failed) => vec![
            Line::from(""),
            Line::from(Span::styled(
                PREVIEW_HR,
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "Could not fetch SKILL.md",
                Style::default().fg(Color::Red),
            )),
        ],
        // NotAvailable or None: show nothing.
        Some(SkillPreviewState::NotAvailable) | None => Vec::new(),
    }
}

/// Build the preview text for a single search result.
fn build_preview_lines<'a>(
    item: &'a SearchResult,
    audit_state: Option<&'a AuditState>,
    skill_preview: Option<&'a SkillPreviewState>,
) -> Vec<Line<'a>> {
    let mut lines: Vec<Line<'_>> = Vec::with_capacity(16);

    // Name
    lines.push(Line::from(Span::styled(
        item.name.as_str(),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));

    // Registry
    lines.push(Line::from(vec![
        Span::styled("Registry:       ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            item.registry.as_str(),
            Style::default().fg(registry_color(item.registry)),
        ),
    ]));

    // Owner
    if !item.owner.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("Owner:          ", Style::default().fg(Color::DarkGray)),
            Span::raw(item.owner.as_str()),
        ]));
    }

    // Stars
    if let Some(stars) = item.stars {
        lines.push(Line::from(vec![
            Span::styled("Stars:          ", Style::default().fg(Color::DarkGray)),
            Span::styled(format!("{stars}"), Style::default().fg(Color::Yellow)),
        ]));
    }

    // Security score
    if let Some(score) = item.security_score {
        lines.push(Line::from(vec![
            Span::styled("Security Score: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{score}/100"),
                Style::default().fg(score_color(score)),
            ),
        ]));
    }

    // Source repo
    if let Some(repo) = &item.source_repo {
        lines.push(Line::from(vec![
            Span::styled("Source:         ", Style::default().fg(Color::DarkGray)),
            Span::raw(repo.as_str()),
        ]));
    }

    // URL
    lines.push(Line::from(vec![
        Span::styled("URL:            ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            item.url.as_str(),
            Style::default()
                .fg(Color::Blue)
                .add_modifier(Modifier::UNDERLINED),
        ),
    ]));

    // Security audit results
    lines.extend(build_audit_lines(item.registry, audit_state));

    // Description
    lines.extend(build_description_lines(item.description.as_deref()));

    // SKILL.md preview (below description)
    lines.extend(build_skill_preview_section(skill_preview));

    lines
}

/// Fetch a skills.sh skill page and extract security audit results.
///
/// Uses `ureq` directly rather than the `HttpClient` trait because this
/// runs in a fire-and-forget background thread and scrapes HTML — a
/// concern orthogonal to the JSON API client abstraction.
fn fetch_skillssh_audits(url: &str) -> Result<Vec<SecurityAudit>, String> {
    let resp = ureq::get(url)
        .call()
        .map_err(|e| format!("fetch failed: {e}"))?;
    let body = resp
        .into_body()
        .read_to_string()
        .map_err(|e| format!("read failed: {e}"))?;
    Ok(parse_skillssh_audits(&body))
}

/// Known security audit providers on skills.sh.
///
/// Each tuple is `(display_name, url_slug)` used to locate the
/// provider's pass/fail badge in the skill page HTML.
const AUDIT_PROVIDERS: &[(&str, &str)] = &[
    ("Agent Trust Hub", "agent-trust-hub"),
    ("Socket", "socket"),
    ("Snyk", "snyk"),
];

/// Parse security audit pass/fail results from skills.sh HTML.
///
/// Looks for the pattern:
///   `security/{slug}">...<span>Provider Name</span>...<span>Pass|Fail</span>`
fn parse_skillssh_audits(html: &str) -> Vec<SecurityAudit> {
    let mut audits = Vec::new();
    for &(provider, slug) in AUDIT_PROVIDERS {
        let marker = format!("security/{slug}\">");
        if let Some(pos) = html.find(&marker) {
            // Look for the first occurrence of Pass or Fail within the next ~500 chars.
            let window = &html[pos..std::cmp::min(pos + 500, html.len())];
            let pass_pos = window.find(">Pass<");
            let fail_pos = window.find(">Fail<");
            let passed = match (pass_pos, fail_pos) {
                (Some(p), Some(f)) => p < f,
                (Some(_), None) => true,
                (None, Some(_)) => false,
                (None, None) => continue,
            };
            audits.push(SecurityAudit {
                provider: provider.to_string(),
                passed,
            });
        }
    }
    audits
}

/// Map a registry to its display color.
fn registry_color(registry: RegistryId) -> Color {
    match registry {
        RegistryId::AgentskillSh => Color::Magenta,
        RegistryId::SkillsSh => Color::Cyan,
        RegistryId::SkillhubClub => Color::Green,
    }
}

/// Map a security score (0-100) to a traffic-light color.
fn score_color(score: u8) -> Color {
    match score {
        80..=100 => Color::Green,
        50..=79 => Color::Yellow,
        _ => Color::Red,
    }
}

// ===========================================================================
// Terminal lifecycle
// ===========================================================================

/// Resolve the confirmed selection index from the app state.
fn resolve_selection(app: &App<'_>) -> Option<usize> {
    app.list_state
        .selected()
        .and_then(|i| app.filtered.get(i).copied())
}

/// Handle a single key event from the terminal.
fn process_terminal_event(app: &mut App<'_>) -> Result<(), std::io::Error> {
    if let Event::Key(key) = event::read()? {
        handle_key(app, key);
    }
    Ok(())
}

/// Run the TUI event loop. Returns the selected SearchResult index (into the
/// original `items` slice), or `None` if the user cancelled.
pub fn run_tui(items: &[SearchResult], total: usize) -> Result<Option<usize>, std::io::Error> {
    // Set up terminal
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = std::io::stderr();
    crossterm::execute!(
        stdout,
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::EnableMouseCapture
    )?;

    // Install panic hook so the terminal is restored if we panic mid-TUI.
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

    let mut app = App::new(items, total);

    // Event loop — polls with timeout so background fetches can land.
    let result = loop {
        app.maybe_fetch_audits();
        app.poll_audits();
        app.maybe_fetch_skill_preview();
        app.poll_skill_previews();

        terminal.draw(|f| draw(f, &mut app))?;

        if event::poll(Duration::from_millis(100))? {
            process_terminal_event(&mut app)?;
        }

        if app.should_quit() {
            let selection = app.confirmed.then(|| resolve_selection(&app)).flatten();
            break selection;
        }
    };

    // Restore terminal and remove TUI panic hook.
    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::event::DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    let _ = std::panic::take_hook(); // drop TUI hook, restores default

    Ok(result)
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_items() -> Vec<SearchResult> {
        vec![
            SearchResult {
                name: "code-reviewer".to_string(),
                owner: "alice".to_string(),
                description: Some("Review code changes automatically".to_string()),
                security_score: Some(92),
                stars: Some(150),
                url: "https://agentskill.sh/@alice/code-reviewer".to_string(),
                registry: RegistryId::AgentskillSh,
                source_repo: Some("alice/code-reviewer".to_string()),
                source_path: None,
            },
            SearchResult {
                name: "docker-helper".to_string(),
                owner: "dockerfan".to_string(),
                description: None,
                security_score: None,
                stars: Some(500),
                url: "https://skills.sh/dockerfan/docker-helper/docker-helper".to_string(),
                registry: RegistryId::SkillsSh,
                source_repo: Some("dockerfan/docker-helper".to_string()),
                source_path: None,
            },
            SearchResult {
                name: "testing-pro".to_string(),
                owner: "testmaster".to_string(),
                description: Some("Advanced testing utilities for CI/CD".to_string()),
                security_score: Some(88),
                stars: Some(75),
                url: "https://www.skillhub.club/skills/testing-pro".to_string(),
                registry: RegistryId::SkillhubClub,
                source_repo: None,
                source_path: None,
            },
        ]
    }

    // -- App state tests -------------------------------------------------------

    #[test]
    fn app_new_selects_first_item() {
        let items = sample_items();
        let app = App::new(&items, 3);
        assert_eq!(app.list_state.selected(), Some(0));
        assert_eq!(app.filtered.len(), 3);
    }

    #[test]
    fn app_new_empty_items() {
        let items: Vec<SearchResult> = vec![];
        let app = App::new(&items, 0);
        assert_eq!(app.list_state.selected(), None);
        assert!(app.filtered.is_empty());
    }

    #[test]
    fn app_selected_returns_correct_item() {
        let items = sample_items();
        let app = App::new(&items, 3);
        let selected = app.selected().unwrap();
        assert_eq!(selected.name, "code-reviewer");
    }

    // -- Filter tests ----------------------------------------------------------

    #[test]
    fn filter_narrows_results() {
        let items = sample_items();
        let mut app = App::new(&items, 3);
        app.filter = "docker".to_string();
        app.refilter();
        assert_eq!(app.filtered.len(), 1);
        assert_eq!(app.selected().unwrap().name, "docker-helper");
    }

    #[test]
    fn filter_no_match_clears_selection() {
        let items = sample_items();
        let mut app = App::new(&items, 3);
        app.filter = "zzz_nonexistent_zzz".to_string();
        app.refilter();
        assert!(app.filtered.is_empty());
        assert!(app.selected().is_none());
    }

    #[test]
    fn filter_by_registry_name() {
        let items = sample_items();
        let mut app = App::new(&items, 3);
        app.filter = "skillhub".to_string();
        app.refilter();
        assert_eq!(app.filtered.len(), 1);
        assert_eq!(app.selected().unwrap().name, "testing-pro");
    }

    #[test]
    fn filter_multi_word() {
        let items = sample_items();
        let mut app = App::new(&items, 3);
        app.filter = "alice review".to_string();
        app.refilter();
        assert_eq!(app.filtered.len(), 1);
        assert_eq!(app.selected().unwrap().name, "code-reviewer");
    }

    #[test]
    fn filter_case_insensitive() {
        let items = sample_items();
        let mut app = App::new(&items, 3);
        app.filter = "DOCKER".to_string();
        app.refilter();
        assert_eq!(app.filtered.len(), 1);
    }

    #[test]
    fn clear_filter_restores_all() {
        let items = sample_items();
        let mut app = App::new(&items, 3);
        app.filter = "docker".to_string();
        app.refilter();
        assert_eq!(app.filtered.len(), 1);

        app.filter.clear();
        app.refilter();
        assert_eq!(app.filtered.len(), 3);
    }

    // -- Navigation tests -------------------------------------------------------

    #[test]
    fn move_selection_down() {
        let items = sample_items();
        let mut app = App::new(&items, 3);
        move_selection(&mut app, 1);
        assert_eq!(app.list_state.selected(), Some(1));
    }

    #[test]
    fn move_selection_wraps_bottom_to_top() {
        let items = sample_items();
        let mut app = App::new(&items, 3);
        app.list_state.select(Some(2));
        move_selection(&mut app, 1);
        assert_eq!(app.list_state.selected(), Some(0));
    }

    #[test]
    fn move_selection_wraps_top_to_bottom() {
        let items = sample_items();
        let mut app = App::new(&items, 3);
        move_selection(&mut app, -1);
        assert_eq!(app.list_state.selected(), Some(2));
    }

    #[test]
    fn move_selection_empty_list() {
        let items: Vec<SearchResult> = vec![];
        let mut app = App::new(&items, 0);
        move_selection(&mut app, 1); // should not panic
        assert!(app.list_state.selected().is_none());
    }

    // -- Key handling tests ----------------------------------------------------

    #[test]
    fn esc_cancels() {
        let items = sample_items();
        let mut app = App::new(&items, 3);
        let key = event::KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        handle_key(&mut app, key);
        assert!(app.cancelled);
        assert!(app.should_quit());
    }

    #[test]
    fn enter_confirms_selection() {
        let items = sample_items();
        let mut app = App::new(&items, 3);
        let key = event::KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        handle_key(&mut app, key);
        assert!(app.confirmed);
        assert!(app.should_quit());
    }

    #[test]
    fn enter_on_empty_does_not_confirm() {
        let items: Vec<SearchResult> = vec![];
        let mut app = App::new(&items, 0);
        let key = event::KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        handle_key(&mut app, key);
        assert!(!app.confirmed);
        assert!(!app.should_quit());
    }

    #[test]
    fn q_cancels_when_filter_empty() {
        let items = sample_items();
        let mut app = App::new(&items, 3);
        let key = event::KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        handle_key(&mut app, key);
        assert!(app.cancelled);
    }

    #[test]
    fn q_types_into_filter_when_filter_has_text() {
        let items = sample_items();
        let mut app = App::new(&items, 3);
        app.filter = "doc".to_string();
        let key = event::KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        handle_key(&mut app, key);
        assert!(!app.cancelled);
        assert_eq!(app.filter, "docq");
    }

    #[test]
    fn ctrl_c_cancels() {
        let items = sample_items();
        let mut app = App::new(&items, 3);
        let key = event::KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        handle_key(&mut app, key);
        assert!(app.cancelled);
    }

    #[test]
    fn typing_updates_filter() {
        let items = sample_items();
        let mut app = App::new(&items, 3);
        for c in "doc".chars() {
            let key = event::KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
            handle_key(&mut app, key);
        }
        assert_eq!(app.filter, "doc");
        assert_eq!(app.filtered.len(), 1); // only docker-helper
    }

    #[test]
    fn backspace_removes_from_filter() {
        let items = sample_items();
        let mut app = App::new(&items, 3);
        app.filter = "doc".to_string();
        app.refilter();
        let key = event::KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE);
        handle_key(&mut app, key);
        assert_eq!(app.filter, "do");
    }

    #[test]
    fn j_k_navigate_when_filter_empty() {
        let items = sample_items();
        let mut app = App::new(&items, 3);
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
        let mut app = App::new(&items, 3);
        app.list_state.select(Some(2));
        let key = event::KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE);
        handle_key(&mut app, key);
        assert_eq!(app.list_state.selected(), Some(0));
    }

    #[test]
    fn shift_g_jumps_to_bottom() {
        let items = sample_items();
        let mut app = App::new(&items, 3);
        let key = event::KeyEvent::new(KeyCode::Char('G'), KeyModifiers::NONE);
        handle_key(&mut app, key);
        assert_eq!(app.list_state.selected(), Some(2));
    }

    // -- Registry tag tests ----------------------------------------------------

    #[test]
    fn registry_color_mapping() {
        assert_eq!(registry_color(RegistryId::AgentskillSh), Color::Magenta);
        assert_eq!(registry_color(RegistryId::SkillsSh), Color::Cyan);
        assert_eq!(registry_color(RegistryId::SkillhubClub), Color::Green);
    }

    #[test]
    fn score_color_ranges() {
        assert_eq!(score_color(100), Color::Green);
        assert_eq!(score_color(80), Color::Green);
        assert_eq!(score_color(79), Color::Yellow);
        assert_eq!(score_color(50), Color::Yellow);
        assert_eq!(score_color(49), Color::Red);
        assert_eq!(score_color(0), Color::Red);
    }

    // -- Preview tests ---------------------------------------------------------

    #[test]
    fn build_preview_includes_all_fields() {
        let item = &sample_items()[0]; // code-reviewer — has all fields
        let lines = build_preview_lines(item, Option::<&AuditState>::None, None);
        let text: String = lines
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("code-reviewer"), "missing name");
        assert!(text.contains("alice"), "missing owner");
        assert!(text.contains("92/100"), "missing score");
        assert!(text.contains("150"), "missing stars");
        assert!(text.contains("agentskill.sh"), "missing registry");
        assert!(text.contains("alice/code-reviewer"), "missing source_repo");
        assert!(text.contains("Review code changes"), "missing description");
    }

    #[test]
    fn build_preview_handles_missing_fields() {
        let item = &sample_items()[1]; // docker-helper — no description, no score
        let lines = build_preview_lines(item, Option::<&AuditState>::None, None);
        let text: String = lines
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("docker-helper"));
        assert!(text.contains("No description"));
        assert!(!text.contains("Security:"));
    }

    // -- Audit parsing tests ---------------------------------------------------

    #[test]
    fn parse_skillssh_audits_extracts_pass_fail() {
        let html = r#"
            <a href="/owner/repo/skill/security/agent-trust-hub"><div>
            <span>Gen Agent Trust Hub</span><span>Fail</span></div></a>
            <a href="/owner/repo/skill/security/socket"><div>
            <span>Socket</span><span>Pass</span></div></a>
            <a href="/owner/repo/skill/security/snyk"><div>
            <span>Snyk</span><span>Pass</span></div></a>
        "#;
        let audits = parse_skillssh_audits(html);
        assert_eq!(audits.len(), 3);
        assert_eq!(audits[0].provider, "Agent Trust Hub");
        assert!(!audits[0].passed);
        assert_eq!(audits[1].provider, "Socket");
        assert!(audits[1].passed);
        assert_eq!(audits[2].provider, "Snyk");
        assert!(audits[2].passed);
    }

    #[test]
    fn parse_skillssh_audits_all_pass() {
        let html = r#"
            security/agent-trust-hub"><span>X</span><span>Pass</span>
            security/socket"><span>X</span><span>Pass</span>
            security/snyk"><span>X</span><span>Pass</span>
        "#;
        let audits = parse_skillssh_audits(html);
        assert_eq!(audits.len(), 3);
        assert!(audits.iter().all(|a| a.passed));
    }

    #[test]
    fn parse_skillssh_audits_no_matches_returns_empty() {
        let audits = parse_skillssh_audits("<html><body>no audit data</body></html>");
        assert!(audits.is_empty());
    }

    #[test]
    fn build_preview_shows_loaded_audits() {
        let item = &sample_items()[1]; // docker-helper — skills.sh
        let audits = AuditState::Loaded(vec![
            SecurityAudit {
                provider: "Agent Trust Hub".into(),
                passed: true,
            },
            SecurityAudit {
                provider: "Socket".into(),
                passed: true,
            },
            SecurityAudit {
                provider: "Snyk".into(),
                passed: false,
            },
        ]);
        let lines = build_preview_lines(item, Some(&audits), None);
        let text: String = lines
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Agent Trust Hub"), "missing provider");
        assert!(text.contains("Snyk"), "missing provider");
    }

    #[test]
    fn build_preview_shows_loading_audits() {
        let item = &sample_items()[1]; // docker-helper — skills.sh
        let lines = build_preview_lines(item, Some(&AuditState::Loading), None);
        let text: String = lines
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("loading"), "should show loading state");
    }

    // -- SkillPreviewState tests -----------------------------------------------

    #[test]
    fn skill_preview_spawns_fetch_for_all_registries() {
        let items = sample_items();
        // testing-pro (index 2) is skillhub.club — no source_repo.
        // With Strategy pattern dispatch, all items go through the thread.
        let mut app = App::new(&items, 3);
        app.list_state.select(Some(2));

        app.maybe_fetch_skill_preview();

        let url = &items[2].url;
        assert!(
            matches!(
                app.skill_preview_cache.get(url),
                Some(SkillPreviewState::Loading)
            ),
            "all registries should spawn a fetch thread"
        );
    }

    #[test]
    fn skill_preview_loading_set_for_item_with_source_repo() {
        let items = sample_items();
        // code-reviewer (index 0) has source_repo = Some("alice/code-reviewer").
        let mut app = App::new(&items, 3);
        // index 0 is already selected by default.

        app.maybe_fetch_skill_preview();

        let url = &items[0].url;
        // Should be Loading (thread spawned) or already Loaded/Failed if thread was fast.
        assert!(
            app.skill_preview_cache.contains_key(url),
            "cache should have an entry for the highlighted item"
        );
    }

    #[test]
    fn skill_preview_not_cached_twice() {
        let items = sample_items();
        let mut app = App::new(&items, 3);

        // First call inserts Loading.
        app.maybe_fetch_skill_preview();
        let url = &items[0].url;
        assert!(app.skill_preview_cache.contains_key(url));

        // Manually replace with NotAvailable to confirm second call is a no-op.
        app.skill_preview_cache
            .insert(url.clone(), SkillPreviewState::NotAvailable);
        app.maybe_fetch_skill_preview();

        // Still NotAvailable — second call did not overwrite.
        match app.skill_preview_cache.get(url) {
            Some(SkillPreviewState::NotAvailable) => {}
            other => panic!("expected NotAvailable after second call, got {other:?}"),
        }
    }

    #[test]
    fn build_preview_includes_skill_content() {
        let item = &sample_items()[0]; // code-reviewer
        let content = PreviewContent {
            name: Some("Code Reviewer".to_string()),
            description: Some("Automated code review skill".to_string()),
            risk: Some("low".to_string()),
            source: None,
            body_excerpt: Some("## Use this skill when\n- You want automated review".to_string()),
        };
        let skill_state = SkillPreviewState::Loaded(content);
        let lines = build_preview_lines(item, Option::<&AuditState>::None, Some(&skill_state));
        let text: String = lines
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("SKILL.md:"), "missing SKILL.md header");
        assert!(text.contains("Code Reviewer"), "missing skill name");
        assert!(
            text.contains("Automated code review skill"),
            "missing skill description"
        );
        assert!(text.contains("low"), "missing risk");
        assert!(text.contains("Use this skill when"), "missing body excerpt");
    }

    #[test]
    fn build_preview_shows_loading_skill_preview() {
        let item = &sample_items()[0];
        let lines = build_preview_lines(
            item,
            Option::<&AuditState>::None,
            Some(&SkillPreviewState::Loading),
        );
        let text: String = lines
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Loading SKILL.md"), "missing loading message");
    }

    #[test]
    fn build_preview_shows_failed_skill_preview() {
        let item = &sample_items()[0];
        let lines = build_preview_lines(
            item,
            Option::<&AuditState>::None,
            Some(&SkillPreviewState::Failed),
        );
        let text: String = lines
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            text.contains("Could not fetch SKILL.md"),
            "missing failure message"
        );
    }

    #[test]
    fn build_preview_no_skill_section_when_not_available() {
        let item = &sample_items()[2]; // testing-pro — no source_repo
        let lines = build_preview_lines(
            item,
            Option::<&AuditState>::None,
            Some(&SkillPreviewState::NotAvailable),
        );
        let text: String = lines
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !text.contains("SKILL.md:"),
            "should not show SKILL.md section for NotAvailable"
        );
    }

    #[test]
    fn poll_skill_previews_drains_channel() {
        let items = sample_items();
        let mut app = App::new(&items, 3);
        let url = items[0].url.clone();

        // Send a result directly via the tx.
        app.skill_preview_tx
            .send((url.clone(), SkillPreviewState::NotAvailable))
            .unwrap();

        app.poll_skill_previews();

        match app.skill_preview_cache.get(&url) {
            Some(SkillPreviewState::NotAvailable) => {}
            other => panic!("expected NotAvailable after poll, got {other:?}"),
        }
    }
}
