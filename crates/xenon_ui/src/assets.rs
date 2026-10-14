//! Icons bundled into the binary (Lucide, ISC licensed; see assets/icons/LICENSE-LUCIDE).

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

macro_rules! icons {
    ($($name:literal),* $(,)?) => {
        const ICONS: &[(&str, &[u8])] = &[
            $((concat!("icons/", $name, ".svg"), include_bytes!(concat!("../../../assets/icons/", $name, ".svg")))),*
        ];
    };
}

icons![
    "clock",
    "close",
    "database",
    "eye",
    "panel-left-close",
    "panel-left-open",
    "plus",
    "search",
    "table-2",
    "x"
];

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(ICONS
            .iter()
            .find(|(name, _)| *name == path)
            .map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(ICONS
            .iter()
            .filter(|(name, _)| name.starts_with(path))
            .map(|(name, _)| SharedString::new_static(name))
            .collect())
    }
}

/// Path of a bundled icon, for `gpui::svg().path(…)`.
pub fn icon(name: &'static str) -> SharedString {
    debug_assert!(
        ICONS
            .iter()
            .any(|(path, _)| path.trim_start_matches("icons/").trim_end_matches(".svg") == name)
    );
    SharedString::from(format!("icons/{name}.svg"))
}
