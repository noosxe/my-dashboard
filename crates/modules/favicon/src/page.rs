//! The generator page (docs/0064 §4): source tabs + styling on the
//! left, the raster preview ladder on the right, the export flow at
//! the bottom. Every interactive element carries a `favicon-*` bridge
//! id + role (D10, docs/0010 D10).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    ActiveTheme, Disableable as _, Sizable as _,
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    color_picker::{ColorPicker, ColorPickerEvent, ColorPickerState},
    h_flex,
    input::{Input, InputEvent, InputState, Textarea, TextareaState},
    slider::{Slider, SliderEvent, SliderState},
    switch::Switch,
    tab::{Tab, TabBar},
    v_flex,
};

use dashboard_core::toasts::{self, Level, Toast};
use dashboard_core::usage_records::{RecordDraft, Recorder, Transport};

use crate::export;
use crate::render::{self, RenderError};
use crate::spec::{DesignSpec, EMOJI_GRID, FONT_FAMILIES, ImageMime, Source, ThemeColors};
use crate::state::SpecStore;

/// Preview ladder (D2/D10): raster frames of the exact emitted SVG.
const PREVIEW_SIZES: [u32; 5] = [16, 32, 96, 180, 512];

/// The source tabs (D10 order: text, emoji, image, svg).
const SOURCE_TABS: [&str; 4] = ["Text", "Emoji", "Image", "SVG"];

const WEIGHTS: [u32; 4] = [300, 400, 600, 700];

/// The single state entity (docs/0003 §4.1): the spec, its widget
/// mirrors, and the preview frames. The card and the page both observe
/// it.
pub struct FaviconModel {
    pub(crate) spec: DesignSpec,
    store: SpecStore,
    recorder: Recorder,
    fonts: Arc<resvg::usvg::fontdb::Database>,
    /// Preview frames keyed by size — rasters of the exact emitted SVG
    /// (D2), rebuilt off-thread.
    pub(crate) previews: HashMap<u32, Arc<RenderImage>>,
    /// Stale-guard for in-flight preview rebuilds.
    preview_gen: u64,
    /// Pending overwrite confirmation (D7): `Some` while the pre-flight
    /// list is on screen.
    pub(crate) collisions: Option<Vec<String>>,
    pub(crate) exporting: bool,
    /// Last validation error for the pasted-SVG input, if any.
    pub(crate) svg_error: Option<String>,

    // Widget mirrors (single-owner: the page renders them, the model
    // owns them so card + page share one truth).
    chars_input: Entity<InputState>,
    family_input: Entity<InputState>,
    emoji_input: Entity<InputState>,
    svg_textarea: Entity<TextareaState>,
    dir_input: Entity<InputState>,
    app_name_input: Entity<InputState>,
    scale_slider: Entity<SliderState>,
    radius_slider: Entity<SliderState>,
    fg_light: Entity<ColorPickerState>,
    bg0_light: Entity<ColorPickerState>,
    bg1_light: Entity<ColorPickerState>,
    dark_switch: bool, // mirrors spec.dark.is_some() for cheap render
    fg_dark: Entity<ColorPickerState>,
    bg0_dark: Entity<ColorPickerState>,
    bg1_dark: Entity<ColorPickerState>,
}

