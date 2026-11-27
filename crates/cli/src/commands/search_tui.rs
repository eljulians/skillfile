//! TUI for `skillfile search` — split-pane interactive result browser.
//!
//! Architecture: Elm-style Model-View-Update.
//!
//! - **Model** ([`App`]): search results, which only grow (new pages are
//!   appended, existing rows never move), plus mutable UI state (filter text,
//!   selected index, scroll offset).
//! - **View** ([`draw`]): renders the list pane (left) and detail preview (right).
//! - **Update** ([`handle_key`]): maps key events to state transitions.
//!
//! More pages load only on user intent: moving towards the end of the list
//! sets a load request that the event loop hands to the [`Pager`]. A narrow
//! filter therefore can't crawl every page by itself.
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
use skillfile_sources::registry::{RegistryId, SearchResult, SearchSession};

use super::search_pager::{Pager, PagerStatus};
use super::skill_preview::{
    build_skill_content_lines, parse_skill_frontmatter, PreviewContent, PREVIEW_HR,
};

/// Request the next page once the selection is this close to the last row.
const LOAD_AHEAD: usize = 10;

// ===========================================================================
// Model
// ===========================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecurityAudit {
    pub provider: String,
    pub passed: bool,
}

#[derive(Debug, Clone)]
pub enum AuditState {
    Loading,
    Loaded(Vec<SecurityAudit>),
    Failed,
}

#[derive(Debug, Clone)]
pub enum SkillPreviewState {
    Loading,
    Loaded(PreviewContent),
    /// No GitHub coordinates available — cannot fetch SKILL.md preview.
    NotAvailable,
    Failed,
}

pub struct App {
    /// All loaded search results. Append-only, so indices stay valid.
    items: Vec<SearchResult>,
    /// Indices into `items` that match the current filter.
    filtered: Vec<usize>,
    filter: String,
    /// ListState tracks the currently highlighted row.
    list_state: ListState,
    /// Whether the user confirmed a selection (Enter).
    confirmed: bool,
    /// Whether the user cancelled (Esc / q).
    cancelled: bool,
    pager: Pager,
    /// Set by navigation near the end of the list; consumed by the event loop.
    load_requested: bool,
    /// Rows of `filtered` visible after the last draw: `(offset, height)`.
    viewport: (usize, usize),
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

impl App {
    pub fn new(items: Vec<SearchResult>, pager: Pager) -> Self {
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
            pager,
            load_requested: false,
            viewport: (0, 0),
            audit_cache: HashMap::new(),
            audit_rx,
            audit_tx,
            skill_preview_cache: HashMap::new(),
            skill_preview_rx,
            skill_preview_tx,
        }
    }

    pub fn selected(&self) -> Option<&SearchResult> {
        let idx = self.list_state.selected()?;
        let original_idx = *self.filtered.get(idx)?;
        self.items.get(original_idx)
    }

    /// Append a newly loaded page. Rows already shown keep their position
    /// and the selection doesn't move.
    fn append_batch(&mut self, batch: Vec<SearchResult>) {
        let query = self.filter.to_lowercase();
        let start = self.items.len();
        self.items.extend(batch);
        let items = &self.items;
        self.filtered
            .extend((start..items.len()).filter(|&i| item_matches_query(&items[i], &query)));
        if self.list_state.selected().is_none() && !self.filtered.is_empty() {
            self.list_state.select(Some(0));
        }
    }

    /// Request more results if the selection is within [`LOAD_AHEAD`] rows
    /// of the end.
    fn request_more_if_near_end(&mut self) {
        let Some(selected) = self.list_state.selected() else {
            return;
        };
        if selected + LOAD_AHEAD >= self.filtered.len() {
            self.load_requested = true;
        }
    }

    /// Hand pending load requests to the pager and append any finished page.
    fn pump_pages(&mut self) {
        if std::mem::take(&mut self.load_requested) {
            self.pager.request();
        }
        if let Some(batch) = self.pager.poll() {
            self.append_batch(batch);
        }
    }

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

    fn should_quit(&self) -> bool {
        self.confirmed || self.cancelled
    }

