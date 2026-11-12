use std::{ collections::HashSet, env, fs, io };
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{ Path, PathBuf };
use std::process::{ Command, Stdio };
use std::time::SystemTime;
use chrono::{ DateTime, Local };
use unicode_width::UnicodeWidthStr;
use ratatui::{
    crossterm:: {
        cursor::{ Hide, MoveTo, Show },
        event::{ self, Event, KeyCode, KeyEventKind, KeyModifiers },
        execute,
        terminal::{ Clear, ClearType, disable_raw_mode, enable_raw_mode },
    },
    layout::{ Constraint, Layout, Rect },
    style::{ Modifier, Style },
    text::Line,
    widgets::{ Block, List, ListItem, ListState, Paragraph },
    DefaultTerminal,
    Frame,
    TerminalOptions,
    Viewport,
};

const PANE_HEIGHT: u16 = 16;
const PAGE_STEP: usize = PANE_HEIGHT as usize - 2; // visible rows inside the border
// ratatui's List reserves this much width on *every* row for the highlight symbol ("> "),
// not just the selected one, so column padding must account for it too.
const SELECTION_GUTTER_WIDTH: usize = 2;
const DIR_ICON: &str = "📁";
const FILE_ICON: &str = "📄";
const EXE_ELF_ICON: &str = "➤";
const HINTS: &[&str] = &[
    "(q) quit", "(j) ↓", "(k) ↑", "{/} page", "(c) cd", "(.) ..", "(o) open", "(s) select",
    "(e) move", "(y) copy", "(d) cmd",  "(m) mkdir", "(t) touch", "(r) rename", "(g) grep",
    "(alt+a) hide/show", "(alt+s) sort",
];

struct FileEntry {
    name: String,
    is_dir: bool,
    size: u64,
    created: Option<SystemTime>,
    modified: Option<SystemTime>,
    accessed: Option<SystemTime>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SortKey {
    Name,
    Size,
    Type,
    Created,
    Modified,
    Accessed,
}

enum PendingAction {
    Move(Vec<PathBuf>),
    Copy(Vec<PathBuf>),
}

/// The file extension nav sorts/displays under "type" — lowercased, "dir" for directories,
/// "-" when there's no extension.
fn file_type(entry: &FileEntry) -> String {
    if entry.is_dir {
        return "dir".to_string();
    }
    Path::new(&entry.name)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_lowercase)
        .unwrap_or_else(|| "-".to_string())
}

fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes}B")
    } else {
        format!("{size:.1}{}", UNITS[unit])
    }
}

fn format_time(time: Option<SystemTime>) -> String {
    match time {
        Some(time) => DateTime::<Local>::from(time).format("%Y-%m-%d %H:%M").to_string(),
        None => "-".to_string(),
    }
}

/// Column text shown to the right of each entry for every sort key but `Name` (which needs
/// no extra column since the name itself is already the sort key).
fn column_value(entry: &FileEntry, sort_key: SortKey) -> Option<String> {
    match sort_key {
        SortKey::Name => None,
        SortKey::Size => Some(format_size(entry.size)),
        SortKey::Type => Some(file_type(entry)),
        SortKey::Created => Some(format_time(entry.created)),
        SortKey::Modified => Some(format_time(entry.modified)),
        SortKey::Accessed => Some(format_time(entry.accessed)),
    }
}

/// Fixed display width per sort key, so the column lines up across rows regardless of how
/// long any individual value happens to be (dates are always 16 chars; size/type are padded
/// generously for the rare long value).
fn column_width(sort_key: SortKey) -> usize {
    match sort_key {
        SortKey::Name => 0,
        SortKey::Size => 7,
        SortKey::Type => 8,
        SortKey::Created | SortKey::Modified | SortKey::Accessed => 16,
    }
}

/// Directories always sort before files; within each group, entries sort by `sort_key`.
/// Size and the time-based keys sort newest/largest first since that's usually what's
/// wanted; name and type sort ascending (type ties break on name).
fn sort_entries(entries: &mut [FileEntry], sort_key: SortKey) {
    entries.sort_by(|a, b| {
        b.is_dir.cmp(&a.is_dir).then_with(|| match sort_key {
            SortKey::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            SortKey::Type => file_type(a).cmp(&file_type(b)).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())),
            SortKey::Size => b.size.cmp(&a.size),
            SortKey::Created => b.created.cmp(&a.created),
            SortKey::Modified => b.modified.cmp(&a.modified),
            SortKey::Accessed => b.accessed.cmp(&a.accessed),
        })
    });
}

