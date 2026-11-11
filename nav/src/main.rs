use std::{ env, fs, io };
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{ Path, PathBuf };
use std::process::Command;
use ratatui::{
    crossterm:: {
        cursor::{ Hide, MoveTo, Show },
        event::{ self, Event, KeyCode, KeyEventKind },
        execute,
        terminal::{ Clear, ClearType, disable_raw_mode, enable_raw_mode },
    },
    layout::Rect,
    style::{ Modifier, Style },
    widgets::{ Block, List, ListItem, ListState },
    DefaultTerminal,
    Frame,
    TerminalOptions,
    Viewport,
};

const PANE_HEIGHT: u16 = 8;
const PAGE_STEP: usize = PANE_HEIGHT as usize - 2; // visible rows inside the border
const DIR_ICON: &str = "📁";
const FILE_ICON: &str = "📄";
const EXE_ELF_ICON: &str = "➤";

struct FileEntry {
    name: String,
    is_dir: bool,
}

struct Browser {
    cwd: PathBuf,
    entries: Vec<FileEntry>,
    selected: usize,
}

impl Browser {
    fn selected_entry(&self) -> Option<&FileEntry> {
        self.entries.get(self.selected)
    }

    fn move_selection(&mut self, delta: isize) {
        if self.entries.is_empty() {
            return;
        }
        let max = (self.entries.len() - 1) as isize;
        self.selected = (self.selected as isize + delta).clamp(0, max) as usize;
    }

    fn go_up(&mut self) -> io::Result<()> {
        if let Some(parent) = self.cwd.parent().map(Path::to_path_buf) {
            self.cwd = parent;
            self.entries = list_entries(&self.cwd)?;
            self.selected = 0;
        }
        Ok(())
    }

    fn enter_dir(&mut self, name: &str) -> io::Result<()> {
        self.cwd = self.cwd.join(name);
        self.entries = list_entries(&self.cwd)?;
        self.selected = 0;
        Ok(())
    }
}

fn list_entries(path: &Path) -> io::Result<Vec<FileEntry>> {
    let mut entries: Vec<FileEntry> = fs::read_dir(path)?
        .filter_map(|entry| entry.ok())
        .map(|entry| FileEntry {
            name: entry.file_name().to_string_lossy().into_owned(),
            is_dir: entry.path().is_dir(),
        })
        .collect();
    // directories first, then files, each alphabetical (case-insensitive)
    entries.sort_by_key(|entry| (!entry.is_dir, entry.name.to_lowercase()));
    Ok(entries)
}

fn is_executable(path: &Path) -> bool {
    fs::metadata(path)
        .map(|meta| meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

fn draw(frame: &mut Frame, browser: &Browser) {
    let title = format!(" {} ", browser.cwd.display());
    let hints = " ↑/↓ select, PgUp/PgDn page, Enter: run command, c: step into dir, . : up a dir, q: quit ";
    let block = Block::bordered().title_top(title).title_bottom(hints);

    let items: Vec<ListItem> = if browser.entries.is_empty() {
        vec![ListItem::new("(empty)")]
    } else {
        browser.entries.iter()
            .map(|entry| {
                let icon = if entry.is_dir {
                    DIR_ICON
                } else if is_executable(&browser.cwd.join(&entry.name)) {
                    EXE_ELF_ICON
                } else {
                    FILE_ICON
                };
                ListItem::new(format!("{icon} {}", entry.name))
            })
            .collect()
    };

    let list = List::new(items)
        .block(block)
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED))
        .highlight_symbol("> ");

    let mut state = ListState::default();
    if !browser.entries.is_empty() {
        state.select(Some(browser.selected));
    }
    frame.render_stateful_widget(list, frame.area(), &mut state);
}

fn run_command_on_entry(
    terminal: &mut DefaultTerminal,
    viewport_area: Rect,
    cwd: &Path,
    name: &str,
) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(io::stdout(), MoveTo(0, viewport_area.bottom()), Show)?;

    print!("command on {name} (use {{}} for the path, otherwise it's appended): ");
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let input = input.trim();

    if !input.is_empty() {
        let full_path = cwd.join(name);
        let command_str = if input.contains("{}") {
            input.replace("{}", &full_path.to_string_lossy())
        } else {
            format!("{input} {}", full_path.display())
        };

        match Command::new("sh").arg("-c").arg(&command_str).current_dir(cwd).status() {
            Ok(status) if !status.success() => println!("(command exited with {status})"),
            Err(err) => println!("(failed to run command: {err})"),
            _ => {}
        }

        println!("press Enter to return to the list...");
        let mut pause = String::new();
        io::stdin().read_line(&mut pause)?;
    }

    execute!(io::stdout(), Hide)?;
    enable_raw_mode()?;
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

        if let Event::Key(key_event) = event::read()? {
            if key_event.kind != KeyEventKind::Press {
                continue;
            }

            match key_event.code {
                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                KeyCode::Up | KeyCode::Char('k') => browser.move_selection(-1),
                KeyCode::Down | KeyCode::Char('j') => browser.move_selection(1),
                KeyCode::PageUp => browser.move_selection(-(PAGE_STEP as isize)),
                KeyCode::PageDown => browser.move_selection(PAGE_STEP as isize),
                KeyCode::Char('.') => browser.go_up()?,
                KeyCode::Char('c') => {
                    if let Some(entry) = browser.selected_entry() {
                        if entry.is_dir {
                            let name = entry.name.clone();
                            browser.enter_dir(&name)?;
                        }
                    }
                }
                KeyCode::Enter => {
                    if let Some(name) = browser.selected_entry().map(|entry| entry.name.clone()) {
                        run_command_on_entry(terminal, viewport_area, &browser.cwd, &name)?;
                    }
                }
                _ => {}
            }
        }
    }
}

fn main() -> io::Result<()> {
    let cwd = env::current_dir()?;
    let entries = list_entries(&cwd)?;
    let mut browser = Browser { cwd, entries, selected: 0 };

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