    /// URLs of on-screen rows that support audits and aren't cached yet.
    ///
    /// Limited to the viewport: with hundreds of loaded results, fetching
    /// audits for every filtered row would start hundreds of requests at once.
    fn audit_urls_to_fetch(&self) -> Vec<String> {
        let (offset, height) = self.viewport;
        self.filtered
            .iter()
            .skip(offset)
            .take(height)
            .map(|&idx| &self.items[idx])
            .filter(|item| {
                item.registry.has_security_audits() && !self.audit_cache.contains_key(&item.url)
            })
            .map(|item| item.url.clone())
            .collect()
    }

    /// Spawn background audit fetches for visible rows.
    fn maybe_fetch_audits(&mut self) {
        for url in self.audit_urls_to_fetch() {
            self.audit_cache.insert(url.clone(), AuditState::Loading);
            spawn_audit_fetch(url, self.audit_tx.clone());
        }
    }

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
        let Some(item) = self
            .selected()
            .filter(|item| !self.skill_preview_cache.contains_key(&item.url))
            .cloned()
        else {
            return;
        };
        let url = item.url.clone();
        self.skill_preview_cache
            .insert(url.clone(), SkillPreviewState::Loading);
        let tx = self.skill_preview_tx.clone();
        std::thread::spawn(move || {
            let state = fetch_skill_preview_for_item(&item);
            let _ = tx.send((url, state));
        });
    }

    fn poll_skill_previews(&mut self) {
        while let Ok((url, state)) = self.skill_preview_rx.try_recv() {
            self.skill_preview_cache.insert(url, state);
        }
    }
}

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

fn fetch_skill_preview_for_item(item: &SearchResult) -> SkillPreviewState {
    match skillfile_sources::registry::fetch_skill_content_for(item) {
        Some(content) => SkillPreviewState::Loaded(parse_skill_frontmatter(&content)),
        None => SkillPreviewState::NotAvailable,
    }
}

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

