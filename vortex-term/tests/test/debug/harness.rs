//! The editor driven by the debug tests: an application whose input the test controls and that
//! is pumped until a condition holds (with a deadline), like the terminal panel's tests.

use std::{
    io,
    time::{Duration, Instant},
};

use termina::event::{Event, KeyEvent};
use tokio::sync::mpsc::UnboundedSender;
use tokio_stream::wrappers::UnboundedReceiverStream;
use vortex_core::diagnostic::Severity;
use vortex_term::application::Application;
use vortex_view::input::parse_macro;

use super::super::helpers::AppBuilder;

pub const DEADLINE: Duration = Duration::from_secs(10);

pub struct Harness {
    pub app: Application,
    tx: UnboundedSender<io::Result<Event>>,
    rx: UnboundedReceiverStream<io::Result<Event>>,
    running: bool,
    /// Every error the status line showed, as seen between two steps of the event loop.
    pub errors: Vec<String>,
}

impl Harness {
    pub fn new(builder: AppBuilder) -> anyhow::Result<Self> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        Ok(Self {
            app: builder.build()?,
            tx,
            rx: UnboundedReceiverStream::new(rx),
            running: true,
            errors: Vec::new(),
        })
    }

    fn note_errors(&mut self) {
        if let Some((message, Severity::Error)) = self.app.editor.get_status() {
            let message = message.to_string();
            if self.errors.last() != Some(&message) {
                self.errors.push(message);
            }
        }
    }

    /// Runs the event loop until it is idle, at most `limit`.
    pub async fn step(&mut self, limit: Duration) {
        if !self.running {
            return;
        }
        let idle = tokio::time::timeout(limit, self.app.event_loop_until_idle(&mut self.rx));
        if let Ok(still_running) = idle.await {
            self.running = still_running;
        }
        self.note_errors();
    }

    /// Sends the keys (`parse_macro` syntax) and lets the editor process them.
    pub async fn keys(&mut self, keys: &str) -> anyhow::Result<()> {
        for key in parse_macro(keys)? {
            self.tx.send(Ok(Event::Key(KeyEvent::from(key))))?;
        }
        self.step(Duration::from_secs(3)).await;
        Ok(())
    }

    /// Runs the event loop until `cond` holds. Panics with `what` after the deadline.
    pub async fn until(&mut self, what: &str, cond: impl Fn(&Application) -> bool) {
        let start = Instant::now();
        loop {
            if cond(&self.app) {
                return;
            }
            assert!(
                start.elapsed() < DEADLINE,
                "timed out waiting for: {what}\nstatus: {:?}\nerrors: {:?}\ndebug status: {:?}\nconsole: {:?}",
                self.app.editor.get_status(),
                self.errors,
                self.app.editor.debug.status,
                self.app.editor.debug.console.lines,
            );
            self.step(Duration::from_millis(100)).await;
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Runs the event loop for `duration` (to check that something does *not* happen).
    pub async fn pump_for(&mut self, duration: Duration) {
        let start = Instant::now();
        while start.elapsed() < duration {
            self.step(Duration::from_millis(50)).await;
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Quits the editor and closes the application.
    pub async fn finish(mut self) -> anyhow::Result<()> {
        if self.running {
            for key in parse_macro("<esc><esc>:q!<ret>")? {
                self.tx.send(Ok(Event::Key(KeyEvent::from(key))))?;
            }
            let _ = tokio::time::timeout(Duration::from_secs(2), self.app.event_loop(&mut self.rx))
                .await;
        }
        let errs = self.app.close().await;
        anyhow::ensure!(errs.is_empty(), "errors closing the app: {errs:?}");
        Ok(())
    }
}
