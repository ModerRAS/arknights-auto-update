use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use tracing::Subscriber;
use tracing_subscriber::layer::Context;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{fmt, EnvFilter, Layer};

use crate::state::{BusEvent, LogLine, LOG_CAPACITY};
use crate::util::now_ms;

pub struct SseLogLayer {
    tx: tokio::sync::broadcast::Sender<BusEvent>,
    recent: Arc<Mutex<VecDeque<LogLine>>>,
}

impl<S> Layer<S> for SseLogLayer
where
    S: Subscriber,
{
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let mut message = String::new();
        event.record(&mut MessageVisitor {
            message: &mut message,
        });
        if message.starts_with('"') && message.ends_with('"') {
            if let Ok(unquoted) = serde_json::from_str::<String>(&message) {
                message = unquoted;
            }
        }
        if message.chars().count() > 2000 {
            message = message.chars().take(2000).collect();
        }
        let meta = event.metadata();
        let line = LogLine {
            ts_ms: now_ms(),
            level: meta.level().to_string(),
            target: meta.target().to_string(),
            message,
        };
        if let Ok(mut recent) = self.recent.lock() {
            if recent.len() >= LOG_CAPACITY {
                recent.pop_front();
            }
            recent.push_back(line.clone());
        }
        let _ = self.tx.send(BusEvent::Log {
            ts_ms: line.ts_ms,
            level: line.level,
            target: line.target,
            message: line.message,
        });
    }
}

struct MessageVisitor<'a> {
    message: &'a mut String,
}

impl tracing::field::Visit for MessageVisitor<'_> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            use std::fmt::Write as _;
            let _ = write!(self.message, "{value:?}");
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message.clear();
            self.message.push_str(value);
        }
    }
}

pub fn init(
    log_dir: &Path,
    tx: tokio::sync::broadcast::Sender<BusEvent>,
    recent: Arc<Mutex<VecDeque<LogLine>>>,
) -> anyhow::Result<()> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new("info,ark_updater=debug,hyper=warn,reqwest=warn,tower_http=info")
    });
    let sse = SseLogLayer { tx, recent };
    let stderr_layer = fmt::layer().with_target(true);
    if std::fs::create_dir_all(log_dir).is_ok() {
        let appender = tracing_appender::rolling::daily(log_dir, "ark-updater.log");
        let (writer, guard) = tracing_appender::non_blocking(appender);
        static GUARD: OnceLock<tracing_appender::non_blocking::WorkerGuard> = OnceLock::new();
        let _ = GUARD.set(guard);
        tracing_subscriber::registry()
            .with(filter)
            .with(stderr_layer)
            .with(fmt::layer().with_ansi(false).with_writer(writer))
            .with(sse)
            .init();
    } else {
        tracing_subscriber::registry()
            .with(filter)
            .with(stderr_layer)
            .with(sse)
            .init();
    }
    Ok(())
}