/// Describes a pending move/copy for the status bar and command-failure messages: the single
/// path when there's just one (matching the original single-file wording), or a count once
/// multiple marked entries are involved.
fn describe_sources(sources: &[PathBuf]) -> String {
    match sources {
        [single] => single.display().to_string(),
        _ => format!("{} items", sources.len()),
    }
}

struct Browser {
    cwd: PathBuf,
    entries: Vec<FileEntry>,
    selected: usize,
    show_hidden: bool,
    sort_key: SortKey,
    pending_action: Option<PendingAction>,
    marked: HashSet<PathBuf>,
    status: Option<String>,
}

impl Browser {
    fn selected_entry(&self) -> Option<&FileEntry> {
        self.entries.get(self.selected)
    }

    /// The paths an `e`/`y` press should act on: every marked entry if any are marked,
    /// otherwise just the entry under the cursor.
    fn pending_sources(&self) -> Option<Vec<PathBuf>> {
        if !self.marked.is_empty() {
            return Some(self.marked.iter().cloned().collect());
        }
        self.selected_entry().map(|entry| vec![self.cwd.join(&entry.name)])
    }

    fn toggle_mark(&mut self) {
        if let Some(entry) = self.selected_entry() {
            let path = self.cwd.join(&entry.name);
            if !self.marked.remove(&path) {
                self.marked.insert(path);
            }
        }
    }

    fn move_selection(&mut self, delta: isize) {
        if self.entries.is_empty() {
            return;
        }
        let max = (self.entries.len() - 1) as isize;
        self.selected = (self.selected as isize + delta).clamp(0, max) as usize;
    }

    /// Lists `target` before committing to it, so a permission-denied or since-deleted
    /// directory shows an inline error instead of propagating and killing the whole program.
    fn navigate_to(&mut self, target: PathBuf) {
        match list_entries(&target, self.show_hidden, self.sort_key) {
            Ok(entries) => {
                self.cwd = target;
                self.entries = entries;
                self.selected = 0;
                self.status = None;
            }
            Err(err) => self.status = Some(format!("cannot open {}: {err}", target.display())),
        }
    }

    fn go_up(&mut self) {
        if let Some(parent) = self.cwd.parent().map(Path::to_path_buf) {
            self.navigate_to(parent);
        }
    }

    fn enter_dir(&mut self, name: &str) {
        self.navigate_to(self.cwd.join(name));
    }

    fn toggle_hidden(&mut self) {
        self.show_hidden = !self.show_hidden;
        self.refresh();
    }

    fn refresh(&mut self) {
        match list_entries(&self.cwd, self.show_hidden, self.sort_key) {
            Ok(entries) => {
                self.entries = entries;
                self.selected = self.selected.min(self.entries.len().saturating_sub(1));
                self.status = None;
            }
            Err(err) => self.status = Some(format!("cannot read {}: {err}", self.cwd.display())),
        }
    }
}

fn list_entries(path: &Path, show_hidden: bool, sort_key: SortKey) -> io::Result<Vec<FileEntry>> {
    let mut entries: Vec<FileEntry> = fs::read_dir(path)?
        .filter_map(|entry| entry.ok())
        .filter(|entry| show_hidden || !entry.file_name().to_string_lossy().starts_with('.'))
        .map(|entry| {
            // Metadata follows symlinks (unlike DirEntry::metadata), matching the old
            // `entry.path().is_dir()` behavior for symlinked directories. A failed stat (e.g.
            // a broken symlink) still shows the entry, just with no size/dates to sort by.
            let metadata = fs::metadata(entry.path()).ok();
            FileEntry {
                name: entry.file_name().to_string_lossy().into_owned(),
                is_dir: metadata.as_ref().is_some_and(fs::Metadata::is_dir),
                size: metadata.as_ref().map_or(0, fs::Metadata::len),
                created: metadata.as_ref().and_then(|meta| meta.created().ok()),
                modified: metadata.as_ref().and_then(|meta| meta.modified().ok()),
                accessed: metadata.as_ref().and_then(|meta| meta.accessed().ok()),
            }
        })
        .collect();
    sort_entries(&mut entries, sort_key);
    Ok(entries)
}

