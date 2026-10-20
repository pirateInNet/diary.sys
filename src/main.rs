use std::{
    cell::Cell,
    env, fs,
    io::{self, Stdout},
    path::PathBuf,
    process,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use chrono::{Local, TimeZone};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
    Frame, Terminal,
};
use serde::{Deserialize, Serialize};

const AMBER: Color = Color::Rgb(255, 176, 0);
const AMBER_BRIGHT: Color = Color::Rgb(255, 209, 102);
const AMBER_DIM: Color = Color::Rgb(138, 96, 24);
const MUTED: Color = Color::Rgb(122, 98, 72);
const DANGER: Color = Color::Rgb(255, 92, 71);

const HELP_NORMAL: &str =
    "j/k move  enter read  n new  e edit  / filter  b beacon  d delete  r reload  q quit";
const HELP_VIEW: &str = "j/k scroll  h/l prev/next  e edit  b beacon  g/G top/end  q close";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    id: String,
    num: u32,
    text: String,
    ts: i64,
    #[serde(default)]
    beacon: bool,
}

#[derive(PartialEq)]
enum Mode {
    Normal,
    Insert,
    Review,
    ConfirmDelete,
    View,
    Filter,
}

struct App {
    entries: Vec<Entry>,
    selected: usize,
    mode: Mode,
    input: String,
    cursor: usize,              // char index into `input`
    editing_id: Option<String>, // Some(id) = editing an existing entry
    input_beacon: bool,
    filter: String,
    status: String,
    is_err: bool,
    log_path: PathBuf,
    // Written by the draw code (which only gets &App), read by key handlers.
    view_scroll: Cell<usize>,
    edit_scroll: Cell<usize>,
    edit_cap: Cell<usize>, // wrap width used by the composer
}

impl App {
    fn new(log_path: PathBuf) -> io::Result<Self> {
        let entries = load_entries(&log_path)?;
        Ok(App {
            entries,
            selected: 0,
            mode: Mode::Normal,
            input: String::new(),
            cursor: 0,
            editing_id: None,
            input_beacon: false,
            filter: String::new(),
            status: HELP_NORMAL.to_string(),
            is_err: false,
            log_path,
            view_scroll: Cell::new(0),
            edit_scroll: Cell::new(0),
            edit_cap: Cell::new(60),
        })
    }

    fn matches_filter(&self, e: &Entry) -> bool {
        if self.filter.trim().is_empty() {
            return true;
        }
        let wanted: Vec<String> = self
            .filter
            .split_whitespace()
            .map(|t| t.trim_start_matches('#').to_lowercase())
            .filter(|t| !t.is_empty())
            .collect();
        if wanted.is_empty() {
            return true;
        }
        let tags = tags_of(&e.text);
        // every wanted tag must prefix-match one of the entry's tags
        wanted
            .iter()
            .all(|w| tags.iter().any(|t| t.starts_with(w.as_str())))
    }

    /// Newest first, and only entries matching the tag filter.
    /// `selected` indexes into this list.
    fn sorted_indices(&self) -> Vec<usize> {
        let mut idx: Vec<usize> = (0..self.entries.len())
            .filter(|&i| self.matches_filter(&self.entries[i]))
            .collect();
        idx.sort_by(|&a, &b| self.entries[b].ts.cmp(&self.entries[a].ts));
        idx
    }

    fn clamp_selected(&mut self) {
        let len = self.sorted_indices().len();
        if len == 0 {
            self.selected = 0;
        } else if self.selected >= len {
            self.selected = len - 1;
        }
    }

    fn reload(&mut self) {
        match load_entries(&self.log_path) {
            Ok(entries) => {
                self.entries = entries;
                self.clamp_selected();
                self.status = format!("reloaded — {} entries", self.entries.len());
                self.is_err = false;
            }
            Err(e) => {
                self.status = format!("reload failed: {e}");
                self.is_err = true;
            }
        }
    }

    fn save(&mut self) -> io::Result<()> {
        save_entries(&self.log_path, &self.entries)
    }

