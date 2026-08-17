// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Process tracing: one stream, level from `RUST_LOG`.
//!
//! Timestamps use the host local timezone (not UTC).
//! During a research session, the same lines are also teed into
//! `pw/screenshots/<Draft-ID>/product.log`.
//!
//! ## Levels
//!
//! | Macro | Use |
//! | --- | --- |
//! | `error!` | Operator-visible failure (bind, config, publish, server) |
//! | `warn!` | Degraded / skipped / continuing without a subsystem |
//! | `info!` | Lifecycle (listen, ready, drip sleep, webhook wake) |
//! | `debug!` | Detail for local diagnosis |
//!
//! Prefer `error = %err` (Display). Use `?err` only when Debug is required.
//! Slack / E2E `eprintln!` banners are pane chrome, not structured logs.
//! The TUI does not install a subscriber; product logs stay on the product window.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use tracing_subscriber::fmt::time::ChronoLocal;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::{fmt, EnvFilter};

/// Default when `RUST_LOG` is unset: quiet crates.io deps, full `itcy` levels through info.
/// Override per environment, e.g. `RUST_LOG=warn,itcy=debug` or `RUST_LOG=trace`.
const DEFAULT_FILTER: &str = "warn,itcy=info";

static SESSION_LOG: OnceLock<Mutex<Option<Arc<Mutex<File>>>>> = OnceLock::new();

fn session_slot() -> &'static Mutex<Option<Arc<Mutex<File>>>> {
    SESSION_LOG.get_or_init(|| Mutex::new(None))
}

/// Start teeing product logs into `path` (usually `…/product.log` in the session folder).
///
/// # Errors
///
/// Returns an [`std::io::Error`] when the filesystem operation fails.
pub fn attach_session_log(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new().create(true).append(true).open(path)?;
    let mut guard = session_slot()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *guard = Some(Arc::new(Mutex::new(file)));
    drop(guard);
    Ok(())
}

/// Stop teeing into the session log file.
pub fn detach_session_log() {
    let mut guard = session_slot()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *guard = None;
}

/// Append a raw note into the active session log (no-op if none attached).
pub fn append_session_log_note(text: &str) {
    let file = {
        let guard = session_slot()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.clone()
    };
    let Some(file) = file else {
        return;
    };
    let Ok(mut f) = file.lock() else {
        return;
    };
    let _ = writeln!(f, "{text}");
    let _ = f.flush();
}

/// Installs a global subscriber on stdout. Safe to call once at process start.
///
/// All levels (error/warn/info/debug/trace) share **one** stream (stdout) so
/// `make run` / GNU screen show everything. Filtering is only via `RUST_LOG`.
///
/// During a research session, the same bytes are also appended to `product.log`.
pub fn init_tracing() {
    init_tracing_with(TeeMakeWriter, DEFAULT_FILTER);
}

/// stderr subscriber for CLI bins / MCP. `default_filter` when `RUST_LOG` is unset.
pub fn init_tracing_stderr(default_filter: &str) {
    // CLI bins: no session tee (stdout/stderr only).
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_filter));
    let ansi = log_ansi_enabled();
    fmt()
        .with_env_filter(filter)
        .with_timer(ChronoLocal::rfc_3339())
        .with_target(true)
        .with_level(true)
        .with_ansi(ansi)
        .with_writer(io::stderr)
        .init();
}

fn init_tracing_with<W>(writer: W, default_filter: &str)
where
    W: for<'a> MakeWriter<'a> + Send + Sync + 'static,
{
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_filter));
    let ansi = log_ansi_enabled();

    fmt()
        .with_env_filter(filter)
        .with_timer(ChronoLocal::rfc_3339())
        .with_target(true)
        .with_level(true)
        .with_ansi(ansi)
        .with_writer(writer)
        .init();
}

fn log_ansi_enabled() -> bool {
    !matches!(
        std::env::var("ITCY_LOG_ANSI"),
        Ok(v) if v == "0" || v.eq_ignore_ascii_case("false")
    )
}

/// Writes each log line to stdout and, when attached, to the session `product.log`.
#[derive(Clone, Debug, Default)]
struct TeeMakeWriter;

struct TeeWriter {
    stdout: io::Stdout,
    session: Option<Arc<Mutex<File>>>,
}

impl<'a> MakeWriter<'a> for TeeMakeWriter {
    type Writer = TeeWriter;

    fn make_writer(&'a self) -> Self::Writer {
        let session = session_slot()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        TeeWriter {
            stdout: io::stdout(),
            session,
        }
    }
}

impl Write for TeeWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.stdout.write(buf)?;
        if let Some(ref file) = self.session {
            if let Ok(mut f) = file.lock() {
                // product.log must stay plain text (no ANSI escape codes).
                let plain = strip_ansi(buf);
                let _ = f.write_all(&plain);
            }
        }
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stdout.flush()?;
        if let Some(ref file) = self.session {
            if let Ok(mut f) = file.lock() {
                let _ = f.flush();
            }
        }
        Ok(())
    }
}

/// Strip CSI / OSC ANSI sequences for session product.log readability.
fn strip_ansi(buf: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(buf.len());
    let mut i = 0;
    while i < buf.len() {
        if buf[i] == 0x1b {
            i += 1;
            if i >= buf.len() {
                break;
            }
            match buf[i] {
                b'[' => {
                    i += 1;
                    while i < buf.len() && !(buf[i]).is_ascii_alphabetic() {
                        i += 1;
                    }
                    if i < buf.len() {
                        i += 1; // final letter
                    }
                }
                b']' => {
                    i += 1;
                    while i < buf.len() && buf[i] != 7 && buf[i] != b'\\' {
                        i += 1;
                    }
                    if i < buf.len() {
                        i += 1;
                    }
                }
                _ => {
                    // skip single-char ESC sequence
                    i += 1;
                }
            }
            continue;
        }
        out.push(buf[i]);
        i += 1;
    }
    out
}

/// Path helper for tests / callers.
#[must_use]
pub fn session_product_log_path(session_root: &Path) -> PathBuf {
    session_root.join("product.log")
}

#[cfg(test)]
mod tests {
    use super::strip_ansi;

    #[test]
    fn strip_ansi_removes_color_codes() {
        let raw = b"\x1b[32m INFO\x1b[0m hello";
        let plain = strip_ansi(raw);
        assert_eq!(String::from_utf8_lossy(&plain), " INFO hello");
    }
}
