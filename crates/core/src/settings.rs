//! App settings (docs/0006): values, persistence, theme application and the
//! core-owned settings subpages (Appearance, About).
//!
//! Values live in the [`AppSettings`] global; writes go through the setters
//! below, which apply immediately (no apply/cancel — every control is live)
//! and schedule a debounced save to `<state>/settings.json` via the
//! [`SettingsStore`] global. Modules contribute their own subpages through
//! `ModuleEntry.settings` (docs/0006 §4.3); this module only owns the
//! app-level ones.

use std::path::PathBuf;
use std::time::Duration;

use gpui::{App, Global, IntoElement, ParentElement as _, SharedString, Styled as _, Window, div};
use gpui_component::{
    ActiveTheme, Icon, IconName, Sizable as _, Theme, ThemeMode, ThemeRegistry,
    button::Button,
    h_flex,
    setting::{
        RenderOptions, SettingField, SettingFieldElement, SettingGroup, SettingItem, SettingPage,
    },
    v_flex,
};
use serde::{Deserialize, Serialize};

use crate::module::ModuleIcon;
use crate::store::{DebouncedSave, load_json};

/// Bundled dark palette: Catppuccin Mocha with the sapphire accent
/// (docs/0006 §3.6). Installed into the registry at boot and made the
/// app's dark-mode look by occupying the `Theme` global's dark slot —
/// plain "Dark" mode (dropdown or persisted mode) then uses it without
/// requiring a persisted theme name.
pub const CATPPUCCIN_MOCHA: &str = include_str!("../assets/themes/catppuccin-mocha.json");
pub const CATPPUCCIN_MOCHA_NAME: &str = "Catppuccin Mocha";
// ---------------------------------------------------------------------------
// Values + persistence
// ---------------------------------------------------------------------------

/// App-level settings values (docs/0006 §4.4) → `<state>/settings.json`.
/// All fields optional with serde defaults: missing files and unknown
/// fields degrade to library defaults, never errors.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppSettings {
    /// The live appearance (`System` is implicit: `None`); see `MODE_SYSTEM`.
    pub theme_mode: Option<String>,
    /// Palette of the **light** slot — a light-mode `ThemeRegistry` name
    /// (e.g. `"Default Light"`); `None` = library default.
    pub theme_name_light: Option<String>,
    /// Palette of the **dark** slot — a dark-mode `ThemeRegistry` name
    /// (e.g. the bundled `"Catppuccin Mocha"`); `None` = factory dark
    /// (Mocha, installed by [`install_bundled_theme`]).
    pub theme_name_dark: Option<String>,
}

impl Global for AppSettings {}

/// Persistence handle for [`AppSettings`]; installed by [`init`] when a
/// settings path was provided (tests may run without one — setters then
/// apply in memory only).
#[derive(Clone)]
struct SettingsStore {
    save: DebouncedSave,
}

impl Global for SettingsStore {}

/// App identity for the About page (the app crate's name/version — core
/// cannot know them at compile time).
static APP_INFO: std::sync::OnceLock<(&'static str, &'static str)> = std::sync::OnceLock::new();

/// Called once from `main` right after `gpui_component::init` (which
/// registers the default themes): load persisted values, install the save
/// handle, apply the saved theme before any window exists.
pub fn init(path: Option<PathBuf>, cx: &mut App) {
    let values = path.as_deref().and_then(|p| {
        load_json::<AppSettings>(p)
            .inspect_err(|err| eprintln!("dashboard: app settings load failed: {err}"))
            .ok()
            .flatten()
    });
    cx.set_global(values.unwrap_or_default());
    if let Some(path) = path {
        cx.set_global(SettingsStore {
            save: DebouncedSave::new(path, Duration::from_millis(500)),
        });
    }
    install_bundled_theme(cx);
    apply_saved_theme(cx);
}

/// App name + version for the About page (from the app crate).
pub fn set_app_info(name: &'static str, version: &'static str) {
    let _ = APP_INFO.set((name, version));
}