    // ---- composer editing helpers -------------------------------------

    fn input_len(&self) -> usize {
        self.input.chars().count()
    }

    fn byte_at(&self, char_idx: usize) -> usize {
        self.input
            .char_indices()
            .nth(char_idx)
            .map(|(b, _)| b)
            .unwrap_or(self.input.len())
    }

    fn insert_char(&mut self, c: char) {
        let b = self.byte_at(self.cursor);
        self.input.insert(b, c);
        self.cursor += 1;
    }

    fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            let b = self.byte_at(self.cursor);
            self.input.remove(b);
        }
    }

    fn delete_forward(&mut self) {
        if self.cursor < self.input_len() {
            let b = self.byte_at(self.cursor);
            self.input.remove(b);
        }
    }

    fn rows_and_cursor(&self) -> (Vec<(usize, usize)>, usize) {
        let chars: Vec<char> = self.input.chars().collect();
        let rows = layout(&chars, self.edit_cap.get().max(1));
        let r = cursor_row(&rows, self.cursor);
        (rows, r)
    }

    fn move_vertical(&mut self, down: bool) {
        let (rows, r) = self.rows_and_cursor();
        let col = self.cursor - rows[r].0;
        let t = if down {
            if r + 1 >= rows.len() {
                return;
            }
            r + 1
        } else {
            if r == 0 {
                return;
            }
            r - 1
        };
        let (s, e) = rows[t];
        let wrapped = t + 1 < rows.len() && rows[t + 1].0 == e;
        let max_col = (e - s).saturating_sub(wrapped as usize);
        self.cursor = s + col.min(max_col);
    }

    fn move_home(&mut self) {
        let (rows, r) = self.rows_and_cursor();
        self.cursor = rows[r].0;
    }

    fn move_end(&mut self) {
        let (rows, r) = self.rows_and_cursor();
        let (s, e) = rows[r];
        let wrapped = r + 1 < rows.len() && rows[r + 1].0 == e;
        self.cursor = s + (e - s).saturating_sub(wrapped as usize);
    }

    fn finish_compose(&mut self) {
        self.input.clear();
        self.cursor = 0;
        self.input_beacon = false;
        self.editing_id = None;
        self.edit_scroll.set(0);
        self.mode = Mode::Normal;
    }

    /// Load the selected entry into the composer so it can be edited anywhere.
    fn start_edit(&mut self) {
        let order = self.sorted_indices();
        if let Some(&i) = order.get(self.selected) {
            let e = self.entries[i].clone();
            self.input = e.text;
            self.input_beacon = e.beacon;
            self.cursor = self.input.chars().count();
            self.editing_id = Some(e.id);
            self.edit_scroll.set(0);
            self.status = format!("editing LOG#{:04}", e.num);
            self.is_err = false;
            self.mode = Mode::Insert;
        }
    }

    fn commit_entry(&mut self) {
        let text = self.input.trim().to_string();
        if text.is_empty() {
            self.status = "empty entry, not saved".to_string();
            self.is_err = true;
            self.finish_compose();
            return;
        }
        // Re-read from disk first so we don't clobber changes made elsewhere.
        // On any failure the text is kept and we stay in Review so nothing is lost.
        let mut fresh = match load_entries(&self.log_path) {
            Ok(v) => v,
            Err(e) => {
                self.status = format!("could not read log: {e} — text kept, s to retry");
                self.is_err = true;
                return;
            }
        };
        let done = if let Some(id) = &self.editing_id {
            match fresh.iter_mut().find(|e| &e.id == id) {
                Some(e) => {
                    e.text = text;
                    e.beacon = self.input_beacon;
                    format!("updated LOG#{:04}", e.num)
                }
                None => {
                    self.status = "that entry no longer exists on disk — text kept".to_string();
                    self.is_err = true;
                    return;
                }
            }
        } else {
            let next_num = fresh.iter().map(|e| e.num).max().unwrap_or(0) + 1;
            fresh.push(Entry {
                id: new_id(),
                num: next_num,
                text,
                ts: now_millis(),
                beacon: self.input_beacon,
            });
            format!("logged as LOG#{:04}", next_num)
        };
        self.entries = fresh;
        match self.save() {
            Ok(_) => {
                self.status = done;
                self.is_err = false;
                self.finish_compose();
            }
            Err(e) => {
                self.status = format!("write failed: {e} — text kept, s to retry");
                self.is_err = true;
            }
        }
    }

    fn delete_selected(&mut self) {
        let order = self.sorted_indices();
        if let Some(&real_idx) = order.get(self.selected) {
            let removed = self.entries.remove(real_idx);
            match self.save() {
                Ok(_) => {
                    self.status = format!("deleted LOG#{:04}", removed.num);
                    self.is_err = false;
                }
                Err(e) => {
                    // put it back, the write failed
                    self.entries.insert(real_idx, removed);
                    self.status = format!("delete failed: {e}");
                    self.is_err = true;
                }
            }
            self.clamp_selected();
        }
        self.mode = Mode::Normal;
    }

    fn toggle_beacon_selected(&mut self) {
        let order = self.sorted_indices();
        if let Some(&real_idx) = order.get(self.selected) {
            self.entries[real_idx].beacon = !self.entries[real_idx].beacon;
            match self.save() {
                Ok(_) => {
                    self.status = if self.entries[real_idx].beacon {
                        "marked as beacon".to_string()
                    } else {
                        "unmarked".to_string()
                    };
                    self.is_err = false;
                }
                Err(e) => {
                    self.status = format!("save failed: {e}");
                    self.is_err = true;
                }
            }
        }
    }
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn new_id() -> String {
    format!("{}-{:x}", now_millis(), process::id())
}

