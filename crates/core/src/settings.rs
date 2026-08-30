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

use gpui::{App, Global, IntoElement, ParentElement as _, SharedString, Styled as _, div};
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
use crate::paths::AppPaths;
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
    /// `"light"` / `"dark"` (`ThemeMode::name()`); `None` = library default.
    pub theme_mode: Option<String>,
    /// A `ThemeRegistry` theme name (e.g. `"Default Dark"`); `None` =
    /// library default.
    pub theme_name: Option<String>,
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

/// Register bundled themes and install Mocha as the dark-mode palette:
/// the config takes the `Theme` global's dark slot, so every plain switch
/// to dark (dropdown or persisted mode, with or without a persisted
/// theme name) renders Catppuccin Mocha. A user-picked theme still
/// overrides it (set_theme writes the same slot).
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

/// Persisted theme name (`"light"`/`"dark"`); anything else → `Light`,
/// matching `ThemeMode`'s own default.
fn parse_mode(name: &str) -> ThemeMode {
    if name.eq_ignore_ascii_case("dark") {
        ThemeMode::Dark
    } else {
        ThemeMode::Light
    }
}

/// Install a named registry theme into the active mode slot and apply it.
/// Returns `false` (with a stderr note) for names the registry doesn't
/// know — hand-edited or stale settings degrade to the current theme.
fn apply_named_theme(name: &str, cx: &mut App) -> bool {
    let Some(config) = ThemeRegistry::global(cx).themes().get(name).cloned() else {
        eprintln!("dashboard: unknown theme name in settings: {name:?}");
        return false;
    };
    // Pattern from the upstream theme picker (color_theme_story.rs): the
    // config lands in the slot matching ITS mode, then `Theme::change`
    // applies the slot for the (config's) mode and syncs the base layer.
    let theme_mode = config.mode;
    let theme = Theme::global_mut(cx);
    if theme_mode.is_dark() {
        theme.dark_theme = config;
    } else {
        theme.light_theme = config;
    }
    Theme::change(theme_mode, None, cx);
    true
}

/// Boot-time restore: named theme first (its config carries a mode), then
/// the explicit mode override. Both are idempotent no-ops at defaults.
fn apply_saved_theme(cx: &mut App) {
    let name = cx.global::<AppSettings>().theme_name.clone();
    let mode = cx.global::<AppSettings>().theme_mode.clone();
    if let Some(name) = name {
        apply_named_theme(&name, cx);
    }
    if let Some(mode) = mode {
        Theme::change(parse_mode(&mode), None, cx);
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

/// Switch light/dark and persist. `cx.refresh_windows` re-styles every
/// view against the new global (the setter closures have no window).
pub(crate) fn set_theme_mode(name: &str, cx: &mut App) {
    let mode = parse_mode(name);
    Theme::change(mode, None, cx);
    cx.global_mut::<AppSettings>().theme_mode = Some(mode.name().to_string());
    persist(cx);
    cx.refresh_windows();
}

/// Select a named registry theme and persist. The theme's own mode wins
/// (the picker switches both together), mirroring the upstream picker.
pub(crate) fn set_theme(name: &str, cx: &mut App) {
    if apply_named_theme(name, cx) {
        let mode_name = cx.theme().mode.name().to_string();
        let values = cx.global_mut::<AppSettings>();
        values.theme_name = Some(name.to_string());
        values.theme_mode = Some(mode_name);
        persist(cx);
        cx.refresh_windows();
    }
}

// ---------------------------------------------------------------------------
// Appearance page
// ---------------------------------------------------------------------------

/// The core-owned Appearance subpage (docs/0006 §4.4).
pub fn appearance_page(cx: &App) -> SettingPage {
    // Registry content is static per boot (defaults + ./themes dir watched
    // by the library, if ever used) — rebuilt per render, cheap.
    let themes: Vec<(String, String)> = ThemeRegistry::global(cx)
        .sorted_themes()
        .into_iter()
        .map(|t| (t.name.to_string(), t.name.to_string()))
        .collect();
    let theme_options: Vec<(gpui::SharedString, gpui::SharedString)> = themes
        .iter()
        .map(|(name, label)| (name.clone().into(), label.clone().into()))
        .collect();

    SettingPage::new("Appearance")
        .icon(Icon::new(IconName::Palette))
        .description("Theme mode and color palette.")
        .groups(vec![SettingGroup::new().title("Theme").items(vec![
            SettingItem::new(
                "Mode",
                SettingField::dropdown(
                    vec![
                        ("light".into(), "Light".into()),
                        ("dark".into(), "Dark".into()),
                    ],
                    |cx: &App| gpui::SharedString::from(cx.theme().mode.name()),
                    |value: gpui::SharedString, cx: &mut App| set_theme_mode(&value, cx),
                )
                .default_value("light"),
            )
            .description("Light or dark appearance.")
            .keywords(["dark", "light", "mode", "night"]),
            SettingItem::new(
                "Theme",
                SettingField::scrollable_dropdown(
                    theme_options,
                    |cx: &App| cx.theme().theme_name().clone(),
                    |value: gpui::SharedString, cx: &mut App| set_theme(&value, cx),
                )
                // Not resettable: Reset All would force-apply a theme and
                // flip mode — theme choice is not a "factory default"
                // concept here (mode alone is).
            )
            .description("Color palette for the selected mode.")
            .keywords(["colors", "palette", "colorscheme"]),
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

fn open_folder_row(id: &'static str, label: &str, path: &std::path::Path) -> SettingItem {
    let url = format!("file://{}", path.display());
    let label = label.to_string();
    SettingItem::new(
        label.clone(),
        SettingField::element(OpenPathField {
            id: id.into(),
            label: label.into(),
            url,
        }),
    )
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
        .group(SettingGroup::new().title("Storage").items(vec![
                open_folder_row(
                    "open-state",
                    "State folder",
                    AppPaths::init().state_dir(),
                )
                .description("UI state and app settings (settings.json).")
                .keywords(["settings", "xdg", "state"]),
                open_folder_row(
                    "open-data",
                    "Data folder",
                    AppPaths::init().data_dir(),
                )
                .description("Module data (notes, GitHub last-known cache).")
                .keywords(["modules", "cache", "data"]),
                open_folder_row(
                    "open-cache",
                    "Cache folder",
                    AppPaths::init().cache_dir(),
                )
                .description("Ephemeral caches.")
                .keywords(["tmp", "cache"]),
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
            theme_name: Some("Default Dark".into()),
        };
        let json = serde_json::to_string(&values).unwrap();
        let back: AppSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(back.theme_mode.as_deref(), Some("dark"));
        assert_eq!(back.theme_name.as_deref(), Some("Default Dark"));
    }

    #[test]
    fn app_settings_tolerates_unknown_fields_and_empty_objects() {
        // Forward compatibility: a file written by a newer build with extra
        // fields must load fine (serde default behavior).
        let back: AppSettings =
            serde_json::from_str(r#"{"theme_mode":"light","future_field":42}"#).unwrap();
        assert_eq!(back.theme_mode.as_deref(), Some("light"));
        assert!(back.theme_name.is_none());

        let empty: AppSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, AppSettings::default());
    }

    #[test]
    fn parse_mode_defaults_to_light() {
        assert!(matches!(parse_mode("dark"), ThemeMode::Dark));
        assert!(matches!(parse_mode("DARK"), ThemeMode::Dark));
        assert!(matches!(parse_mode("light"), ThemeMode::Light));
        assert!(matches!(parse_mode("bogus"), ThemeMode::Light));
        assert!(matches!(parse_mode(""), ThemeMode::Light));
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