fn is_executable(path: &Path) -> bool {
    fs::metadata(path)
        .map(|meta| meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

fn file_icon(name: &str) -> &'static str {
    let ext = Path::new(name)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_lowercase);

    match ext.as_deref() {
        Some("png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "svg" | "ico") => "📷",
        Some("mp3" | "wav" | "flac" | "ogg" | "m4a") => "🎵",
        Some("mp4" | "mkv" | "mov" | "avi" | "webm") => "🎬",
        Some("zip" | "tar" | "gz" | "bz2" | "xz" | "7z" | "rar") => "📦",
        Some("ods" | "fods"| "gnm"| "csv"| "psv"| "tsv"| "xls"| "slxb"| "slxm" | "xlsx") => "📊",
        Some("doc" | "docx" | "rtf" | "pdf" | "odt") => "🗎",
        Some("json") => "{}",
        Some("nix") => "❄️",
        Some("lua") => "◉°",
        Some("py") => "🐍",
        Some("rs") => "🦀",
        Some("php") => "🐘",
        Some("rb") => "♦️",
        Some("java") => "♨️",
        Some("cs") => "#",
        Some("html" | "xml") => "<>",
        Some("js" | "ts") => "🌐",
        Some("swift") => "🪽",
        Some("sql") => "🐿️",
        Some("md" | "markdown") => "📝",
        Some("sqlite" | "sqlite3") => "🪶",
        Some("css" | "qml") => "🖌️",
        Some("jsx" | "tsx") => "⚛️",
        Some("zig" | "odin" | "go" |"c" | "h" | "r" | "cpp" | "hpp" | "f90" | "F90" | "f" | "F") => "📜",
        Some("yaml" | "yml" | "toml" | "ini" | "conf" | "config" | "cfg" | "plist") => "🔧",
        Some("iso" | "bin" | "cue" | "img"  | "rom" | "sfc" | "nes" | "gba" | "z64") => "💾",
        Some("db" | ".bak" | "mdf" | "ndf" | "mdb" | "db3" | "dbf" | "odb" | "myc" | "myd" | "myi") => "📀",
        Some("sh" | "bash" | "zsh" | "ksh") => ">_",
        _ => FILE_ICON,
    }
}

/// Packs hint segments onto lines of at most `width` columns, breaking only at `|` boundaries
/// (never mid-segment) so the hint bar wraps instead of clipping on narrow terminals.
fn wrap_hints(width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();

    for hint in HINTS {
        let candidate = if current.is_empty() {
            hint.to_string()
        } else {
            format!("{current} | {hint}")
        };

        if candidate.chars().count() > width && !current.is_empty() {
            lines.push(current);
            current = hint.to_string();
        } else {
            current = candidate;
        }
    }

    if !current.is_empty() {
        lines.push(current);
    }

    lines
}

fn draw(frame: &mut Frame, browser: &Browser) {
    let area = frame.area();
    let hint_lines = wrap_hints(area.width as usize);
    let [box_area, hints_area] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(hint_lines.len() as u16),
    ])
    .areas(area);

    let dir_name = browser.cwd.file_name().map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    let title = format!(" /{dir_name} ");
    let status = if let Some(msg) = &browser.status {
        format!(" {msg} ")
    } else {
        match &browser.pending_action {
            Some(PendingAction::Move(sources)) => format!(
                " moving: {} — select a dir, (e) to drop it here (Esc to cancel) ",
                describe_sources(sources)
            ),
            Some(PendingAction::Copy(sources)) => format!(
                " copying: {} — select a dir, (y) to drop it here (Esc to cancel) ",
                describe_sources(sources)
            ),
            None if !browser.marked.is_empty() => format!(
                " {} marked — (s) toggle, (e)/(y) move/copy, Esc clears ",
                browser.marked.len()
            ),
            None if browser.show_hidden => " all files ".to_string(),
            None => " files hidden ".to_string(),
        }
    };
    let block = Block::bordered()
        .title_top(title)
        .title_top(Line::from(status).right_aligned());
    let list_area = block.inner(box_area);
    frame.render_widget(block, box_area);

    let items: Vec<ListItem> = if browser.entries.is_empty() {
        vec![ListItem::new("(empty)")]
    } else {
        browser.entries.iter()
            .map(|entry| {
                let full_path = browser.cwd.join(&entry.name);
                let icon = if entry.is_dir {
                    DIR_ICON
                } else if is_executable(&full_path) {
                    EXE_ELF_ICON
                } else {
                    file_icon(&entry.name)
                };
                let mark = if browser.marked.contains(&full_path) { " 🟢" } else { "" };
                let left = format!("{icon} {}{mark}", entry.name);

                let line = match column_value(entry, browser.sort_key) {
                    Some(value) => {
                        let value = format!("{value:>width$}", width = column_width(browser.sort_key));
                        let available = (list_area.width as usize).saturating_sub(SELECTION_GUTTER_WIDTH);
                        let pad = available.saturating_sub(left.width() + value.width());
                        format!("{left}{}{value}", " ".repeat(pad))
                    }
                    None => left,
                };
                ListItem::new(line)
            })
            .collect()
    };

    let list = List::new(items)
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED))
        .highlight_symbol("> ");

    let mut state = ListState::default();
    if !browser.entries.is_empty() {
        state.select(Some(browser.selected));
    }
    frame.render_stateful_widget(list, list_area, &mut state);

    let hint_text: Vec<Line> = hint_lines.into_iter().map(Line::from).collect();
    frame.render_widget(Paragraph::new(hint_text), hints_area);
}

