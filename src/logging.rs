use std::collections::VecDeque;
use std::fmt;
use std::sync::{Arc, Mutex};

use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::fmt::format::FmtSpan;
use tracing_subscriber::fmt::time::ChronoLocal;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

use crate::config::LoggingConfig;

const LOG_BUFFER_CAPACITY: usize = 200;

#[derive(Clone, Debug)]
pub struct LogLine {
    pub level: String,
    pub message: String,
}

#[derive(Clone, Default)]
pub struct LogBuffer {
    inner: Arc<Mutex<VecDeque<LogLine>>>,
}

impl LogBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&self, level: &str, message: String) {
        let mut guard = self.inner.lock().unwrap();
        if guard.len() >= LOG_BUFFER_CAPACITY {
            guard.pop_front();
        }
        guard.push_back(LogLine {
            level: level.to_string(),
            message,
        });
    }

    pub fn lines(&self) -> Vec<LogLine> {
        self.inner.lock().unwrap().iter().cloned().collect()
    }
}

struct BufferLayer {
    buffer: LogBuffer,
}

impl<S> Layer<S> for BufferLayer
where
    S: Subscriber,
{
    fn on_event(&self, event: &Event<'_>, _ctx: tracing_subscriber::layer::Context<'_, S>) {
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        let level = event.metadata().level().to_string();
        let target = event.metadata().target();
        let message = if visitor.message.is_empty() {
            target.to_string()
        } else {
            visitor.message
        };
        self.buffer.push(&level, message);
    }
}

#[derive(Default)]
struct MessageVisitor {
    message: String,
}

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}").trim_matches('"').to_string();
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        }
    }
}

pub fn init_logging(config: &LoggingConfig, buffer: LogBuffer) -> Result<(), String> {
    let filter = EnvFilter::try_new(config.level.clone())
        .or_else(|_| EnvFilter::try_new("info"))
        .map_err(|e| e.to_string())?;

    let buffer_layer = BufferLayer { buffer };

    let fmt_layer = match config.format.as_str() {
        "json" => tracing_subscriber::fmt::layer()
            .json()
            .with_timer(ChronoLocal::rfc_3339())
            .with_span_events(FmtSpan::CLOSE)
            .boxed(),
        "compact" => tracing_subscriber::fmt::layer()
            .compact()
            .with_timer(ChronoLocal::rfc_3339())
            .with_span_events(FmtSpan::CLOSE)
            .boxed(),
        _ => tracing_subscriber::fmt::layer()
            .pretty()
            .with_timer(ChronoLocal::rfc_3339())
            .with_span_events(FmtSpan::CLOSE)
            .boxed(),
    };

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt_layer)
        .with(buffer_layer)
        .try_init()
        .map_err(|e| e.to_string())
}

pub fn log_banner() {
    tracing::info!(
        "Microsoft 365 Copilot OpenAI Proxy — github.com/nizarfadlan/m365-copilot-proxy"
    );
}
