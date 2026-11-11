use std::{ env, fs, io };
use std::path::{ Path, PathBuf };
use ratatui::{
    crossterm:: {
        event::{ self, Event, KeyCode, KeyEventKind },
        cursor::MoveTo, execute,
        terminal::{ Clear, ClearType },
    },
    style::{ Modifier, Style },
    text::{ Line, Span },
    widgets::{ Block, Paragraph },
    DefaultTerminal, 
    Frame, 
    TerminalOptions, 
    Viewport,
};

const PANE_HEIGHT: u16 = 8;
const MAX_NAME_WIDTH: usize = 20;
const DIR_ICON: &str = "📁";
const FILE_ICON: &str = "📄";
const EXE_ELF_ICON: &str = "➤";


struct Knowledge {
    cwd: PathBuf,
    dirs: Vec<String>,
 //   files: Vec<String>,
 //   exe_elfs: Vec<String>,
    selected: usize,
    scroll: usize,
    columns: usize,
    status: Option<String>,
}

impl Knowledge {
    fn new(cwd: PathBuf) -> io::Result<Self> {
        let dirs = list_dirs(&cwd)?;
        Ok(Self { cwd, dirs, selected: 0, scroll: 0, columns: 1, status: None })
    }

    /// Enter: open the highlighted directory
    fn enter_selected(&mut self) {
        if let Some(name) = self.dirs.get(self.selected) {
            let path = self.cwd.join(name);
            self.open(path);
        }
    }

    /// Arrow keys: move the highlight around the grid
    fn move_selected(&mut self, key: KeyCode) {
        if self.dirs.is_empty() 
        { return; }

        let last = self.dirs.len() - 1;
        let cols = self.columns;
        let i = self.selected;

        self.selected = match key {
            KeyCode::Left => i.saturating_sub(1),
            KeyCode::Right => (i + 1).min(last),
            KeyCode::Up => i.saturating_sub(cols),
            KeyCode::Down => (i + cols).min(last),
            _ => i,
        };
    }

    /// show `path`'s subdirectories. If it can't be read, say put and say why.
    fn open(&mut self, path: PathBuf) {
        match list_dirs(&path) {
            Ok(dirs) => {
                self.cwd = path;
                self.dirs = dirs;
                self.selected = 0;
                self.scroll = 0;
                self.status = None;
            }
            Err(error) => {
                self.status = Some(format!("can't open {}: {}", path.display(), error));
            }
        }
    }
}

fn run(terminal: &mut DefaultTerminal, knowledge: &mut Knowledge) -> io::Result<()> {
    loop {
        terminal.draw(|frame| draw(frame, knowledge))?;
        if let Event::Key(event_key) = event::read()? {
            if event_key.kind != KeyEventKind::Press 
            { continue; }

            match event_key.code {
                KeyCode::Char('Q') => return Ok(()),
                KeyCode::Char('q') => return Ok(()),
                KeyCode::Esc => return Ok(()),

                KeyCode::Enter => knowledge.enter_selected(),

                KeyCode::Left |
                KeyCode::Right |
                KeyCode::Up |
                KeyCode::Down => knowledge.move_selected(event_key.code),

                _ => {}
            }
        }
    }
}

fn truncate(name: &str, max_width: usize) -> String {
    if name.chars().count() > max_width {
        let mut truncated: String = name.chars().take(max_width - 1).collect();
        truncated.push('…');
        truncated
    } else {
        name.to_string()
    }
}

/*
fn grid_rows(dirs: &[String], width: usize) -> Vec<Line> {
    let longest = dirs.iter().map(|dir| dir.chars().count()).max().unwrap_or(0);
    let name_width = longest.min(MAX_NAME_WIDTH);
    let columns = (width / (name_width + 4)).max(1);
    
    dirs.chunks(columns) // one slice per row
    .map(|row| {
        row.iter().map(
            |name| format!("{} {:<width$} ", DIR_ICON, truncate(name, name_width))
        ).collect()
    }).collect()
}
*/

fn grid_shape(dirs: &[String], width: usize) -> (usize, usize) {
    let longest = dirs.iter().map(|dir| dir.chars().count()).max().unwrap_or(0);
    let name_width = longest.min(MAX_NAME_WIDTH);
    let cell_width = (width / (name_width + 4)).max(1);
    (name_width, (width / cell_width).max(1))
}

fn grid_lines(dirs: &[String], selected: usize, name_width: usize, columns: usize) -> Vec<Line<'static>> {
    dirs.chunks(columns)
        .enumerate()
        .map(|(row, names)| {
            let cells: Vec<Span<'static>> = names
                .iter()
                .enumerate()
                .map(|(column, name)| {
                    let text = format!("{} {:<name_width$} ", DIR_ICON, truncate(name, name_width));
                    if row * columns + column == selected {
                        Span::styled(text, Style::new().add_modifier(Modifier::REVERSED))
                    } else {
                        Span::raw(text)
                    }
                })
                .collect();
            Line::from(cells)
        })
        .collect()
}

fn draw(frame: &mut Frame, knowledge: &mut Knowledge) {
    let mut block = Block::bordered().title(format!(" {}", knowledge.cwd.display()));
    if let Some(status) = &knowledge.status {
        block = block.title(format!(" {} {}", knowledge.cwd.display(), status));
    }
    let inner = block.inner(frame.area());

    if knowledge.dirs.is_empty() {
        frame.render_widget(Paragraph::new(Line::from("(no subdirectories)")).block(block), frame.area());
        return;
    } 

    let (name_width, columns) = grid_shape(&knowledge.dirs, inner.width as usize);
    let visible_rows = (inner.height as usize).max(1);
    let selected_row = knowledge.selected / columns;
    if selected_row < knowledge.scroll {
        knowledge.scroll = selected_row;
    } else if selected_row >= knowledge.scroll + visible_rows {
        knowledge.scroll = selected_row - visible_rows + 1;
    }

    let lines: Vec<Line> = grid_lines(&knowledge.dirs, knowledge.selected, name_width, columns)
        .into_iter()
        .skip(knowledge.scroll)
        .take(visible_rows)
        .collect();
    
    frame.render_widget(Paragraph::new(lines).block(block), frame.area());
}

fn list_dirs(path: &Path) -> io::Result<Vec<String>> {
    let mut dirs: Vec<String> = fs::read_dir(path)?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    dirs.sort_by_key(|name| name.to_lowercase()); // case-insensitive, like Explorer
    Ok(dirs)
}

fn main() -> io::Result<()> {
    /*
    let cwd = env::current_dir()?;
    let dirs = fs::read_dir(&cwd)?
    .filter_map(|entry| entry.ok())
    .filter(|entry| entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false))
    .map(|entry| entry.file_name().into_string().unwrap_or_default())
    .collect::<Vec<_>>();
    */

    let mut knowledge = Knowledge::new(env::current_dir()?)?;

    // draw up to PANE_HEIGHT lines
    let mut terminal = ratatui::init_with_options(TerminalOptions {
        viewport: Viewport::Inline(PANE_HEIGHT),
    });
    let result = run(&mut terminal, &mut knowledge);
    ratatui::restore();

    // clear terminal
    execute!(io::stdout(), Clear(ClearType::All), MoveTo(0, 0))?;

    result
}