fn load_entries(path: &PathBuf) -> io::Result<Vec<Entry>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(path)?;
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let entries: Vec<Entry> =
        serde_json::from_str(trimmed).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(entries)
}

fn save_entries(path: &PathBuf, entries: &[Entry]) -> io::Result<()> {
    let json = serde_json::to_string_pretty(entries)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    // Write to a temp file then rename, so a crash mid-write can't corrupt the log.
    let tmp_path = path.with_extension("json.tmp");
    fs::write(&tmp_path, json)?;
    fs::rename(&tmp_path, path)?;
    Ok(())
}

fn fmt_date(ts_millis: i64) -> String {
    match Local.timestamp_millis_opt(ts_millis) {
        chrono::LocalResult::Single(dt) => dt.format("%Y-%m-%d %H:%M").to_string(),
        _ => "----".to_string(),
    }
}

/// Tags are just `#words` written anywhere in the text. Nothing extra is stored,
/// so existing logs work unchanged.
fn tags_of(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for w in text.split_whitespace() {
        if let Some(rest) = w.strip_prefix('#') {
            let t: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
                .collect::<String>()
                .to_lowercase();
            if !t.is_empty() && !out.contains(&t) {
                out.push(t);
            }
        }
    }
    out
}

/// Split text into visual rows of at most `cap` chars, breaking on spaces when
/// possible. Returns (start, end) char ranges. Always returns at least one row.
/// Because we wrap ourselves, cursor position and scrolling are exact.
fn layout(chars: &[char], cap: usize) -> Vec<(usize, usize)> {
    let mut rows = Vec::new();
    let mut start = 0;
    loop {
        let mut j = start;
        while j < chars.len() && chars[j] != '\n' && j - start < cap {
            j += 1;
        }
        if j < chars.len() && chars[j] != '\n' {
            // hit the width limit mid-line: back up to the last space if any
            if let Some(p) = chars[start..j].iter().rposition(|&c| c == ' ') {
                if p > 0 {
                    j = start + p + 1;
                }
            }
        }
        rows.push((start, j));
        if j >= chars.len() {
            break;
        }
        start = if chars[j] == '\n' { j + 1 } else { j };
    }
    rows
}

fn cursor_row(rows: &[(usize, usize)], c: usize) -> usize {
    rows.iter().rposition(|&(s, _)| s <= c).unwrap_or(0)
}