/// Clears the terminal's visible screen *and* scrollback history (`ClearType::Purge`, CSI
/// `3J`), then re-anchors a fresh inline viewport at row 0. Call this whenever a resize is
/// detected.
///
/// Shrinking a terminal below nav's rendered height pushes the old (too-tall) frame into
/// scrollback before nav even receives the resize event — that part happens at the terminal
/// emulator level and can't be prevented. But without purging, nav then draws a *new* frame on
/// top, so every resize leaves one more duplicate sitting in scrollback history. Purging means
/// there's nothing left to scroll back to — at the cost of also wiping any of the user's own
/// shell history that predates nav, since purge clears the whole scrollback buffer, not just
/// nav's own output.
fn reset_terminal_on_resize(terminal: &mut DefaultTerminal) -> io::Result<()> {
    execute!(io::stdout(), Clear(ClearType::All), Clear(ClearType::Purge), MoveTo(0, 0))?;
    *terminal = ratatui::init_with_options(TerminalOptions {
        viewport: Viewport::Inline(PANE_HEIGHT),
    });
    Ok(())
}

/// Marks the browser as awaiting input on `label` and forces an immediate redraw, so the
/// title bar's "(Esc to cancel)" hint appears before the blocking prompt takes over input —
/// mirroring how the pending-move hint already appears via the normal redraw cycle.
fn show_cancelable_prompt(terminal: &mut DefaultTerminal, browser: &mut Browser, label: &str) -> io::Result<()> {
    browser.status = Some(format!("{label} (Esc to cancel)"));
    terminal.draw(|frame| draw(frame, browser))?;
    Ok(())
}

fn run_command_on_entry(
    terminal: &mut DefaultTerminal,
    browser: &Browser,
    viewport_area: &mut Rect,
    name: &str,
) -> io::Result<()> {
    let cwd: &Path = &browser.cwd;
    let header = format!("command on {name} (use {{}} for the path, otherwise it's appended)");
    let Some(input) = read_cancelable_line(terminal, browser, viewport_area, Some(&header), ": ")? else {
        terminal.clear()?;
        return Ok(());
    };
    let input = input.trim();

    if !input.is_empty() {
        disable_raw_mode()?;
        execute!(io::stdout(), MoveTo(0, viewport_area.bottom() + 1), Show)?;

        let full_path = cwd.join(name);
        let command_str = if input.contains("{}") {
            input.replace("{}", &full_path.to_string_lossy())
        } else {
            format!("{input} {}", full_path.display())
        };

        let failed = match Command::new("sh").arg("-c").arg(&command_str).current_dir(cwd).status() {
            Ok(status) if !status.success() => {
                println!("command failed: {command_str} ({status})");
                true
            }
            Err(err) => {
                println!("command failed: {command_str} ({err})");
                true
            }
            _ => false,
        };

        if failed {
            println!("press Enter to return to the list...");
            let mut pause = String::new();
            io::stdin().read_line(&mut pause)?;
        }

        execute!(io::stdout(), Hide)?;
        enable_raw_mode()?;
    }

    terminal.clear()?;
    Ok(())
}