/// Register bundled themes and install Mocha as the factory dark palette:
/// the config takes the `Theme` global's dark slot, so a fresh boot with
/// the OS in dark renders Catppuccin Mocha. A persisted dark palette or
/// slot picker choice overrides it (same slot).
fn install_bundled_theme(cx: &mut App) {
    let registry = ThemeRegistry::global_mut(cx);
    if let Err(err) = registry.load_themes_from_str(CATPPUCCIN_MOCHA) {
        eprintln!("dashboard: failed to load bundled theme: {err}");
        return;
    }
    let Some(config) = ThemeRegistry::global(cx)
        .themes()
        .get(CATPPUCCIN_MOCHA_NAME)
        .cloned()
    else {
        return;
    };
    let mode = cx.theme().mode;
    Theme::global_mut(cx).dark_theme = config;
    // If the app is currently dark, re-apply so Mocha takes effect now
    // (boot restore with a persisted dark mode runs after this).
    if mode.is_dark() {
        Theme::change(mode, None, cx);
    }
}

// ---------------------------------------------------------------------------
// Theme application (docs/0006 §4.4)
// ---------------------------------------------------------------------------

/// Dropdown value meaning "follow the OS appearance"; while selected,
/// the mode is deliberately not persisted — first boot and `System` both
/// derive from the platform (xdg-desktop-portal on Linux).
pub const MODE_SYSTEM: &str = "system";

fn parse_mode_opt(name: &str) -> Option<ThemeMode> {
    if name.eq_ignore_ascii_case("dark") {
        Some(ThemeMode::Dark)
    } else if name.eq_ignore_ascii_case("light") {
        Some(ThemeMode::Light)
    } else {
        None
    }
}

/// Install a named registry theme into one specific slot — no mode
/// change, no refresh. Returns `false` for unknown names or configs
/// whose own mode doesn't match the target slot (stderr note; hand-edited
/// or stale settings degrade to the current palette).
fn install_slot_theme(mode: ThemeMode, name: &str, cx: &mut App) -> bool {
    let Some(config) = ThemeRegistry::global(cx).themes().get(name).cloned() else {
        eprintln!("dashboard: unknown theme name in settings: {name:?}");
        return false;
    };
    if config.mode != mode {
        eprintln!("dashboard: theme {name:?} is not a {} palette", mode.name());
        return false;
    }
    // Pattern from the upstream theme picker (color_theme_story.rs): the
    // config lands in its slot; `Theme::change` then applies the live
    // slot and syncs the base layer.
    let theme = Theme::global_mut(cx);
    if mode.is_dark() {
        theme.dark_theme = config;
    } else {
        theme.light_theme = config;
    }
    true
}

/// Boot-time restore: saved palettes install into their slots (light and
/// dark independently), then the saved mode applies — or, with no saved
/// mode (first boot / `System`), the OS appearance. All idempotent at
/// defaults.
fn apply_saved_theme(cx: &mut App) {
    let light = cx.global::<AppSettings>().theme_name_light.clone();
    let dark = cx.global::<AppSettings>().theme_name_dark.clone();
    let mode = cx.global::<AppSettings>().theme_mode.clone();
    if let Some(name) = light {
        install_slot_theme(ThemeMode::Light, &name, cx);
    }
    if let Some(name) = dark {
        install_slot_theme(ThemeMode::Dark, &name, cx);
    }
    match mode.as_deref().and_then(parse_mode_opt) {
        Some(mode) => Theme::change(mode, None, cx),
        // No window yet — the platform fallback decides
        // (`cx.window_appearance()`, xdg-desktop-portal on Linux).
        None => Theme::sync_system_appearance(None, cx),
    }
}

/// Schedule a debounced write of the current values (no-op without a
/// settings path).
fn persist(cx: &mut App) {
    let save = cx.try_global::<SettingsStore>().map(|s| s.save.clone());
    if let Some(save) = save {
        save.schedule(cx.global::<AppSettings>().clone(), cx);
    }
}