/// Where the log lives when no path is given on the command line:
/// $DIARY_LOG, else $XDG_DATA_HOME/diary/diary-log.json,
/// else ~/.local/share/diary/diary-log.json.
fn default_log_path() -> PathBuf {
    if let Some(p) = env::var_os("DIARY_LOG") {
        return PathBuf::from(p);
    }
    let base = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("diary").join("diary-log.json")
}

fn main() -> io::Result<()> {
    let log_path = env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(default_log_path);

    if let Some(dir) = log_path.parent() {
        if let Err(e) = fs::create_dir_all(dir) {
            eprintln!("could not create {}: {e}", dir.display());
            process::exit(1);
        }
    }

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = match App::new(log_path) {
        Ok(app) => app,
        Err(e) => {
            restore_terminal(&mut terminal)?;
            eprintln!("could not start diary.sys: {e}");
            process::exit(1);
        }
    };

    let result = run_app(&mut terminal, &mut app);

    restore_terminal(&mut terminal)?;

    if let Err(e) = result {
        eprintln!("error: {e}");
    }
    Ok(())
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    Ok(())
}

fn run_app(terminal: &mut Terminal<CrosstermBackend<Stdout>>, app: &mut App) -> io::Result<()> {
    loop {
        terminal.draw(|f| ui(f, app))?;

        if !event::poll(Duration::from_millis(200))? {
            continue;
        }

        if let Event::Key(key) = event::read()? {
            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                return Ok(());
            }

            match app.mode {
                Mode::Normal => match key.code {
                    KeyCode::Char('q') => return Ok(()),
                    KeyCode::Esc => {
                        if app.filter.is_empty() {
                            return Ok(());
                        }
                        app.filter.clear();
                        app.selected = 0;
                    }
                    KeyCode::Char('j') | KeyCode::Down => {
                        let len = app.sorted_indices().len();
                        if len > 0 {
                            app.selected = (app.selected + 1).min(len - 1);
                        }
                    }
                    KeyCode::Char('k') | KeyCode::Up => {
                        app.selected = app.selected.saturating_sub(1);
                    }
                    KeyCode::Enter => {
                        if !app.sorted_indices().is_empty() {
                            app.view_scroll.set(0);
                            app.status = HELP_VIEW.to_string();
                            app.is_err = false;
                            app.mode = Mode::View;
                        }
                    }
                    KeyCode::Char('e') => app.start_edit(),
                    KeyCode::Char('/') => app.mode = Mode::Filter,
                    KeyCode::Char('n') => {
                        app.mode = Mode::Insert;
                        app.input.clear();
                        app.cursor = 0;
                        app.editing_id = None;
                        app.edit_scroll.set(0);
                        app.input_beacon = false;
                    }
                    KeyCode::Char('b') => {
                        if !app.sorted_indices().is_empty() {
                            app.toggle_beacon_selected();
                        }
                    }
                    KeyCode::Char('d') => {
                        if !app.sorted_indices().is_empty() {
                            app.mode = Mode::ConfirmDelete;
                        }
                    }
                    KeyCode::Char('r') => app.reload(),
                    _ => {}
                },
                Mode::View => match key.code {
                    KeyCode::Char('q') | KeyCode::Esc | KeyCode::Enter => {
                        app.mode = Mode::Normal;
                        app.status = HELP_NORMAL.to_string();
                        app.is_err = false;
                    }
                    KeyCode::Char('j') | KeyCode::Down => {
                        app.view_scroll.set(app.view_scroll.get().saturating_add(1))
                    }
                    KeyCode::Char('k') | KeyCode::Up => {
                        app.view_scroll.set(app.view_scroll.get().saturating_sub(1))
                    }
                    KeyCode::PageDown | KeyCode::Char(' ') => app
                        .view_scroll
                        .set(app.view_scroll.get().saturating_add(10)),
                    KeyCode::PageUp => app
                        .view_scroll
                        .set(app.view_scroll.get().saturating_sub(10)),
                    KeyCode::Char('g') => app.view_scroll.set(0),
                    KeyCode::Char('G') => app.view_scroll.set(usize::MAX), // clamped when drawn
                    KeyCode::Char('l') | KeyCode::Right => {
                        let len = app.sorted_indices().len();
                        if len > 0 {
                            app.selected = (app.selected + 1).min(len - 1);
                        }
                        app.view_scroll.set(0);
                    }
                    KeyCode::Char('h') | KeyCode::Left => {
                        app.selected = app.selected.saturating_sub(1);
                        app.view_scroll.set(0);
                    }
                    KeyCode::Char('e') => app.start_edit(),
                    KeyCode::Char('b') => app.toggle_beacon_selected(),
                    _ => {}
                },
                Mode::Filter => match key.code {
                    KeyCode::Enter => app.mode = Mode::Normal,
                    KeyCode::Esc => {
                        app.filter.clear();
                        app.selected = 0;
                        app.mode = Mode::Normal;
                    }
                    KeyCode::Backspace => {
                        app.filter.pop();
                        app.selected = 0;
                    }
                    KeyCode::Char(c) => {
                        app.filter.push(c);
                        app.selected = 0;
                    }
                    _ => {}
                },
                Mode::Insert => match key.code {
                    KeyCode::Esc => {
                        if app.input.trim().is_empty() {
                            app.finish_compose();
                            app.status = HELP_NORMAL.to_string();
                            app.is_err = false;
                        } else {
                            app.mode = Mode::Review;
                        }
                    }
                    KeyCode::Enter => app.insert_char('\n'),
                    KeyCode::Backspace => app.backspace(),
                    KeyCode::Delete => app.delete_forward(),
                    KeyCode::Left => app.cursor = app.cursor.saturating_sub(1),
                    KeyCode::Right => app.cursor = (app.cursor + 1).min(app.input_len()),
                    KeyCode::Up => app.move_vertical(false),
                    KeyCode::Down => app.move_vertical(true),
                    KeyCode::Home => app.move_home(),
                    KeyCode::End => app.move_end(),
                    KeyCode::Tab => app.input_beacon = !app.input_beacon,
                    KeyCode::Char(c) => app.insert_char(c),
                    _ => {}
                },
                Mode::Review => match key.code {
                    KeyCode::Char('s') => app.commit_entry(),
                    KeyCode::Char('e') => app.mode = Mode::Insert,
                    KeyCode::Char('j') | KeyCode::Down => {
                        app.edit_scroll.set(app.edit_scroll.get().saturating_add(1))
                    }
                    KeyCode::Char('k') | KeyCode::Up => {
                        app.edit_scroll.set(app.edit_scroll.get().saturating_sub(1))
                    }
                    KeyCode::PageDown => app
                        .edit_scroll
                        .set(app.edit_scroll.get().saturating_add(10)),
                    KeyCode::PageUp => app
                        .edit_scroll
                        .set(app.edit_scroll.get().saturating_sub(10)),
                    KeyCode::Char('c') | KeyCode::Esc => {
                        app.finish_compose();
                        app.status = "discarded".to_string();
                        app.is_err = false;
                    }
                    _ => {}
                },
                Mode::ConfirmDelete => match key.code {
                    KeyCode::Char('y') => app.delete_selected(),
                    KeyCode::Char('n') | KeyCode::Esc => {
                        app.mode = Mode::Normal;
                    }
                    _ => {}
                },
            }
        }
    }
}

