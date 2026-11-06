//! The state of the terminal panel: the shell in it, whether it is shown and focused, and the
//! editors that `hx` started from it wait on. `helix-term` draws it and sends it keys.

use serde::{Deserialize, Serialize};

use crate::{
    editor::{get_terminal_provider, ExternalTerminal},
    graphics::Rect,
    DocumentId,
};

pub use helix_terminal::*;

/// `[editor.terminal]`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", default, deny_unknown_fields)]
pub struct TerminalConfig {
    /// Height of the panel, in percent of the editor's height. Defaults to 30.
    pub height: u16,
    /// The shell to run, with its arguments. Defaults to `$SHELL`.
    pub shell: Vec<String>,
    /// Report when a command runs and how it ended (bash and zsh, without touching their rc
    /// files). Defaults to true.
    pub shell_integration: bool,
    /// Lines of scrollback. Defaults to 10000.
    pub scrollback: usize,
    /// The external terminal the debugger runs programs in (`runInTerminal`). Defaults to a tmux
    /// or WezTerm split when Helix runs in one (`wt` or `conhost` on Windows).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub command: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
}

impl TerminalConfig {
    /// The debugger's external terminal, when there is one.
    pub fn external(&self) -> Option<ExternalTerminal> {
        (!self.command.is_empty()).then(|| ExternalTerminal {
            command: self.command.clone(),
            args: self.args.clone(),
        })
    }
}

impl Default for TerminalConfig {
    fn default() -> Self {
        let external = get_terminal_provider().unwrap_or_default();
        Self {
            command: external.command,
            args: external.args,
            height: 30,
            shell: Vec::new(),
            shell_integration: true,
            scrollback: 10_000,
        }
    }
}

/// A buffer an `hx` in the panel waits on (`hx` as `$EDITOR`): `done` is called when it closes.
pub struct Waiter {
    pub doc: DocumentId,
    pub done: Box<dyn FnOnce() + Send>,
}

/// The terminal panel below the editor's splits.
#[derive(Default)]
pub struct Panel {
    /// The shell, while it runs. Its exit closes the panel.
    pub shell: Option<Shell>,
    /// Shown below the splits. The shell keeps running while it is hidden.
    pub visible: bool,
    /// Keys go to the shell.
    pub focused: bool,
    /// Where the screen of the shell was last drawn (without the border).
    pub area: Rect,
    /// Buffers an `hx` in the panel waits on.
    pub waiters: Vec<Waiter>,
}

impl Panel {
    pub fn is_focused(&self) -> bool {
        self.focused && self.visible && self.shell.is_some()
    }

    pub fn is_visible(&self) -> bool {
        self.visible && self.shell.is_some()
    }

    /// Calls the waiters of `doc`, which was closed.
    pub fn document_closed(&mut self, doc: DocumentId) {
        let (done, waiting) = std::mem::take(&mut self.waiters)
            .into_iter()
            .partition(|waiter| waiter.doc == doc);
        self.waiters = waiting;
        for waiter in done {
            (waiter.done)();
        }
    }

    pub fn is_awaited(&self, doc: DocumentId) -> bool {
        self.waiters.iter().any(|waiter| waiter.doc == doc)
    }
}