/// Reads a line of input without leaving raw mode, so Esc cancels immediately (returning
/// `Ok(None)`) instead of just being inserted as a literal character like it would under the
/// normal cooked-mode `read_line` prompts. Also watches for `Event::Resize`: unlike the main
/// loop, this loop doesn't call `terminal.draw` every iteration, so without
/// `reset_terminal_on_resize` a resize mid-prompt would leave the border drawn at the old size.
///
/// The prompt-line cursor move is routed through `terminal.set_cursor_position` rather than a
/// raw crossterm `MoveTo`, so ratatui's internal "last known cursor position" bookkeeping
/// stays truthful between resizes instead of drifting from under it.
fn read_cancelable_line(
    terminal: &mut DefaultTerminal,
    browser: &Browser,
    viewport_area: &mut Rect,
    header: Option<&str>,
    prompt: &str,
) -> io::Result<Option<String>> {
    execute!(io::stdout(), Show)?;
    let mut input = String::new();

    let result = loop {
        // When there's a header, it takes the row right below the box and the input prompt
        // moves down one row to follow it; otherwise the input prompt takes that row itself.
        if let Some(header) = header {
            terminal.set_cursor_position((0, viewport_area.bottom()))?;
            execute!(io::stdout(), Clear(ClearType::CurrentLine))?;
            print!("{header}");
        }
        let input_row = viewport_area.bottom() + u16::from(header.is_some());
        terminal.set_cursor_position((0, input_row))?;
        execute!(io::stdout(), Clear(ClearType::CurrentLine))?;
        print!("{prompt}{input}");
        io::stdout().flush()?;

        match event::read()? {
            Event::Key(key_event) => {
                if key_event.kind != KeyEventKind::Press {
                    continue;
                }
                match key_event.code {
                    KeyCode::Esc => break Ok(None),
                    KeyCode::Enter => break Ok(Some(input)),
                    KeyCode::Backspace => {
                        input.pop();
                    }
                    KeyCode::Char(c) => input.push(c),
                    _ => {}
                }
            }
            Event::Resize(_, _) => {
                reset_terminal_on_resize(terminal)?;
                terminal.draw(|frame| {
                    *viewport_area = frame.area();
                    draw(frame, browser);
                })?;
                execute!(io::stdout(), Show)?;
            }
            _ => {}
        }
    };

    execute!(io::stdout(), Hide)?;
    result
}

/// Like `read_cancelable_line` but keys a single option directly instead of typing text
/// followed by Enter: a letter from the header selects immediately, Esc cancels.
fn prompt_sort_key(
    terminal: &mut DefaultTerminal,
    browser: &Browser,
    viewport_area: &mut Rect,
) -> io::Result<Option<SortKey>> {
    let header = "sort by: (n)ame (s)ize (t)ype (c)reated (m)odified (a)ccessed (Esc to cancel)";
    execute!(io::stdout(), Show)?;

    let result = loop {
        terminal.set_cursor_position((0, viewport_area.bottom()))?;
        execute!(io::stdout(), Clear(ClearType::CurrentLine))?;
        print!("{header}");
        io::stdout().flush()?;

        match event::read()? {
            Event::Key(key_event) => {
                if key_event.kind != KeyEventKind::Press {
                    continue;
                }
                match key_event.code {
                    KeyCode::Esc => break Ok(None),
                    KeyCode::Char(c) => match c.to_ascii_lowercase() {
                        'n' => break Ok(Some(SortKey::Name)),
                        's' => break Ok(Some(SortKey::Size)),
                        't' => break Ok(Some(SortKey::Type)),
                        'c' => break Ok(Some(SortKey::Created)),
                        'm' => break Ok(Some(SortKey::Modified)),
                        'a' => break Ok(Some(SortKey::Accessed)),
                        _ => {}
                    },
                    _ => {}
                }
            }
            Event::Resize(_, _) => {
                reset_terminal_on_resize(terminal)?;
                terminal.draw(|frame| {
                    *viewport_area = frame.area();
                    draw(frame, browser);
                })?;
                execute!(io::stdout(), Show)?;
            }
            _ => {}
        }
    };

    execute!(io::stdout(), Hide)?;
    result
}

fn run_named_command(
    terminal: &mut DefaultTerminal,
    browser: &Browser,
    viewport_area: &mut Rect,
    program: &str,
) -> io::Result<()> {
    let cwd: &Path = &browser.cwd;
    let prompt = format!("{program} in {}: ", cwd.display());
    let Some(name) = read_cancelable_line(terminal, browser, viewport_area, None, &prompt)? else {
        terminal.clear()?;
        return Ok(());
    };
    let name = name.trim();

    if !name.is_empty() {
        disable_raw_mode()?;
        execute!(io::stdout(), MoveTo(0, viewport_area.bottom()), Show)?;

        let failed = match Command::new(program).arg(name).current_dir(cwd).status() {
            Ok(status) if !status.success() => {
                println!("{program} failed: {name} ({status})");
                true
            }
            Err(err) => {
                println!("{program} failed: {name} ({err})");
                true
            }
            _ => false,
        };

        if failed {
            println!("press Enter to return to the list...");
            let mut pause = String::new();
            io::stdin().read_line(&mut pause)?;
        }

        execute!(io::stdout(), Hide)?;
        enable_raw_mode()?;
    }

    terminal.clear()?;
    Ok(())
}