/// Mode-switch entry point for the dropdown. `system` clears the saved
/// mode (back to implicit follow) and re-syncs; light/dark persist.
/// `cx.refresh_windows` re-styles every view against the new global (the
/// setter closures have no window).
pub(crate) fn set_theme_mode(name: &str, cx: &mut App) {
    if name == MODE_SYSTEM {
        cx.global_mut::<AppSettings>().theme_mode = None;
        persist(cx);
        // No window here — the platform fallback decides the appearance.
        Theme::sync_system_appearance(None, cx);
        cx.refresh_windows();
        return;
    }
    let Some(mode) = parse_mode_opt(name) else {
        return;
    };
    Theme::change(mode, None, cx);
    cx.global_mut::<AppSettings>().theme_mode = Some(mode.name().to_string());
    persist(cx);
    cx.refresh_windows();
}

/// Live system-appearance follow, wired per-window from the shell: while
/// the mode is implicit (`System`), OS appearance changes re-apply the
/// theme. Explicit Light/Dark choices ignore appearance changes.
pub fn sync_window_appearance(window: &mut Window, cx: &mut App) {
    if cx.global::<AppSettings>().theme_mode.is_none() {
        Theme::sync_system_appearance(Some(window), cx);
    }
}

/// Set the palette of one slot (light or dark) and persist it. Never
/// touches the live mode: if that slot is currently displayed it is
/// re-applied, otherwise the change lands silently until the mode
/// switches (mode and palette are orthogonal).
pub(crate) fn set_theme_slot(mode: ThemeMode, name: &str, cx: &mut App) {
    if !install_slot_theme(mode, name, cx) {
        return;
    }
    let live_mode = cx.theme().mode;
    if live_mode == mode {
        Theme::change(mode, None, cx);
    }
    let values = cx.global_mut::<AppSettings>();
    if mode.is_dark() {
        values.theme_name_dark = Some(name.to_string());
    } else {
        values.theme_name_light = Some(name.to_string());
    }
    persist(cx);
    cx.refresh_windows();
}

// ---------------------------------------------------------------------------
// Appearance page
// ---------------------------------------------------------------------------

/// The core-owned Appearance subpage (docs/0006 §4.4).
pub fn appearance_page(cx: &App) -> SettingPage {
    // Registry content is static per boot (defaults + ./themes dir watched
    // by the library, if ever used) — rebuilt per render, cheap.
    let options = |mode: ThemeMode| -> Vec<(gpui::SharedString, gpui::SharedString)> {
        ThemeRegistry::global(cx)
            .sorted_themes()
            .into_iter()
            .filter(|t| t.mode == mode)
            .map(|t| {
                let name = t.name.to_string();
                (name.clone().into(), name.into())
            })
            .collect()
    };
    let light_options = options(ThemeMode::Light);
    let dark_options = options(ThemeMode::Dark);
    let default_light = ThemeRegistry::global(cx)
        .default_light_theme()
        .name
        .to_string();
    SettingPage::new("Appearance")
        .icon(Icon::new(IconName::Palette))
        .description("Theme mode and color palette.")
        .groups(vec![SettingGroup::new().title("Theme").items(vec![
            SettingItem::new(
                "Mode",
                SettingField::dropdown(
                    vec![
                        (MODE_SYSTEM.into(), "System".into()),
                        ("light".into(), "Light".into()),
                        ("dark".into(), "Dark".into()),
                    ],
                    |cx: &App| {
                        cx.global::<AppSettings>()
                            .theme_mode
                            .clone()
                            .unwrap_or_else(|| MODE_SYSTEM.to_string())
                            .into()
                    },
                    |value: gpui::SharedString, cx: &mut App| set_theme_mode(&value, cx),
                )
                .default_value(MODE_SYSTEM),
            )
            .description("Follow the OS appearance, or force light or dark.")
            .keywords(["system", "dark", "light", "mode", "night"]),
            SettingItem::new(
                "Light palette",
                SettingField::scrollable_dropdown(
                    light_options,
                    |cx: &App| cx.theme().light_theme.name.clone(),
                    |value: gpui::SharedString, cx: &mut App| {
                        set_theme_slot(ThemeMode::Light, &value, cx)
                    },
                )
                .default_value(default_light.as_str()),
            )
            .description("Color palette applied in light mode.")
            .keywords(["light", "palette", "colors", "colorscheme"]),
            SettingItem::new(
                "Dark palette",
                SettingField::scrollable_dropdown(
                    dark_options,
                    |cx: &App| cx.theme().dark_theme.name.clone(),
                    |value: gpui::SharedString, cx: &mut App| {
                        set_theme_slot(ThemeMode::Dark, &value, cx)
                    },
                )
                // Factory dark = the bundled Mocha (install_bundled_theme).
                .default_value(CATPPUCCIN_MOCHA_NAME),
            )
            .description("Color palette applied in dark mode.")
            .keywords(["dark", "palette", "mocha", "colors", "colorscheme"]),
        ])])
}

