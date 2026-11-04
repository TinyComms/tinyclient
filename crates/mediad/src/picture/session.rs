//! Which kind of Linux desktop this process is on, from the variables a session sets.
//!
//! Wayland wins when both a Wayland display and an X11 `DISPLAY` are set: that `DISPLAY` is
//! XWayland, and grabbing its root window is a black picture. An X11-only session (XFCE, MATE,
//! Cinnamon, i3, Openbox, and the rest that set `DISPLAY` and not Wayland) never asks the
//! portal, so those desktops do not get a dialog on top of a grab that already works.

/// How the screen can be read on this seat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    /// A Wayland session, whichever compositor. The portal is the grab.
    Wayland,
    /// An X11 session. The root window is the grab.
    X11,
    /// Neither variable is set.
    None,
}

/// `wayland_display` is `WAYLAND_DISPLAY`, `session_type` is `XDG_SESSION_TYPE`, and
/// `x11_display` is `DISPLAY`. Empty is the same as missing.
pub(super) fn classify(
    wayland_display: Option<&str>,
    session_type: Option<&str>,
    x11_display: Option<&str>,
) -> Kind {
    let wayland = wayland_display.is_some_and(|value| !value.is_empty())
        || session_type.is_some_and(|value| value.eq_ignore_ascii_case("wayland"));
    if wayland {
        return Kind::Wayland;
    }
    if x11_display.is_some_and(|value| !value.is_empty()) {
        Kind::X11
    } else {
        Kind::None
    }
}

/// What this process's environment says.
#[cfg(all(target_os = "linux", not(target_env = "ohos")))]
pub(super) fn current() -> Kind {
    let wayland = std::env::var("WAYLAND_DISPLAY").ok();
    let session = std::env::var("XDG_SESSION_TYPE").ok();
    let display = std::env::var("DISPLAY").ok();
    classify(wayland.as_deref(), session.as_deref(), display.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wayland_wins_when_xwayland_is_also_there() {
        assert_eq!(
            classify(Some("wayland-0"), Some("wayland"), Some(":0")),
            Kind::Wayland
        );
        assert_eq!(classify(None, Some("wayland"), Some(":0")), Kind::Wayland);
        assert_eq!(classify(Some("wayland-1"), None, Some(":1")), Kind::Wayland);
    }

    #[test]
    fn an_x11_desktop_stays_on_x11() {
        assert_eq!(classify(None, Some("x11"), Some(":0")), Kind::X11);
        assert_eq!(classify(Some(""), Some("x11"), Some(":0")), Kind::X11);
        assert_eq!(classify(None, None, Some(":0")), Kind::X11);
    }

    #[test]
    fn nothing_set_is_neither() {
        assert_eq!(classify(None, None, None), Kind::None);
        assert_eq!(classify(Some(""), Some(""), Some("")), Kind::None);
    }
}