impl FaviconModel {
    pub fn new(
        store: SpecStore,
        recorder: Recorder,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let spec = store.load();

        let chars_input = cx.new(|cx| InputState::new(window, cx).placeholder("A"));
        let family_input = cx
            .new(|cx| InputState::new(window, cx).placeholder("Helvetica — any installed family"));
        let emoji_input = cx.new(|cx| InputState::new(window, cx).placeholder("🚀"));
        let svg_textarea = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(6)
                .placeholder("Paste SVG markup…")
        });
        let dir_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Export directory — type a path or Browse")
        });
        let app_name_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("My App (manifest name)"));

        let scale = source_scale(&spec).unwrap_or(0.6);
        let shape_radius = spec.shape.radius_frac;
        let scale_slider = cx.new(|_| {
            SliderState::new()
                .min(0.1)
                .max(1.0)
                .step(0.05)
                .default_value(scale)
        });
        let radius_slider = cx.new(|_| {
            SliderState::new()
                .min(0.0)
                .max(0.5)
                .step(0.01)
                .default_value(shape_radius)
        });

        let fg_light = color_picker(&spec.light.fg, window, cx);
        let bg0_light = color_picker(&spec.light.bg_start, window, cx);
        let bg1_light = color_picker(&spec.light.bg_end, window, cx);
        let dark = spec.dark.clone().unwrap_or_default();
        let fg_dark = color_picker(&dark.fg, window, cx);
        let bg0_dark = color_picker(&dark.bg_start, window, cx);
        let bg1_dark = color_picker(&dark.bg_end, window, cx);

        // Seed the free-text mirrors from the loaded spec.
        if let Source::Text { chars, family, .. } = &spec.source {
            chars_input.update(cx, |s, cx| s.set_value(chars.clone(), window, cx));
            family_input.update(cx, |s, cx| s.set_value(family.clone(), window, cx));
        }
        if let Some(dir) = &spec.export_dir {
            dir_input.update(cx, |s, cx| s.set_value(dir.clone(), window, cx));
        }
        if !spec.app_name.is_empty() {
            app_name_input.update(cx, |s, cx| s.set_value(spec.app_name.clone(), window, cx));
        }
        if let Source::Svg { data, .. } = &spec.source {
            if let Some(markup) = decode_b64(data) {
                svg_textarea.update(cx, |s, cx| s.set_value(markup, window, cx));
            }
        }

        let mut this = Self {
            store,
            recorder,
            fonts: render::system_font_db(),
            previews: HashMap::new(),
            preview_gen: 0,
            collisions: None,
            exporting: false,
            svg_error: None,
            spec,
            chars_input,
            family_input,
            emoji_input,
            svg_textarea,
            dir_input,
            app_name_input,
            scale_slider,
            radius_slider,
            fg_light,
            bg0_light,
            bg1_light,
            dark_switch: false,
            fg_dark,
            bg0_dark,
            bg1_dark,
        };
        this.dark_switch = this.spec.dark.is_some();

        // --- subscriptions: every input funnels through `edit` ---
        cx.subscribe(&this.chars_input, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                let chars = this.chars_input.read(cx).value().to_string();
                this.edit(cx, |spec| set_text_chars(spec, &chars));
            }
        })
        .detach();
        cx.subscribe(&this.family_input, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                let family = this.family_input.read(cx).value().to_string();
                this.edit(cx, |spec| {
                    if let Source::Text { family: f, .. } = &mut spec.source {
                        *f = family;
                    }
                });
            }
        })
        .detach();
        cx.subscribe(&this.emoji_input, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                let glyph = this.emoji_input.read(cx).value().to_string();
                this.edit(cx, |spec| {
                    if !glyph.trim().is_empty() {
                        spec.source = Source::Emoji {
                            glyph: glyph.trim().to_string(),
                            scale: source_scale(spec).unwrap_or(0.6),
                        };
                    }
                });
            }
        })
        .detach();
        cx.subscribe(&this.svg_textarea, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                let markup = this.svg_textarea.read(cx).value().to_string();
                this.accept_svg_markup(&markup, cx);
            }
        })
        .detach();
        cx.subscribe(&this.dir_input, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                let dir = this.dir_input.read(cx).value().to_string();
                this.edit(cx, |spec| {
                    spec.export_dir = if dir.trim().is_empty() {
                        None
                    } else {
                        Some(dir)
                    };
                });
            }
        })
        .detach();
        cx.subscribe(&this.app_name_input, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                let name = this.app_name_input.read(cx).value().to_string();
                this.edit(cx, |spec| spec.app_name = name);
            }
        })
        .detach();
        cx.subscribe(&this.scale_slider, |this, _, event: &SliderEvent, cx| {
            // `SliderEvent` is exactly Change|Release — both drag states
            // set the same scale.
            let (SliderEvent::Change(v) | SliderEvent::Release(v)) = event;
            this.edit(cx, |spec| set_source_scale(spec, v.end()));
        })
        .detach();
        cx.subscribe(&this.radius_slider, |this, _, event: &SliderEvent, cx| {
            let (SliderEvent::Change(v) | SliderEvent::Release(v)) = event;
            this.edit(cx, |spec| spec.shape.radius_frac = v.end().clamp(0.0, 0.5));
        })
        .detach();

        // Color pickers: each subscription knows its own palette + slot,
        // and edits through the same funnel. Dark picks materialize the
        // dark palette on first change (so does the toggle).
        macro_rules! color_sub {
            ($entity:expr, $light:expr, $slot:expr) => {
                cx.subscribe(&$entity, move |this, _, event: &ColorPickerEvent, cx| {
                    if let ColorPickerEvent::Change(Some(hsla)) = event {
                        let hex = hsla_to_hex(*hsla);
                        this.edit(cx, |spec| {
                            let theme = if $light {
                                &mut spec.light
                            } else {
                                spec.dark.get_or_insert_with(ThemeColors::default)
                            };
                            match $slot {
                                0 => theme.fg = hex,
                                1 => theme.bg_start = hex,
                                _ => theme.bg_end = hex,
                            }
                        });
                    }
                })
                .detach();
            };
        }
        color_sub!(this.fg_light, true, 0);
        color_sub!(this.bg0_light, true, 1);
        color_sub!(this.bg1_light, true, 2);
        color_sub!(this.fg_dark, false, 0);
        color_sub!(this.bg0_dark, false, 1);
        color_sub!(this.bg1_dark, false, 2);

        this
    }

    /// The one funnel (D3): mutate the spec, persist (debounced),
    /// rebuild previews, notify.
    fn edit(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut DesignSpec)) {
        f(&mut self.spec);
        // Any spec edit invalidates a pending overwrite confirmation —
        // the pre-flight list may no longer match what would be written.
        self.collisions = None;
        self.store.schedule(&self.spec, cx);
        self.refresh_previews(cx);
    }

    /// Re-raster the preview ladder off the UI thread (D2). A
    /// generation token drops stale results — the last edit wins.
    pub(crate) fn refresh_previews(&mut self, cx: &mut Context<Self>) {
        self.preview_gen += 1;
        let token = self.preview_gen;
        let spec = self.spec.clone();
        let db = self.fonts.clone();
        let model = cx.entity();
        cx.spawn(async move |_this, cx| {
            let frames = cx
                .background_spawn(async move {
                    let svg = render::emit_svg(&spec);
                    let mut out = HashMap::new();
                    for size in PREVIEW_SIZES {
                        if let Ok(pixmap) = render::rasterize(&svg, size, &db) {
                            out.insert(size, render::preview_frame(pixmap));
                        }
                    }
                    out
                })
                .await;
            cx.update(|cx| {
                model.update(cx, |model, cx| {
                    if token != model.preview_gen {
                        return; // a newer edit already re-rasterized
                    }
                    model.previews = frames;
                    cx.notify();
                });
            });
        })
        .detach();
    }

    /// Initial preview build — called once from `FaviconModule::init`
    /// (the entity exists by then).
    pub fn boot(&mut self, cx: &mut Context<Self>) {
        self.refresh_previews(cx);
    }

    // ---- source-tab actions ----

    pub fn select_source(&mut self, ix: usize, cx: &mut Context<Self>) {
        let source = match ix {
            0 => Source::default(),
            1 => Source::Emoji {
                glyph: current_emoji(&self.spec),
                scale: source_scale(&self.spec).unwrap_or(0.6),
            },
            2 => Source::Image {
                mime: ImageMime::Png,
                data: current_image_data(&self.spec).unwrap_or_default(),
                w: 0,
                h: 0,
            },
            _ => Source::Svg {
                data: current_svg_data(&self.spec).unwrap_or_default(),
                w: 0,
                h: 0,
            },
        };
        let scale = source_scale(&self.spec).unwrap_or(0.6);
        self.edit(cx, |spec| {
            spec.source = source;
            set_source_scale(spec, scale);
        });
    }

    pub fn set_weight(&mut self, weight: u32, cx: &mut Context<Self>) {
        self.edit(cx, |spec| {
            if let Source::Text { weight: w, .. } = &mut spec.source {
                *w = weight;
            }
        });
    }

    pub fn set_font_family(&mut self, family: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.family_input
            .update(cx, |s, cx| s.set_value(family.to_string(), window, cx));
        self.edit(cx, |spec| {
            if let Source::Text { family: f, .. } = &mut spec.source {
                *f = family.to_string();
            }
        });
    }

    pub fn pick_emoji(&mut self, glyph: &str, cx: &mut Context<Self>) {
        self.edit(cx, |spec| {
            spec.source = Source::Emoji {
                glyph: glyph.to_string(),
                scale: source_scale(spec).unwrap_or(0.6),
            }
        });
    }

    pub fn toggle_circle(&mut self, cx: &mut Context<Self>) {
        self.edit(cx, |spec| spec.shape.circle = !spec.shape.circle);
    }

    pub fn toggle_dark(&mut self, cx: &mut Context<Self>) {
        let enabling = self.spec.dark.is_none();
        self.dark_switch = enabling;
        self.edit(cx, |spec| {
            spec.dark = enabling.then(ThemeColors::default);
        });
    }

    pub fn toggle_platform(&mut self, key: &str, cx: &mut Context<Self>) {
        self.edit(cx, |spec| match key {
            "web" => spec.platforms.web = !spec.platforms.web,
            "ios" => {
                spec.platforms.ios = !spec.platforms.ios;
                if !spec.platforms.ios {
                    spec.platforms.ios_1024 = false;
                }
            }
            "pwa" => spec.platforms.pwa = !spec.platforms.pwa,
            _ => {}
        });
    }

    pub fn toggle_ios_1024(&mut self, cx: &mut Context<Self>) {
        self.edit(cx, |spec| {
            spec.platforms.ios_1024 = spec.platforms.ios && !spec.platforms.ios_1024;
        });
    }

    // ---- uploads (D4) ----

    /// Native file picker on the main thread (modal by nature), then
    /// decode + accept on the background executor.
    pub fn pick_image_upload(&mut self, cx: &mut Context<Self>) {
        let model = cx.entity();
        cx.spawn(async move |_this, cx| {
            let picked = rfd::AsyncFileDialog::new()
                .add_filter("images", &["png", "jpg", "jpeg"])
                .set_title("Choose an image")
                .pick_file()
                .await;
            let Some(file) = picked else { return };
            let path = PathBuf::from(file.path());
            let bytes = cx
                .background_spawn(async move { std::fs::read(path).ok() })
                .await;
            cx.update(|cx| {
                model.update(cx, |model, cx| match bytes {
                    Some(bytes) => model.accept_raster(&bytes, cx),
                    None => toasts::raise(
                        cx,
                        Toast::new(Level::Error, "could not read the picked file")
                            .source("favicon"),
                    ),
                });
            });
        })
        .detach();
    }

    pub fn pick_svg_upload(&mut self, cx: &mut Context<Self>) {
        let model = cx.entity();
        cx.spawn(async move |_this, cx| {
            let picked = rfd::AsyncFileDialog::new()
                .add_filter("svg", &["svg"])
                .set_title("Choose an SVG")
                .pick_file()
                .await;
            let Some(file) = picked else { return };
            let path = PathBuf::from(file.path());
            let markup = cx
                .background_spawn(async move { std::fs::read_to_string(path).ok() })
                .await;
            cx.update(|cx| {
                model.update(cx, |model, cx| match markup {
                    Some(markup) => model.accept_svg(markup, cx),
                    None => toasts::raise(
                        cx,
                        Toast::new(Level::Error, "could not read the picked file")
                            .source("favicon"),
                    ),
                });
            });
        })
        .detach();
    }

    /// Normalize + accept a raster upload (JPEG re-encoded as PNG, D4).
    fn accept_raster(&mut self, bytes: &[u8], cx: &mut Context<Self>) {
        match render::normalize_raster(bytes) {
            Ok((png, w, h)) => {
                let data = render::b64(&png);
                self.edit(cx, |spec| {
                    spec.source = Source::Image {
                        mime: ImageMime::Png,
                        data,
                        w,
                        h,
                    }
                });
            }
            Err(err) => {
                toasts::raise(
                    cx,
                    Toast::new(Level::Error, err.to_string()).source("favicon"),
                );
            }
        }
    }

    /// Validate + accept pasted/uploaded SVG markup (D4): the same
    /// usvg options the raster will use, so accepted input always
    /// renders.
    fn accept_svg_markup(&mut self, markup: &str, cx: &mut Context<Self>) {
        let trimmed = markup.trim();
        if trimmed.is_empty() {
            self.svg_error = None;
            cx.notify();
            return;
        }
        match render::validate_svg(trimmed, &self.fonts) {
            Ok((w, h)) => {
                self.svg_error = None;
                let data = render::b64(trimmed.as_bytes());
                self.edit(cx, |spec| {
                    spec.source = Source::Svg {
                        data,
                        w: w.round() as u32,
                        h: h.round() as u32,
                    }
                });
            }
            Err(RenderError::Parse(err)) => {
                // Keep the previous source; surface why this one won't
                // take (the preview keeps showing the accepted design).
                self.svg_error = Some(err);
                cx.notify();
            }
            Err(_) => {}
        }
    }

    fn accept_svg(&mut self, markup: String, cx: &mut Context<Self>) {
        self.accept_svg_markup(&markup, cx);
    }

    // ---- export (D7) ----

    pub fn pick_export_dir(&mut self, cx: &mut Context<Self>) {
        let model = cx.entity();
        cx.spawn(async move |_this, cx| {
            let picked = rfd::AsyncFileDialog::new()
                .set_title("Choose the export directory")
                .pick_folder()
                .await;
            let Some(folder) = picked else { return };
            let path = folder.path().to_path_buf();
            cx.update(|cx| {
                model.update(cx, |model, cx| {
                    // The visible field re-syncs at next render (the
                    // reconcile in `FaviconPage::render`).
                    let dir = path.display().to_string();
                    model.edit(cx, |spec| spec.export_dir = Some(dir.clone()));
                });
            });
        })
        .detach();
    }

    /// Export click: pre-flight → maybe confirm → run (D7). `force`
    /// is the confirm button's call.
    pub fn begin_export(&mut self, force: bool, cx: &mut Context<Self>) {
        if self.exporting {
            return;
        }
        let Some(dir) = self.export_dir_path() else {
            toasts::raise(
                cx,
                Toast::new(Level::Warning, "set an export directory first").source("favicon"),
            );
            return;
        };
        if export::plan(&self.spec).is_empty() {
            toasts::raise(
                cx,
                Toast::new(Level::Warning, "select at least one platform").source("favicon"),
            );
            return;
        }
        if !force {
            let hit = export::collisions(&dir, &export::plan(&self.spec));
            if !hit.is_empty() {
                self.collisions = Some(hit);
                cx.notify();
                return;
            }
        }
        self.collisions = None;
        self.exporting = true;
        cx.notify();

        let spec = self.spec.clone();
        let db = self.fonts.clone();
        let recorder = self.recorder.clone();
        let model = cx.entity();
        let started = std::time::Instant::now();
        let spec_echo = spec.clone();
        cx.spawn(async move |_this, cx| {
            let dir_echo = dir.clone();
            let outcome = cx
                .background_spawn(async move { export::export(&spec, &dir, &db) })
                .await;
            let elapsed = started.elapsed().as_millis() as u32;
            let bytes: u64 = outcome
                .written
                .iter()
                .filter_map(|name| std::fs::metadata(dir_echo.join(name)).ok())
                .map(|m| m.len())
                .sum();
            cx.update(|cx| {
                model.update(cx, |model, cx| {
                    model.exporting = false;
                    let ok = outcome.ok();
                    let message = if ok {
                        format!("{} files → {}", outcome.written.len(), dir_echo.display())
                    } else {
                        let failed: Vec<String> = outcome
                            .failed
                            .iter()
                            .map(|(name, err)| format!("{name}: {err}"))
                            .collect();
                        format!(
                            "export finished with errors — {} written, {} failed: {}",
                            outcome.written.len(),
                            outcome.failed.len(),
                            failed.join("; ")
                        )
                    };
                    toasts::raise(
                        cx,
                        Toast::new(if ok { Level::Success } else { Level::Error }, message)
                            .title("Favicon export")
                            .source("favicon"),
                    );
                    // One usage record per export run (D9) — Local
                    // transport, never a quota cost.
                    recorder.emit(RecordDraft {
                        transport: Transport::Local,
                        op: "export".into(),
                        surface: "page".into(),
                        trigger: "manual".into(),
                        ident: Some(spec_echo.platform_code().into()),
                        status: if ok { 200 } else { 500 },
                        etag_304: false,
                        cost: None,
                        remaining_after: None,
                        limit_after: None,
                        cycle_reset_epoch: None,
                        duration_ms: elapsed,
                        queue_ms: None,
                        resp_bytes: Some(bytes.min(u32::MAX as u64) as u32),
                        network_error: false,
                    });
                    cx.notify();
                });
            });
        })
        .detach();
    }

    pub fn cancel_overwrite(&mut self, cx: &mut Context<Self>) {
        self.collisions = None;
        cx.notify();
    }
    /// The visible export-dir field text (render reconcile, D7).
    pub fn dir_input_value(&self, cx: &App) -> String {
        self.dir_input.read(cx).value().to_string()
    }

    /// Point the visible field at `dir` without emitting a Change event
    /// (the spec already carries it — no echo through `edit`).
    pub fn sync_dir_input(&self, dir: &str, window: &mut Window, cx: &mut App) {
        self.dir_input
            .update(cx, |s, cx| s.set_value(dir.to_string(), window, cx));
    }

    fn export_dir_path(&self) -> Option<PathBuf> {
        self.spec
            .export_dir
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
    }

    // ---- render-facing getters ----

    pub fn source_index(&self) -> usize {
        match self.spec.source {
            Source::Text { .. } => 0,
            Source::Emoji { .. } => 1,
            Source::Image { .. } => 2,
            Source::Svg { .. } => 3,
        }
    }

    pub fn weight_index(&self) -> usize {
        let weight = match &self.spec.source {
            Source::Text { weight, .. } => *weight,
            _ => 400,
        };
        WEIGHTS.iter().position(|w| *w == weight).unwrap_or(1)
    }

    pub fn spec(&self) -> &DesignSpec {
        &self.spec
    }

    pub fn widget_refs(&self) -> WidgetHandles {
        WidgetHandles {
            chars: self.chars_input.clone(),
            family: self.family_input.clone(),
            emoji: self.emoji_input.clone(),
            svg: self.svg_textarea.clone(),
            dir: self.dir_input.clone(),
            app_name: self.app_name_input.clone(),
            scale: self.scale_slider.clone(),
            radius: self.radius_slider.clone(),
            fg_light: self.fg_light.clone(),
            bg0_light: self.bg0_light.clone(),
            bg1_light: self.bg1_light.clone(),
            fg_dark: self.fg_dark.clone(),
            bg0_dark: self.bg0_dark.clone(),
            bg1_dark: self.bg1_dark.clone(),
        }
    }
}

