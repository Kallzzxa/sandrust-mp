//! MC-RP Launcher: the GUI front-end for `samp-launcher`.
//!
//! Windows-only (Win32). On any other host the binary just says so: the
//! query/favorites logic is tested through the lib instead.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(windows)]
mod gui;

fn main() {
    #[cfg(windows)]
    gui::run();
    #[cfg(not(windows))]
    {
        eprintln!("mc-rp-launcher is a Windows GUI app: run it on Windows next to samp-launcher.exe.");
    }
}
