// ---------- theme and fonts ----------

pub(super) fn configure_native_theme(cx: &mut gpui_kit::App) {
    use gpui_kit::component::{Theme, ThemeRegistry};
    let registry = ThemeRegistry::global_mut(cx);
    let registered = registry.themes().contains_key(design::LIGHT_THEME);
    if !registered {
        registry
            .load_themes_from_str(&design::kit_theme_json())
            .expect("the product theme parses");
    }
    let light = with_syntax_colors(
        &registry.themes()[design::LIGHT_THEME],
        registry.default_light_theme(),
        &design::LIGHT,
    );
    let dark = with_syntax_colors(
        &registry.themes()[design::DARK_THEME],
        registry.default_dark_theme(),
        &design::DARK,
    );
    let theme = Theme::global_mut(cx);
    theme.light_theme = light;
    theme.dark_theme = dark;
    let mode = theme.mode;
    Theme::change(mode, None, cx);
}

pub(super) fn with_syntax_colors(
    product: &std::rc::Rc<gpui_kit::component::ThemeConfig>,
    defaults: &std::rc::Rc<gpui_kit::component::ThemeConfig>,
    palette: &design::Palette,
) -> std::rc::Rc<gpui_kit::component::ThemeConfig> {
    let mut theme = (**product).clone();
    theme.font_family = Some(FAMILY_UI.into());
    theme.mono_font_family = Some(FAMILY_MONO.into());
    let mut style = defaults.highlight.clone().unwrap_or_default();
    style.editor_background = Some(hsla_of(palette.background));
    theme.highlight = Some(style);
    std::rc::Rc::new(theme)
}

pub(super) fn hsla_of(color: design::Color) -> gpui_kit::Hsla {
    let [r, g, b, a] = color;
    gpui_kit::Rgba { r, g, b, a }.into()
}

/// The room macOS's traffic lights take at a title bar's left end, when
/// this window draws the bar itself (macOS, not fullscreen); `None`
/// elsewhere, where the system draws the title bar.
pub(super) fn traffic_lights(window: &gpui_kit::Window) -> Option<f32> {
    (cfg!(target_os = "macos") && !window.is_fullscreen()).then_some(78.)
}

pub(crate) use crate::fonts::{FAMILY_MONO, FAMILY_UI};
