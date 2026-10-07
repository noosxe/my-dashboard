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
    ActiveTheme, Icon, IconName, Sizable as _, Theme, ThemeMode, ThemeRegistry, ThemeSet,
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
use crate::toasts::{self, Level};

/// Registry names of the two bundled palettes (docs/0056 D2). The
/// registry is name-keyed and first registration wins, so user files
/// re-using these names are skipped: bundled palettes can't be shadowed.
pub const DEFAULT_LIGHT_NAME: &str = "Aurora Light";
pub const DEFAULT_DARK_NAME: &str = "Aurora Dark";

/// The factory palettes, embedded at compile time (docs/0056 D1) and
/// replayed into every [`rebuild_themes`]. Replaces docs/0025 D5's
/// boot-time snapshot of the upstream pair: the shipped defaults are
/// ours, not the library's, and a checked-in file is reviewable and
/// diffable in a way a runtime snapshot never was. Upstream's
/// `init_default_themes` stays private — irrelevant now, since nothing
/// replays its pair.
static BUNDLED_DEFAULTS: &str = include_str!("../assets/themes/aurora.json");

/// Load every flat `*.json` under `dir` into the registry (docs/0025
/// D1/D7): per-file tolerance — an unreadable or invalid file logs a
/// warning and its siblings still load. First registration wins (D6):
/// `load_themes_from_str` skips names already present; the pre-parse
/// exists only to say *which* names were skipped.
fn load_themes_dir(dir: &std::path::Path, cx: &mut App) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return; // no dir yet — nothing user-provided (the watcher creates it)
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("json"))
        .collect();
    files.sort(); // deterministic order across rebuilds
    for path in files {
        let Ok(content) = std::fs::read_to_string(&path) else {
            log::warn!("theme file unreadable: {}", path.display());
            continue;
        };
        match serde_json::from_str::<ThemeSet>(&content) {
            Ok(set) => {
                for theme in &set.themes {
                    if ThemeRegistry::global(cx).themes().contains_key(&theme.name) {
                        log::warn!(
                            "theme {:?} skipped: name already registered ({})",
                            theme.name,
                            path.display()
                        );
                    }
                }
                if let Err(err) = ThemeRegistry::global_mut(cx).load_themes_from_str(&content) {
                    log::warn!("theme file rejected: {}: {err}", path.display());
                }
            }
            Err(err) => log::warn!("ignored invalid theme file: {}: {err}", path.display()),
        }
    }
}

/// Fresh-global rebuild (docs/0025 D5): replay the bundled defaults,
/// then the user dir. The registry only ever *adds* —
/// `load_themes_from_str` skips known names and nothing evicts — so a
/// fresh global is the only way to reflect edits/removals of user files
/// on the fly. The library's registry observer re-resolves the active
/// slot configs by name and refreshes windows as the mutations land.
fn rebuild_themes(dir: Option<&std::path::Path>, cx: &mut App) {
    cx.set_global(ThemeRegistry::default());
    if let Err(err) = ThemeRegistry::global_mut(cx).load_themes_from_str(BUNDLED_DEFAULTS) {
        log::warn!("bundled defaults replay failed: {err}");
    }
    if let Some(dir) = dir {
        load_themes_dir(dir, cx);
    }
}
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
    /// (e.g. `"Aurora Light"`); `None` = factory default (docs/0056).
    pub theme_name_light: Option<String>,
    /// Palette of the **dark** slot — a dark-mode `ThemeRegistry` name;
    /// `None` = factory dark (`Aurora Dark`, docs/0056 D2).
    pub theme_name_dark: Option<String>,
    /// Notification hub preferences (docs/0062 D7); `None` = factory
    /// behavior (numeric badge, all levels, nothing muted).
    pub notifications: Option<NotificationsSettings>,
    /// Agent data bridge kill switch (docs/0068 D2); `None` = factory
    /// default, **on** (owner decision, docs/0068 §9). `Some(false)` at
    /// startup never binds `agent.sock`. No settings UI in v1 — edit
    /// `settings.json` by hand and restart the app.
    pub agent_bridge: Option<bool>,
}

