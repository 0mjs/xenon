use std::borrow::Cow;

use gpui::App;

const FONTS: [&[u8]; 8] = [
    include_bytes!("../../../assets/fonts/Archivo-Regular.ttf"),
    include_bytes!("../../../assets/fonts/Archivo-Medium.ttf"),
    include_bytes!("../../../assets/fonts/Archivo-SemiBold.ttf"),
    include_bytes!("../../../assets/fonts/Archivo-Bold.ttf"),
    include_bytes!("../../../assets/fonts/IBMPlexMono-Regular.ttf"),
    include_bytes!("../../../assets/fonts/IBMPlexMono-Medium.ttf"),
    include_bytes!("../../../assets/fonts/IBMPlexMono-SemiBold.ttf"),
    include_bytes!("../../../assets/fonts/IBMPlexMono-Italic.ttf"),
];

pub fn load(cx: &App) -> gpui::Result<()> {
    cx.text_system()
        .add_fonts(FONTS.iter().map(|bytes| Cow::Borrowed(*bytes)).collect())
}
