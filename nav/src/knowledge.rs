
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
        Ok(self { cwd, dirs, selected: 0, scroll: o, columns: 1, status: None })
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
        if self.dirs.is_empty() { return; }

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