impl AppSettings {
    /// Effective kill-switch value (docs/0068 D2): the field is optional
    /// with the documented factory default **on**.
    pub fn agent_bridge_enabled(&self) -> bool {
        self.agent_bridge.unwrap_or(true)
    }
}

/// Startup kill-switch read (docs/0068 D2): straight from the settings
/// file, because the agent bridge starts earlier in boot than the gpui
/// settings global is installed. A missing file or field, or a file that
/// fails to parse, means the documented default: on.
pub fn agent_bridge_enabled(path: &std::path::Path) -> bool {
    match load_json::<AppSettings>(path) {
        Ok(Some(values)) => values.agent_bridge_enabled(),
        Ok(None) => true,
        Err(err) => {
            log::warn!("agent_bridge kill switch unreadable ({err}); defaulting to on");
            true
        }
    }
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
/// handle, apply the saved theme before any window exists. With a themes
/// dir (docs/0025), the embedded factory pair (docs/0056 D1) replays, the
/// dir loads synchronously (D4), and a watcher rebuilds the registry on
/// every change under it (D5).
pub fn init(path: Option<PathBuf>, themes_dir: Option<PathBuf>, cx: &mut App) {
    let values = path.as_deref().and_then(|p| {
        load_json::<AppSettings>(p)
            .inspect_err(|err| log::warn!("app settings load failed: {err}"))
            .ok()
            .flatten()
    });
    cx.set_global(values.unwrap_or_default());
    if let Some(path) = path {
        cx.set_global(SettingsStore {
            // Registered for the quit-time flush (docs/0045 §9, #283).
            save: {
                let save = DebouncedSave::new(path, Duration::from_millis(500));
                save.register_flush_on_quit();
                save
            },
        });
    }
    rebuild_themes(themes_dir.as_deref(), cx);
    apply_saved_theme(cx);
    if let Some(dir) = themes_dir {
        // Fires once up front too (an idempotent duplicate of the sync
        // walk above) and on every create/modify/remove under the dir.
        if let Err(err) = ThemeRegistry::watch_dir(dir.clone(), cx, move |cx| {
            rebuild_themes(Some(&dir), cx);
        }) {
            log::warn!("theme watcher unavailable: {err}");
        }
    }
}

/// App name + version for the About page (from the app crate).
pub fn set_app_info(name: &'static str, version: &'static str) {
    let _ = APP_INFO.set((name, version));
}

/// App name + version (About surfaces: the D12 dialog, the
/// docs/0006 §4.4 settings page).
pub fn app_info() -> (&'static str, &'static str) {
    APP_INFO.get().copied().unwrap_or(("Dashboard", "0.0.0"))
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
        log::warn!("unknown theme name in settings: {name:?}");
        return false;
    };
    if config.mode != mode {
        log::warn!("theme {name:?} is not a {} palette", mode.name());
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

/// Install `name` into its slot; a missing name (fresh install) or one
/// that failed (stale/deleted, docs/0025 D8 — warned, persisted value
/// untouched) falls through to the embedded factory palette for that
/// mode (docs/0056 D3). Without the fallthrough the slot would keep the
/// `Theme` global's bootstrap value, which comes from the library's
/// private fallback map — still the upstream pair.
fn install_slot_or_factory(mode: ThemeMode, name: Option<String>, cx: &mut App) {
    if let Some(name) = &name {
        if install_slot_theme(mode, name, cx) {
            return;
        }
    }
    let factory = if mode.is_dark() {
        DEFAULT_DARK_NAME
    } else {
        DEFAULT_LIGHT_NAME
    };
    install_slot_theme(mode, factory, cx);
}

/// Boot-time restore: saved palettes install into their slots (light and
/// dark independently), then the saved mode applies — or, with no saved
/// mode (first boot / `System`), the OS appearance. All idempotent at
/// defaults.
fn apply_saved_theme(cx: &mut App) {
    // The factory fallback lives HERE, not in the library bootstrap: the
    // registry's private fallback map still holds the upstream pair
    // (init_default_themes, docs/0025 §2), and a fresh `Theme` global
    // bootstraps from it. Installing the embedded pair by name —
    // registered moments earlier by rebuild_themes — is what makes a
    // factory boot, and a stale-name boot, land on Aurora (docs/0056 D3).
    let values = cx.global::<AppSettings>();
    let light = values.theme_name_light.clone();
    let dark = values.theme_name_dark.clone();
    let mode = values.theme_mode.clone();
    install_slot_or_factory(ThemeMode::Light, light, cx);
    install_slot_or_factory(ThemeMode::Dark, dark, cx);
    match mode.as_deref().and_then(parse_mode_opt) {
        Some(mode) => Theme::change(mode, None, cx),
        // No window yet — the platform fallback decides
        // (`cx.window_appearance()`, xdg-desktop-portal on Linux).
        None => Theme::sync_system_appearance(None, cx),
    }
    // docs/0008 §5: what the boot restore settled on (values, not a
    // palette dump — names only).
    let values = cx.global::<AppSettings>();
    log::debug!(
        "theme applied: mode={}, light={}, dark={}",
        mode.as_deref().unwrap_or("system"),
        values.theme_name_light.as_deref().unwrap_or("(default)"),
        values.theme_name_dark.as_deref().unwrap_or("(default)"),
    );
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
// ---------------------------------------------------------------------------
// Notification settings (docs/0062 D3–D5, D7)
// ---------------------------------------------------------------------------

/// Persisted notification-hub values (docs/0062 D7). Every field
/// optional; a missing section is the factory default.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationsSettings {
    /// `"count"` (default) or `"dot"`; unknown loads degrade to count.
    pub badge_style: Option<String>,
    /// Level names counted by the badge (docs/0062 D4); `None` = all
    /// four, `Some(vec![])` = badge off. Unknown names drop on load.
    pub badge_levels: Option<Vec<String>>,
    /// Muted source ids (docs/0062 D3) — absolute suppression.
    pub muted: Option<Vec<String>>,
}

/// Badge visual mode (docs/0062 D5).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BadgeStyle {
    #[default]
    Count,
    Dot,
}

impl BadgeStyle {
    pub fn name(self) -> &'static str {
        match self {
            BadgeStyle::Count => "count",
            BadgeStyle::Dot => "dot",
        }
    }

    /// Unknown names degrade to the factory default (count).
    pub fn from_name(name: &str) -> Self {
        match name {
            "dot" => BadgeStyle::Dot,
            _ => BadgeStyle::Count,
        }
    }
}