// ---- small pure helpers ----

fn color_picker(hex: &str, window: &mut Window, cx: &mut App) -> Entity<ColorPickerState> {
    // Inside the entity constructor the value is committed directly —
    // `set_value` (no emitted change) on the fresh state.
    cx.new(|cx| {
        let mut state = ColorPickerState::new(window, cx);
        state.set_value(hex_to_hsla(hex), window, cx);
        state
    })
}

fn hex_to_hsla(hex: &str) -> Hsla {
    let parsed = crate::spec::normalize_hex(hex).unwrap_or_else(|| "#6366f1".into());
    let v = u32::from_str_radix(parsed.trim_start_matches('#'), 16).unwrap_or(0x6366f1);
    // Channel struct, not the disallowed theme-color constructor: this
    // is a parsed user value feeding ColorPickerState, never styling.
    Rgba {
        r: ((v >> 16) & 0xff) as f32 / 255.0,
        g: ((v >> 8) & 0xff) as f32 / 255.0,
        b: (v & 0xff) as f32 / 255.0,
        a: 1.0,
    }
    .into()
}

fn hsla_to_hex(color: Hsla) -> String {
    let c = Rgba::from(color);
    let channel = |f: f32| (f.clamp(0.0, 1.0) * 255.0).round() as u32;
    format!(
        "#{:02x}{:02x}{:02x}",
        channel(c.r),
        channel(c.g),
        channel(c.b)
    )
}

