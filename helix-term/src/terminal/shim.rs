//! `hx` inside the panel: instead of starting a nested Helix, it asks this editor to open its
//! files (or just to take focus) over a Unix socket, and as `$EDITOR` waits until the buffer
//! is closed.
