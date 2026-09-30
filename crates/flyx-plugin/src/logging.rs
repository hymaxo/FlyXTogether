//! Plugin logging.
//!
//! Events go to `FlyXTogether.log` in the plugin folder through a writer
//! thread, so the main thread never touches the disk. Errors, and events
//! with target [`XPLANE_LOG`], are also forwarded to X-Plane's `Log.txt`.
//! `XPLMDebugString` is only called on the main thread; events from other
//! threads are queued and flushed by [`flush_xplane_queue`].

use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::path::Path;
use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, Once};
use std::thread::{self, JoinHandle, ThreadId};

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::prelude::*;

/// Target for events that should also appear in X-Plane's `Log.txt`.
pub const XPLANE_LOG: &str = "flyx::xplane";

static FILE_SINK: Mutex<Option<Sender<Vec<u8>>>> = Mutex::new(None);
static WRITER_THREAD: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);
static MAIN_THREAD: Mutex<Option<ThreadId>> = Mutex::new(None);
static XPLANE_QUEUE: Mutex<Vec<String>> = Mutex::new(Vec::new());
static INSTALL: Once = Once::new();

/// Starts the log writer and installs the subscriber. Must be called on the
/// main thread. Safe to call again after [`stop`].
///
/// The file is recreated once per X-Plane process: a plugin reload inside
/// the same process appends, so reload cycles stay visible in one file.
pub fn start(log_file: &Path) {
    *MAIN_THREAD.lock().unwrap_or_else(|e| e.into_inner()) = Some(thread::current().id());

    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    let path = log_file.to_path_buf();
    // The writer thread opens the file so the main thread never touches disk.
    let handle = thread::Builder::new()
        .name("flyx-log".into())
        .spawn(move || match open_log(&path) {
            Ok(file) => {
                let mut out = BufWriter::new(file);
                for line in rx {
                    let _ = out.write_all(&line);
                    // Flush per event so the log survives a crash.
                    let _ = out.flush();
                }
            }
            Err(err) => {
                XPLANE_QUEUE
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(format!(
                        "FlyXTogether: cannot open log file {}: {err}",
                        path.display()
                    ));
            }
        })
        .ok();
    *FILE_SINK.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
    *WRITER_THREAD.lock().unwrap_or_else(|e| e.into_inner()) = handle;

    // The global subscriber can only be installed once per process. If
    // X-Plane restarts the plugin without unloading it, the existing
    // subscriber keeps working because the sinks above are swappable.
    INSTALL.call_once(|| {
        let file_layer = tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_thread_names(true)
            .with_writer(FileMakeWriter);
        // INFO and above, plus DEBUG from our own crates. Dependencies such
        // as quinn trace every packet, which would flood the log.
        let filter = tracing_subscriber::filter::Targets::new()
            .with_default(tracing::Level::INFO)
            .with_target("FlyXTogether", tracing::Level::DEBUG)
            .with_target("flyx", tracing::Level::DEBUG)
            .with_target("flyx_net", tracing::Level::DEBUG)
            .with_target("flyx_sync", tracing::Level::DEBUG)
            .with_target("flyx_xplm", tracing::Level::DEBUG);
        let _ = tracing_subscriber::registry()
            .with(filter)
            .with(file_layer)
            .with(XPlaneLogLayer)
            .try_init();
    });
}

/// Flushes and closes the log file and joins the writer thread.
pub fn stop() {
    flush_xplane_queue();
    FILE_SINK.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(handle) = WRITER_THREAD
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
    {
        let _ = handle.join();
    }
}

/// Writes queued `Log.txt` lines. Call from the main thread.
pub fn flush_xplane_queue() {
    let lines = std::mem::take(&mut *XPLANE_QUEUE.lock().unwrap_or_else(|e| e.into_inner()));
    for line in lines {
        flyx_xplm::debug_string(&line);
    }
}

/// First line of the log file; identifies the X-Plane process that wrote it.
fn process_header() -> String {
    format!("FlyXTogether log, process {}\n", std::process::id())
}

/// Appends if the file was started by this process, otherwise recreates it.
fn open_log(path: &Path) -> io::Result<File> {
    let header = process_header();
    let same_process = File::open(path)
        .ok()
        .and_then(|f| {
            let mut first = String::new();
            BufReader::new(f).read_line(&mut first).ok()?;
            Some(first == header)
        })
        .unwrap_or(false);
    if same_process {
        OpenOptions::new().append(true).open(path)
    } else {
        // Keep the previous run's log: after a crash it is the one needed.
        if path.exists() {
            let _ = std::fs::rename(path, path.with_extension("previous.log"));
        }
        let mut file = File::create(path)?;
        file.write_all(header.as_bytes())?;
        Ok(file)
    }
}

fn on_main_thread() -> bool {
    *MAIN_THREAD.lock().unwrap_or_else(|e| e.into_inner()) == Some(thread::current().id())
}

struct FileMakeWriter;

impl<'a> MakeWriter<'a> for FileMakeWriter {
    type Writer = EventBuffer;

    fn make_writer(&'a self) -> Self::Writer {
        EventBuffer(Vec::with_capacity(256))
    }
}

/// Collects one formatted event and hands it to the writer thread on drop.
struct EventBuffer(Vec<u8>);

impl Write for EventBuffer {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for EventBuffer {
    fn drop(&mut self) {
        if let Some(tx) = FILE_SINK.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            let _ = tx.send(std::mem::take(&mut self.0));
        }
    }
}

struct XPlaneLogLayer;

impl<S: Subscriber> Layer<S> for XPlaneLogLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        if *meta.level() != Level::ERROR && meta.target() != XPLANE_LOG {
            return;
        }
        let mut fields = FieldText::default();
        event.record(&mut fields);
        let line = format!("FlyXTogether [{}]: {}", meta.level(), fields.text);
        if on_main_thread() {
            flush_xplane_queue();
            flyx_xplm::debug_string(&line);
        } else {
            XPLANE_QUEUE
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(line);
        }
    }
}

#[derive(Default)]
struct FieldText {
    text: String,
}

impl Visit for FieldText {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if !self.text.is_empty() {
            self.text.push(' ');
        }
        if field.name() == "message" {
            let _ = write!(self.text, "{value:?}");
        } else {
            let _ = write!(self.text, "{}={value:?}", field.name());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn read(path: &Path) -> String {
        let mut s = String::new();
        File::open(path).unwrap().read_to_string(&mut s).unwrap();
        s
    }

    #[test]
    fn same_process_appends_other_process_recreates() {
        let dir = std::env::temp_dir().join(format!("flyx-log-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("FlyXTogether.log");

        // A log left by an earlier X-Plane process is replaced and kept as
        // the previous log.
        std::fs::write(&path, "FlyXTogether log, process 1\nold line\n").unwrap();
        let mut f = open_log(&path).unwrap();
        f.write_all(b"first start\n").unwrap();
        drop(f);
        assert_eq!(read(&path), format!("{}first start\n", process_header()));
        assert_eq!(
            read(&dir.join("FlyXTogether.previous.log")),
            "FlyXTogether log, process 1\nold line\n"
        );

        // A reload inside the same process appends.
        let mut f = open_log(&path).unwrap();
        f.write_all(b"after reload\n").unwrap();
        drop(f);
        assert_eq!(
            read(&path),
            format!("{}first start\nafter reload\n", process_header())
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