fn ui(f: &mut Frame, app: &App) {
    let size = f.size();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(3),
        ])
        .split(size);

    draw_header(f, app, chunks[0]);

    match app.mode {
        Mode::Insert | Mode::Review => draw_composer(f, app, chunks[1]),
        Mode::View => draw_view(f, app, chunks[1]),
        _ => draw_feed(f, app, chunks[1]),
    }

    draw_footer(f, app, chunks[2]);

    if app.mode == Mode::ConfirmDelete {
        draw_confirm_delete(f, app, size);
    }
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let total = app.entries.len();
    let beacons = app.entries.iter().filter(|e| e.beacon).count();
    let stats = if total == 0 {
        "no transmissions yet".to_string()
    } else {
        format!(
            "{total} logged  ·  {beacons} beacon{}",
            if beacons == 1 { "" } else { "s" }
        )
    };
    let mut spans = vec![
        Span::styled(
            "diary.sys",
            Style::default()
                .fg(AMBER_BRIGHT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  ", Style::default()),
        Span::styled(stats, Style::default().fg(MUTED)),
    ];
    if !app.filter.trim().is_empty() {
        let shown = app.sorted_indices().len();
        spans.push(Span::styled(
            format!("  ·  filter: {}  ({shown}/{total})", app.filter.trim()),
            Style::default().fg(AMBER),
        ));
    }
    let block = Block::default()
        .borders(Borders::BOTTOM)
        .border_style(Style::default().fg(AMBER_DIM));
    let p = Paragraph::new(Line::from(spans))
        .block(block)
        .alignment(Alignment::Left);
    f.render_widget(p, area);
}

fn draw_feed(f: &mut Frame, app: &App, area: Rect) {
    let order = app.sorted_indices();

    if order.is_empty() {
        let msg = if app.filter.trim().is_empty() {
            "log is empty. press n to write your first entry.".to_string()
        } else {
            format!(
                "no entries match \"{}\". Esc clears the filter.",
                app.filter.trim()
            )
        };
        let empty = Paragraph::new(msg)
            .style(Style::default().fg(MUTED))
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true });
        f.render_widget(empty, area);
        return;
    }

    let items: Vec<ListItem> = order
        .iter()
        .map(|&idx| {
            let e = &app.entries[idx];
            let tag = if e.beacon { "  BEACON" } else { "" };
            let meta_color = if e.beacon { AMBER_BRIGHT } else { MUTED };
            let tags = tags_of(&e.text);
            let mut meta_spans = vec![
                Span::styled(format!("LOG#{:04}", e.num), Style::default().fg(AMBER_DIM)),
                Span::raw("  "),
                Span::styled(fmt_date(e.ts), Style::default().fg(MUTED)),
                Span::styled(
                    tag,
                    Style::default().fg(meta_color).add_modifier(Modifier::BOLD),
                ),
            ];
            if !tags.is_empty() {
                let joined = tags
                    .iter()
                    .map(|t| format!("#{t}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                meta_spans.push(Span::styled(
                    format!("  {joined}"),
                    Style::default().fg(AMBER_DIM),
                ));
            }
            let meta = Line::from(meta_spans);
            let body = Line::from(Span::styled(
                e.text.lines().next().unwrap_or("").to_string(),
                Style::default().fg(AMBER_BRIGHT),
            ));
            let extra_lines = e.text.lines().count().saturating_sub(1);
            let mut lines = vec![meta, body];
            if extra_lines > 0 {
                lines.push(Line::from(Span::styled(
                    format!(
                        "  … +{extra_lines} more line{}  (enter to read)",
                        if extra_lines == 1 { "" } else { "s" }
                    ),
                    Style::default().fg(MUTED),
                )));
            }
            lines.push(Line::from(""));
            ListItem::new(lines)
        })
        .collect();

    let list = List::new(items)
        .block(Block::default())
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("> ");

    let mut state = ListState::default();
    state.select(Some(app.selected.min(order.len().saturating_sub(1))));

    f.render_stateful_widget(list, area, &mut state);
}

