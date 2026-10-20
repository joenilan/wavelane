#![cfg(any(target_os = "linux", target_os = "freebsd"))]
mod linux;

pub use linux::{current_platform, linux_platform};

/// LinX patch: an xdg-activation token received after startup (for example
/// from the tray's StatusNotifierItem click). The next new toplevel window,
/// or the next `activate` of an existing one, uses it so the compositor
/// raises and focuses the window instead of only marking it urgent.
pub fn set_pending_activation_token(token: String) {
    if let Ok(mut pending) = linux::PENDING_ACTIVATION_TOKEN.lock() {
        *pending = Some(token).filter(|t| !t.is_empty());
    }
}

/// LinX patch: the next `activate` of a window asks the compositor for an
/// xdg-activation token (from the latest click) instead of raising it, and
/// sends it here. Hand it to a process LinX starts, as XDG_ACTIVATION_TOKEN,
/// so that process's window opens in front. Nothing arrives on X11.
pub fn request_launch_activation_token() -> std::sync::mpsc::Receiver<String> {
    let (sender, receiver) = std::sync::mpsc::channel();
    if let Ok(mut request) = linux::LAUNCH_TOKEN_REQUEST.lock() {
        *request = Some(sender);
    }
    receiver
}