impl AppSettings {
    /// Badge style, defaulted (docs/0062 D5).
    pub fn badge_style(&self) -> BadgeStyle {
        self.notifications
            .as_ref()
            .and_then(|n| n.badge_style.as_deref())
            .map(BadgeStyle::from_name)
            .unwrap_or_default()
    }

    /// Levels the badge counts (docs/0062 D4): the saved set with
    /// unknown names dropped, or all four when unsaved. An explicitly
    /// empty saved set stays empty — the badge-off preference.
    pub fn badge_levels(&self) -> Vec<Level> {
        self.notifications
            .as_ref()
            .and_then(|n| n.badge_levels.as_ref())
            .map(|names| {
                names
                    .iter()
                    .filter_map(|name| Level::from_name(name))
                    .collect()
            })
            .unwrap_or_else(|| Level::ALL.to_vec())
    }

    /// Muted source ids (docs/0062 D3).
    pub fn muted_sources(&self) -> &[String] {
        self.notifications
            .as_ref()
            .and_then(|n| n.muted.as_deref())
            .unwrap_or(&[])
    }
}

/// Set the badge style (docs/0062 D5).
pub fn set_badge_style(style: BadgeStyle, cx: &mut App) {
    let notifications = cx
        .global_mut::<AppSettings>()
        .notifications
        .get_or_insert_with(Default::default);
    notifications.badge_style = Some(style.name().to_string());
    persist(cx);
    cx.refresh_windows();
}