fn source_scale(spec: &DesignSpec) -> Option<f32> {
    match &spec.source {
        Source::Text { scale, .. } | Source::Emoji { scale, .. } => Some(*scale),
        _ => None,
    }
}

fn set_source_scale(spec: &mut DesignSpec, scale: f32) {
    match &mut spec.source {
        Source::Text { scale: s, .. } | Source::Emoji { scale: s, .. } => {
            *s = scale.clamp(0.1, 1.0);
        }
        _ => {}
    }
}

fn set_text_chars(spec: &mut DesignSpec, raw: &str) {
    let chars: String = raw.chars().take(2).collect();
    match &mut spec.source {
        Source::Text { chars: c, .. } => *c = chars,
        _ => {
            spec.source = Source::Text {
                chars,
                family: "Helvetica".into(),
                weight: 400,
                scale: 0.6,
            };
        }
    }
}

fn current_emoji(spec: &DesignSpec) -> String {
    match &spec.source {
        Source::Emoji { glyph, .. } => glyph.clone(),
        _ => "🚀".into(),
    }
}

fn current_image_data(spec: &DesignSpec) -> Option<String> {
    match &spec.source {
        Source::Image { data, w, h, .. } if *w > 0 => Some(data.clone()),
        _ => None,
    }
}

fn current_svg_data(spec: &DesignSpec) -> Option<String> {
    match &spec.source {
        Source::Svg { data, w, h, .. } if *w > 0 => Some(data.clone()),
        _ => None,
    }
}