fn open_entry(terminal: &mut DefaultTerminal, viewport_area: Rect, cwd: &Path, name: &str) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(io::stdout(), MoveTo(0, viewport_area.bottom()), Show)?;

    let full_path = cwd.join(name);
    let failed = match Command::new("xdg-open").arg(&full_path).status() {
        Ok(status) if !status.success() => {
            println!("xdg-open failed: {} ({status})", full_path.display());
            true
        }
        Err(err) => {
            println!("xdg-open failed: {} ({err})", full_path.display());
            true
        }
        _ => false,
    };

    if failed {
        println!("press Enter to return to the list...");
        let mut pause = String::new();
        io::stdin().read_line(&mut pause)?;
    }

    execute!(io::stdout(), Hide)?;
    enable_raw_mode()?;
    terminal.clear()?;
    Ok(())
}

fn rename_entry(
    terminal: &mut DefaultTerminal,
    browser: &Browser,
    viewport_area: &mut Rect,
    old_name: &str,
) -> io::Result<()> {
    let cwd: &Path = &browser.cwd;
    let prompt = format!("rename {old_name} to: ");
    let Some(new_name) = read_cancelable_line(terminal, browser, viewport_area, None, &prompt)? else {
        terminal.clear()?;
        return Ok(());
    };
    let new_name = new_name.trim();

    if !new_name.is_empty() {
        disable_raw_mode()?;
        execute!(io::stdout(), MoveTo(0, viewport_area.bottom()), Show)?;

        let failed = match Command::new("mv").arg(old_name).arg(new_name).current_dir(cwd).status() {
            Ok(status) if !status.success() => {
                println!("mv failed: {old_name} -> {new_name} ({status})");
                true
            }
            Err(err) => {
                println!("mv failed: {old_name} -> {new_name} ({err})");
                true
            }
            _ => false,
        };

        if failed {
            println!("press Enter to return to the list...");
            let mut pause = String::new();
            io::stdin().read_line(&mut pause)?;
        }

        execute!(io::stdout(), Hide)?;
        enable_raw_mode()?;
    }

    terminal.clear()?;
    Ok(())
}

fn move_entry(terminal: &mut DefaultTerminal, viewport_area: Rect, sources: &[PathBuf], dest_dir: &Path) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(io::stdout(), MoveTo(0, viewport_area.bottom()), Show)?;

    let failed = match Command::new("mv").args(sources).arg(dest_dir).status() {
        Ok(status) if !status.success() => {
            println!("mv failed: {} -> {} ({status})", describe_sources(sources), dest_dir.display());
            true
        }
        Err(err) => {
            println!("mv failed: {} -> {} ({err})", describe_sources(sources), dest_dir.display());
            true
        }
        _ => false,
    };

    if failed {
        println!("press Enter to return to the list...");
        let mut pause = String::new();
        io::stdin().read_line(&mut pause)?;
    }

    execute!(io::stdout(), Hide)?;
    enable_raw_mode()?;
    terminal.clear()?;
    Ok(())
}

fn copy_entry(terminal: &mut DefaultTerminal, viewport_area: Rect, sources: &[PathBuf], dest_dir: &Path) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(io::stdout(), MoveTo(0, viewport_area.bottom()), Show)?;

    let failed = match Command::new("cp").arg("-r").args(sources).arg(dest_dir).status() {
        Ok(status) if !status.success() => {
            println!("cp failed: {} -> {} ({status})", describe_sources(sources), dest_dir.display());
            true
        }
        Err(err) => {
            println!("cp failed: {} -> {} ({err})", describe_sources(sources), dest_dir.display());
            true
        }
        _ => false,
    };

    if failed {
        println!("press Enter to return to the list...");
        let mut pause = String::new();
        io::stdin().read_line(&mut pause)?;
    }

    execute!(io::stdout(), Hide)?;
    enable_raw_mode()?;
    terminal.clear()?;
    Ok(())
}

const TERMINAL_CANDIDATES: &[&str] = &[
    "x-terminal-emulator", "konsole", "gnome-terminal", "xfce4-terminal",
    "alacritty", "kitty", "foot", "xterm",
];

