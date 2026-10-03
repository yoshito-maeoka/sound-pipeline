//! Terminal UI: chain view, filter picker, parameter editors, mouse support.

mod app;
mod editors;
mod layout;
mod view;

use std::io::stdout;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use ratatui::crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event};
use ratatui::crossterm::execute;

use crate::audio::decode_file;
use crate::engine::EngineHandle;
use crate::plugin::Registry;

use app::App;

/// Decode `path`, start the engine on `device`, and run the UI until quit.
pub fn run(registry: Registry, path: PathBuf, device: Option<String>) -> Result<()> {
    let data = decode_file(&path).with_context(|| format!("cannot open '{}'", path.display()))?;
    let engine = EngineHandle::start(data, device).context("cannot start audio engine")?;
    let mut app = App::new(registry, engine, path);

    let mut terminal = ratatui::init();
    // ratatui::init installed a panic hook that restores the terminal; also drop mouse capture.
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(stdout(), DisableMouseCapture);
        prev(info);
    }));
    let _ = execute!(stdout(), EnableMouseCapture);

    let result = event_loop(&mut terminal, &mut app);

    let _ = execute!(stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}

fn event_loop(terminal: &mut ratatui::DefaultTerminal, app: &mut App) -> Result<()> {
    while !app.quit {
        app.tick();
        terminal.draw(|f| view::draw(f, app))?;
        if event::poll(Duration::from_millis(33))? {
            loop {
                match event::read()? {
                    Event::Key(k) => app.handle_key(k),
                    Event::Mouse(m) => app.handle_mouse(m),
                    _ => {}
                }
                if app.quit || !event::poll(Duration::ZERO)? {
                    break;
                }
            }
        }
    }
    Ok(())
}
