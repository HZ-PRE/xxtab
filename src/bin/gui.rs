#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]
#[cfg(windows)]
#[path = "../gui.rs"]
mod gui;
fn main() {
    #[cfg(windows)]
    gui::run();
    #[cfg(not(windows))]
    eprintln!("The native GUI is available on Windows only; use xxtab on Linux.");
}