/// Opens `command_str` in a terminal emulator, preferring `$TERMINAL` if set and otherwise
/// trying common emulators in turn until one actually launches.
fn spawn_in_terminal(cwd: &Path, command_str: &str) -> io::Result<()> {
    let held = format!("{command_str}; echo; read -p 'press Enter to close...' _");

    let mut candidates: Vec<String> = Vec::new();
    if let Ok(term) = env::var("TERMINAL") {
        candidates.push(term);
    }
    candidates.extend(TERMINAL_CANDIDATES.iter().map(|term| term.to_string()));

    let mut last_err = io::Error::new(io::ErrorKind::NotFound, "no terminal emulator found");
    for term in candidates {
        match Command::new(&term)
            .args(["-e", "sh", "-c", &held])
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(_) => return Ok(()),
            Err(err) => last_err = err,
        }
    }
    Err(last_err)
}

fn grep_entry(
    terminal: &mut DefaultTerminal,
    browser: &Browser,
    viewport_area: &mut Rect,
    name: &str,
) -> io::Result<()> {
    let cwd: &Path = &browser.cwd;
    let prompt = format!("grep [options] \"pattern\" on {name}: grep ");
    let Some(input) = read_cancelable_line(terminal, browser, viewport_area, None, &prompt)? else {
        terminal.clear()?;
        return Ok(());
    };
    let input = input.trim();

    if !input.is_empty() {
        let full_path = cwd.join(name);

        let out_prompt = "output file name (leave blank to open a new terminal instead): ";
        let Some(out_name) = read_cancelable_line(terminal, browser, viewport_area, None, out_prompt)? else {
            terminal.clear()?;
            return Ok(());
        };
        let out_name = out_name.trim();

        disable_raw_mode()?;
        execute!(io::stdout(), MoveTo(0, viewport_area.bottom()), Show)?;

        let failed = if out_name.is_empty() {
            let command_str = format!("grep {input} {}", full_path.display());
            // open in its own terminal window (held open until the user closes it) so the
            // match output isn't wiped by nav's own redraw once we return to the list
            match spawn_in_terminal(cwd, &command_str) {
                Ok(()) => false,
                Err(err) => {
                    println!("failed to open terminal: {err}");
                    true
                }
            }
        } else {
            let command_str = format!("grep {input} {} > {out_name}", full_path.display());
            match Command::new("sh").arg("-c").arg(&command_str).current_dir(cwd).status() {
                Ok(status) if !status.success() => {
                    println!("command failed: {command_str} ({status})");
                    true
                }
                Err(err) => {
                    println!("command failed: {command_str} ({err})");
                    true
                }
                _ => false,
            }
        };

        if failed {
            println!("press Enter to return to the list...");
            let mut pause = String::new();
            io::stdin().read_line(&mut pause)?;
        }

        // fully refresh nav's own pane rather than trusting its last remembered position
        execute!(io::stdout(), Clear(ClearType::All), MoveTo(0, 0), Hide)?;
        enable_raw_mode()?;
    }

    terminal.clear()?;
    Ok(())
}

