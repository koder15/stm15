mod app;
mod tunnel;

use app::{execute_action, Action, App};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tunnel::TunnelManager;

fn main() -> anyhow::Result<()> {
    let config_path = std::env::args().nth(1).map(std::path::PathBuf::from);

    let mgr = TunnelManager::new(config_path);
    let manager = Arc::new(Mutex::new(mgr));

    // Start enabled tunnels
    {
        let m = manager.lock().unwrap();
        m.start_all_enabled();
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;

    // Spawn health check loop
    {
        let mgr = manager.clone();
        rt.spawn(tunnel::health_check_loop(mgr));
    }

    // Terminal setup
    enable_raw_mode()?;
    std::io::stdout().execute(EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;

    let mut app = App::new(manager.clone());

    let result = run_tui(&rt, &mut terminal, &mut app, manager.clone());

    // Cleanup
    disable_raw_mode()?;
    std::io::stdout().execute(LeaveAlternateScreen)?;

    // Stop all tunnels before exit
    {
        if let Ok(mgr) = manager.lock() {
            mgr.stop_all();
        }
    }

    result
}

fn run_tui(
    rt: &tokio::runtime::Runtime,
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
    manager: Arc<Mutex<TunnelManager>>,
) -> anyhow::Result<()> {
    loop {
        // Drain pending actions
        let actions = app.take_actions();
        for action in actions {
            let is_quit = matches!(action, Action::Quit);
            rt.block_on(execute_action(action, manager.clone(), app));
            if is_quit {
                return Ok(());
            }
        }

        // Handle input
        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press || key.kind == KeyEventKind::Repeat {
                    if key.code == KeyCode::Char('c')
                        && key.modifiers == crossterm::event::KeyModifiers::CONTROL
                    {
                        return Ok(());
                    }
                    app.handle_key(key);
                }
            }
            if let Event::Resize(_, _) = event::read().unwrap_or(Event::Resize(0, 0)) {
                // resize is handled on next draw
            }
        }

        // Redraw
        terminal.draw(|f| app.render(f))?;
    }
}
