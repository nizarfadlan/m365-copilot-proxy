use std::io;
use std::path::{Path, PathBuf};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use tracing::info;

use crate::bootstrap::BootstrapReport;
use crate::cdp::{list_detected_browsers, DetectedBrowser};
use crate::config::{AppConfig, ServeOverrides};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Welcome,
    Browser,
    Server,
    Options,
    Confirm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditField {
    Host,
    Port,
    CdpPort,
    CustomBrowserPath,
}

struct OnboardState {
    config: AppConfig,
    step: Step,
    browsers: Vec<DetectedBrowser>,
    /// 0 = auto-detect, 1..=browsers.len() = pick from list, browsers.len()+1 = custom path
    browser_choice: usize,
    custom_browser_path: String,
    editing: Option<EditField>,
    option_idx: usize,
    cancelled: bool,
}

pub fn needs_onboarding(report: &BootstrapReport, _config: &AppConfig) -> bool {
    report.config_created
}

pub fn run_onboarding(initial: &AppConfig, config_path: &Path) -> Result<AppConfig, String> {
    if !atty::is(atty::Stream::Stdout) {
        return Err(
            "onboarding requires an interactive terminal (TTY). Run from a terminal, not a pipe."
                .into(),
        );
    }

    let mut state = OnboardState::new(initial.clone());
    let result = run_onboard_tui(&mut state)?;
    if state.cancelled {
        return Err("onboarding cancelled".into());
    }
    if !result {
        return Err("onboarding cancelled".into());
    }

    apply_browser_choice(&mut state);
    state.config.save_to(config_path)?;
    info!(path = %config_path.display(), "saved onboarding configuration");
    Ok(state.config)
}

pub fn maybe_run_onboarding(
    report: &BootstrapReport,
    overrides: &ServeOverrides,
    config: &AppConfig,
    config_path: &Path,
) -> Result<AppConfig, String> {
    if overrides.skip_onboarding || !needs_onboarding(report, config) {
        return Ok(config.clone());
    }
    if !atty::is(atty::Stream::Stdout) || !config.ui.tui {
        eprintln!(
            "First run: configuration written to {}.",
            config_path.display()
        );
        eprintln!("Run `copilot-openai-proxy onboard` to configure interactively.");
        return Ok(config.clone());
    }
    run_onboarding(config, config_path)
}

fn apply_browser_choice(state: &mut OnboardState) {
    if state.browser_choice == 0 {
        state.config.edge.executable = None;
        return;
    }
    if state.browser_choice <= state.browsers.len() {
        let browser = &state.browsers[state.browser_choice - 1];
        state.config.edge.executable = Some(browser.path.clone());
        return;
    }
    let path = state.custom_browser_path.trim();
    if path.is_empty() {
        state.config.edge.executable = None;
    } else {
        state.config.edge.executable = Some(PathBuf::from(path));
    }
}

impl OnboardState {
    fn new(mut config: AppConfig) -> Self {
        let browsers = list_detected_browsers();
        let mut custom_browser_path = String::new();
        let browser_choice = if let Some(ref exec) = config.edge.executable {
            browsers
                .iter()
                .position(|b| b.path == *exec)
                .map(|i| i + 1)
                .unwrap_or_else(|| {
                    custom_browser_path = exec.display().to_string();
                    browsers.len() + 1
                })
        } else {
            0
        };

        #[cfg(target_os = "macos")]
        if config.ui.tray {
            config.ui.tray = false;
        }

        Self {
            config,
            step: Step::Welcome,
            browsers,
            browser_choice,
            custom_browser_path,
            editing: None,
            option_idx: 0,
            cancelled: false,
        }
    }
}

fn run_onboard_tui(state: &mut OnboardState) -> Result<bool, String> {
    enable_raw_mode().map_err(|e| e.to_string())?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).map_err(|e| e.to_string())?;
    let mut terminal = ratatui::init();

    let mut saved = false;

    loop {
        terminal
            .draw(|frame| draw_onboard(frame, state))
            .map_err(|e| e.to_string())?;

        if event::poll(std::time::Duration::from_millis(100)).map_err(|e| e.to_string())? {
            if let Event::Key(key) = event::read().map_err(|e| e.to_string())? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                if handle_key(state, key.code, key.modifiers) {
                    if state.cancelled {
                        break;
                    }
                    if state.step == Step::Confirm {
                        saved = true;
                        break;
                    }
                }
            }
        }

        if saved {
            break;
        }
    }

    disable_raw_mode().map_err(|e| e.to_string())?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen).map_err(|e| e.to_string())?;
    ratatui::restore();
    Ok(saved)
}