fn run(terminal: &mut DefaultTerminal, browser: &mut Browser) -> io::Result<()> {
    let mut viewport_area = Rect::default();
    loop {
        terminal.draw(|frame| {
            viewport_area = frame.area();
            draw(frame, browser);
        })?;

        let event = event::read()?;
        if let Event::Resize(_, _) = event {
            reset_terminal_on_resize(terminal)?;
            continue;
        }

        if let Event::Key(key_event) = event {
            if key_event.kind != KeyEventKind::Press {
                continue;
            }

            let code = match key_event.code {
                KeyCode::Char(c) => KeyCode::Char(c.to_ascii_lowercase()),
                other => other,
            };

            match code {
                KeyCode::Char('q') => return Ok(()),
                KeyCode::Esc if browser.pending_action.take().is_none() => {
                    if browser.marked.is_empty() {
                        return Ok(());
                    }
                    browser.marked.clear();
                }
                KeyCode::Esc => {}
                KeyCode::Up | KeyCode::Char('k') => browser.move_selection(-1),
                KeyCode::Down | KeyCode::Char('j') => browser.move_selection(1),
                KeyCode::PageUp | KeyCode::Char('{') => browser.move_selection(-(PAGE_STEP as isize)),
                KeyCode::PageDown | KeyCode::Char('}') => browser.move_selection(PAGE_STEP as isize),
                KeyCode::Char('.') => browser.go_up(),
                KeyCode::Char('a') if key_event.modifiers.contains(KeyModifiers::ALT) => browser.toggle_hidden(),
                KeyCode::Char('m') => {
                    show_cancelable_prompt(terminal, browser, "mkdir")?;
                    run_named_command(terminal, browser, &mut viewport_area, "mkdir")?;
                    browser.refresh();
                }
                KeyCode::Char('t') => {
                    show_cancelable_prompt(terminal, browser, "touch")?;
                    run_named_command(terminal, browser, &mut viewport_area, "touch")?;
                    browser.refresh();
                }
                KeyCode::Char('c') => {
                    if let Some(entry) = browser.selected_entry()
                        && entry.is_dir
                    {
                        let name = entry.name.clone();
                        browser.enter_dir(&name);
                    }
                }
                KeyCode::Char('o') => {
                    if let Some(name) = browser.selected_entry().map(|entry| entry.name.clone()) {
                        open_entry(terminal, viewport_area, &browser.cwd, &name)?;
                    }
                }
                KeyCode::Char('r') => {
                    if let Some(name) = browser.selected_entry().map(|entry| entry.name.clone()) {
                        show_cancelable_prompt(terminal, browser, "rename")?;
                        rename_entry(terminal, browser, &mut viewport_area, &name)?;
                        browser.refresh();
                    }
                }
                KeyCode::Char('g') => {
                    if let Some(name) = browser.selected_entry().map(|entry| entry.name.clone()) {
                        show_cancelable_prompt(terminal, browser, "grep")?;
                        grep_entry(terminal, browser, &mut viewport_area, &name)?;
                        browser.refresh();
                    }
                }
                KeyCode::Char('s') if key_event.modifiers.contains(KeyModifiers::ALT) => {
                    show_cancelable_prompt(terminal, browser, "sort")?;
                    if let Some(sort_key) = prompt_sort_key(terminal, browser, &mut viewport_area)? {
                        browser.sort_key = sort_key;
                    }
                    terminal.clear()?;
                    browser.refresh();
                }
                KeyCode::Char('s') => browser.toggle_mark(),
                KeyCode::Char('e') => {
                    match browser.pending_action.take() {
                        Some(PendingAction::Move(sources)) => match browser.selected_entry() {
                            Some(entry) if entry.is_dir => {
                                let dest = browser.cwd.join(&entry.name);
                                move_entry(terminal, viewport_area, &sources, &dest)?;
                                browser.marked.clear();
                                browser.refresh();
                            }
                            _ => browser.pending_action = Some(PendingAction::Move(sources)),
                        },
                        other => {
                            browser.pending_action = browser.pending_sources().map(PendingAction::Move).or(other);
                        }
                    }
                }
                KeyCode::Char('y') => {
                    match browser.pending_action.take() {
                        Some(PendingAction::Copy(sources)) => match browser.selected_entry() {
                            Some(entry) if entry.is_dir => {
                                let dest = browser.cwd.join(&entry.name);
                                copy_entry(terminal, viewport_area, &sources, &dest)?;
                                browser.marked.clear();
                                browser.refresh();
                            }
                            _ => browser.pending_action = Some(PendingAction::Copy(sources)),
                        },
                        other => {
                            browser.pending_action = browser.pending_sources().map(PendingAction::Copy).or(other);
                        }
                    }
                }
                KeyCode::Char('d') => {
                    if let Some(name) = browser.selected_entry().map(|entry| entry.name.clone()) {
                        show_cancelable_prompt(terminal, browser, "cmd")?;
                        run_command_on_entry(terminal, browser, &mut viewport_area, &name)?;
                        browser.refresh();
                    }
                }
                _ => {}
            }
        }
    }
}

fn main() -> io::Result<()> {
    let cwd = env::current_dir()?;
    let show_hidden = false;
    let sort_key = SortKey::Name;
    let entries = list_entries(&cwd, show_hidden, sort_key)?;
    let mut browser = Browser {
        cwd, entries, selected: 0, show_hidden, sort_key,
        pending_action: None, marked: HashSet::new(), status: None,
    };

    // clear terminal before drawing so the pane starts on a clean screen
    execute!(io::stdout(), Clear(ClearType::All), MoveTo(0, 0))?;

    // draw up to PANE_HEIGHT lines
    let mut terminal = ratatui::init_with_options(TerminalOptions {
        viewport: Viewport::Inline(PANE_HEIGHT),
    });
    let result = run(&mut terminal, &mut browser);
    ratatui::restore();

    // clear terminal
    execute!(io::stdout(), Clear(ClearType::All), MoveTo(0, 0))?;

    result
}