fn decode_b64(data: &str) -> Option<String> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(data)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
}

// ---- the page ----

pub struct FaviconPage {
    model: Entity<FaviconModel>,
}

impl FaviconPage {
    pub fn new(model: Entity<FaviconModel>, _window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self { model }
    }

    #[allow(clippy::too_many_arguments)]
    fn source_panel(
        &self,
        source_index: usize,
        weight_index: usize,
        svg_error: Option<String>,
        image_note: String,
        widgets: &WidgetHandles,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let WidgetHandles {
            chars,
            family,
            emoji,
            svg,
            scale,
            ..
        } = widgets;
        match source_index {
            0 => v_flex()
                .gap_2()
                .child(
                    h_flex().gap_2().child(
                        div().id("favicon-text-input").child(Input::new(chars).w(px(80.))),
                    ),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            div()
                                .id("favicon-font-family")
                                .child(Input::new(family).w(px(220.))),
                        )
                        .child(
                            TabBar::new("favicon-font-weight")
                                .segmented()
                                .selected_index(weight_index)
                                .on_click(cx.listener(|this, ix: &usize, _, cx| {
                                    if let Some(w) = WEIGHTS.get(*ix) {
                                        this.model.update(cx, |m, cx| m.set_weight(*w, cx));
                                    }
                                }))
                                .children(WEIGHTS.map(|w| Tab::new().label(w.to_string()))),
                        ),
                )
                .child(
                    v_flex()
                        .gap_1()
                        .child(
                            div()
                                .id("favicon-font-chips")
                                .flex_wrap()
                                .gap_1()
                        .children(FONT_FAMILIES.iter().enumerate().map(|(i, f)| {
                            let family = *f;
                            Button::new(format!("favicon-font-chip-{i}"))
                                .label(family)
                                .ghost()
                                .xsmall()
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.model.update(cx, |m, cx| {
                                        m.set_font_family(family, window, cx)
                                    });
                                }))
                        })),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("Families resolve from your installed fonts; the SVG keeps the name — machines without it fall back."),
                        ),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(div().id("favicon-scale").child(Slider::new(scale).horizontal().w(px(180.))))
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("Glyph size"),
                        ),
                )
                .into_any_element(),
            1 => v_flex()
                .gap_2()
                .child(
                    h_flex().gap_2().child(
                        div().id("favicon-emoji-input").child(Input::new(emoji).w(px(120.))),
                    ),
                )
                .child(
                    div()
                        .id("favicon-emoji-grid")
                        .flex_wrap()
                        .gap_1()
                        .children(EMOJI_GRID.iter().enumerate().map(|(i, e)| {
                            let glyph = (*e).to_string();
                            let label = glyph.clone();
                            Button::new(format!("favicon-emoji-{i}"))
                                .label(label)
                                .ghost()
                                .xsmall()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.model
                                        .update(cx, |m, cx| m.pick_emoji(&glyph, cx));
                                }))
                        })),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(div().id("favicon-scale").child(Slider::new(scale).horizontal().w(px(180.)))),
                )
                .into_any_element(),
            2 => v_flex()
                .gap_2()
                .child(
                    Button::new("favicon-image-upload")
                        .label("Upload image…")
                        .ghost()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.model.update(cx, |m, cx| m.pick_image_upload(cx));
                        })),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!(
                            "PNG/JPEG — embedded as a data URI. {image_note}",
                        )),
                )
                .into_any_element(),
            _ => v_flex()
                .gap_2()
                .child(
                    Button::new("favicon-svg-upload")
                        .label("Upload SVG…")
                        .ghost()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.model.update(cx, |m, cx| m.pick_svg_upload(cx));
                        })),
                )
                .child(div().id("favicon-svg-input").child(Textarea::new(svg).w_full()))
                .when_some(svg_error, |el, err| {
                    el.child(
                        div()
                            .id("favicon-svg-error")
                            .text_xs()
                            .text_color(cx.theme().danger_foreground)
                            .child(err),
                    )
                })
                .into_any_element(),
        }
    }
}

