use std::{env, io, path::PathBuf, process::ExitCode};

use facet_tui::{App, Appearance, Depth, Theme};
use ratatui::{Terminal, backend::CrosstermBackend};

fn main() -> ExitCode {
    let cli = match parse_args(env::args().skip(1)) {
        Ok(cli) => cli,
        Err(message) => {
            if message == "help" {
                print_help();
                return ExitCode::SUCCESS;
            }
            eprintln!("facet-tui: {message}");
            print_help();
            return ExitCode::from(2);
        }
    };

    let mut terminal = match setup_terminal() {
        Ok(terminal) => terminal,
        Err(error) => {
            eprintln!("facet-tui: failed to enter raw mode: {error}");
            return ExitCode::from(1);
        }
    };

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("facet-tui: failed to start tokio runtime: {error}");
            let _ = restore_terminal();
            return ExitCode::from(1);
        }
    };

    let exit_code = runtime.block_on(async {
        let mut app = App::load(cli.path.as_deref()).await;
        if let Some(appearance) = cli.appearance {
            app.apply_theme(Theme::new(appearance).with_depth(Depth::from_env()));
        }
        if let Some(theme) = &cli.theme {
            app.apply_theme_file(theme);
        }
        app.refresh_live_theme();
        let result = app.run(&mut terminal).await;
        if let Err(error) = result {
            eprintln!("facet-tui: {error}");
            ExitCode::from(1)
        } else {
            ExitCode::SUCCESS
        }
    });

    if let Err(error) = restore_terminal() {
        eprintln!("facet-tui: failed to restore terminal: {error}");
    }
    exit_code
}

struct Cli {
    appearance: Option<Appearance>,
    theme: Option<String>,
    path: Option<PathBuf>,
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Cli, String> {
    let mut appearance = None;
    let mut theme = None;
    let mut path = None;
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        if arg == "--help" || arg == "-h" {
            return Err("help".to_string());
        }
        if arg == "--appearance" {
            let value = iter.next().ok_or_else(|| {
                " --appearance requires graphite or porcelain"
                    .trim()
                    .to_string()
            })?;
            appearance = Some(
                Appearance::from_flag(&value)
                    .ok_or_else(|| format!("unknown appearance '{value}'"))?,
            );
            continue;
        }
        if arg == "--theme" {
            let value = iter
                .next()
                .ok_or_else(|| "--theme requires a theme name or path".to_string())?;
            theme = Some(value);
            continue;
        }
        if let Some(value) = arg.strip_prefix("--appearance=") {
            appearance = Some(
                Appearance::from_flag(value)
                    .ok_or_else(|| format!("unknown appearance '{value}'"))?,
            );
            continue;
        }
        if let Some(value) = arg.strip_prefix("--theme=") {
            theme = Some(value.to_string());
            continue;
        }
        if path.is_none() && !arg.starts_with('-') {
            path = Some(PathBuf::from(arg));
            continue;
        }
        return Err(format!("unexpected argument: {arg}"));
    }
    Ok(Cli {
        appearance,
        theme,
        path,
    })
}

fn print_help() {
    eprintln!(
        "Usage: facet-tui [--appearance graphite|porcelain] [--theme <name|path>] [collection.yml]\n\
         \n\
         Graphite Honey is the default. Porcelain Honey is the light appearance.\n\
         --theme loads a theme file; invalid files fall back to the built-in.\n\
         Keys: j/k move · Enter send · i insert · : command · ? help · q quit"
    );
}

fn setup_terminal() -> io::Result<Terminal<CrosstermBackend<io::Stdout>>> {
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(
        stdout,
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::EnableMouseCapture,
        crossterm::event::EnableFocusChange,
        crossterm::cursor::Hide
    )?;
    let backend = CrosstermBackend::new(stdout);
    Terminal::new(backend)
}

fn restore_terminal() -> io::Result<()> {
    crossterm::terminal::disable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(
        stdout,
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::event::DisableMouseCapture,
        crossterm::event::DisableFocusChange,
        crossterm::cursor::Show
    )?;
    Ok(())
}