/// Toggle one level in the badge's counted set (docs/0062 D4). The
/// unsaved state reads as all-on; the first toggle materializes the
/// full set, then adds/removes the named level. Removing the last
/// level is legal — the badge-off preference.
pub fn set_badge_level(level: Level, on: bool, cx: &mut App) {
    let settings = cx.global::<AppSettings>();
    let mut names: Vec<String> = settings
        .badge_levels()
        .iter()
        .map(|l| l.name().to_string())
        .collect();
    match (on, names.iter().position(|n| n == level.name())) {
        (true, None) => names.push(level.name().to_string()),
        (false, Some(ix)) => {
            names.remove(ix);
        }
        _ => {}
    }
    let notifications = cx
        .global_mut::<AppSettings>()
        .notifications
        .get_or_insert_with(Default::default);
    notifications.badge_levels = Some(names);
    persist(cx);
    cx.refresh_windows();
}

/// Mute or unmute one source (docs/0062 D3).
pub fn set_source_muted(id: &str, muted: bool, cx: &mut App) {
    let notifications = cx
        .global_mut::<AppSettings>()
        .notifications
        .get_or_insert_with(Default::default);
    let muted_list = notifications.muted.get_or_insert_with(Vec::new);
    match (muted, muted_list.iter().position(|m| m == id)) {
        (true, None) => muted_list.push(id.to_string()),
        (false, Some(ix)) => {
            muted_list.remove(ix);
        }
        _ => {}
    }
    persist(cx);
    cx.refresh_windows();
}

/// One-shot deep-link request (docs/0062 D1/D8): the panel's gear sets
/// it, then navigates to Settings; the Settings render consumes it and
/// preselects the Notifications subpage — effective when the settings
/// subnav state is fresh (upstream keyed state remembers an existing
/// selection; a remembered subpage wins in that case).
#[derive(Default)]
struct NotificationsDeepLink(bool);

impl Global for NotificationsDeepLink {}

pub fn request_notifications_settings(cx: &mut App) {
    cx.set_global(NotificationsDeepLink(true));
}

pub(crate) fn take_notifications_settings_request(cx: &mut App) -> bool {
    let requested = cx
        .try_global::<NotificationsDeepLink>()
        .is_some_and(|g| g.0);
    if requested {
        cx.global_mut::<NotificationsDeepLink>().0 = false;
    }
    requested
}

/// The core-owned Notifications subpage (docs/0062 D1): badge style,
/// counted levels, per-source muting. Rebuilt per render from live
/// state — the source list unions the registry with log-seen ids.
pub fn notifications_page(cx: &App) -> SettingPage {
    let mut sources = toasts::sources(cx);
    if let Some(hub) = toasts::try_hub(cx) {
        for entry in hub.read(cx).log() {
            let id = entry.toast.effective_source();
            if !sources.iter().any(|s| s.id == id) {
                sources.push(toasts::SourceEntry {
                    id: id.into(),
                    label: id.into(),
                });
            }
        }
    }
    // `app` pinned first (docs/0062 §4); the rest keep arrival order.
    sources.sort_by_key(|s| s.id.as_ref() != toasts::APP_SOURCE);

    let level_items = Level::ALL
        .iter()
        .map(|&level| {
            SettingItem::new(
                level.display_name(),
                SettingField::checkbox(
                    move |cx| cx.global::<AppSettings>().badge_levels().contains(&level),
                    move |on, cx| set_badge_level(level, on, cx),
                ),
            )
            .description(format!(
                "Count {} notifications in the badge.",
                level.display_name()
            ))
        })
        .collect::<Vec<_>>();

    let source_items = sources
        .iter()
        .map(|source| {
            let id = source.id.to_string();
            let mute_id = id.clone();
            SettingItem::new(
                source.label.clone(),
                SettingField::switch(
                    move |cx| {
                        cx.global::<AppSettings>()
                            .muted_sources()
                            .iter()
                            .any(|m| m == &id)
                    },
                    move |muted, cx| set_source_muted(&mute_id, muted, cx),
                ),
            )
            .description(format!("Mute notifications from {}.", source.label))
        })
        .collect::<Vec<_>>();

    SettingPage::new("Notifications")
        .icon(Icon::new(IconName::Bell))
        .description(
            "Badge style, counted levels, and per-source muting for the notification hub.",
        )
        .groups(vec![
            SettingGroup::new()
                .title("Badge")
                .items(vec![SettingItem::new(
                    "Style",
                    SettingField::dropdown(
                        vec![
                            ("count".into(), "Count".into()),
                            ("dot".into(), "Dot".into()),
                        ],
                        |cx: &App| {
                            cx.global::<AppSettings>()
                                .badge_style()
                                .name()
                                .into()
                        },
                        |value: SharedString, cx: &mut App| {
                            set_badge_style(BadgeStyle::from_name(&value), cx);
                        },
                    )
                    .default_value("count"),
                )
                .description("Numeric unread count, or a plain dot.")
                .keywords(["badge", "count", "dot", "style", "unread"]),]),
            SettingGroup::new()
                .title("Levels")
                .description(
                    "Which levels count toward the badge. Unchecking all hides the badge; the panel still lists every unread notification.",
                )
                .items(level_items),
            SettingGroup::new()
                .title("Sources")
                .description(
                    "Muting hides all notifications from a source, including errors. Failures still show inline and in the log.",
                )
                .items(source_items),
        ])
}

