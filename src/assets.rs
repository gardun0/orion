use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

pub const ICON_CAT: &str = "icons/cil--cat.svg";
pub const ICON_BOOKMARK_FILLED: &str = "icons/ix--bookmark-filled.svg";
pub const ICON_BOOKMARK: &str = "icons/ix--bookmark.svg";
pub const ICON_CONFIGURATION: &str = "icons/ix--configuration.svg";
pub const ICON_DEVICE_UNAVAILABLE: &str = "icons/ix--generic-device-io-unavailable.svg";
pub const ICON_MIX: &str = "icons/ix--mix.svg";
pub const ICON_ROUTE: &str = "icons/ix--route.svg";
pub const ICON_SAVE: &str = "icons/ix--save-all.svg";
pub const ICON_SOUND: &str = "icons/ix--sound-loud-filled.svg";
pub const ICON_MUTE: &str = "icons/ix--sound-mute-filled.svg";
pub const ICON_TYPE_PHYSICAL: &str = "icons/tdesign--audio.svg";
pub const ICON_TYPE_APP: &str = "icons/tdesign--app.svg";
pub const ICON_TYPE_VIRTUAL: &str = "icons/hugeicons--audio-wave-02.svg";
pub const ICON_ARROW_DOWN: &str = "icons/eva--arrow-down-fill.svg";
pub const ICON_ARROW_UP: &str = "icons/eva--arrow-up-fill.svg";

const ICON_PATHS: [&str; 15] = [
    ICON_CAT,
    ICON_BOOKMARK_FILLED,
    ICON_BOOKMARK,
    ICON_CONFIGURATION,
    ICON_DEVICE_UNAVAILABLE,
    ICON_MIX,
    ICON_ROUTE,
    ICON_SAVE,
    ICON_SOUND,
    ICON_MUTE,
    ICON_TYPE_PHYSICAL,
    ICON_TYPE_APP,
    ICON_TYPE_VIRTUAL,
    ICON_ARROW_DOWN,
    ICON_ARROW_UP,
];

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        let bytes: &'static [u8] = match path {
            ICON_CAT => include_bytes!("../assets/icons/cil--cat.svg"),
            ICON_BOOKMARK_FILLED => include_bytes!("../assets/icons/ix--bookmark-filled.svg"),
            ICON_BOOKMARK => include_bytes!("../assets/icons/ix--bookmark.svg"),
            ICON_CONFIGURATION => include_bytes!("../assets/icons/ix--configuration.svg"),
            ICON_DEVICE_UNAVAILABLE => {
                include_bytes!("../assets/icons/ix--generic-device-io-unavailable.svg")
            }
            ICON_MIX => include_bytes!("../assets/icons/ix--mix.svg"),
            ICON_ROUTE => include_bytes!("../assets/icons/ix--route.svg"),
            ICON_SAVE => include_bytes!("../assets/icons/ix--save-all.svg"),
            ICON_SOUND => include_bytes!("../assets/icons/ix--sound-loud-filled.svg"),
            ICON_MUTE => include_bytes!("../assets/icons/ix--sound-mute-filled.svg"),
            ICON_TYPE_PHYSICAL => include_bytes!("../assets/icons/tdesign--audio.svg"),
            ICON_TYPE_APP => include_bytes!("../assets/icons/tdesign--app.svg"),
            ICON_TYPE_VIRTUAL => include_bytes!("../assets/icons/hugeicons--audio-wave-02.svg"),
            ICON_ARROW_DOWN => include_bytes!("../assets/icons/eva--arrow-down-fill.svg"),
            ICON_ARROW_UP => include_bytes!("../assets/icons/eva--arrow-up-fill.svg"),
            _ => return Ok(None),
        };
        Ok(Some(Cow::Borrowed(bytes)))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        if path != "icons" {
            return Ok(Vec::new());
        }

        Ok(ICON_PATHS
            .iter()
            .map(|path| SharedString::from(path.rsplit('/').next().unwrap_or(path)))
            .collect())
    }
}
