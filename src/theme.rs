//! Temas de cores. A interface usa apenas os papéis semânticos de [`Palette`];
//! para criar um tema novo basta descrever uma paleta e incluí-la em [`Theme::builtin`].

use eframe::egui::{
    self, Color32, CornerRadius, FontFamily, FontId, Margin, Shadow, Stroke, TextStyle, Visuals,
    vec2,
};

/// Papéis de cor usados pela interface.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    /// Fundo da janela.
    pub background: Color32,
    /// Fundo dos cartões (itens da fila).
    pub surface: Color32,
    /// Cartão sob o mouse.
    pub surface_hover: Color32,
    /// Cartão do item atual.
    pub surface_active: Color32,
    /// Fundo de elementos rebaixados (placeholder de thumbnail, campos).
    pub sunken: Color32,
    pub border: Color32,
    pub text: Color32,
    pub text_muted: Color32,
    /// Cor de destaque (botões principais, item atual).
    pub accent: Color32,
    pub accent_hover: Color32,
    /// Texto sobre a cor de destaque.
    pub on_accent: Color32,
    pub danger: Color32,
}

#[derive(Debug, Clone)]
pub struct Theme {
    pub name: &'static str,
    pub dark: bool,
    pub palette: Palette,
}

/// Raios de borda e espaçamentos comuns a todos os temas.
pub mod metrics {
    pub const RADIUS_SM: u8 = 6;
    pub const RADIUS_MD: u8 = 10;
    pub const RADIUS_LG: u8 = 14;
    pub const GAP: f32 = 8.0;
    pub const PAGE_MARGIN: i8 = 16;
}