fn handle_key(state: &mut OnboardState, code: KeyCode, modifiers: KeyModifiers) -> bool {
    if let Some(field) = state.editing {
        return handle_edit_key(state, field, code, modifiers);
    }

    match state.step {
        Step::Welcome => match code {
            KeyCode::Enter => {
                state.step = Step::Browser;
            }
            KeyCode::Char('q') | KeyCode::Esc => state.cancelled = true,
            _ => {}
        },
        Step::Browser => match code {
            KeyCode::Up | KeyCode::Char('k') => {
                state.browser_choice = state.browser_choice.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let max = state.browsers.len() + 1;
                state.browser_choice = (state.browser_choice + 1).min(max);
            }
            KeyCode::Enter => {
                if state.browser_choice == state.browsers.len() + 1 {
                    state.editing = Some(EditField::CustomBrowserPath);
                } else {
                    state.step = Step::Server;
                }
            }
            KeyCode::Char('q') | KeyCode::Esc => state.cancelled = true,
            _ => {}
        },
        Step::Server => match code {
            KeyCode::Up | KeyCode::Char('k') => {
                state.option_idx = state.option_idx.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                state.option_idx = (state.option_idx + 1).min(2);
            }
            KeyCode::Enter => match state.option_idx {
                0 => state.editing = Some(EditField::Host),
                1 => state.editing = Some(EditField::Port),
                2 => state.editing = Some(EditField::CdpPort),
                _ => {}
            },
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                state.step = Step::Options;
                state.option_idx = 0;
            }
            KeyCode::Char('q') | KeyCode::Esc => state.cancelled = true,
            _ => {}
        },
        Step::Options => match code {
            KeyCode::Up | KeyCode::Char('k') => {
                state.option_idx = state.option_idx.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                state.option_idx = (state.option_idx + 1).min(4);
            }
            KeyCode::Enter | KeyCode::Char(' ') => toggle_option(state),
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                state.step = Step::Confirm;
            }
            KeyCode::Char('q') | KeyCode::Esc => state.cancelled = true,
            _ => {}
        },
        Step::Confirm => match code {
            KeyCode::Enter => return true,
            KeyCode::Char('q') | KeyCode::Esc => state.cancelled = true,
            _ => {}
        },
    }
    false
}

fn handle_edit_key(
    state: &mut OnboardState,
    field: EditField,
    code: KeyCode,
    modifiers: KeyModifiers,
) -> bool {
    match code {
        KeyCode::Esc => state.editing = None,
        KeyCode::Enter => {
            apply_edit_field(state, field);
            state.editing = None;
            if state.step == Step::Browser && field == EditField::CustomBrowserPath {
                state.step = Step::Server;
            }
        }
        KeyCode::Backspace => match field {
            EditField::CustomBrowserPath => {
                state.custom_browser_path.pop();
            }
            EditField::Host => {
                state.config.server.host.pop();
            }
            EditField::Port => {
                state.config.server.port = pop_digit(state.config.server.port);
            }
            EditField::CdpPort => {
                state.config.edge.cdp_port = pop_digit(state.config.edge.cdp_port);
            }
        },
        KeyCode::Char(c) if !modifiers.contains(KeyModifiers::CONTROL) => match field {
            EditField::CustomBrowserPath => state.custom_browser_path.push(c),
            EditField::Host => state.config.server.host.push(c),
            EditField::Port => {
                if c.is_ascii_digit() {
                    state.config.server.port =
                        state.config.server.port.saturating_mul(10) + (c as u16 - b'0' as u16);
                }
            }
            EditField::CdpPort => {
                if c.is_ascii_digit() {
                    state.config.edge.cdp_port =
                        state.config.edge.cdp_port.saturating_mul(10) + (c as u16 - b'0' as u16);
                }
            }
        },
        _ => {}
    }
    false
}

