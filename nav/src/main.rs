use std::{ env, fs, io };
use std::path::{ Path, PathBuf };
use ratatui::{
    crossterm:: {
        event::{ self, Event, KeyCode, KeyEventKind },
        cursor::MoveTo, execute,
        terminal::{ Clear, ClearType },
    },
    text::Line,
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
}

fn run(terminal: &mut DefaultTerminal, knowledge: &Knowledge) -> io::Result<()> {
    loop {
        terminal.draw(|frame| draw(frame, knowledge))?;
        if let Event::Key(key_event) = event::read()? {

            if key_event.kind == KeyEventKind::Press {
                match key_event.code {
                    KeyCode::Char('q') => return Ok(()),
                    KeyCode::Esc => return Ok(()),
                    _ => {}
                }
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

fn draw(frame: &mut Frame, knowledge: &Knowledge) {
    let block = Block::bordered().title(format!(" {}", knowledge.cwd.display()));
    let inner = block.inner(frame.area());

    let lines: Vec<Line> = if knowledge.dirs.is_empty() {
        vec![Line::from("(no subdirectories)")]
    } else {
        grid_rows(&knowledge.dirs, inner.width as usize)
            .into_iter()
            .map(Line::from)
            .collect()
    };
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
    let cwd = env::current_dir()?;
    let dirs = fs::read_dir(&cwd)?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false))
        .map(|entry| entry.file_name().into_string().unwrap_or_default())
        .collect::<Vec<_>>();

    let knowledge = Knowledge {
        cwd, dirs
    };

    // draw up to PANE_HEIGHT lines
    let mut terminal = ratatui::init_with_options(TerminalOptions {
        viewport: Viewport::Inline(PANE_HEIGHT),
    });
    let result = run(&mut terminal, &knowledge);
    ratatui::restore();

    // clear terminal
    execute!(io::stdout(), Clear(ClearType::All), MoveTo(0, 0))?;

    result
}
