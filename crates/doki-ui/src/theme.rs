use doki_core::config::Theme;
use gpui_kit::Rgba;
use gpui_kit::WindowAppearance;
use gpui_kit::component::ThemeMode;

#[derive(Clone, Copy)]
pub struct Palette {
    pub canvas: Rgba,
    pub chrome: Rgba,
    pub pane: Rgba,
    pub pane_raised: Rgba,
    pub surface: Rgba,
    pub card: Rgba,
    pub journal_card: Rgba,
    pub patch: Rgba,
    pub diff_add: Rgba,
    pub diff_del: Rgba,
    pub button: Rgba,
    pub button_hover: Rgba,
    pub hairline: Rgba,
    pub hairline_soft: Rgba,
    pub text: Rgba,
    pub text_muted: Rgba,
    pub text_dim: Rgba,
    pub text_faint: Rgba,
    pub accent: Rgba,
    pub accent_subtle: Rgba,
    pub accent_surface: Rgba,
    pub accent_border: Rgba,
    pub success: Rgba,
    pub success_soft: Rgba,
    pub success_border: Rgba,
    pub warning: Rgba,
    pub warning_soft: Rgba,
    pub warning_border: Rgba,
    pub warning_aura: Rgba,
    pub danger: Rgba,
    pub danger_soft: Rgba,
    pub danger_card: Rgba,
    pub danger_surface: Rgba,
    pub danger_border: Rgba,
}

const SUMI: Palette = Palette {
    canvas: hex(0x0b0c0e),
    chrome: hex(0x07080a),
    pane: hex(0x0b0c0e),
    pane_raised: hex(0x101216),
    surface: hex(0x14161d),
    card: rgba(0x10, 0x12, 0x16, 0.8),
    journal_card: hex(0x101216),
    patch: hex(0x07080a),
    diff_add: rgba(0x12, 0x26, 0x1e, 0.4),
    diff_del: rgba(0x2e, 0x18, 0x16, 0.4),
    button: hex(0x14161d),
    button_hover: hex(0x1b1e26),
    hairline: hex(0x20242e),
    hairline_soft: rgba(0x20, 0x24, 0x2e, 0.6),
    text: hex(0xf0f2f5),
    text_muted: hex(0x88909e),
    text_dim: hex(0x525866),
    text_faint: hex(0x323745),
    accent: hex(0x7aa2f7),
    accent_subtle: hex(0x6c8299),
    accent_surface: rgba(0x7a, 0xa2, 0xf7, 0.08),
    accent_border: rgba(0x7a, 0xa2, 0xf7, 0.28),
    success: hex(0x3da678),
    success_soft: hex(0x12261e),
    success_border: hex(0x1d4233),
    warning: hex(0xc89d5c),
    warning_soft: hex(0x2a2216),
    warning_border: hex(0x4a3b22),
    warning_aura: rgba(0xc8, 0x9d, 0x5c, 0.18),
    danger: hex(0xe05244),
    danger_soft: hex(0x2e1816),
    danger_card: rgba(0x2e, 0x18, 0x16, 0.3),
    danger_surface: rgba(0xe0, 0x52, 0x44, 0.1),
    danger_border: hex(0x57221d),
};

const WASHI: Palette = Palette {
    canvas: hex(0xfaf9f5),
    chrome: hex(0xf7f6f1),
    pane: hex(0xf2f1eb),
    pane_raised: hex(0xf2f1eb),
    surface: hex(0xffffff),
    card: hex(0xffffff),
    journal_card: hex(0xffffff),
    patch: hex(0xffffff),
    diff_add: rgba(0x2b, 0x7a, 0x4b, 0.08),
    diff_del: rgba(0xc5, 0x30, 0x22, 0.08),
    button: hex(0xffffff),
    button_hover: hex(0xf7f6f1),
    hairline: hex(0xe6e4dc),
    hairline_soft: rgba(0xe6, 0xe4, 0xdc, 0.7),
    text: hex(0x19191b),
    text_muted: hex(0x64636b),
    text_dim: hex(0x9b99a4),
    text_faint: hex(0xb8b6c1),
    accent: hex(0x3b5998),
    accent_subtle: hex(0x3b5998),
    accent_surface: rgba(0x3b, 0x59, 0x98, 0.07),
    accent_border: rgba(0x3b, 0x59, 0x98, 0.22),
    success: hex(0x2b7a4b),
    success_soft: rgba(0x2b, 0x7a, 0x4b, 0.08),
    success_border: hex(0xbce4cd),
    warning: hex(0xc27803),
    warning_soft: rgba(0xc2, 0x78, 0x03, 0.08),
    warning_border: hex(0xf2dfbc),
    warning_aura: rgba(0xc2, 0x78, 0x03, 0.18),
    danger: hex(0xc53022),
    danger_soft: rgba(0xc5, 0x30, 0x22, 0.08),
    danger_card: rgba(0xc5, 0x30, 0x22, 0.08),
    danger_surface: rgba(0xc5, 0x30, 0x22, 0.08),
    danger_border: hex(0xf0c2bd),
};

const fn hex(v: u32) -> Rgba {
    Rgba {
        r: ((v >> 16) & 0xff) as f32 / 255.,
        g: ((v >> 8) & 0xff) as f32 / 255.,
        b: (v & 0xff) as f32 / 255.,
        a: 1.,
    }
}

const fn rgba(r: u32, g: u32, b: u32, a: f32) -> Rgba {
    Rgba {
        r: r as f32 / 255.,
        g: g as f32 / 255.,
        b: b as f32 / 255.,
        a,
    }
}

pub fn palette(theme: Theme) -> Palette {
    match theme {
        Theme::Washi => WASHI,
        Theme::Sumi | Theme::System => SUMI,
    }
}

/// `system` follows the window, not the process: KDE can flip appearance live.
pub fn resolve(theme: Theme, appearance: WindowAppearance) -> Theme {
    match theme {
        Theme::System => match appearance {
            WindowAppearance::Light | WindowAppearance::VibrantLight => Theme::Washi,
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Theme::Sumi,
        },
        theme => theme,
    }
}

pub fn mode(theme: Theme) -> ThemeMode {
    match theme {
        Theme::Washi => ThemeMode::Light,
        Theme::Sumi | Theme::System => ThemeMode::Dark,
    }
}

pub fn label(theme: Theme) -> &'static str {
    match theme {
        Theme::Sumi => "Sumi",
        Theme::Washi => "Washi",
        Theme::System => "System",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_theme_follows_the_window_appearance() {
        assert_eq!(
            resolve(Theme::System, WindowAppearance::Light),
            Theme::Washi
        );
        assert_eq!(
            resolve(Theme::System, WindowAppearance::VibrantLight),
            Theme::Washi
        );
        assert_eq!(resolve(Theme::System, WindowAppearance::Dark), Theme::Sumi);
        assert_eq!(resolve(Theme::Sumi, WindowAppearance::Light), Theme::Sumi);
        assert_eq!(resolve(Theme::Washi, WindowAppearance::Dark), Theme::Washi);
        assert_eq!(
            palette(resolve(Theme::Washi, WindowAppearance::Dark)).canvas,
            WASHI.canvas
        );
        assert_eq!(mode(Theme::Washi), ThemeMode::Light);
        assert_eq!(mode(Theme::System), ThemeMode::Dark);
        assert_eq!(label(Theme::Washi), "Washi");
    }
}