fn apply_edit_field(state: &mut OnboardState, field: EditField) {
    match field {
        EditField::Host if state.config.server.host.is_empty() => {
            state.config.server.host = "127.0.0.1".into();
        }
        EditField::Port if state.config.server.port == 0 => {
            state.config.server.port = 8000;
        }
        EditField::CdpPort if state.config.edge.cdp_port == 0 => {
            state.config.edge.cdp_port = 9222;
        }
        _ => {}
    }
}

fn pop_digit(mut value: u16) -> u16 {
    value /= 10;
    value
}

fn toggle_option(state: &mut OnboardState) {
    match state.option_idx {
        0 => state.config.edge.launch_on_start = !state.config.edge.launch_on_start,
        1 => state.config.token.auto_refresh = !state.config.token.auto_refresh,
        2 => state.config.token.capture_on_start = !state.config.token.capture_on_start,
        3 => state.config.ui.tui = !state.config.ui.tui,
        4 => state.config.ui.tray = !state.config.ui.tray,
        _ => {}
    }
}

fn draw_onboard(frame: &mut ratatui::Frame, state: &OnboardState) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(8), Constraint::Length(3)])
        .split(area);

    let title = Paragraph::new(" M365 Copilot Proxy — Setup")
        .style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .block(Block::default().borders(Borders::ALL).title("Onboarding"));
    frame.render_widget(title, chunks[0]);

    match state.step {
        Step::Welcome => draw_welcome(frame, chunks[1]),
        Step::Browser => draw_browser(frame, state, chunks[1]),
        Step::Server => draw_server(frame, state, chunks[1]),
        Step::Options => draw_options(frame, state, chunks[1]),
        Step::Confirm => draw_confirm(frame, state, chunks[1]),
    }

    let help = match state.step {
        Step::Welcome => " Enter  continue   q  cancel ",
        Step::Browser => " ↑↓  select   Enter  next   q  cancel ",
        Step::Server => " ↑↓  field   Enter  edit   Tab  next   q  cancel ",
        Step::Options => " ↑↓  option   Space  toggle   Tab  review   q  cancel ",
        Step::Confirm => " Enter  save & start   q  cancel ",
    };
    let help_line = Paragraph::new(help).block(Block::default().borders(Borders::ALL));
    frame.render_widget(help_line, chunks[2]);
}

fn draw_welcome(frame: &mut ratatui::Frame, area: ratatui::layout::Rect) {
    let text = vec![
        Line::from("Welcome! Configure the proxy before first run."),
        Line::from(""),
        Line::from("You will set:"),
        Line::from("  • Chromium browser (auto-detect, Playwright, or custom path)"),
        Line::from("  • HTTP listen address and CDP port"),
        Line::from("  • Token capture and UI options"),
        Line::from(""),
        Line::from("Press Enter to continue."),
    ];
    frame.render_widget(
        Paragraph::new(text).block(Block::default().borders(Borders::ALL).title("Welcome")),
        area,
    );
}

fn draw_browser(frame: &mut ratatui::Frame, state: &OnboardState, area: ratatui::layout::Rect) {
    let mut items = vec![ListItem::new(if state.browser_choice == 0 {
        "▸ Auto-detect (recommended)"
    } else {
        "  Auto-detect (recommended)"
    })];

    for (idx, browser) in state.browsers.iter().enumerate() {
        let selected = state.browser_choice == idx + 1;
        let prefix = if selected { "▸ " } else { "  " };
        items.push(ListItem::new(format!(
            "{prefix}{} — {}",
            browser.label,
            browser.path.display()
        )));
    }

    let custom_idx = state.browsers.len() + 1;
    let custom_selected = state.browser_choice == custom_idx;
    let custom_label = if state.editing == Some(EditField::CustomBrowserPath) {
        format!(
            "▸ Custom path: {}_",
            state.custom_browser_path
        )
    } else if custom_selected {
        format!(
            "▸ Custom path: {}",
            if state.custom_browser_path.is_empty() {
                "(Enter to type)".to_string()
            } else {
                state.custom_browser_path.clone()
            }
        )
    } else {
        "  Custom path...".to_string()
    };
    items.push(ListItem::new(custom_label));

    if state.browsers.is_empty() {
        items.insert(
            1,
            ListItem::new(Span::styled(
                "  (no browsers found — install Chrome/Edge or use Playwright)",
                Style::default().fg(Color::Yellow),
            )),
        );
    }

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title("Browser for token capture"),
    );
    frame.render_widget(list, area);
}