const fn hex(rgb: u32) -> Color32 {
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

impl Theme {
    pub fn builtin() -> Vec<Theme> {
        vec![
            Theme {
                name: "Meia-noite",
                dark: true,
                palette: Palette {
                    background: hex(0x0E1016),
                    surface: hex(0x161922),
                    surface_hover: hex(0x1D212C),
                    surface_active: hex(0x221F3A),
                    sunken: hex(0x0A0C11),
                    border: hex(0x262A36),
                    text: hex(0xE7E9F0),
                    text_muted: hex(0x8A90A2),
                    accent: hex(0x7C6CFF),
                    accent_hover: hex(0x9284FF),
                    on_accent: hex(0xFFFFFF),
                    danger: hex(0xF0646E),
                },
            },
            Theme {
                name: "Catppuccin Mocha",
                dark: true,
                palette: Palette {
                    background: hex(0x1E1E2E),
                    surface: hex(0x262637),
                    surface_hover: hex(0x2E2E42),
                    surface_active: hex(0x34304A),
                    sunken: hex(0x181825),
                    border: hex(0x363649),
                    text: hex(0xCDD6F4),
                    text_muted: hex(0x9399B2),
                    accent: hex(0xCBA6F7),
                    accent_hover: hex(0xD7BBFA),
                    on_accent: hex(0x1E1E2E),
                    danger: hex(0xF38BA8),
                },
            },
            Theme {
                name: "Dracula",
                dark: true,
                palette: Palette {
                    background: hex(0x21222C),
                    surface: hex(0x282A36),
                    surface_hover: hex(0x343746),
                    surface_active: hex(0x3B3552),
                    sunken: hex(0x191A21),
                    border: hex(0x44475A),
                    text: hex(0xF8F8F2),
                    text_muted: hex(0x8E95BC),
                    accent: hex(0xBD93F9),
                    accent_hover: hex(0xCBA9FB),
                    on_accent: hex(0x282A36),
                    danger: hex(0xFF5555),
                },
            },
            Theme {
                name: "Tokyo Night",
                dark: true,
                palette: Palette {
                    background: hex(0x1A1B26),
                    surface: hex(0x1F2335),
                    surface_hover: hex(0x292E42),
                    surface_active: hex(0x2A3352),
                    sunken: hex(0x16161E),
                    border: hex(0x2F3549),
                    text: hex(0xC0CAF5),
                    text_muted: hex(0x737AA2),
                    accent: hex(0x7AA2F7),
                    accent_hover: hex(0x8DB0F9),
                    on_accent: hex(0x1A1B26),
                    danger: hex(0xF7768E),
                },
            },
            Theme {
                name: "Gruvbox",
                dark: true,
                palette: Palette {
                    background: hex(0x1D2021),
                    surface: hex(0x282828),
                    surface_hover: hex(0x32302F),
                    surface_active: hex(0x413B2A),
                    sunken: hex(0x141617),
                    border: hex(0x3C3836),
                    text: hex(0xEBDBB2),
                    text_muted: hex(0xA89984),
                    accent: hex(0xFABD2F),
                    accent_hover: hex(0xFCCB5A),
                    on_accent: hex(0x282828),
                    danger: hex(0xFB4934),
                },
            },
            Theme {
                name: "Claro",
                dark: false,
                palette: Palette {
                    background: hex(0xF4F5F9),
                    surface: hex(0xFFFFFF),
                    surface_hover: hex(0xF0F1F7),
                    surface_active: hex(0xEDEBFF),
                    sunken: hex(0xE6E8EF),
                    border: hex(0xE1E4EC),
                    text: hex(0x1A1D26),
                    text_muted: hex(0x6B7183),
                    accent: hex(0x5B4BFF),
                    accent_hover: hex(0x6E60FF),
                    on_accent: hex(0xFFFFFF),
                    danger: hex(0xD93A4A),
                },
            },
        ]
    }

    /// Nome atual de um tema salvo com um nome antigo.
    pub fn migrate_name(name: &str) -> &str {
        match name {
            "Mocha" => "Catppuccin Mocha",
            other => other,
        }
    }

    /// Aplica o tema ao contexto do egui (cores, tipografia e espaçamentos).
    pub fn apply(&self, ctx: &egui::Context) {
        let p = &self.palette;
        let mut visuals = if self.dark {
            Visuals::dark()
        } else {
            Visuals::light()
        };

        visuals.override_text_color = Some(p.text);
        visuals.weak_text_color = Some(p.text_muted);
        visuals.panel_fill = p.background;
        visuals.window_fill = p.surface;
        visuals.window_stroke = Stroke::new(1.0, p.border);
        visuals.window_corner_radius = CornerRadius::same(metrics::RADIUS_MD);
        visuals.menu_corner_radius = CornerRadius::same(metrics::RADIUS_MD);
        visuals.window_shadow = Shadow {
            offset: [0, 6],
            blur: 18,
            spread: 0,
            color: Color32::from_black_alpha(60),
        };
        visuals.popup_shadow = visuals.window_shadow;
        visuals.extreme_bg_color = p.sunken;
        visuals.faint_bg_color = p.surface;
        visuals.hyperlink_color = p.accent;
        visuals.error_fg_color = p.danger;
        visuals.selection.bg_fill = p.accent.gamma_multiply(0.35);
        visuals.selection.stroke = Stroke::new(1.0, p.accent);

        let radius = CornerRadius::same(metrics::RADIUS_SM);
        let w = &mut visuals.widgets;
        w.noninteractive.bg_fill = p.surface;
        w.noninteractive.weak_bg_fill = p.surface;
        w.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
        w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
        for (state, fill) in [
            (&mut w.inactive, p.surface_hover),
            (&mut w.hovered, p.border),
            (&mut w.active, p.border),
            (&mut w.open, p.surface_hover),
        ] {
            state.bg_fill = fill;
            state.weak_bg_fill = fill;
            state.bg_stroke = Stroke::NONE;
            state.fg_stroke = Stroke::new(1.0, p.text);
            state.corner_radius = radius;
            state.expansion = 0.0;
        }
        w.noninteractive.corner_radius = radius;

        let egui_theme = if self.dark {
            egui::Theme::Dark
        } else {
            egui::Theme::Light
        };
        ctx.set_theme(egui_theme);
        ctx.style_mut_of(egui_theme, |style| {
            style.visuals = visuals;
            style.spacing.item_spacing = vec2(metrics::GAP, 6.0);
            style.spacing.button_padding = vec2(12.0, 6.0);
            style.spacing.interact_size.y = 30.0;
            style.spacing.window_margin = Margin::same(12);
            style.text_styles = [
                (
                    TextStyle::Heading,
                    FontId::new(20.0, FontFamily::Proportional),
                ),
                (TextStyle::Body, FontId::new(14.5, FontFamily::Proportional)),
                (
                    TextStyle::Button,
                    FontId::new(14.0, FontFamily::Proportional),
                ),
                (
                    TextStyle::Small,
                    FontId::new(12.0, FontFamily::Proportional),
                ),
                (
                    TextStyle::Monospace,
                    FontId::new(13.0, FontFamily::Monospace),
                ),
            ]
            .into();
        });
    }
}
