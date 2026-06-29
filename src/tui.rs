use std::io;
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use tokio::sync::mpsc;

use crate::config::AppConfig;
use crate::logging::LogBuffer;
use crate::runtime_status::RuntimeStatus;
use crate::token_store::{AccessTokenStore, TokenStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiAction {
    Quit,
    RefreshToken,
    LaunchEdge,
}

pub struct TuiContext {
    pub config: AppConfig,
    pub token_store: Arc<AccessTokenStore>,
    pub log_buffer: LogBuffer,
    pub listen_addr: String,
    pub runtime_status: Arc<RuntimeStatus>,
}

pub async fn run_tui(ctx: TuiContext, action_tx: mpsc::Sender<UiAction>) -> io::Result<()> {
    if !ctx.config.ui.tui || !atty::is(atty::Stream::Stdout) {
        wait_for_ctrl_c(action_tx).await;
        return Ok(());
    }

    let result = tokio::task::spawn_blocking(move || tui_loop(ctx, action_tx)).await;
    match result {
        Ok(inner) => inner,
        Err(e) => Err(io::Error::other(e.to_string())),
    }
}

fn tui_loop(ctx: TuiContext, action_tx: mpsc::Sender<UiAction>) -> io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let mut terminal = ratatui::init();

    let mut scroll: u16 = 0;
    let mut last_line_count = 0usize;
    let tick_rate = Duration::from_millis(250);
    let mut last_tick = std::time::Instant::now();

    loop {
        terminal.draw(|frame| {
            let status = ctx.token_store.status();
            let logs = ctx.log_buffer.lines();
            if logs.len() > last_line_count {
                scroll = scroll.saturating_add((logs.len() - last_line_count) as u16);
            }
            last_line_count = logs.len();
            draw_ui(frame, &ctx, &status, &logs, scroll);
        })?;

        let timeout = tick_rate.saturating_sub(last_tick.elapsed());
        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Char('Q') => {
                            let _ = action_tx.blocking_send(UiAction::Quit);
                            break;
                        }
                        KeyCode::Char('r') | KeyCode::Char('R') => {
                            let _ = action_tx.blocking_send(UiAction::RefreshToken);
                        }
                        KeyCode::Char('e') | KeyCode::Char('E') => {
                            let _ = action_tx.blocking_send(UiAction::LaunchEdge);
                        }
                        KeyCode::Up | KeyCode::Char('k') => scroll = scroll.saturating_sub(1),
                        KeyCode::Down | KeyCode::Char('j') => scroll = scroll.saturating_add(1),
                        KeyCode::PageUp => scroll = scroll.saturating_sub(10),
                        KeyCode::PageDown => scroll = scroll.saturating_add(10),
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            let _ = action_tx.blocking_send(UiAction::Quit);
                            break;
                        }
                        _ => {}
                    }
                }
            }
        }

        if last_tick.elapsed() >= tick_rate {
            last_tick = std::time::Instant::now();
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    ratatui::restore();
    Ok(())
}

fn level_color(level: &str) -> Color {
    match level.to_uppercase().as_str() {
        "ERROR" => Color::Red,
        "WARN" | "WARNING" => Color::Yellow,
        "INFO" => Color::Cyan,
        "DEBUG" => Color::DarkGray,
        "TRACE" => Color::DarkGray,
        _ => Color::White,
    }
}