pub fn handle_key(app: &mut App, key: event::KeyEvent) {
    match key.code {
        KeyCode::Esc => app.cancelled = true,
        KeyCode::Char('q') if app.filter.is_empty() => app.cancelled = true,
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.cancelled = true;
        }
        KeyCode::Enter => handle_key_enter(app),
        KeyCode::PageDown => page_down(app),
        KeyCode::PageUp => page_up(app),
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

fn handle_key_enter(app: &mut App) {
    if app.selected().is_some() {
        app.confirmed = true;
    }
}

fn handle_key_jump_top(app: &mut App) {
    if !app.filtered.is_empty() {
        app.list_state.select(Some(0));
    }
}

fn handle_key_jump_bottom(app: &mut App) {
    if !app.filtered.is_empty() {
        app.list_state.select(Some(app.filtered.len() - 1));
        app.request_more_if_near_end();
    }
}

/// Move selection by `delta` rows (negative = up, positive = down).
///
/// Wraps around at either end once every result is loaded. While more
/// results may follow, moving down from the last row stays put and requests
/// the next page instead.
fn move_selection(app: &mut App, delta: i32) {
    let len = app.filtered.len();
    if len == 0 {
        return;
    }
    let current = app.list_state.selected().unwrap_or(0);
    let more_coming = app.pager.status() != PagerStatus::AllLoaded;
    if delta > 0 && current + 1 == len && more_coming {
        app.load_requested = true;
        return;
    }
    #[allow(clippy::cast_possible_wrap)] // list length is always small
    let next = (current as isize + delta as isize).rem_euclid(len as isize) as usize;
    app.list_state.select(Some(next));
    if delta > 0 {
        app.request_more_if_near_end();
    }
}

/// Rows per page: the list height from the last draw, at least 1.
fn page_size(app: &App) -> usize {
    app.viewport.1.max(1)
}

fn page_down(app: &mut App) {
    let Some(last) = app.filtered.len().checked_sub(1) else {
        return;
    };
    let current = app.list_state.selected().unwrap_or(0);
    app.list_state
        .select(Some((current + page_size(app)).min(last)));
    app.request_more_if_near_end();
}

fn page_up(app: &mut App) {
    if app.filtered.is_empty() {
        return;
    }
    let current = app.list_state.selected().unwrap_or(0);
    app.list_state
        .select(Some(current.saturating_sub(page_size(app))));
}

// ===========================================================================
// View
// ===========================================================================

/// Render the full TUI frame: status bar + split pane (list | preview).
pub fn draw(frame: &mut Frame, app: &mut App) {
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

fn draw_status_bar(frame: &mut Frame, area: Rect, app: &App) {
    let filter_display = if app.filter.is_empty() {
        String::from(" type to filter, Enter to select, Esc to cancel")
    } else {
        format!(" filter: {}_", app.filter)
    };

    let count = format!(
        " {}/{}{} ",
        app.filtered.len(),
        app.items.len(),
        pager_status_text(&app.pager)
    );

    let bar = Line::from(vec![
        Span::styled(filter_display, Style::default().fg(Color::Yellow)),
        Span::raw("  "),
        Span::styled(count, Style::default().fg(Color::DarkGray)),
    ]);
    frame.render_widget(Paragraph::new(bar), area);
}

fn pager_status_text(pager: &Pager) -> String {
    let status = match pager.status() {
        PagerStatus::Loading => " \u{b7} loading more\u{2026}",
        PagerStatus::MoreAvailable => " \u{b7} \u{2193} more",
        PagerStatus::AllLoaded => " \u{b7} all loaded",
    };
    match pager.last_error() {
        Some(error) => format!("{status} \u{b7} {error}"),
        None => status.to_string(),
    }
}

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

fn draw_list(frame: &mut Frame, area: Rect, app: &mut App) {
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
    // Rendering scrolls `list_state` to keep the selection visible.
    let inner_height = usize::from(area.height.saturating_sub(2));
    app.viewport = (app.list_state.offset(), inner_height);
}

fn draw_preview(frame: &mut Frame, area: Rect, app: &App) {
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

fn resolve_selection(app: &App) -> Option<usize> {
    app.list_state
        .selected()
        .and_then(|i| app.filtered.get(i).copied())
}

fn process_terminal_event(app: &mut App) -> Result<(), std::io::Error> {
    if let Event::Key(key) = event::read()? {
        handle_key(app, key);
    }
    Ok(())
}

/// Run the TUI event loop over `initial` results, loading further pages from
/// `session` on demand. Returns the selected result, or `None` if the user
/// cancelled.
pub fn run_tui(
    initial: Vec<SearchResult>,
    session: Option<SearchSession>,
) -> Result<Option<SearchResult>, std::io::Error> {
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

    let mut app = App::new(initial, Pager::new(session));

    // Event loop — polls with timeout so background fetches can land.
    let result = loop {
        app.pump_pages();
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

    Ok(result.map(|idx| app.items.swap_remove(idx)))
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use skillfile_sources::registry::SessionOptions;

    /// App with every result already loaded.
    fn app_with(items: &[SearchResult]) -> App {
        App::new(items.to_vec(), Pager::new(None))
    }

    /// App whose pager reports more results. Nothing is fetched unless the
    /// event loop calls `pump_pages`, so these tests stay offline.
    fn app_with_more(items: &[SearchResult]) -> App {
        let opts = SessionOptions {
            registry: Some(RegistryId::SkillsSh),
            ..SessionOptions::default()
        };
        let pager = Pager::new(Some(SearchSession::new("q", &opts)));
        App::new(items.to_vec(), pager)
    }

    fn numbered_items(n: usize) -> Vec<SearchResult> {
        (0..n)
            .map(|i| SearchResult {
                name: format!("skill-{i}"),
                owner: "owner".to_string(),
                description: None,
                security_score: None,
                stars: None,
                url: format!("https://skills.sh/owner/repo/skill-{i}"),
                registry: RegistryId::SkillsSh,
                source_repo: None,
                source_path: None,
            })
            .collect()
    }

    fn press(app: &mut App, code: KeyCode) {
        handle_key(app, event::KeyEvent::new(code, KeyModifiers::NONE));
    }

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
        let app = app_with(&items);
        assert_eq!(app.list_state.selected(), Some(0));
        assert_eq!(app.filtered.len(), 3);
    }

    #[test]
    fn app_new_empty_items() {
        let items: Vec<SearchResult> = vec![];
        let app = app_with(&items);
        assert_eq!(app.list_state.selected(), None);
        assert_eq!(app.filtered, Vec::<usize>::new());
    }

    #[test]
    fn app_selected_returns_correct_item() {
        let items = sample_items();
        let app = app_with(&items);
        let selected = app.selected().unwrap();
        assert_eq!(selected.name, "code-reviewer");
    }

    // -- Filter tests ----------------------------------------------------------

    #[test]
    fn filter_narrows_results() {
        let items = sample_items();
        let mut app = app_with(&items);
        app.filter = "docker".to_string();
        app.refilter();
        assert_eq!(app.filtered.len(), 1);
        assert_eq!(app.selected().unwrap().name, "docker-helper");
    }

    #[test]
    fn filter_no_match_clears_selection() {
        let items = sample_items();
        let mut app = app_with(&items);
        app.filter = "zzz_nonexistent_zzz".to_string();
        app.refilter();
        assert_eq!(app.filtered, Vec::<usize>::new());
        assert!(app.selected().is_none());
    }

    #[test]
    fn filter_by_registry_name() {
        let items = sample_items();
        let mut app = app_with(&items);
        app.filter = "skillhub".to_string();
        app.refilter();
        assert_eq!(app.filtered.len(), 1);
        assert_eq!(app.selected().unwrap().name, "testing-pro");
    }

    #[test]
    fn filter_multi_word() {
        let items = sample_items();
        let mut app = app_with(&items);
        app.filter = "alice review".to_string();
        app.refilter();
        assert_eq!(app.filtered.len(), 1);
        assert_eq!(app.selected().unwrap().name, "code-reviewer");
    }

    #[test]
    fn filter_case_insensitive() {
        let items = sample_items();
        let mut app = app_with(&items);
        app.filter = "DOCKER".to_string();
        app.refilter();
        assert_eq!(app.filtered.len(), 1);
    }

    #[test]
    fn clear_filter_restores_all() {
        let items = sample_items();
        let mut app = app_with(&items);
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
        let mut app = app_with(&items);
        move_selection(&mut app, 1);
        assert_eq!(app.list_state.selected(), Some(1));
    }

    #[test]
    fn move_selection_wraps_bottom_to_top() {
        let items = sample_items();
        let mut app = app_with(&items);
        app.list_state.select(Some(2));
        move_selection(&mut app, 1);
        assert_eq!(app.list_state.selected(), Some(0));
    }

    #[test]
    fn move_selection_wraps_top_to_bottom() {
        let items = sample_items();
        let mut app = app_with(&items);
        move_selection(&mut app, -1);
        assert_eq!(app.list_state.selected(), Some(2));
    }

    #[test]
    fn move_selection_empty_list() {
        let items: Vec<SearchResult> = vec![];
        let mut app = app_with(&items);
        move_selection(&mut app, 1); // should not panic
        assert!(app.list_state.selected().is_none());
    }

    // -- Lazy loading tests -----------------------------------------------------

    #[test]
    fn down_at_bottom_requests_more_instead_of_wrapping() {
        let items = sample_items();
        let mut app = app_with_more(&items);
        app.list_state.select(Some(2));
        move_selection(&mut app, 1);
        assert_eq!(app.list_state.selected(), Some(2));
        assert!(app.load_requested);
    }

    #[test]
    fn down_while_loading_does_not_wrap() {
        let items = sample_items();
        let mut app = App::new(items.clone(), Pager::loading());
        app.list_state.select(Some(2));
        move_selection(&mut app, 1);
        assert_eq!(app.list_state.selected(), Some(2));
    }

    #[test]
    fn moving_near_the_end_requests_more() {
        // After moving down, the selection (row 1) is LOAD_AHEAD rows from the end.
        let items = numbered_items(LOAD_AHEAD + 1);
        let mut app = app_with_more(&items);
        press(&mut app, KeyCode::Down);
        assert!(app.load_requested);
    }

    #[test]
    fn moving_far_from_the_end_does_not_request_more() {
        let items = numbered_items(LOAD_AHEAD + 20);
        let mut app = app_with_more(&items);
        press(&mut app, KeyCode::Down);
        assert!(!app.load_requested);
    }

    #[test]
    fn moving_up_never_requests_more() {
        let items = sample_items();
        let mut app = app_with_more(&items);
        app.list_state.select(Some(2));
        press(&mut app, KeyCode::Up);
        assert!(!app.load_requested);
    }

    #[test]
    fn jump_to_bottom_requests_more() {
        let items = numbered_items(50);
        let mut app = app_with_more(&items);
        press(&mut app, KeyCode::End);
        assert_eq!(app.list_state.selected(), Some(49));
        assert!(app.load_requested);
    }

    #[test]
    fn pump_pages_consumes_the_request_flag() {
        let items = sample_items();
        let mut app = app_with(&items);
        app.load_requested = true;
        app.pump_pages();
        assert!(!app.load_requested);
    }

    #[test]
    fn append_batch_keeps_selection_and_existing_rows() {
        let items = numbered_items(3);
        let mut app = app_with_more(&items);
        app.list_state.select(Some(1));

        app.append_batch(numbered_items(6).split_off(3));

        assert_eq!(app.items.len(), 6);
        assert_eq!(app.filtered, vec![0, 1, 2, 3, 4, 5]);
        assert_eq!(app.selected().unwrap().name, "skill-1");
    }

    #[test]
    fn append_batch_applies_the_active_filter() {
        let items = sample_items();
        let mut app = app_with_more(&items);
        app.filter = "docker".to_string();
        app.refilter();

        let mut more = numbered_items(2);
        more[1].name = "docker-compose".to_string();
        app.append_batch(more);

        let names: Vec<&str> = app
            .filtered
            .iter()
            .map(|&i| app.items[i].name.as_str())
            .collect();
        assert_eq!(names, ["docker-helper", "docker-compose"]);
        assert_eq!(app.selected().unwrap().name, "docker-helper");
    }

    #[test]
    fn append_batch_selects_first_match_when_nothing_was_selected() {
        let items = sample_items();
        let mut app = app_with_more(&items);
        app.filter = "skill-1".to_string();
        app.refilter();
        assert!(app.selected().is_none());

        app.append_batch(numbered_items(2));

        assert_eq!(app.selected().unwrap().name, "skill-1");
    }

    #[test]
    fn page_down_moves_by_viewport_and_clamps() {
        let items = numbered_items(30);
        let mut app = app_with(&items);
        app.viewport = (0, 10);
        press(&mut app, KeyCode::PageDown);
        assert_eq!(app.list_state.selected(), Some(10));
        press(&mut app, KeyCode::PageDown);
        press(&mut app, KeyCode::PageDown);
        assert_eq!(app.list_state.selected(), Some(29));
    }

    #[test]
    fn page_up_moves_by_viewport_and_clamps() {
        let items = numbered_items(30);
        let mut app = app_with(&items);
        app.viewport = (0, 10);
        app.list_state.select(Some(15));
        press(&mut app, KeyCode::PageUp);
        assert_eq!(app.list_state.selected(), Some(5));
        press(&mut app, KeyCode::PageUp);
        assert_eq!(app.list_state.selected(), Some(0));
    }

    #[test]
    fn page_down_near_end_requests_more() {
        let items = numbered_items(30);
        let mut app = app_with_more(&items);
        app.viewport = (0, 25);
        press(&mut app, KeyCode::PageDown);
        assert!(app.load_requested);
    }

    #[test]
    fn audits_are_fetched_only_for_visible_rows() {
        let items = numbered_items(100);
        let mut app = app_with(&items);
        app.viewport = (40, 5);
        let urls = app.audit_urls_to_fetch();
        let expected: Vec<String> = items[40..45].iter().map(|i| i.url.clone()).collect();
        assert_eq!(urls, expected);
    }

    #[test]
    fn audits_are_not_fetched_before_first_draw() {
        let items = numbered_items(10);
        let app = app_with(&items);
        assert_eq!(app.audit_urls_to_fetch(), Vec::<String>::new());
    }

    #[test]
    fn draw_records_viewport() {
        let items = numbered_items(100);
        let mut app = app_with(&items);
        render_to_text(|f| draw(f, &mut app));
        assert_eq!(app.viewport.0, 0);
        assert!(app.viewport.1 > 0);
    }

    // -- Key handling tests ----------------------------------------------------

    #[test]
    fn esc_cancels() {
        let items = sample_items();
        let mut app = app_with(&items);
        let key = event::KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        handle_key(&mut app, key);
        assert!(app.cancelled);
        assert!(app.should_quit());
    }

    #[test]
    fn enter_confirms_selection() {
        let items = sample_items();
        let mut app = app_with(&items);
        let key = event::KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        handle_key(&mut app, key);
        assert!(app.confirmed);
        assert!(app.should_quit());
    }

    #[test]
    fn enter_on_empty_does_not_confirm() {
        let items: Vec<SearchResult> = vec![];
        let mut app = app_with(&items);
        let key = event::KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        handle_key(&mut app, key);
        assert!(!app.confirmed);
        assert!(!app.should_quit());
    }

    #[test]
    fn q_cancels_when_filter_empty() {
        let items = sample_items();
        let mut app = app_with(&items);
        let key = event::KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        handle_key(&mut app, key);
        assert!(app.cancelled);
    }

    #[test]
    fn q_types_into_filter_when_filter_has_text() {
        let items = sample_items();
        let mut app = app_with(&items);
        app.filter = "doc".to_string();
        let key = event::KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        handle_key(&mut app, key);
        assert!(!app.cancelled);
        assert_eq!(app.filter, "docq");
    }

    #[test]
    fn ctrl_c_cancels() {
        let items = sample_items();
        let mut app = app_with(&items);
        let key = event::KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        handle_key(&mut app, key);
        assert!(app.cancelled);
    }

    #[test]
    fn typing_updates_filter() {
        let items = sample_items();
        let mut app = app_with(&items);
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
        let mut app = app_with(&items);
        app.filter = "doc".to_string();
        app.refilter();
        let key = event::KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE);
        handle_key(&mut app, key);
        assert_eq!(app.filter, "do");
    }

    #[test]
    fn j_k_navigate_when_filter_empty() {
        let items = sample_items();
        let mut app = app_with(&items);
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
        let mut app = app_with(&items);
        app.list_state.select(Some(2));
        let key = event::KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE);
        handle_key(&mut app, key);
        assert_eq!(app.list_state.selected(), Some(0));
    }

    #[test]
    fn shift_g_jumps_to_bottom() {
        let items = sample_items();
        let mut app = app_with(&items);
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
        assert_eq!(audits, Vec::new());
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
        let mut app = app_with(&items);
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
        let mut app = app_with(&items);
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
        let mut app = app_with(&items);

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
        let mut app = app_with(&items);
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

    // -- Rendering tests (TestBackend) -----------------------------------------

    use crate::commands::test_support::render_to_text;

    #[test]
    fn render_initial_state() {
        let items = sample_items();
        let mut app = app_with(&items);
        insta::assert_snapshot!(render_to_text(|f| draw(f, &mut app)));
    }

    #[test]
    fn render_filtered() {
        let items = sample_items();
        let mut app = app_with(&items);
        app.filter = "docker".to_string();
        app.refilter();
        insta::assert_snapshot!(render_to_text(|f| draw(f, &mut app)));
    }

    #[test]
    fn render_empty_filter() {
        let items = sample_items();
        let mut app = app_with(&items);
        app.filter = "zzz_nonexistent_zzz".to_string();
        app.refilter();
        insta::assert_snapshot!(render_to_text(|f| draw(f, &mut app)));
    }

    /// Status bar hints that scrolling down loads more results.
    #[test]
    fn render_more_available() {
        let items = sample_items();
        let mut app = app_with_more(&items);
        insta::assert_snapshot!(render_to_text(|f| draw(f, &mut app)));
    }

    #[test]
    fn render_loading_more() {
        let mut app = App::new(sample_items(), Pager::loading());
        insta::assert_snapshot!(render_to_text(|f| draw(f, &mut app)));
    }

    /// List items show audit pass/fail icons when `audit_cache` is populated.
    #[test]
    fn render_with_loaded_audits() {
        let items = sample_items();
        let mut app = app_with(&items);
        let url = items[1].url.clone();
        app.audit_cache.insert(
            url,
            AuditState::Loaded(vec![
                SecurityAudit {
                    provider: "Socket".into(),
                    passed: true,
                },
                SecurityAudit {
                    provider: "Snyk".into(),
                    passed: false,
                },
            ]),
        );
        insta::assert_snapshot!(render_to_text(|f| draw(f, &mut app)));
    }

    /// Preview pane shows SKILL.md content when cache is populated.
    #[test]
    fn render_with_loaded_skill_preview() {
        let items = sample_items();
        let mut app = app_with(&items);
        let url = items[0].url.clone();
        app.skill_preview_cache.insert(
            url,
            SkillPreviewState::Loaded(PreviewContent {
                name: Some("Code Reviewer".into()),
                description: Some("Automated review".into()),
                risk: Some("low".into()),
                source: None,
                body_excerpt: Some("## When to use\n- PR review".into()),
            }),
        );
        insta::assert_snapshot!(render_to_text(|f| draw(f, &mut app)));
    }

    /// Preview shows failure state when audit fetch failed.
    #[test]
    fn render_with_failed_audits() {
        let items = sample_items();
        let mut app = app_with(&items);
        app.list_state.select(Some(1));
        let url = items[1].url.clone();
        app.audit_cache.insert(url, AuditState::Failed);
        insta::assert_snapshot!(render_to_text(|f| draw(f, &mut app)));
    }
}
