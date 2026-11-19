use gpui_kit::*;
use std::borrow::Cow;

pub const SANS: &str = "Inter";
pub const MONO: &str = "JetBrains Mono";

static INTER: [&[u8]; 4] = [
    include_bytes!("../../../assets/fonts/inter/Inter-Regular.ttf"),
    include_bytes!("../../../assets/fonts/inter/Inter-Medium.ttf"),
    include_bytes!("../../../assets/fonts/inter/Inter-SemiBold.ttf"),
    include_bytes!("../../../assets/fonts/inter/Inter-Bold.ttf"),
];

static MONO_FACES: [&[u8]; 4] = [
    include_bytes!("../../../assets/fonts/jetbrains-mono/JetBrainsMono-Regular.ttf"),
    include_bytes!("../../../assets/fonts/jetbrains-mono/JetBrainsMono-Medium.ttf"),
    include_bytes!("../../../assets/fonts/jetbrains-mono/JetBrainsMono-SemiBold.ttf"),
    include_bytes!("../../../assets/fonts/jetbrains-mono/JetBrainsMono-Bold.ttf"),
];

pub fn register(cx: &mut App) {
    let fonts: Vec<Cow<'static, [u8]>> = INTER
        .iter()
        .chain(MONO_FACES.iter())
        .map(|bytes| Cow::Borrowed(*bytes))
        .collect();
    cx.text_system().add_fonts(fonts).expect("bundled fonts");
    let theme = gpui_kit::component::Theme::global_mut(cx);
    theme.font_family = SANS.into();
    theme.mono_font_family = MONO.into();
}