fn draw_server(frame: &mut ratatui::Frame, state: &OnboardState, area: ratatui::layout::Rect) {
    let fields = [
        ("HTTP host", state.config.server.host.clone()),
        ("HTTP port", state.config.server.port.to_string()),
        ("CDP port", state.config.edge.cdp_port.to_string()),
    ];
    let items: Vec<ListItem> = fields
        .iter()
        .enumerate()
        .map(|(idx, (label, value))| {
            let marker = if state.option_idx == idx { "▸ " } else { "  " };
            let editing = state.editing.is_some_and(|f| {
                matches!(
                    (idx, f),
                    (0, EditField::Host) | (1, EditField::Port) | (2, EditField::CdpPort)
                )
            });
            let display = if editing { format!("{value}_") } else { value.clone() };
            ListItem::new(format!("{marker}{label}: {display}"))
        })
        .collect();
    frame.render_widget(
        List::new(items).block(
            Block::default()
                .borders(Borders::ALL)
                .title("Proxy server"),
        ),
        area,
    );
}

fn draw_options(frame: &mut ratatui::Frame, state: &OnboardState, area: ratatui::layout::Rect) {
    let options = [
        ("Launch browser on start", state.config.edge.launch_on_start),
        ("Auto-refresh token", state.config.token.auto_refresh),
        ("Capture token on start", state.config.token.capture_on_start),
        ("Terminal dashboard (TUI)", state.config.ui.tui),
        ("System tray icon", state.config.ui.tray),
    ];
    let items: Vec<ListItem> = options
        .iter()
        .enumerate()
        .map(|(idx, (label, on))| {
            let marker = if state.option_idx == idx { "▸ " } else { "  " };
            ListItem::new(format!("{marker}{label}: {}", on_off(*on)))
        })
        .collect();

    let mut lines = items;
    lines.push(ListItem::new(""));
    #[cfg(target_os = "macos")]
    lines.push(ListItem::new(Span::styled(
        "  Note: system tray on macOS requires the main thread; left off by default.",
        Style::default().fg(Color::DarkGray),
    )));

    frame.render_widget(
        List::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title("Options"),
        ),
        area,
    );
}

fn draw_confirm(frame: &mut ratatui::Frame, state: &OnboardState, area: ratatui::layout::Rect) {
    let browser = browser_summary(state);
    let text = vec![
        Line::from("Review configuration:"),
        Line::from(""),
        Line::from(format!("  Browser: {browser}")),
        Line::from(format!(
            "  Listen:  http://{}:{}",
            state.config.server.host, state.config.server.port
        )),
        Line::from(format!("  CDP:     :{}", state.config.edge.cdp_port)),
        Line::from(format!(
            "  Token:   auto-refresh {}, capture on start {}",
            on_off(state.config.token.auto_refresh),
            on_off(state.config.token.capture_on_start),
        )),
        Line::from(format!(
            "  UI:      TUI {}, tray {}",
            on_off(state.config.ui.tui),
            on_off(state.config.ui.tray),
        )),
        Line::from(""),
        Line::from("Press Enter to save and start the proxy."),
    ];
    frame.render_widget(
        Paragraph::new(text).block(Block::default().borders(Borders::ALL).title("Confirm")),
        area,
    );
}

fn browser_summary(state: &OnboardState) -> String {
    if state.browser_choice == 0 {
        return "auto-detect".into();
    }
    if state.browser_choice <= state.browsers.len() {
        return state.browsers[state.browser_choice - 1].label.clone();
    }
    if state.custom_browser_path.trim().is_empty() {
        "auto-detect".into()
    } else {
        state.custom_browser_path.clone()
    }
}

fn on_off(value: bool) -> &'static str {
    if value { "on" } else { "off" }
}
