use std::collections::VecDeque;
use std::fmt;
use std::sync::{Arc, Mutex};

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
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
        if message.is_empty() {
            return;
        }
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
        if *event.metadata().level() > Level::INFO {
            return;
        }

        let mut visitor = LogVisitor::default();
        event.record(&mut visitor);

        if should_skip_event(event.metadata(), &visitor) {
            return;
        }

        let level = event.metadata().level().to_string();
        let message = visitor.format();
        self.buffer.push(&level, message);
    }
}

#[derive(Default)]
struct LogVisitor {
    message: String,
    method: Option<String>,
    uri: Option<String>,
    status: Option<String>,
    latency_ms: Option<String>,
}

impl LogVisitor {
    fn format(&self) -> String {
        if let (Some(method), Some(uri)) = (&self.method, &self.uri) {
            if let (Some(status), Some(latency)) = (&self.status, &self.latency_ms) {
                return format!("{method} {uri} → {status} ({latency}ms)");
            }
            return format!("{method} {uri}");
        }
        if !self.message.is_empty() {
            return self.message.clone();
        }
        String::new()
    }
}

impl Visit for LogVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        let text = format!("{value:?}").trim_matches('"').to_string();
        self.record_field(field.name(), text);
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.record_field(field.name(), value.to_string());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.record_field(field.name(), value.to_string());
    }
}

impl LogVisitor {
    fn record_field(&mut self, name: &str, value: String) {
        match name {
            "message" => self.message = value,
            "method" => self.method = Some(value),
            "uri" => self.uri = Some(value),
            "status" => self.status = Some(value),
            "latency_ms" => self.latency_ms = Some(value),
            _ => {}
        }
    }
}

fn should_skip_event(meta: &tracing::Metadata<'_>, visitor: &LogVisitor) -> bool {
    let message = visitor.message.as_str();
    if message.contains("time.busy") || message.starts_with("close,") {
        return true;
    }
    if meta.target().starts_with("hyper") || meta.target().starts_with("h2") {
        return true;
    }
    if message.is_empty() && visitor.method.is_none() {
        return true;
    }
    false
}

pub fn init_logging(
    config: &LoggingConfig,
    buffer: LogBuffer,
    tui_active: bool,
) -> Result<(), String> {
    let filter = EnvFilter::try_new(config.level.clone())
        .or_else(|_| EnvFilter::try_new("info"))
        .map_err(|e| e.to_string())?;

    let buffer_layer = BufferLayer { buffer };

    let registry = tracing_subscriber::registry()
        .with(filter)
        .with(buffer_layer);

    if tui_active {
        registry.try_init().map_err(|e| e.to_string())
    } else {
        let fmt_layer = match config.format.as_str() {
            "json" => tracing_subscriber::fmt::layer()
                .json()
                .with_timer(ChronoLocal::rfc_3339())
                .boxed(),
            "compact" => tracing_subscriber::fmt::layer()
                .compact()
                .with_timer(ChronoLocal::rfc_3339())
                .boxed(),
            _ => tracing_subscriber::fmt::layer()
                .with_timer(ChronoLocal::rfc_3339())
                .boxed(),
        };
        registry
            .with(fmt_layer)
            .try_init()
            .map_err(|e| e.to_string())
    }
}

pub fn log_banner() {
    tracing::info!(
        "Microsoft 365 Copilot OpenAI Proxy — github.com/nizarfadlan/m365-copilot-proxy"
    );
}
