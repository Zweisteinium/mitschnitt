//! Platform implementations behind the shared modules of this crate; `os` is the one for this build.

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(windows)]
pub mod windows;

#[cfg(target_os = "linux")]
pub(crate) use linux as os;
#[cfg(windows)]
pub(crate) use windows as os;

#[cfg(not(any(windows, target_os = "linux")))]
compile_error!("Mitschnitt supports Windows and Linux");