// Appearance page
// ---------------------------------------------------------------------------

/// The core-owned Appearance subpage (docs/0006 §4.4).
pub fn appearance_page(cx: &App) -> SettingPage {
    // Registry content changes under the watcher (docs/0025 D5) — the
    // page rebuilds per render, so reloaded themes appear for free.
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
    SettingPage::new("Appearance")
        .icon(Icon::new(IconName::Palette))
        .description(format!(
            "Theme mode and color palette. Add your own: drop ThemeSet JSON files into {}.",
            AppPaths::init().themes_dir().display()
        ))
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
                .default_value(DEFAULT_LIGHT_NAME),
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
                // Factory dark = the library default (docs/0025 D3).
                .default_value(DEFAULT_DARK_NAME),
            )
            .description("Color palette applied in dark mode.")
            .keywords(["dark", "palette", "colors", "colorscheme"]),
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
    let (app_name, app_version) = app_info();

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
                                .text_base(),
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
            theme_name_dark: Some("Custom Dark".into()),
            notifications: None,
        };
        let json = serde_json::to_string(&values).unwrap();
        let back: AppSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(back.theme_mode.as_deref(), Some("dark"));
        assert_eq!(back.theme_name_light.as_deref(), Some("Default Light"));
        assert_eq!(back.theme_name_dark.as_deref(), Some("Custom Dark"));
    }

    #[test]
    fn app_settings_tolerates_unknown_fields_and_empty_objects() {
        // Forward compatibility: a file written by a newer build with extra
        // fields must load fine (serde default behavior).
        let back: AppSettings =
            serde_json::from_str(r#"{"theme_mode":"light","future_field":42}"#).unwrap();
        assert_eq!(back.theme_mode.as_deref(), Some("light"));
        // Legacy pre-split files: `theme_name` is dropped like any unknown
        // field; the palettes fall back to factory (docs/0025 D3 defaults).
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

    /// Unique scratch dir for theme-file fixtures (self-cleaning on
    /// success; tests that panic may leave one behind under /tmp).
    fn scratch_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "dash-settings-themes-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Boot-shaped harness: library init, factory-asset replay + dir
    /// load. Returns the sorted registry names.
    fn boot_and_names(cx: &mut gpui::TestAppContext, dir: Option<&std::path::Path>) -> Vec<String> {
        cx.update(gpui_component::init);
        cx.update(|cx| {
            rebuild_themes(dir, cx);
            let mut names = ThemeRegistry::global(cx)
                .themes()
                .keys()
                .map(|name| name.to_string())
                .collect::<Vec<_>>();
            names.sort();
            names
        })
    }

    #[gpui::test]
    fn rebuild_replays_the_bundled_defaults(cx: &mut gpui::TestAppContext) {
        // Fresh-global rebuild (as after any watcher event) must bring
        // the factory pair back — nothing else ships (docs/0056 D1).
        let names = boot_and_names(cx, None);
        assert_eq!(names, ["Aurora Dark", "Aurora Light"]);
    }

    #[gpui::test]
    fn load_dir_loads_skips_and_never_duplicates(cx: &mut gpui::TestAppContext) {
        let dir = scratch_dir("tolerant");
        std::fs::write(
            dir.join("good.json"),
            r#"{"themes":[{"name":"Test Light","mode":"light"}]}"#,
        )
        .unwrap();
        std::fs::write(dir.join("broken.json"), "{ not json").unwrap();
        std::fs::write(
            dir.join("collide.json"),
            r#"{"themes":[{"name":"Aurora Light","mode":"light"}]}"#,
        )
        .unwrap();
        let names = boot_and_names(cx, Some(&dir));
        // Valid file in; invalid file out; the default-name collision
        // skipped (D6) — one "Default Light", no duplicates (D7).
        assert_eq!(names, ["Aurora Dark", "Aurora Light", "Test Light"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[gpui::test]
    fn rebuild_reflects_edits_and_removals(cx: &mut gpui::TestAppContext) {
        let dir = scratch_dir("hotreload");
        let path = dir.join("mine.json");
        std::fs::write(
            &path,
            r##"{"themes":[{"name":"Mine","mode":"dark","colors":{"background":"#111111"}}]}"##,
        )
        .unwrap();
        cx.update(gpui_component::init);
        cx.update(|cx| {
            rebuild_themes(Some(&dir), cx);
            let config = ThemeRegistry::global(cx).themes()["Mine"].clone();
            assert_eq!(config.colors.background.as_deref(), Some("#111111"));
        });
        // Edit: same name, new hex — the add-only registry would keep
        // the stale entry; a rebuild must not.
        std::fs::write(
            &path,
            r##"{"themes":[{"name":"Mine","mode":"dark","colors":{"background":"#222222"}}]}"##,
        )
        .unwrap();
        cx.update(|cx| rebuild_themes(Some(&dir), cx));
        cx.update(|cx| {
            let config = ThemeRegistry::global(cx).themes()["Mine"].clone();
            assert_eq!(config.colors.background.as_deref(), Some("#222222"));
        });
        // Removal: the name must actually evict.
        std::fs::remove_file(&path).unwrap();
        cx.update(|cx| rebuild_themes(Some(&dir), cx));
        cx.update(|cx| {
            let names = boot_shape_names(cx);
            assert!(!names.contains(&"Mine".to_string()));
            assert_eq!(names.len(), 2); // the defaults are back, untouched
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn boot_shape_names(cx: &mut gpui::App) -> Vec<String> {
        let mut names = ThemeRegistry::global(cx)
            .themes()
            .keys()
            .map(|name| name.to_string())
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    #[gpui::test]
    fn stale_persisted_dark_name_falls_back_to_default(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(|cx| {
            rebuild_themes(None, cx);
            // Pre-0025 install: settings.json still names the removed
            // Mocha (D8). Boot restore must warn + land on the default,
            // and leave the persisted value alone.
            cx.set_global(AppSettings {
                theme_mode: Some("dark".into()),
                theme_name_light: None,
                theme_name_dark: Some("Catppuccin Mocha".into()),
                notifications: None,
            });
            apply_saved_theme(cx);
            // D8 degrade + docs/0056 D3: the unknown name warns and the
            // dark slot lands on the embedded factory pair.
            assert_eq!(cx.theme().dark_theme.name, "Aurora Dark");
            assert_eq!(
                cx.global::<AppSettings>().theme_name_dark.as_deref(),
                Some("Catppuccin Mocha")
            );
        });
    }

    #[test]
    fn notifications_settings_roundtrip_and_degrade() {
        // Round-trip a fully populated section.
        let values = AppSettings {
            notifications: Some(NotificationsSettings {
                badge_style: Some("dot".into()),
                badge_levels: Some(vec!["error".into(), "warning".into()]),
                muted: Some(vec!["github".into()]),
            }),
            ..AppSettings::default()
        };
        let json = serde_json::to_string(&values).unwrap();
        let back: AppSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(back.notifications, values.notifications);
        assert_eq!(back.badge_style(), BadgeStyle::Dot);
        assert_eq!(
            back.badge_levels(),
            vec![Level::Error, Level::Warning],
            "the saved order round-trips"
        );
        assert_eq!(back.muted_sources(), ["github".to_string()]);

        // Factory defaults: no section, empty object, missing fields.
        let factory: AppSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(factory.badge_style(), BadgeStyle::Count);
        assert_eq!(factory.badge_levels(), Level::ALL.to_vec());
        assert!(factory.muted_sources().is_empty());

        // Unknown values degrade: style falls back to count, unknown
        // level names drop, a bogus style never errors (D7).
        let hand_edited: AppSettings = serde_json::from_str(
            r#"{"notifications":{"badge_style":"neon","badge_levels":["error","bogus"],"muted":["x"]}}"#,
        )
        .unwrap();
        assert_eq!(hand_edited.badge_style(), BadgeStyle::Count);
        assert_eq!(hand_edited.badge_levels(), vec![Level::Error]);
        assert_eq!(hand_edited.muted_sources(), ["x".to_string()]);

        // An explicitly empty level set is the badge-off preference (D4).
        let badge_off: AppSettings =
            serde_json::from_str(r#"{"notifications":{"badge_levels":[]}}"#).unwrap();
        assert!(badge_off.badge_levels().is_empty());
    }

    #[gpui::test]
    fn notification_setters_toggle_levels_styles_and_mutes(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            cx.set_global(AppSettings::default());

            // Style: set and degrade-back on an unknown hand-edit.
            set_badge_style(BadgeStyle::Dot, cx);
            assert_eq!(cx.global::<AppSettings>().badge_style(), BadgeStyle::Dot);
            cx.global_mut::<AppSettings>().notifications = Some(NotificationsSettings {
                badge_style: Some("neon".into()),
                ..Default::default()
            });
            assert_eq!(cx.global::<AppSettings>().badge_style(), BadgeStyle::Count);

            // Levels: the unsaved state reads all-on; toggling Info off
            // materializes the other three; toggling the rest off lands
            // on the (legal) empty set; Info back on re-adds it.
            set_badge_level(Level::Info, false, cx);
            assert_eq!(
                cx.global::<AppSettings>().badge_levels(),
                vec![Level::Success, Level::Warning, Level::Error]
            );
            for level in [Level::Success, Level::Warning, Level::Error] {
                set_badge_level(level, false, cx);
            }
            assert!(cx.global::<AppSettings>().badge_levels().is_empty());
            set_badge_level(Level::Info, true, cx);
            assert_eq!(cx.global::<AppSettings>().badge_levels(), vec![Level::Info]);
            // Idempotent toggles.
            set_badge_level(Level::Info, true, cx);
            assert_eq!(cx.global::<AppSettings>().badge_levels(), vec![Level::Info]);

            // Mutes: add, dedupe, remove.
            set_source_muted("github", true, cx);
            set_source_muted("github", true, cx);
            set_source_muted("app", true, cx);
            assert_eq!(
                cx.global::<AppSettings>().muted_sources(),
                ["github".to_string(), "app".to_string()]
            );
            set_source_muted("github", false, cx);
            assert_eq!(
                cx.global::<AppSettings>().muted_sources(),
                ["app".to_string()]
            );

            // The deep-link flag is one-shot (D1/D8).
            assert!(!take_notifications_settings_request(cx));
            request_notifications_settings(cx);
            assert!(take_notifications_settings_request(cx));
            assert!(!take_notifications_settings_request(cx), "consumed once");
        });
    }
}