fn draw_ui(
    frame: &mut ratatui::Frame,
    ctx: &TuiContext,
    status: &TokenStatus,
    logs: &[crate::logging::LogLine],
    scroll: u16,
) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(9),
            Constraint::Min(6),
            Constraint::Length(3),
        ])
        .split(area);

    let title = Paragraph::new(Line::from(vec![
        Span::styled(
            " M365 Copilot OpenAI Proxy ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" — "),
        Span::styled(
            "github.com/nizarfadlan/m365-copilot-proxy",
            Style::default().fg(Color::DarkGray),
        ),
    ]))
    .block(Block::default().borders(Borders::ALL).title("Proxy"));
    frame.render_widget(title, chunks[0]);

    let token_label = if status.valid {
        format!("valid · {}s remaining", status.seconds_remaining)
    } else {
        status
            .error
            .clone()
            .unwrap_or_else(|| "missing or expired".into())
    };
    let token_color = if status.valid {
        Color::Green
    } else {
        Color::Red
    };

    let phase = ctx.runtime_status.phase();
    let phase_color = match phase {
        crate::runtime_status::ServicePhase::Ready => Color::Green,
        crate::runtime_status::ServicePhase::CaptureFailed => Color::Red,
        _ => Color::Yellow,
    };

    let status_text = vec![
        Line::from(vec![
            Span::styled("Phase  ", Style::default().fg(Color::DarkGray)),
            Span::styled(phase.label(), Style::default().fg(phase_color)),
        ]),
        Line::from(vec![
            Span::styled("Listen  ", Style::default().fg(Color::DarkGray)),
            Span::raw(format!("http://{}", ctx.listen_addr)),
        ]),
        Line::from(vec![
            Span::styled("Model   ", Style::default().fg(Color::DarkGray)),
            Span::raw(&ctx.config.token.model_alias),
        ]),
        Line::from(vec![
            Span::styled("Token   ", Style::default().fg(Color::DarkGray)),
            Span::styled(token_label, Style::default().fg(token_color)),
        ]),
        Line::from(vec![
            Span::styled("Browser ", Style::default().fg(Color::DarkGray)),
            Span::raw(format!(
                "CDP :{} · {} · auto-refresh {} · capture {}",
                ctx.config.edge.cdp_port,
                if ctx.config.edge.headless_when_authenticated {
                    "headless when authed"
                } else {
                    "visible browser"
                },
                on_off(ctx.config.token.auto_refresh),
                on_off(ctx.config.token.capture_on_start),
            )),
        ]),
        Line::from(vec![
            Span::styled("Tray    ", Style::default().fg(Color::DarkGray)),
            Span::raw(on_off(ctx.config.ui.tray)),
        ]),
    ];
    let status_block =
        Paragraph::new(status_text).block(Block::default().borders(Borders::ALL).title("Status"));
    frame.render_widget(status_block, chunks[1]);

    let visible_height = chunks[2].height.saturating_sub(2) as usize;
    let start = scroll as usize;
    let visible: Vec<ListItem<'_>> = logs
        .iter()
        .skip(start)
        .take(visible_height)
        .map(|line| {
            let msg = truncate_line(&line.message, chunks[2].width.saturating_sub(8) as usize);
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{:5} ", line.level.to_uppercase()),
                    Style::default().fg(level_color(&line.level)),
                ),
                Span::raw(msg),
            ]))
        })
        .collect();
    let visible_list = List::new(visible).block(Block::default().borders(Borders::ALL).title(
        format!("Logs ({}/{})", start.saturating_add(1).max(1), logs.len()),
    ));
    frame.render_widget(visible_list, chunks[2]);

    let help = Paragraph::new(Line::from(vec![
        Span::styled(" q ", Style::default().fg(Color::Yellow)),
        Span::raw("quit  "),
        Span::styled(" r ", Style::default().fg(Color::Yellow)),
        Span::raw("refresh token  "),
        Span::styled(" e ", Style::default().fg(Color::Yellow)),
        Span::raw("launch browser  "),
        Span::styled(" ↑↓ ", Style::default().fg(Color::Yellow)),
        Span::raw("scroll logs"),
    ]))
    .block(Block::default().borders(Borders::ALL));
    frame.render_widget(help, chunks[3]);
}

fn on_off(value: bool) -> &'static str {
    if value {
        "on"
    } else {
        "off"
    }
}

fn truncate_line(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let trimmed: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{trimmed}…")
}

async fn wait_for_ctrl_c(action_tx: mpsc::Sender<UiAction>) {
    if tokio::signal::ctrl_c().await.is_ok() {
        let _ = action_tx.send(UiAction::Quit).await;
    }
}
