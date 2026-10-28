//! Fission: printing on Bambu Lab printers from SolidCraft.
//!
//! - [`profiles`]: Bambu Studio's printer, process and filament presets (the installed ones, the ones it
//!   updated online, and your own), merged the way Bambu Studio merges them.
//! - [`studio`]: what Bambu Studio's settings file knows: your LAN printers, your send-print choices, the
//!   filaments it last saw in each AMS, and which system filaments you show.
//! - [`model3mf`]: the plate as a 3MF Bambu Studio reads, each object on its filament.
//! - [`slice`]: slicing with Bambu Studio's own command line, and reading the result.
//! - [`lan`]: the printers on your network: status (MQTT over TLS), upload (FTPS) and starting a print.
//!
//! Desktop only (it runs programs and opens network connections); the engine leaves it out of the web
//! build. Nothing here panics: every failure is an `Err(String)` worded for the user.

pub mod gcode;
pub mod lan;
pub mod model3mf;
mod pattern;
pub mod profiles;
pub mod slice;
pub mod studio;

/// Errors are messages for the user.
pub type Result<T> = std::result::Result<T, String>;

/// Fission's own folder, `~/Fission` (sliced plates, the slicer log).
pub fn home_folder() -> Option<std::path::PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(|d| std::path::PathBuf::from(d).join("Fission"))
}
