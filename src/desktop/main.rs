#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod app;
#[cfg(windows)]
mod tools;

fn main() {
    #[cfg(windows)]
    if let Err(error) = app::run() {
        tinyfiledialogs::message_box_ok(
            "enenSU",
            &format!("{error:#}"),
            tinyfiledialogs::MessageBoxIcon::Error,
        );
    }
    #[cfg(not(windows))]
    eprintln!("The desktop application targets Windows.");
}