/// The page's widget handles, cloned per render (entities are cheap
/// Rc bumps) — owning them avoids holding the model borrow through the
/// element tree.
#[derive(Clone)]
pub struct WidgetHandles {
    pub chars: Entity<InputState>,
    pub family: Entity<InputState>,
    pub emoji: Entity<InputState>,
    pub svg: Entity<TextareaState>,
    pub dir: Entity<InputState>,
    pub app_name: Entity<InputState>,
    pub scale: Entity<SliderState>,
    pub radius: Entity<SliderState>,
    pub fg_light: Entity<ColorPickerState>,
    pub bg0_light: Entity<ColorPickerState>,
    pub bg1_light: Entity<ColorPickerState>,
    pub fg_dark: Entity<ColorPickerState>,
    pub bg0_dark: Entity<ColorPickerState>,
    pub bg1_dark: Entity<ColorPickerState>,
}

fn image_status(spec: &DesignSpec) -> String {
    match &spec.source {
        Source::Image { w, h, .. } if *w > 0 => format!("Loaded: {w}×{h}."),
        _ => "No image loaded yet.".into(),
    }
}

impl Render for FaviconPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Browse-picked dir (D7): the picker updates the spec off-band;
        // reconcile the visible field toward it. Typing keeps them equal
        // through the subscription, so this only fires after a browse.
        {
            let spec_dir = self
                .model
                .read(cx)
                .spec
                .export_dir
                .clone()
                .unwrap_or_default();
            if self.model.read(cx).dir_input_value(cx) != spec_dir {
                self.model.update(cx, |model, cx| {
                    model.sync_dir_input(&spec_dir, window, cx);
                });
            }
        }
        let model = self.model.clone();
        let widgets = model.read(cx).widget_refs();
        let m = model.read(cx);
        let dark_on = m.dark_switch;
        let exporting = m.exporting;
        let collisions = m.collisions.clone();
        let previews = m.previews.clone();
        let platforms = m.spec.platforms;
        let shape_circle = m.spec.shape.circle;
        let plan_count = export::plan(m.spec()).len();
        let source_index = m.source_index();
        let weight_index = m.weight_index();
        let svg_error = m.svg_error.clone();
        let image_note = image_status(m.spec());

        // The source-tab inputs render through `source_panel` (it owns
        // the per-tab widget picks); the render tree here needs only the
        // always-visible handles.
        let WidgetHandles {
            dir,
            app_name,
            radius,
            fg_light: fg_l,
            bg0_light: bg0_l,
            bg1_light: bg1_l,
            fg_dark: fg_d,
            bg0_dark: bg0_d,
            bg1_dark: bg1_d,
            ..
        } = widgets.clone();
        v_flex()
            .id("favicon-page")
            .size_full()
            .p_4()
            .gap_3()
            .child(
                h_flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .text_xl()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Favicon generator"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("One design → every platform icon. Files land exactly as the preview shows."),
                    ),
            )
            .child(
                h_flex()
                    .gap_4()
                    .items_start()
                    // Left: controls.
                    .child(
                        v_flex()
                            .gap_3()
                            .w(px(420.))
                            .flex_shrink_0()
                            .child(
                                TabBar::new("favicon-source-tabs")
                                    .segmented()
                                    .selected_index(source_index)
                                    .on_click(cx.listener(|this, ix: &usize, _, cx| {
                                        this.model.update(cx, |m, cx| m.select_source(*ix, cx));
                                    }))
                                    .children(
                                        SOURCE_TABS.iter().map(|t| Tab::new().label(*t)),
                                    ),
                            )
                            .child(
                                self.source_panel(
                                    source_index,
                                    weight_index,
                                    svg_error,
                                    image_note,
                                    &widgets,
                                    cx,
                                ),
                            )
                            .child(
                                v_flex()
                                    .gap_2()
                                    .child(
                                        h_flex()
                                            .gap_2()
                                            .child(ColorPicker::new(&fg_l).label("Glyph"))
                                            .child(ColorPicker::new(&bg0_l).label("Gradient start"))
                                            .child(ColorPicker::new(&bg1_l).label("Gradient end")),
                                    )
                                    .child(
                                        h_flex()
                                            .gap_2()
                                            .child(
                                                Switch::new("favicon-theme-toggle")
                                                    .checked(dark_on)
                                                    .label("Dark palette")
                                                    .on_click(cx.listener(|this, _, _, cx| {
                                                        this.model
                                                            .update(cx, |m, cx| m.toggle_dark(cx));
                                                    })),
                                            ),
                                    )
                                    .when(dark_on, |el| {
                                        el.child(
                                            h_flex().gap_2().child(
                                                ColorPicker::new(&fg_d).label("Glyph (dark)"),
                                            )
                                            .child(ColorPicker::new(&bg0_d).label("Start (dark)"))
                                            .child(ColorPicker::new(&bg1_d).label("End (dark)")),
                                        )
                                    }),
                            )
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(
                                        Switch::new("favicon-shape-circle")
                                            .checked(shape_circle)
                                            .label("Circle")
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.model
                                                    .update(cx, |m, cx| m.toggle_circle(cx));
                                            })),
                                    )
                                    .child(
                                        div()
                                            .id("favicon-shape-radius")
                                            .child(
                                                Slider::new(&radius)
                                                    .horizontal()
                                                    .disabled(shape_circle)
                                                    .w(px(160.)),
                                            ),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .gap_2()
                                    .child(
                                        h_flex()
                                            .gap_3()
                                            .child(
                                                Checkbox::new("favicon-platform-web")
                                                    .checked(platforms.web)
                                                    .label("Web")
                                                    .on_click(cx.listener(|this, _, _, cx| {
                                                        this.model.update(cx, |m, cx| {
                                                            m.toggle_platform("web", cx)
                                                        });
                                                    })),
                                            )
                                            .child(
                                                Checkbox::new("favicon-platform-ios")
                                                    .checked(platforms.ios)
                                                    .label("iOS")
                                                    .on_click(cx.listener(|this, _, _, cx| {
                                                        this.model.update(cx, |m, cx| {
                                                            m.toggle_platform("ios", cx)
                                                        });
                                                    })),
                                            )
                                            .child(
                                                Checkbox::new("favicon-platform-pwa")
                                                    .checked(platforms.pwa)
                                                    .label("Android / PWA")
                                                    .on_click(cx.listener(|this, _, _, cx| {
                                                        this.model.update(cx, |m, cx| {
                                                            m.toggle_platform("pwa", cx)
                                                        });
                                                    })),
                                            ),
                                    )
                                    .when(platforms.ios, |el| {
                                        el.child(
                                            Switch::new("favicon-ios-1024")
                                                .checked(platforms.ios_1024)
                                                .label("Include 1024×1024 marketing icon")
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.model.update(cx, |m, cx| {
                                                        m.toggle_ios_1024(cx)
                                                    });
                                                })),
                                        )
                                    }),
                            )
                            .child(
                                v_flex()
                                    .gap_2()
                                    .child(
                                        h_flex()
                                            .gap_2()
                                            .child(
                                                div()
                                                    .id("favicon-export-dir")
                                                    .child(Input::new(&dir).w(px(280.))),
                                            )
                                            .child(
                                                Button::new("favicon-export-browse")
                                                    .label("Browse…")
                                                    .ghost()
                                                    .on_click(cx.listener(|this, _, _, cx| {
                                                        this.model.update(cx, |m, cx| {
                                                            m.pick_export_dir(cx)
                                                        });
                                                    })),
                                            ),
                                    )
                                    .child(
                                        h_flex()
                                            .gap_2()
                                            .child(
                                                div().id("favicon-app-name").child(
                                                    Input::new(&app_name).w(px(200.)),
                                                ),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(cx.theme().muted_foreground)
                                                    .child(format!("{plan_count} files")),
                                            ),
                                    )
                                    .child(
                                        Button::new("favicon-export")
                                            .label(if exporting {
                                                "Exporting…".to_string()
                                            } else {
                                                "Export".to_string()
                                            })
                                            .primary()
                                            .disabled(exporting)
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.model
                                                    .update(cx, |m, cx| m.begin_export(false, cx));
                                            })),
                                    )
                                    .when_some(collisions, |el, hit| {
                                        el.child(
                                            v_flex()
                                                .id("favicon-overwrite-confirm")
                                                .gap_2()
                                                .p_3()
                                                .rounded_md()
                                                .border_1()
                                                .border_color(cx.theme().border)
                                                .child(
                                                    div().text_sm().child(format!(
                                                        "Overwrite {} existing file(s)?",
                                                        hit.len()
                                                    )),
                                                )
                                                .child(
                                                    div()
                                                        .text_xs()
                                                        .text_color(
                                                            cx.theme().muted_foreground,
                                                        )
                                                        .child(hit.join(", ")),
                                                )
                                                .child(
                                                    h_flex().gap_2().child(
                                                        Button::new("favicon-overwrite-confirm")
                                                            .label("Overwrite")
                                                            .primary()
                                                            .on_click(cx.listener(
                                                                |this, _, _, cx| {
                                                                    this.model.update(cx, |m, cx| {
                                                                        m.begin_export(true, cx)
                                                                    });
                                                                },
                                                            )),
                                                    )
                                                    .child(
                                                        Button::new("favicon-overwrite-cancel")
                                                            .label("Cancel")
                                                            .ghost()
                                                            .on_click(cx.listener(
                                                                |this, _, _, cx| {
                                                                    this.model.update(cx, |m, cx| {
                                                                        m.cancel_overwrite(cx)
                                                                    });
                                                                },
                                                            )),
                                                    ),
                                                ),
                                        )
                                    }),
                            ),
                    )
                    // Right: preview ladder (D2/D10).
                    .child(
                        v_flex()
                            .id("favicon-preview")
                            .gap_3()
                            .flex_wrap()
                            .children(
                                PREVIEW_SIZES
                                    .iter()
                                    .map(|size| preview_run(*size, &previews, cx)),
                            ),
                    ),
            )
    }
}

fn preview_run(
    size: u32,
    previews: &HashMap<u32, Arc<RenderImage>>,
    cx: &Context<FaviconPage>,
) -> AnyElement {
    let display = if size >= 512 { 256. } else { size as f32 };
    let mut frame_div = div()
        .id(format!("favicon-preview-{size}"))
        .role(accesskit::Role::Image)
        .aria_label(format!("favicon preview {size}×{size}"));
    if let Some(frame) = previews.get(&size) {
        frame_div = frame_div.child(img(frame.clone()).size(px(display)).rounded_sm());
    }
    v_flex()
        .gap_1()
        .child(frame_div)
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(format!("{size}×{size}")),
        )
        .into_any_element()
}
