use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

static QUIET: AtomicBool = AtomicBool::new(false);

/// Enable or disable quiet mode (suppresses progress output).
pub fn set_quiet(quiet: bool) {
    QUIET.store(quiet, Ordering::Relaxed);
}

/// Returns `true` if quiet mode is active.
pub fn is_quiet() -> bool {
    QUIET.load(Ordering::Relaxed)
}

/// Print a progress message to stderr (suppressed with `--quiet`).
///
/// Usage: `progress!("Syncing {count} entries...");`
#[macro_export]
macro_rules! progress {
    ($($arg:tt)*) => {
        if !$crate::output::is_quiet() {
            eprintln!($($arg)*);
        }
    };
}

/// Print an inline progress message to stderr without a newline (suppressed with `--quiet`).
///
/// Usage: `progress_inline!("  resolving ...");`
#[macro_export]
macro_rules! progress_inline {
    ($($arg:tt)*) => {
        if !$crate::output::is_quiet() {
            eprint!($($arg)*);
        }
    };
}

// ===========================================================================
// Spinner
// ===========================================================================

const SPINNER_FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

fn run_spinner_loop(stop: &AtomicBool, msg: &str) {
    let mut i = 0usize;
    let mut stderr = std::io::stderr();
    while !stop.load(Ordering::Relaxed) {
        let _ = write!(
            stderr,
            "\r{} {msg}",
            SPINNER_FRAMES[i % SPINNER_FRAMES.len()]
        );
        let _ = stderr.flush();
        i += 1;
        std::thread::sleep(std::time::Duration::from_millis(80));
    }
    // Clear the spinner line
    let _ = write!(stderr, "\r{}\r", " ".repeat(msg.len() + 3));
    let _ = stderr.flush();
}

/// An animated spinner that prints to stderr on a background thread.
///
/// The spinner is suppressed in quiet mode or when stderr is not a terminal.
/// Drop the spinner (or call [`Spinner::finish`]) to stop it and clear the line.
///
/// ```no_run
/// use skillfile_core::output::Spinner;
///
/// let spinner = Spinner::new("Searching registries");
/// // ... blocking work ...
/// spinner.finish(); // or just let it drop
/// ```
pub struct Spinner {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Spinner {
    /// Start a spinner with the given message.
    ///
    /// Returns immediately. The spinner animates on a background thread until
    /// dropped or [`Spinner::finish`] is called.
    pub fn new(message: &str) -> Self {
        let stop = Arc::new(AtomicBool::new(false));

        if is_quiet() || !std::io::stderr().is_terminal() {
            return Self { stop, handle: None };
        }

        let stop_clone = stop.clone();
        let msg = message.to_string();

        let handle = std::thread::spawn(move || run_spinner_loop(&stop_clone, &msg));

        Self {
            stop,
            handle: Some(handle),
        }
    }

    /// Stop the spinner and clear the line. Equivalent to dropping.
    pub fn finish(self) {
        drop(self);
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}
