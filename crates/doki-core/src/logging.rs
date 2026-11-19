use std::collections::VecDeque;
use std::io::{self, Write};
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

pub const RING_CAPACITY: usize = 400;

static RING: OnceLock<Mutex<VecDeque<String>>> = OnceLock::new();
static GUARD: OnceLock<WorkerGuard> = OnceLock::new();

#[derive(Clone, Default)]
struct RingWriter {
    buffer: String,
}

impl RingWriter {
    fn push(&mut self, line: &str) {
        let line = line.trim_end();
        if line.is_empty() {
            return;
        }
        let ring = RING.get_or_init(|| Mutex::new(VecDeque::new()));
        let mut lines = ring.lock().expect("log ring");
        if lines.len() == RING_CAPACITY {
            lines.pop_front();
        }
        lines.push_back(line.to_string());
    }
}

impl Write for RingWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.buffer.push_str(&String::from_utf8_lossy(bytes));
        while let Some(index) = self.buffer.find('\n') {
            let line: String = self.buffer.drain(..=index).collect();
            self.push(&line);
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if !self.buffer.trim().is_empty() {
            let line = std::mem::take(&mut self.buffer);
            self.push(&line);
        }
        Ok(())
    }
}

impl Drop for RingWriter {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

impl<'a> MakeWriter<'a> for RingWriter {
    type Writer = RingWriter;

    fn make_writer(&'a self) -> Self::Writer {
        RingWriter::default()
    }
}

/// Daily files, seven kept, and the same lines mirrored into a bounded ring.
pub fn init(state_dir: &Path) -> bool {
    if tracing::dispatcher::has_been_set() {
        return true;
    }
    let dir = state_dir.join("logs");
    let registry = tracing_subscriber::registry().with(LevelFilter::INFO).with(
        tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_target(true)
            .with_writer(RingWriter::default()),
    );
    let Ok(_dir) = std::fs::create_dir_all(&dir) else {
        return install(registry);
    };
    let appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("doki")
        .filename_suffix("log")
        .max_log_files(7)
        .build(&dir);
    match appender {
        Ok(appender) => {
            let (file, guard) = tracing_appender::non_blocking(appender);
            let _ = GUARD.set(guard);
            install(
                registry.with(
                    tracing_subscriber::fmt::layer()
                        .with_ansi(false)
                        .with_target(true)
                        .with_writer(file),
                ),
            )
        }
        Err(error) => {
            tracing::warn!(%error, "rolling log file unavailable; keeping the in-memory ring only");
            install(registry)
        }
    }
}

fn install(subscriber: impl tracing::Subscriber + Send + Sync + 'static) -> bool {
    subscriber.try_init().is_ok()
}

pub fn recent(limit: usize) -> Vec<String> {
    RING.get_or_init(|| Mutex::new(VecDeque::new()))
        .lock()
        .map(|lines| {
            let skip = lines.len().saturating_sub(limit);
            lines.iter().skip(skip).cloned().collect()
        })
        .unwrap_or_default()
}

pub fn recent_text(limit: usize) -> String {
    recent(limit).join("\n")
}

pub fn clear() {
    if let Some(ring) = RING.get() {
        ring.lock().expect("log ring").clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn the_ring_is_bounded_and_newest_last() {
        let _guard = TEST_LOCK.lock().expect("logging test lock");
        clear();
        for index in 0..RING_CAPACITY + 5 {
            let mut writer = RingWriter::default();
            writer
                .write_all(format!("line {index}\n").as_bytes())
                .unwrap();
            writer.flush().unwrap();
        }
        let lines = recent(RING_CAPACITY + 10);
        assert_eq!(lines.len(), RING_CAPACITY);
        assert_eq!(
            lines.last().unwrap(),
            &format!("line {}", RING_CAPACITY + 4)
        );
        assert_eq!(recent(3).len(), 3);
        assert!(recent_text(1).starts_with("line "));
        clear();
    }

    #[test]
    fn partial_writes_join_into_one_line() {
        let _guard = TEST_LOCK.lock().expect("logging test lock");
        clear();
        let mut writer = RingWriter::default();
        writer.write_all(b"2026-01-01 INFO doki: ").unwrap();
        writer.write_all(b"started\nnext").unwrap();
        let lines = recent(10);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].ends_with("started"), "{lines:?}");
        writer.flush().unwrap();
        assert_eq!(recent(10).last().unwrap(), "next");
        clear();
    }
}
