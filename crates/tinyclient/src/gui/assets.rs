use gpui::{AssetSource, SharedString};
use protocol::Network;
use std::borrow::Cow;

macro_rules! assets {
    ($($path:literal),* $(,)?) => {
        const ASSETS: &[(&str, &[u8])] = &[
            $(($path, include_bytes!(concat!("../../assets/", $path)))),*
        ];
    };
}

assets!(
    "networks/telegram.svg",
    "networks/discord.svg",
    "networks/slack.svg",
    "networks/microsoft-teams.svg",
    "networks/signal.svg",
    "icons/arrow-down.svg",
    "icons/arrow-left.svg",
    "icons/arrow-right.svg",
    "icons/arrow-up-right.svg",
    "icons/at-sign.svg",
    "icons/badge-check.svg",
    "icons/bell-off.svg",
    "icons/bell.svg",
    "icons/bookmark.svg",
    "icons/building-2.svg",
    "icons/calendar.svg",
    "icons/check-check.svg",
    "icons/check.svg",
    "icons/chevron-down.svg",
    "icons/chevron-left.svg",
    "icons/chevron-right.svg",
    "icons/chevron-up.svg",
    "icons/circle-alert.svg",
    "icons/circle-plus.svg",
    "icons/clock.svg",
    "icons/copy.svg",
    "icons/corner-down-right.svg",
    "icons/corner-up-left.svg",
    "icons/download.svg",
    "icons/ellipsis.svg",
    "icons/external-link.svg",
    "icons/eye-off.svg",
    "icons/file.svg",
    "icons/grip-vertical.svg",
    "icons/hash.svg",
    "icons/headphone-off.svg",
    "icons/headphones.svg",
    "icons/image.svg",
    "icons/inbox.svg",
    "icons/info.svg",
    "icons/key.svg",
    "icons/link.svg",
    "icons/list.svg",
    "icons/lock.svg",
    "icons/log-in.svg",
    "icons/log-out.svg",
    "icons/mail.svg",
    "icons/megaphone.svg",
    "icons/message-circle.svg",
    "icons/message-square.svg",
    "icons/messages-square.svg",
    "icons/mic-off.svg",
    "icons/mic.svg",
    "icons/monitor-up.svg",
    "icons/panel-left.svg",
    "icons/paperclip.svg",
    "icons/pencil.svg",
    "icons/phone-incoming.svg",
    "icons/phone-missed.svg",
    "icons/phone-off.svg",
    "icons/phone-outgoing.svg",
    "icons/phone.svg",
    "icons/picture-in-picture-2.svg",
    "icons/pin.svg",
    "icons/plus.svg",
    "icons/refresh-cw.svg",
    "icons/reply.svg",
    "icons/search.svg",
    "icons/send-horizontal.svg",
    "icons/settings.svg",
    "icons/smile-plus.svg",
    "icons/smile.svg",
    "icons/square-pen.svg",
    "icons/square.svg",
    "icons/star.svg",
    "icons/trash-2.svg",
    "icons/triangle-alert.svg",
    "icons/user.svg",
    "icons/users.svg",
    "icons/video.svg",
    "icons/volume-2.svg",
    "icons/volume-x.svg",
    "icons/wifi-off.svg",
    "icons/x.svg",
);

/// The icons, compiled in. Nothing is read from disk or fetched at run time.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        let (path, points) = match path.split_once('@') {
            Some((path, points)) => (path, points.parse::<f32>().ok()),
            None => (path, None),
        };
        let found = ASSETS.iter().find(|(name, _)| *name == path).map(|(_, bytes)| *bytes);
        Ok(match (found, points) {
            (Some(bytes), Some(points)) if path.starts_with("icons/") => Some(stroked(bytes, points)),
            (Some(bytes), Some(points)) => Some(sized(bytes, points)),
            (found, _) => found.map(Cow::Borrowed),
        })
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(ASSETS
            .iter()
            .filter(|(name, _)| name.starts_with(path))
            .map(|(name, _)| (*name).into())
            .collect())
    }
}

/// The network's icon, to be drawn `points` across.
///
/// GPUI rasterizes an image at the size the SVG declares, not the size it is drawn. These icons
/// declare 512 units, which made a 2048 pixel texture per icon that was then shrunk to a blur. The
/// size in the path makes [`Assets`] declare the drawn size instead.
pub fn network_asset(network: Network, points: f32) -> SharedString {
    let name = match network {
        Network::Telegram => "telegram",
        Network::Discord => "discord",
        Network::Slack => "slack",
        Network::Teams => "microsoft-teams",
        Network::Signal => "signal",
    };
    format!("networks/{name}.svg@{}", points.ceil()).into()
}

/// The same SVG, declaring a size that fits `points` square and keeps its shape.
fn sized(bytes: &'static [u8], points: f32) -> Cow<'static, [u8]> {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Cow::Borrowed(bytes);
    };
    let Some(open) = text.find("<svg") else {
        return Cow::Borrowed(bytes);
    };
    let (wide, tall) = view_box(text).unwrap_or((1., 1.));
    let scale = points / wide.max(tall);
    let at = open + "<svg".len();
    Cow::Owned(
        format!(
            "{} width=\"{}\" height=\"{}\"{}",
            &text[..at],
            wide * scale,
            tall * scale,
            &text[at..]
        )
        .into_bytes(),
    )
}

fn view_box(text: &str) -> Option<(f32, f32)> {
    let rest = &text[text.find("viewBox=\"")? + "viewBox=\"".len()..];
    let mut numbers = rest[..rest.find('"')?]
        .split([' ', ','])
        .filter_map(|number| number.parse::<f32>().ok());
    let (_, _, wide, tall) = (numbers.next()?, numbers.next()?, numbers.next()?, numbers.next()?);
    (wide > 0. && tall > 0.).then_some((wide, tall))
}

/// A glyph's path for being drawn `points` across (see [`stroked`]).
pub fn glyph_asset(name: &str, points: f32) -> SharedString {
    format!("{name}@{points}").into()
}

/// The same glyph with its stroke set so that it draws 1.5 points wide at `points` across. GPUI
/// cannot restyle an SVG's stroke, and Lucide's 2 units on a 24 grid would be 1.2 points at 14
/// and 1.7 at 20.
fn stroked(bytes: &'static [u8], points: f32) -> Cow<'static, [u8]> {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Cow::Borrowed(bytes);
    };
    let width = format!("stroke-width=\"{}\"", 1.5 * 24. / points.max(1.));
    Cow::Owned(text.replacen("stroke-width=\"2\"", &width, 1).into_bytes())
}