// ---------------------------------------------------------------------------
// About page
// ---------------------------------------------------------------------------

/// Button-style element opening a folder (or URL) in the system handler —
/// same launcher as the PR links on the GitHub page.
struct OpenPathField {
    id: SharedString,
    label: SharedString,
    url: String,
}

impl SettingFieldElement for OpenPathField {
    type Element = Button;

    fn render_field(&self, options: &RenderOptions, _: &mut gpui::Window, _: &mut App) -> Button {
        let url = self.url.clone();
        Button::new(self.id.clone())
            .outline()
            .label(self.label.clone())
            .with_size(options.size())
            .on_click(move |_, _, cx| cx.open_url(url.as_str()))
    }
}

/// The core-owned About subpage (docs/0006 §4.4). `modules` is a snapshot
/// of the registry (icon + title), taken by the shell per render.
pub fn about_page(modules: &[(Option<ModuleIcon>, gpui::SharedString)]) -> SettingPage {
    let (app_name, app_version) = APP_INFO.get().copied().unwrap_or(("Dashboard", "0.0.0"));

    let module_chips = modules.to_vec();
    let repo_url = "https://github.com/noosxe/my-dashboard";

    SettingPage::new("About")
        .icon(Icon::new(IconName::Info))
        .resettable(false)
        .group(
            SettingGroup::new().item(SettingItem::render(move |_, _, cx| {
                v_flex()
                    .gap_3()
                    .w_full()
                    .items_center()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(div_brand_icon(cx))
                            .child(
                                gpui_component::label::Label::new(format!(
                                    "{app_name} {app_version}"
                                ))
                                .text_size(gpui::px(16.)),
                            ),
                    )
                    .child(
                        gpui_component::label::Label::new(
                            "A personal desktop dashboard — modular, local-first.",
                        )
                        .text_color(cx.theme().muted_foreground),
                    )
                    .into_any_element()
            })),
        )
        .group(
            SettingGroup::new()
                .title("Modules")
                .item(SettingItem::render(move |_, _, cx| {
                    h_flex()
                        .flex_wrap()
                        .gap_2()
                        .children(module_chips.iter().map(|(icon, title)| {
                            h_flex()
                                .gap_1p5()
                                .items_center()
                                .px_2()
                                .py_1()
                                .rounded(cx.theme().radius)
                                .border_1()
                                .border_color(cx.theme().border)
                                .child(match icon {
                                    Some(ModuleIcon::BuiltIn(name)) => {
                                        Icon::new(name.clone()).size_4().into_any_element()
                                    }
                                    Some(ModuleIcon::Custom(path)) => {
                                        Icon::empty().path(path.clone()).size_4().into_any_element()
                                    }
                                    None => div().into_any_element(),
                                })
                                .child(gpui_component::label::Label::new(title.clone()).text_sm())
                        }))
                        .into_any_element()
                })),
        )
        .group(SettingGroup::new().title("Links").items(vec![
            SettingItem::new(
                "Repository",
                SettingField::element(OpenPathField {
                    id: "open-repo".into(),
                    label: "Open GitHub...".into(),
                    url: repo_url.into(),
                }),
            )
            .description("Source, issues and design docs.")
            .keywords(["source", "github", "issues"]),
        ]))
}

