use std::{
    collections::VecDeque,
    fmt,
    sync::{Arc, Mutex, OnceLock},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Status {
    #[default]
    Idle,
    Connecting,
    Connected,
    Reconnecting,
    Failed,
}

#[derive(Default)]
pub struct Feed {
    pub lines: VecDeque<String>,
    pub status: Status,
}
static GUI: OnceLock<Arc<Mutex<Feed>>> = OnceLock::new();

pub fn attach(feed: Arc<Mutex<Feed>>) {
    let _ = GUI.set(feed);
}
pub fn state(status: Status) {
    if let Some(feed) = GUI.get() {
        feed.lock().unwrap_or_else(|e| e.into_inner()).status = status;
    }
}
pub fn write(message: fmt::Arguments<'_>) {
    if let Some(feed) = GUI.get() {
        let mut feed = feed.lock().unwrap_or_else(|e| e.into_inner());
        if feed.lines.len() == 256 {
            feed.lines.pop_front();
        }
        // Bound both log backlog and individual log lines.
        feed.lines
            .push_back(message.to_string().chars().take(2048).collect());
    } else {
        eprintln!("{message}");
    }
}

#[macro_export]
macro_rules! log { ($($arg:tt)*) => { $crate::logging::write(format_args!($($arg)*)) }; }