/// Draws `text` in `block`, wrapped and scrolled by us (not by ratatui) so the
/// cursor and scroll position are exact. With a cursor, the view follows it;
/// without one, `scroll` is just clamped to the content.
fn draw_text_box(
    f: &mut Frame,
    area: Rect,
    block: Block,
    text: &str,
    cursor: Option<usize>,
    scroll: &Cell<usize>,
    cap_out: Option<&Cell<usize>>,
) {
    let inner = block.inner(area);
    let height = inner.height as usize;
    let cap = (inner.width as usize).saturating_sub(1).max(1); // 1 spare column for the cursor
    if let Some(c) = cap_out {
        c.set(cap);
    }

    let chars: Vec<char> = text.chars().collect();
    let rows = layout(&chars, cap);
    let cursor = cursor.map(|c| c.min(chars.len()));
    let crow = cursor.map(|c| cursor_row(&rows, c));

    let mut s = scroll.get();
    if let Some(r) = crow {
        if r < s {
            s = r;
        }
        if height > 0 && r >= s + height {
            s = r + 1 - height;
        }
    }
    s = s.min(rows.len().saturating_sub(height.max(1)));
    scroll.set(s);

    let base = Style::default().fg(AMBER_BRIGHT);
    let lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .skip(s)
        .take(height)
        .map(|(ri, &(a, b))| match (cursor, crow) {
            (Some(c), Some(r)) if r == ri => {
                let before: String = chars[a..c].iter().collect();
                let (at, after) = if c < b {
                    (
                        chars[c].to_string(),
                        chars[c + 1..b].iter().collect::<String>(),
                    )
                } else {
                    (" ".to_string(), String::new())
                };
                Line::from(vec![
                    Span::styled(before, base),
                    Span::styled(at, base.add_modifier(Modifier::REVERSED)),
                    Span::styled(after, base),
                ])
            }
            _ => Line::from(Span::styled(chars[a..b].iter().collect::<String>(), base)),
        })
        .collect();

    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_view(f: &mut Frame, app: &App, area: Rect) {
    let order = app.sorted_indices();
    let Some(&idx) = order.get(app.selected) else {
        return;
    };
    let e = &app.entries[idx];
    let title = format!(
        " LOG#{:04} · {}{} ",
        e.num,
        fmt_date(e.ts),
        if e.beacon { " · BEACON" } else { "" }
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(AMBER))
        .title(Span::styled(title, Style::default().fg(MUTED)));
    draw_text_box(f, area, block, &e.text, None, &app.view_scroll, None);
}

fn draw_composer(f: &mut Frame, app: &App, area: Rect) {
    let reviewing = app.mode == Mode::Review;
    let border_color = if reviewing { AMBER_DIM } else { AMBER };
    let beacon_note = if app.input_beacon { " [beacon]" } else { "" };
    let verb = if app.editing_id.is_some() {
        "editing"
    } else {
        "writing"
    };
    let title = if reviewing {
        format!(" review{beacon_note} — s save · e edit · c cancel · j/k scroll ")
    } else {
        format!(" {verb}{beacon_note} — Tab beacon · Esc done ")
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(title, Style::default().fg(MUTED)));

    let cursor = if reviewing { None } else { Some(app.cursor) };
    draw_text_box(
        f,
        area,
        block,
        &app.input,
        cursor,
        &app.edit_scroll,
        Some(&app.edit_cap),
    );
}

fn draw_confirm_delete(f: &mut Frame, app: &App, size: Rect) {
    let order = app.sorted_indices();
    let num = order
        .get(app.selected)
        .map(|&i| app.entries[i].num)
        .unwrap_or(0);
    let width = 44.min(size.width.saturating_sub(4));
    let height = 5;
    let area = Rect {
        x: (size.width.saturating_sub(width)) / 2,
        y: (size.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(DANGER))
        .title(" confirm ");
    let text = format!("delete LOG#{:04}?\n\ny / n", num);
    let p = Paragraph::new(text)
        .style(Style::default().fg(AMBER_BRIGHT))
        .alignment(Alignment::Center)
        .block(block);
    f.render_widget(ratatui::widgets::Clear, area);
    f.render_widget(p, area);
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let color = if app.is_err { DANGER } else { MUTED };
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(AMBER_DIM));
    let text = if app.mode == Mode::Filter {
        format!(
            "/{}_   Enter keep · Esc clear · several tags = AND",
            app.filter
        )
    } else {
        app.status.clone()
    };
    let p = Paragraph::new(text)
        .style(Style::default().fg(color))
        .wrap(Wrap { trim: true })
        .block(block);
    f.render_widget(p, area);
}