/// The brand mark: primary-colored rounded box with the gallery icon —
/// the sidebar wordmark's icon, reused as an app glyph.
fn div_brand_icon(cx: &App) -> gpui::AnyElement {
    h_flex()
        .items_center()
        .justify_center()
        .rounded(cx.theme().radius)
        .bg(cx.theme().primary)
        .text_color(cx.theme().primary_foreground)
        .size_6()
        .child(Icon::new(IconName::GalleryVerticalEnd).size_4())
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_settings_roundtrip() {
        let values = AppSettings {
            theme_mode: Some("dark".into()),
            theme_name_light: Some("Default Light".into()),
            theme_name_dark: Some("Catppuccin Mocha".into()),
        };
        let json = serde_json::to_string(&values).unwrap();
        let back: AppSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(back.theme_mode.as_deref(), Some("dark"));
        assert_eq!(back.theme_name_light.as_deref(), Some("Default Light"));
        assert_eq!(back.theme_name_dark.as_deref(), Some("Catppuccin Mocha"));
    }

    #[test]
    fn app_settings_tolerates_unknown_fields_and_empty_objects() {
        // Forward compatibility: a file written by a newer build with extra
        // fields must load fine (serde default behavior).
        let back: AppSettings =
            serde_json::from_str(r#"{"theme_mode":"light","future_field":42}"#).unwrap();
        assert_eq!(back.theme_mode.as_deref(), Some("light"));
        // Legacy pre-split files: `theme_name` is dropped like any unknown
        // field; the palettes fall back to factory (Mocha dark, stock light).
        let legacy: AppSettings = serde_json::from_str(r#"{"theme_name":"Default Dark"}"#).unwrap();
        assert!(legacy.theme_name_light.is_none());
        assert!(legacy.theme_name_dark.is_none());

        let empty: AppSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, AppSettings::default());
    }

    #[test]
    fn parse_mode_opt_matches_dropdown_values() {
        assert!(matches!(parse_mode_opt("dark"), Some(ThemeMode::Dark)));
        assert!(matches!(parse_mode_opt("DARK"), Some(ThemeMode::Dark)));
        assert!(matches!(parse_mode_opt("light"), Some(ThemeMode::Light)));
        // `system` is not a mode — selecting it clears the saved mode.
        assert!(parse_mode_opt("system").is_none());
        assert!(parse_mode_opt("").is_none());
        assert!(parse_mode_opt("bogus").is_none());
        // Boot restore: absent/garbage saved values fall back to implicit
        // system-following (`and_then(parse_mode_opt)` in apply_saved_theme).
        let none: Option<String> = None;
        assert!(none.as_deref().and_then(parse_mode_opt).is_none());
    }

    #[test]
    fn bundled_catppuccin_mocha_is_valid_dark_theme() {
        let value: serde_json::Value = serde_json::from_str(CATPPUCCIN_MOCHA).unwrap();
        let theme = &value["themes"][0];
        assert_eq!(theme["name"], "Catppuccin Mocha");
        assert_eq!(theme["mode"], "dark");
        // Sapphire accent drives the control surfaces.
        assert_eq!(theme["colors"]["primary.background"], "#74c7ec");
        assert_eq!(theme["colors"]["ring"], "#74c7ec");
        // Mocha base/mantle/text on the app shell.
        assert_eq!(theme["colors"]["background"], "#1e1e2e");
        assert_eq!(theme["colors"]["sidebar.background"], "#181825");
        assert_eq!(theme["colors"]["foreground"], "#cdd6f4");
        // It parses against the library's schema (same path as boot).
        let set: gpui_component::ThemeSet = serde_json::from_str(CATPPUCCIN_MOCHA).unwrap();
        assert_eq!(set.themes.len(), 1);
        assert!(set.themes[0].mode.is_dark());
    }
}
