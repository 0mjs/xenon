use std::borrow::Cow;

use gpui::App;

/// Every bundled font (all under the SIL Open Font License; see assets/fonts).
const FONTS: [&[u8]; 32] = [
    include_bytes!("../../../assets/fonts/Archivo-Regular.ttf"),
    include_bytes!("../../../assets/fonts/Archivo-Medium.ttf"),
    include_bytes!("../../../assets/fonts/Archivo-SemiBold.ttf"),
    include_bytes!("../../../assets/fonts/Archivo-Bold.ttf"),
    include_bytes!("../../../assets/fonts/IBMPlexMono-Regular.ttf"),
    include_bytes!("../../../assets/fonts/IBMPlexMono-Medium.ttf"),
    include_bytes!("../../../assets/fonts/IBMPlexMono-SemiBold.ttf"),
    include_bytes!("../../../assets/fonts/IBMPlexMono-Italic.ttf"),
    include_bytes!("../../../assets/fonts/Inter-Regular.ttf"),
    include_bytes!("../../../assets/fonts/Inter-Medium.ttf"),
    include_bytes!("../../../assets/fonts/Inter-SemiBold.ttf"),
    include_bytes!("../../../assets/fonts/Inter-Bold.ttf"),
    include_bytes!("../../../assets/fonts/Geist-Regular.ttf"),
    include_bytes!("../../../assets/fonts/Geist-Medium.ttf"),
    include_bytes!("../../../assets/fonts/Geist-SemiBold.ttf"),
    include_bytes!("../../../assets/fonts/Geist-Bold.ttf"),
    include_bytes!("../../../assets/fonts/IBMPlexSans-Regular.ttf"),
    include_bytes!("../../../assets/fonts/IBMPlexSans-Medium.ttf"),
    include_bytes!("../../../assets/fonts/IBMPlexSans-SemiBold.ttf"),
    include_bytes!("../../../assets/fonts/IBMPlexSans-Bold.ttf"),
    include_bytes!("../../../assets/fonts/JetBrainsMono-Regular.ttf"),
    include_bytes!("../../../assets/fonts/JetBrainsMono-Medium.ttf"),
    include_bytes!("../../../assets/fonts/JetBrainsMono-SemiBold.ttf"),
    include_bytes!("../../../assets/fonts/JetBrainsMono-Bold.ttf"),
    include_bytes!("../../../assets/fonts/FiraCode-Regular.ttf"),
    include_bytes!("../../../assets/fonts/FiraCode-Medium.ttf"),
    include_bytes!("../../../assets/fonts/FiraCode-SemiBold.ttf"),
    include_bytes!("../../../assets/fonts/FiraCode-Bold.ttf"),
    include_bytes!("../../../assets/fonts/GeistMono-Regular.ttf"),
    include_bytes!("../../../assets/fonts/GeistMono-Medium.ttf"),
    include_bytes!("../../../assets/fonts/GeistMono-SemiBold.ttf"),
    include_bytes!("../../../assets/fonts/GeistMono-Bold.ttf"),
];

pub fn load(cx: &App) -> gpui::Result<()> {
    cx.text_system()
        .add_fonts(FONTS.iter().map(|bytes| Cow::Borrowed(*bytes)).collect())
}
