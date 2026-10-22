//! The generator page (docs/0064 §12, layout v3): three zones —
//! design → see → ship. The inspector (Source / Colors / Shape) on the
//! left, the stage (the icon at size and in context) in the middle,
//! and the export rail (platforms, folder, commit) on the right; below
//! the 1040 px content-width breakpoint the rail drops under the
//! stage. Every interactive element carries a `favicon-*` bridge id +
//! role (D10 as amended by §12 L8, docs/0010 D10).

use std::cell::Cell;
use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use dashboard_core::ui::{page_canvas, page_title, section_header};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    ActiveTheme, Disableable as _, IndexPath, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    color_picker::{ColorPickerEvent, ColorPickerState, ColorSelect},
    dialog::DialogFooter,
    h_flex,
    input::{Input, InputEvent, InputState, Textarea, TextareaState},
    label::Label,
    select::{SearchableVec, Select, SelectEvent, SelectState},
    slider::{Slider, SliderEvent, SliderState},
    switch::Switch,
    tab::{Tab, TabBar},
    v_flex,
};

use dashboard_core::toasts::{self, Level, Toast};
use dashboard_core::usage_records::{RecordDraft, Recorder, Transport};

use crate::controls::{self, FontChoice, Platform, ShapeKind, Slot};
use crate::export;
use crate::render::{self, PreviewTheme, RenderError};
use crate::spec::{DesignSpec, EMOJI_GRID, ImageMime, Source};
use crate::state::SpecStore;

/// Zone widths (§12 L1, 0002 D10).
const INSPECTOR_W: f32 = 300.0;
const STAGE_MIN_W: f32 = 400.0;
const RAIL_W: f32 = 280.0;
/// Content width below which the rail drops under the stage (§12 L1,
/// owner-accepted §12.4): 300 + 400 + 280 + two 16 px gutters + the
/// canvas inset.
const RAIL_BREAKPOINT: f32 = 1040.0;

/// The inspector's one form lane (§12 L2): a 72 px label lane and one
/// 216 px control width, so every row's right edge lines up at 300.
const LABEL_W: f32 = 72.0;
const CONTROL_W: f32 = 216.0;
/// The slider's share of the control width (the percent readout and
/// its gap take the rest).
const SLIDER_W: f32 = CONTROL_W - PERCENT_W - 8.0;
const PERCENT_W: f32 = 34.0;
/// The emoji grid's fixed scroll height — about five glyph rows (L2).
const EMOJI_GRID_H: f32 = 152.0;
/// The smallest Rounded radius: the slider never reaches 0, so Rounded
/// stays Rounded while dragging (Square is its own choice, L4).
const MIN_RADIUS: f32 = 0.01;

/// Stage sizes (§12 L5): the hero's display size, and the ladder as
/// (raster size, display size, label) — labeled honestly when scaled.
const HERO_PX: f32 = 224.0;
const LADDER: [(u32, f32, &str); 4] = [
    (16, 16.0, "16"),
    (32, 32.0, "32"),
    (96, 64.0, "96 · ⅔"),
    (180, 90.0, "180 · ½"),
];
/// The stage-theme frame sizes (hero, ladder, and the in-context iOS
/// and Android frames). With the other theme's 16 px tab frame that is
/// the 7-frame budget of §12 L5.
const STAGE_SIZES: [u32; 6] = [16, 32, 96, 180, 192, 512];

/// The source tabs (D10 order: text, emoji, image, svg).
const SOURCE_TABS: [&str; 4] = ["Text", "Emoji", "Image", "SVG"];

const WEIGHTS: [u32; 4] = [300, 400, 600, 700];

/// Weight segmented-control labels (0064 §10): named tiers, not raw
/// numbers — the number stays in the spec, the label reads as a choice.
const WEIGHT_LABELS: [&str; 4] = ["Light", "Regular", "Semibold", "Bold"];

/// The font select's delegate (§12 L2).
type FontList = SearchableVec<FontChoice>;

/// A preview frame's key: raster size + which palette it shows.
pub(crate) type FrameKey = (u32, PreviewTheme);

/// The single state entity (docs/0003 §4.1): the spec, its widget
/// mirrors, and the preview frames. The card and the page both observe
/// it.
pub struct FaviconModel {
    pub(crate) spec: DesignSpec,
    store: SpecStore,
    recorder: Recorder,
    fonts: Arc<resvg::usvg::fontdb::Database>,
    /// Every installed family (L2), read once from `fonts`.
    installed_fonts: BTreeSet<String>,
    /// Preview frames — rasters of the exact emitted SVG (D2), or of the
    /// preview-only dark emission (§12 L5) — rebuilt off-thread.
    pub(crate) previews: HashMap<FrameKey, Arc<RenderImage>>,
    /// Stale-guard for in-flight preview rebuilds.
    preview_gen: u64,
    /// Pending overwrite confirmation (D7): `Some` while the pre-flight
    /// list is on screen.
    pub(crate) collisions: Option<Vec<String>>,
    pub(crate) exporting: bool,
    /// Last validation error for the pasted-SVG input, if any.
    pub(crate) svg_error: Option<String>,

    // View state — model-held, never persisted (§12 L2–L5).
    /// "All fonts" toggle (L2); off on every start.
    all_fonts: bool,
    /// The (all_fonts, family) the font select was last built for.
    font_list_key: Option<(bool, String)>,
    /// Which palette the Colors fields edit (L3): false = Light.
    palette_dark: bool,
    /// The last non-zero radius, restored by Rounded (L4).
    last_radius: f32,
    /// Which palette the stage previews (L5).
    pub(crate) stage_theme: PreviewTheme,

    // Widget mirrors (single-owner: the page renders them, the model
    // owns them so card + page share one truth).
    chars_input: Entity<InputState>,
    font_select: Entity<SelectState<FontList>>,
    emoji_input: Entity<InputState>,
    svg_textarea: Entity<TextareaState>,
    dir_input: Entity<InputState>,
    app_name_input: Entity<InputState>,
    scale_slider: Entity<SliderState>,
    radius_slider: Entity<SliderState>,
    /// The three Colors fields (L3) — one set, re-pointed at whichever
    /// palette the mode shows.
    fg_field: Entity<ColorPickerState>,
    bg0_field: Entity<ColorPickerState>,
    bg1_field: Entity<ColorPickerState>,
}

impl FaviconModel {
    pub fn new(
        store: SpecStore,
        recorder: Recorder,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let spec = store.load();
        let fonts = render::system_font_db();
        let installed_fonts = controls::installed_families(&fonts);

        let chars_input = cx.new(|cx| InputState::new(window, cx).placeholder("A"));
        let family = current_family(&spec);
        let choices = controls::font_choices(&installed_fonts, false, &family);
        let selected = choices
            .iter()
            .position(|c| c.family.as_ref() == family)
            .map(|ix| IndexPath::default().row(ix));
        let font_select = cx.new(|cx| {
            SelectState::new(SearchableVec::new(choices), selected, window, cx).searchable(true)
        });
        let emoji_input = cx.new(|cx| InputState::new(window, cx).placeholder("🚀"));
        let svg_textarea = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(6)
                .placeholder("Paste SVG markup…")
        });
        let dir_input = cx.new(|cx| InputState::new(window, cx).placeholder("Choose a folder"));
        let app_name_input = cx.new(|cx| InputState::new(window, cx).placeholder("My App"));

        let scale = source_scale(&spec).unwrap_or(0.6);
        let last_radius = controls::initial_radius(&spec.shape);
        let scale_slider = cx.new(|_| {
            SliderState::new()
                .min(0.1)
                .max(1.0)
                .step(0.05)
                .default_value(scale)
        });
        let radius_slider = cx.new(|_| {
            SliderState::new()
                .min(MIN_RADIUS)
                .max(0.5)
                .step(0.01)
                .default_value(last_radius)
        });

        let fg_field = color_picker(&spec.light.fg, window, cx);
        let bg0_field = color_picker(&spec.light.bg_start, window, cx);
        let bg1_field = color_picker(&spec.light.bg_end, window, cx);

        // Seed the free-text mirrors from the loaded spec.
        if let Source::Text { chars, .. } = &spec.source {
            chars_input.update(cx, |s, cx| s.set_value(chars.clone(), window, cx));
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

        let this = Self {
            store,
            recorder,
            fonts,
            installed_fonts,
            previews: HashMap::new(),
            preview_gen: 0,
            collisions: None,
            exporting: false,
            svg_error: None,
            all_fonts: false,
            font_list_key: Some((false, family)),
            palette_dark: false,
            last_radius,
            stage_theme: PreviewTheme::Light,
            spec,
            chars_input,
            font_select,
            emoji_input,
            svg_textarea,
            dir_input,
            app_name_input,
            scale_slider,
            radius_slider,
            fg_field,
            bg0_field,
            bg1_field,
        };

        // --- subscriptions: every input funnels through `edit` ---
        cx.subscribe(&this.chars_input, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                let chars = this.chars_input.read(cx).value().to_string();
                this.edit(cx, |spec| set_text_chars(spec, &chars));
            }
        })
        .detach();
        cx.subscribe_in(
            &this.font_select,
            window,
            |this, _, event: &SelectEvent<FontList>, window, cx| {
                let SelectEvent::Confirm(Some(family)) = event else {
                    return;
                };
                let family = family.to_string();
                this.edit(cx, |spec| {
                    if let Source::Text { family: f, .. } = &mut spec.source {
                        *f = family;
                    }
                });
                // Rebuild after the select finishes its own confirm
                // bookkeeping: a "(not installed)" row the user left
                // drops out of the list.
                cx.defer_in(window, |this, window, cx| this.sync_font_list(window, cx));
            },
        )
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
            let radius = v.end().clamp(MIN_RADIUS, 0.5);
            this.last_radius = radius;
            this.edit(cx, |spec| spec.shape.radius_frac = radius);
        })
        .detach();

        // Colors fields: each subscription knows its slot; the palette
        // is whichever the mode shows (L3) — a first Dark edit copies
        // the light palette (`controls::set_color`).
        for (field, slot) in [
            (&this.fg_field, Slot::Glyph),
            (&this.bg0_field, Slot::Start),
            (&this.bg1_field, Slot::End),
        ] {
            cx.subscribe(field, move |this, _, event: &ColorPickerEvent, cx| {
                if let ColorPickerEvent::Change(Some(hsla)) = event {
                    let hex = hsla_to_hex(*hsla);
                    let dark = this.palette_dark;
                    this.edit(cx, |spec| controls::set_color(spec, dark, slot, hex));
                }
            })
            .detach();
        }

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

    /// Re-raster the preview frames off the UI thread (D2, §12 L5): the
    /// stage-theme set plus the other theme's 16 px tab frame — 7
    /// rasters, one pass. A generation token drops stale results — the
    /// last edit wins.
    pub(crate) fn refresh_previews(&mut self, cx: &mut Context<Self>) {
        self.preview_gen += 1;
        let token = self.preview_gen;
        let spec = self.spec.clone();
        let db = self.fonts.clone();
        let theme = self.stage_theme;
        let model = cx.entity();
        cx.spawn(async move |_this, cx| {
            let frames = cx
                .background_spawn(async move {
                    let other = match theme {
                        PreviewTheme::Light => PreviewTheme::Dark,
                        PreviewTheme::Dark => PreviewTheme::Light,
                    };
                    let jobs = STAGE_SIZES
                        .iter()
                        .map(|size| (*size, theme))
                        .chain([(16, other)]);
                    let light = render::emit_preview_svg(&spec, PreviewTheme::Light);
                    let dark = render::emit_preview_svg(&spec, PreviewTheme::Dark);
                    let mut out = HashMap::new();
                    for (size, palette) in jobs {
                        let svg = match palette {
                            PreviewTheme::Light => &light,
                            PreviewTheme::Dark => &dark,
                        };
                        if let Ok(pixmap) = render::rasterize(svg, size, &db) {
                            out.insert((size, palette), render::preview_frame(pixmap));
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

    /// One stage-theme frame (hero, ladder, in-context).
    pub(crate) fn stage_frame(&self, size: u32) -> Option<Arc<RenderImage>> {
        self.previews.get(&(size, self.stage_theme)).cloned()
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

    /// "All fonts" (L2): swap the select's list; the selection stays.
    pub fn toggle_all_fonts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.all_fonts = !self.all_fonts;
        self.sync_font_list(window, cx);
        cx.notify();
    }

    /// Rebuild the font select's items when the toggle or the selected
    /// family changed (render reconcile + after a confirm). Cheap when
    /// nothing changed.
    pub fn sync_font_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let family = current_family(&self.spec);
        let key = (self.all_fonts, family.clone());
        if self.font_list_key.as_ref() == Some(&key) {
            return;
        }
        let choices = controls::font_choices(&self.installed_fonts, self.all_fonts, &family);
        self.font_select.update(cx, |select, cx| {
            select.set_items(SearchableVec::new(choices), window, cx);
            select.set_selected_value(&SharedString::from(family), window, cx);
        });
        self.font_list_key = Some(key);
    }

    pub fn pick_emoji(&mut self, glyph: &str, cx: &mut Context<Self>) {
        self.edit(cx, |spec| {
            spec.source = Source::Emoji {
                glyph: glyph.to_string(),
                scale: source_scale(spec).unwrap_or(0.6),
            }
        });
    }

    // ---- colors (L3) ----

    /// Light | Dark: which palette the Colors fields edit.
    pub fn set_palette_mode(&mut self, dark: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.palette_dark = dark;
        self.sync_color_fields(window, cx);
        cx.notify();
    }

    /// "Use light colors": drop the dark palette.
    pub fn reset_dark_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.edit(cx, controls::reset_dark);
        self.sync_color_fields(window, cx);
    }

    /// Point the three fields at the shown palette without emitting a
    /// change (no echo through `edit`).
    fn sync_color_fields(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let palette = controls::shown_palette(&self.spec, self.palette_dark).clone();
        for (field, hex) in [
            (&self.fg_field, &palette.fg),
            (&self.bg0_field, &palette.bg_start),
            (&self.bg1_field, &palette.bg_end),
        ] {
            field.update(cx, |s, cx| s.set_value(hex_to_hsla(hex), window, cx));
        }
    }

    // ---- shape (L4) ----

    pub fn set_shape(&mut self, kind: ShapeKind, window: &mut Window, cx: &mut Context<Self>) {
        let mut held = self.last_radius;
        self.edit(cx, |spec| {
            controls::apply_shape(&mut spec.shape, kind, &mut held)
        });
        self.last_radius = held;
        if kind == ShapeKind::Rounded {
            let radius = self.spec.shape.radius_frac;
            self.radius_slider
                .update(cx, |s, cx| s.set_value(radius, window, cx));
        }
    }

    // ---- stage (L5) ----

    pub fn set_stage_theme(&mut self, theme: PreviewTheme, cx: &mut Context<Self>) {
        if self.stage_theme == theme {
            return;
        }
        self.stage_theme = theme;
        self.refresh_previews(cx);
        cx.notify();
    }

    // ---- export rail (L6) ----

    pub fn toggle_platform(&mut self, platform: Platform, cx: &mut Context<Self>) {
        self.edit(cx, |spec| match platform {
            Platform::Web => spec.platforms.web = !spec.platforms.web,
            Platform::Ios => {
                spec.platforms.ios = !spec.platforms.ios;
                if !spec.platforms.ios {
                    spec.platforms.ios_1024 = false;
                }
            }
            Platform::Pwa => spec.platforms.pwa = !spec.platforms.pwa,
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
                    let mut draft = RecordDraft::new(
                        Transport::Local,
                        SharedString::from("export"),
                        SharedString::from("page"),
                        SharedString::from("manual"),
                        Some(spec_echo.platform_code().into()),
                    );
                    draft.status = if ok { 200 } else { 500 };
                    draft.duration_ms = elapsed;
                    draft.resp_bytes = Some(bytes.min(u32::MAX as u64) as u32);
                    recorder.emit(draft);
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
            font: self.font_select.clone(),
            emoji: self.emoji_input.clone(),
            svg: self.svg_textarea.clone(),
            dir: self.dir_input.clone(),
            app_name: self.app_name_input.clone(),
            scale: self.scale_slider.clone(),
            radius: self.radius_slider.clone(),
            fg: self.fg_field.clone(),
            bg0: self.bg0_field.clone(),
            bg1: self.bg1_field.clone(),
        }
    }
}

// ---- small pure helpers ----

/// The Text source's family, or "" for the other sources (no font row).
fn current_family(spec: &DesignSpec) -> String {
    match &spec.source {
        Source::Text { family, .. } => family.clone(),
        _ => String::new(),
    }
}

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
    /// Whether the last layout pass measured the page at or above the
    /// rail breakpoint (§12 L1). Written by the measuring canvas;
    /// starts wide (the canonical sizes).
    wide: Rc<Cell<bool>>,
}

/// The page's widget handles, cloned per render (entities are cheap
/// Rc bumps) — owning them avoids holding the model borrow through the
/// element tree.
#[derive(Clone)]
pub struct WidgetHandles {
    pub chars: Entity<InputState>,
    pub font: Entity<SelectState<FontList>>,
    pub emoji: Entity<InputState>,
    pub svg: Entity<TextareaState>,
    pub dir: Entity<InputState>,
    pub app_name: Entity<InputState>,
    pub scale: Entity<SliderState>,
    pub radius: Entity<SliderState>,
    pub fg: Entity<ColorPickerState>,
    pub bg0: Entity<ColorPickerState>,
    pub bg1: Entity<ColorPickerState>,
}

/// Everything one render reads off the model, snapshotted so the
/// element tree never holds the model borrow.
struct View {
    spec: DesignSpec,
    widgets: WidgetHandles,
    source_index: usize,
    weight_index: usize,
    svg_error: Option<String>,
    all_fonts: bool,
    palette_dark: bool,
    stage_theme: PreviewTheme,
    previews: HashMap<FrameKey, Arc<RenderImage>>,
    exporting: bool,
}

impl FaviconPage {
    pub fn new(model: Entity<FaviconModel>, _window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self {
            model,
            wide: Rc::new(Cell::new(true)),
        }
    }

    // ---- inspector (L2–L4) ----

    fn inspector(&self, view: &View, cx: &mut Context<Self>) -> Stateful<Div> {
        v_flex()
            .id("favicon-inspector")
            .role(accesskit::Role::Group)
            .aria_label("Design")
            .w(px(INSPECTOR_W))
            .flex_shrink_0()
            .h_full()
            .overflow_y_scroll()
            .gap_6()
            // --- Source ---
            .child(
                v_flex()
                    .gap_3()
                    .child(section_header("Source"))
                    .child(
                        TabBar::new("favicon-source-tabs")
                            .segmented()
                            .selected_index(view.source_index)
                            .on_click(cx.listener(|this, ix: &usize, _, cx| {
                                this.model.update(cx, |m, cx| m.select_source(*ix, cx));
                            }))
                            .children(SOURCE_TABS.iter().map(|t| Tab::new().label(*t))),
                    )
                    .child(self.source_panel(view, cx)),
            )
            // --- Colors ---
            .child(self.colors_section(view, cx))
            // --- Shape ---
            .child(self.shape_section(view, cx))
    }

    fn source_panel(&self, view: &View, cx: &mut Context<Self>) -> AnyElement {
        let WidgetHandles {
            chars,
            font,
            emoji,
            svg,
            scale,
            ..
        } = &view.widgets;
        let size_row = || {
            field_row(
                "Size",
                slider_control("favicon-scale", scale, scale.read(cx).value().end(), cx),
            )
        };
        match view.source_index {
            0 => v_flex()
                .gap_4()
                .child(field_row(
                    "Characters",
                    div()
                        .id("favicon-text-input")
                        .child(Input::new(chars).w(px(CONTROL_W))),
                ))
                .child(field_row_top(
                    "Font",
                    v_flex()
                        .w(px(CONTROL_W))
                        .gap_1p5()
                        .child(
                            Select::new(font)
                                .id("favicon-font-family")
                                .accessibility_label("Font")
                                .search_placeholder("Search fonts")
                                .w_full(),
                        )
                        .child(
                            Switch::new("favicon-font-all")
                                .checked(view.all_fonts)
                                .label("All fonts")
                                .xsmall()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.model
                                        .update(cx, |m, cx| m.toggle_all_fonts(window, cx));
                                })),
                        )
                        .child(muted_note(
                            "Fonts resolve on the viewer's machine; missing ones fall back.",
                            cx,
                        )),
                ))
                .child(field_row_stacked(
                    "Weight",
                    TabBar::new("favicon-font-weight")
                        .segmented()
                        .selected_index(view.weight_index)
                        .on_click(cx.listener(|this, ix: &usize, _, cx| {
                            if let Some(w) = WEIGHTS.get(*ix) {
                                this.model.update(cx, |m, cx| m.set_weight(*w, cx));
                            }
                        }))
                        .children(WEIGHT_LABELS.iter().map(|label| Tab::new().label(*label))),
                ))
                .child(size_row())
                .into_any_element(),
            1 => v_flex()
                .gap_4()
                .child(field_row(
                    "Emoji",
                    div()
                        .id("favicon-emoji-input")
                        .child(Input::new(emoji).w(px(CONTROL_W))),
                ))
                .child(
                    // Fixed-height scroll region (L2): the ~100 glyphs
                    // no longer push Colors and Shape off the column.
                    div()
                        .id("favicon-emoji-grid")
                        .h(px(EMOJI_GRID_H))
                        .overflow_y_scroll()
                        .child(h_flex().w_full().flex_wrap().gap_3().children(
                            EMOJI_GRID.iter().enumerate().map(|(i, e)| {
                                let glyph = (*e).to_string();
                                let label = glyph.clone();
                                Button::new(format!("favicon-emoji-{i}"))
                                    .label(label)
                                    .ghost()
                                    .xsmall()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.model.update(cx, |m, cx| m.pick_emoji(&glyph, cx));
                                    }))
                            }),
                        )),
                )
                .child(size_row())
                .into_any_element(),
            2 => v_flex()
                .gap_1p5()
                .child(
                    Button::new("favicon-image-upload")
                        .label("Upload image…")
                        .outline()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.model.update(cx, |m, cx| m.pick_image_upload(cx));
                        })),
                )
                .child(muted_note(
                    format!(
                        "{} PNG or JPEG, embedded into the SVG.",
                        image_status(&view.spec)
                    ),
                    cx,
                ))
                .into_any_element(),
            _ => v_flex()
                .gap_4()
                .child(
                    v_flex()
                        .gap_1p5()
                        .child(
                            Button::new("favicon-svg-upload")
                                .label("Upload SVG…")
                                .outline()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.model.update(cx, |m, cx| m.pick_svg_upload(cx));
                                })),
                        )
                        .child(match view.svg_error.clone() {
                            Some(err) => div()
                                .id("favicon-svg-error")
                                .text_xs()
                                .text_color(cx.theme().danger_foreground)
                                .child(err)
                                .into_any_element(),
                            None => muted_note(svg_status(&view.spec), cx).into_any_element(),
                        }),
                )
                .child(
                    div()
                        .id("favicon-svg-input")
                        .child(Textarea::new(svg).w_full()),
                )
                .into_any_element(),
        }
    }

    fn colors_section(&self, view: &View, cx: &mut Context<Self>) -> Div {
        let WidgetHandles { fg, bg0, bg1, .. } = &view.widgets;
        let dark = view.palette_dark;
        let mode = if dark { "dark" } else { "light" };
        let field = |id: String, state: &Entity<ColorPickerState>, name: &str| {
            div().id(id).child(
                ColorSelect::new(state)
                    .accessibility_label(format!("{name} color ({mode})"))
                    .w(px(CONTROL_W)),
            )
        };
        v_flex().gap_3().child(section_header("Colors")).child(
            v_flex()
                .gap_4()
                .child(field_row(
                    "Mode",
                    TabBar::new("favicon-palette-mode")
                        .segmented()
                        .selected_index(usize::from(dark))
                        .on_click(cx.listener(|this, ix: &usize, window, cx| {
                            let dark = *ix == 1;
                            this.model
                                .update(cx, |m, cx| m.set_palette_mode(dark, window, cx));
                        }))
                        .w(px(CONTROL_W))
                        .child(Tab::new().flex_1().label("Light"))
                        .child(Tab::new().flex_1().label("Dark")),
                ))
                .child(field_row(
                    "Glyph",
                    field(format!("favicon-fg-{mode}"), fg, "Glyph"),
                ))
                .child(field_row(
                    "Start",
                    field(format!("favicon-bg-start-{mode}"), bg0, "Gradient start"),
                ))
                .child(field_row(
                    "End",
                    field(format!("favicon-bg-end-{mode}"), bg1, "Gradient end"),
                ))
                .when(dark, |el| {
                    el.child(field_row(
                        "",
                        if view.spec.dark.is_some() {
                            Button::new("favicon-dark-reset")
                                .label("Use light colors")
                                .ghost()
                                .small()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.model
                                        .update(cx, |m, cx| m.reset_dark_palette(window, cx));
                                }))
                                .into_any_element()
                        } else {
                            muted_note("Dark reuses these until you change it.", cx)
                                .into_any_element()
                        },
                    ))
                }),
        )
    }

    fn shape_section(&self, view: &View, cx: &mut Context<Self>) -> Div {
        let kind = ShapeKind::of(&view.spec.shape);
        let radius = &view.widgets.radius;
        v_flex().gap_3().child(section_header("Shape")).child(
            v_flex()
                .gap_4()
                .child(field_row(
                    "Shape",
                    TabBar::new("favicon-shape")
                        .segmented()
                        .selected_index(kind.index())
                        .on_click(cx.listener(|this, ix: &usize, window, cx| {
                            if let Some(kind) = ShapeKind::ALL.get(*ix).copied() {
                                this.model.update(cx, |m, cx| m.set_shape(kind, window, cx));
                            }
                        }))
                        .w(px(CONTROL_W))
                        .children(
                            ShapeKind::ALL
                                .iter()
                                .map(|k| Tab::new().flex_1().label(k.label())),
                        ),
                ))
                .when(kind == ShapeKind::Rounded, |el| {
                    el.child(field_row(
                        "Radius",
                        slider_control(
                            "favicon-shape-radius",
                            radius,
                            radius.read(cx).value().end(),
                            cx,
                        ),
                    ))
                }),
        )
    }

    // ---- stage (L5) ----

    fn stage(&self, view: &View, wide: bool, cx: &mut Context<Self>) -> Stateful<Div> {
        let theme = cx.theme();
        let app_name = export::effective_app_name(&view.spec);
        let frame = |size: u32| view.previews.get(&(size, view.stage_theme)).cloned();
        let tab_light = view.previews.get(&(16, PreviewTheme::Light)).cloned();
        let tab_dark = view.previews.get(&(16, PreviewTheme::Dark)).cloned();
        let stage = v_flex()
            .id("favicon-stage")
            .role(accesskit::Role::Group)
            .aria_label("Preview")
            .flex_1()
            .min_w(px(STAGE_MIN_W))
            .bg(theme.muted)
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .p_4()
            .gap_6();
        // Wide: the stage fills its column and scrolls on its own.
        // Narrow: it sits at natural height above the rail, and the
        // shared column scrolls both.
        let stage = if wide {
            stage.h_full().overflow_y_scroll()
        } else {
            stage.flex_none().w_full()
        };
        stage
            .child(
                v_flex()
                    .gap_4()
                    .child(
                        h_flex()
                            .justify_between()
                            .child(section_header("Preview"))
                            .child(
                                TabBar::new("favicon-stage-theme")
                                    .segmented()
                                    .xsmall()
                                    .selected_index(match view.stage_theme {
                                        PreviewTheme::Light => 0,
                                        PreviewTheme::Dark => 1,
                                    })
                                    .on_click(cx.listener(|this, ix: &usize, _, cx| {
                                        let theme = if *ix == 1 {
                                            PreviewTheme::Dark
                                        } else {
                                            PreviewTheme::Light
                                        };
                                        this.model.update(cx, |m, cx| m.set_stage_theme(theme, cx));
                                    }))
                                    .child(Tab::new().label("Light"))
                                    .child(Tab::new().label("Dark")),
                            ),
                    )
                    // Hero: the 512 raster, large enough to dominate.
                    .child(
                        v_flex()
                            .id("favicon-hero")
                            .items_center()
                            .gap_2()
                            .child(preview_frame(512, HERO_PX, frame(512), cx))
                            .child(muted_note("512 × 512 · favicon.svg", cx)),
                    )
                    // Ladder: actual size where it fits, labeled scale
                    // where it doesn't.
                    .child(h_flex().justify_center().items_end().gap_6().children(
                        LADDER.iter().map(|(size, display, label)| {
                            v_flex()
                                .items_center()
                                .gap_1()
                                .child(preview_frame(*size, *display, frame(*size), cx))
                                .child(muted_note(*label, cx))
                        }),
                    )),
            )
            .child(
                v_flex()
                    .gap_3()
                    .child(section_header("In context"))
                    .child(
                        h_flex()
                            .gap_3()
                            .child(context_cell(
                                "Light tab",
                                tab_mock("favicon-context-tab-light", false, tab_light, &app_name),
                                cx,
                            ))
                            .child(context_cell(
                                "Dark tab",
                                tab_mock("favicon-context-tab-dark", true, tab_dark, &app_name),
                                cx,
                            )),
                    )
                    .child(
                        h_flex()
                            .gap_3()
                            .child(context_cell("iOS", ios_mock(frame(180), &app_name, cx), cx))
                            .child(context_cell(
                                "Android",
                                android_mock(frame(192), &app_name, cx),
                                cx,
                            )),
                    ),
            )
    }

    // ---- export rail (L6) ----

    fn rail(&self, view: &View, wide: bool, cx: &mut Context<Self>) -> Stateful<Div> {
        let WidgetHandles { dir, app_name, .. } = &view.widgets;
        let spec = &view.spec;
        let count = export::plan(spec).len();
        let body = v_flex()
            .gap_6()
            .child(
                v_flex()
                    .gap_3()
                    .child(section_header("Export"))
                    .children(Platform::ALL.iter().map(|p| platform_row(*p, spec, cx))),
            )
            .child(
                v_flex()
                    .gap_1p5()
                    .child(Label::new("Folder").text_sm())
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .id("favicon-export-dir")
                                    .flex_1()
                                    .min_w_0()
                                    .child(Input::new(dir).w_full()),
                            )
                            .child(
                                Button::new("favicon-export-browse")
                                    .label("Browse…")
                                    .outline()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.model.update(cx, |m, cx| m.pick_export_dir(cx));
                                    })),
                            ),
                    ),
            )
            .when(spec.platforms.pwa, |el| {
                el.child(
                    v_flex()
                        .gap_1p5()
                        .child(Label::new("App name").text_sm())
                        .child(
                            div()
                                .id("favicon-app-name")
                                .child(Input::new(app_name).w_full()),
                        )
                        .child(muted_note("Written to site.webmanifest.", cx)),
                )
            });
        let commit = Button::new("favicon-export")
            .label(if view.exporting {
                "Exporting…".to_string()
            } else {
                format!("Export {}", controls::files_label(count))
            })
            .primary()
            .w_full()
            .disabled(view.exporting)
            .on_click(cx.listener(|this, _, window, cx| this.export_clicked(window, cx)));
        let rail = v_flex()
            .id("favicon-export-rail")
            .role(accesskit::Role::Group)
            .aria_label("Export")
            .gap_4();
        if wide {
            // The commit is anchored to the rail's bottom; the settings
            // above it scroll if they ever outgrow the column.
            rail.w(px(RAIL_W))
                .flex_shrink_0()
                .h_full()
                .child(
                    div()
                        .id("favicon-export-rail-body")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .child(body),
                )
                .child(commit)
        } else {
            rail.w_full().child(body).child(commit)
        }
    }

    /// Export click: pre-flight in the model; collisions open the
    /// overwrite confirm (D7, §12 L6) — never a silent overwrite.
    fn export_clicked(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.model.update(cx, |m, cx| m.begin_export(false, cx));
        let Some(hit) = self.model.read(cx).collisions.clone() else {
            return;
        };
        let model = self.model.clone();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let confirm = model.clone();
            let cancel = model.clone();
            let closed = model.clone();
            alert
                .title(format!("Overwrite {}?", controls::files_label(hit.len())))
                .description(hit.join(", "))
                .on_close(move |_, _, cx| {
                    closed.update(cx, |m, cx| m.cancel_overwrite(cx));
                })
                .footer(
                    DialogFooter::new()
                        .child(
                            Button::new("favicon-overwrite-cancel")
                                .label("Cancel")
                                .outline()
                                .on_click(move |_, window, cx| {
                                    cancel.update(cx, |m, cx| m.cancel_overwrite(cx));
                                    window.close_dialog(cx);
                                }),
                        )
                        .child(
                            Button::new("favicon-overwrite-confirm")
                                .label("Overwrite")
                                .primary()
                                .on_click(move |_, window, cx| {
                                    confirm.update(cx, |m, cx| m.begin_export(true, cx));
                                    window.close_dialog(cx);
                                }),
                        ),
                )
                .width(px(420.))
        });
    }
}

fn image_status(spec: &DesignSpec) -> String {
    match &spec.source {
        Source::Image { w, h, .. } if *w > 0 => format!("Loaded: {w}×{h}."),
        _ => "No image loaded yet.".into(),
    }
}

fn svg_status(spec: &DesignSpec) -> String {
    match &spec.source {
        Source::Svg { w, h, .. } if *w > 0 => format!("Loaded: {w}×{h}."),
        _ => "Upload a file or paste markup below.".into(),
    }
}

/// Muted helper/caption text — notes, captions, file lists.
fn muted_note(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
}

/// Labeled control row: the 72 px label lane, then the control (§12
/// L2). The lane keeps controls across rows on one spine.
fn field_row(label: &str, control: impl IntoElement) -> AnyElement {
    field_row_aligned(label, control, false)
}

/// [`field_row`] for a multi-line control (the font stack): the label
/// sits on the first line instead of the row's center.
fn field_row_top(label: &str, control: impl IntoElement) -> AnyElement {
    field_row_aligned(label, control, true)
}

/// A label above its control, for the one control wider than the
/// lane: the four weight tiers need ~280 px at the inputs' height, so
/// they take the inspector's full width, like the Source tabs.
fn field_row_stacked(label: &str, control: impl IntoElement) -> AnyElement {
    v_flex()
        .gap_1p5()
        .child(Label::new(label.to_string()).text_sm())
        .child(control)
        .into_any_element()
}

fn field_row_aligned(label: &str, control: impl IntoElement, top: bool) -> AnyElement {
    h_flex()
        .gap_3()
        .when(top, |el| el.items_start())
        .when(!top, |el| el.items_center())
        .child(
            div()
                .w(px(LABEL_W))
                .flex_shrink_0()
                // Align a top label with the first control's text line.
                .when(top, |el| el.pt_1p5())
                .child(Label::new(label.to_string()).text_sm()),
        )
        .child(control)
        .into_any_element()
}

/// Slider plus a right-aligned percent readout, together exactly the
/// control width. Slider values are fractions; the readout is the
/// percent. The row is pixels-only by receipt: the accessible
/// "Size — 60%" belongs on the upstream slider node, which exposes
/// `Role::Slider` with numeric values but no label support yet (see
/// the audit FAILs this page carries until then, #530).
fn slider_control(
    id: &'static str,
    slider: &Entity<SliderState>,
    value: f32,
    cx: &App,
) -> Stateful<Div> {
    let percent = format!("{}%", (value * 100.0).round());
    h_flex()
        .id(id)
        .w(px(CONTROL_W))
        .gap_2()
        .items_center()
        .child(Slider::new(slider).horizontal().w(px(SLIDER_W)))
        .child(
            div()
                .w(px(PERCENT_W))
                .flex_shrink_0()
                .text_sm()
                .text_color(cx.theme().foreground)
                .text_right()
                .child(percent),
        )
}

/// One preview frame at `display` px: the raster of the exact emitted
/// SVG (D2) or the dark preview (§12 L5), or an honest "Rendering…"
/// placeholder while the off-thread rebuild is in flight — a blank
/// frame is a designed-against state (0065 A18).
fn preview_frame(
    size: u32,
    display: f32,
    image: Option<Arc<RenderImage>>,
    cx: &App,
) -> Stateful<Div> {
    let frame = div()
        .id(format!("favicon-preview-{size}"))
        .role(accesskit::Role::Image)
        .aria_label(format!("favicon preview {size}×{size}"));
    match image {
        Some(image) => frame.child(img(image).size(px(display)).rounded_sm()),
        None => frame.child(
            div()
                .size(px(display))
                .rounded_sm()
                .bg(cx.theme().background)
                .flex()
                .items_center()
                .justify_center()
                .when(display >= 48., |el| el.child(muted_note("Rendering…", cx))),
        ),
    }
}

/// One in-context mock with its caption (§12 L5).
fn context_cell(caption: &str, mock: impl IntoElement, cx: &App) -> Div {
    v_flex()
        .flex_1()
        .min_w_0()
        .gap_1p5()
        .child(mock)
        .child(muted_note(caption.to_string(), cx))
}

/// The fixed colors of the in-context mocks — the one place this page
/// paints literal colors. The two tab mocks imitate browser chrome
/// (Chrome's light and dark tab strips) and the Android mock the white
/// disc Chrome draws behind a non-maskable PWA icon: surfaces outside
/// the app's theme, which theme tokens would misrepresent (§12 L5).
struct MockColors {
    strip: Hsla,
    tab: Hsla,
    text: Hsla,
}

#[allow(clippy::disallowed_methods)] // browser-chrome mock colors, see `MockColors`
fn mock_colors(dark: bool) -> MockColors {
    if dark {
        MockColors {
            strip: rgb(0x202124).into(),
            tab: rgb(0x35363a).into(),
            text: rgb(0xe8eaed).into(),
        }
    } else {
        MockColors {
            strip: rgb(0xdee1e6).into(),
            tab: rgb(0xffffff).into(),
            text: rgb(0x202124).into(),
        }
    }
}

/// A browser tab mock: the 16 px frame of its own palette (both tabs
/// are live whatever the stage theme), the app name and a close mark.
fn tab_mock(
    id: &'static str,
    dark: bool,
    frame: Option<Arc<RenderImage>>,
    app_name: &str,
) -> Stateful<Div> {
    let colors = mock_colors(dark);
    let theme = if dark { "dark" } else { "light" };
    div()
        .id(id)
        .role(accesskit::Role::Image)
        .aria_label(format!("{app_name} in a {theme} browser tab"))
        .h(px(48.))
        .rounded_md()
        .bg(colors.strip)
        .px_2()
        .pt_2()
        .flex()
        .items_end()
        .child(
            h_flex()
                .w_full()
                .h(px(32.))
                .px_2()
                .gap_2()
                .rounded_t_md()
                .bg(colors.tab)
                .text_color(colors.text)
                .child(match frame {
                    Some(frame) => img(frame).size(px(16.)).flex_shrink_0().into_any_element(),
                    None => div().size(px(16.)).flex_shrink_0().into_any_element(),
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .truncate()
                        .child(app_name.to_string()),
                )
                .child(div().text_xs().child("×")),
        )
}

/// The iOS home-screen mock: the 180 frame at 48 px under an iOS-style
/// rounded mask, with its label.
fn ios_mock(frame: Option<Arc<RenderImage>>, app_name: &str, cx: &App) -> Stateful<Div> {
    launcher_tile(
        "favicon-context-ios",
        format!("{app_name} on an iOS home screen"),
        match frame {
            // ~22% corner radius: the iOS squircle, approximated.
            Some(frame) => img(frame).size(px(48.)).rounded(px(11.)).into_any_element(),
            None => div().size(px(48.)).into_any_element(),
        },
        app_name,
        cx,
    )
}

/// The Android launcher mock: the 192 frame at 34 px inside a 48 px
/// white disc — how Chrome shows a non-maskable PWA icon (adaptive
/// icons stay #430).
fn android_mock(frame: Option<Arc<RenderImage>>, app_name: &str, cx: &App) -> Stateful<Div> {
    launcher_tile(
        "favicon-context-android",
        format!("{app_name} on an Android launcher"),
        div()
            .size(px(48.))
            .rounded_full()
            .bg(mock_colors(false).tab)
            // The ring keeps the disc visible on a light tile.
            .border_1()
            .border_color(cx.theme().border)
            .flex()
            .items_center()
            .justify_center()
            .when_some(frame, |el, frame| el.child(img(frame).size(px(34.))))
            .into_any_element(),
        app_name,
        cx,
    )
}

fn launcher_tile(
    id: &'static str,
    label: String,
    icon: AnyElement,
    app_name: &str,
    cx: &App,
) -> Stateful<Div> {
    v_flex()
        .id(id)
        .role(accesskit::Role::Image)
        .aria_label(label)
        .h(px(88.))
        .rounded_md()
        .bg(cx.theme().background)
        .items_center()
        .justify_center()
        .gap_1()
        .child(icon)
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().foreground)
                .child(app_name.to_string()),
        )
}

/// One platform checklist row (§12 L6): checkbox + name, a
/// right-aligned file count, and the muted file list — all from
/// `export::plan`, so the rail can't drift from what gets written.
fn platform_row(platform: Platform, spec: &DesignSpec, cx: &mut Context<FaviconPage>) -> Div {
    let files = controls::platform_files(spec, platform);
    let checked = platform.checked(spec);
    let names: Vec<&str> = files.iter().map(|c| c.file_name()).collect();
    v_flex()
        .gap_1()
        .child(
            h_flex()
                .justify_between()
                .child(
                    Checkbox::new(format!("favicon-platform-{}", platform.key()))
                        .checked(checked)
                        .label(platform.label())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.model
                                .update(cx, |m, cx| m.toggle_platform(platform, cx));
                        })),
                )
                .child(muted_note(controls::files_label(files.len()), cx)),
        )
        .child(
            // Indented under the checkbox label.
            div().pl_6().child(muted_note(names.join(" · "), cx)),
        )
        .when(platform == Platform::Ios && checked, |el| {
            el.child(
                div().pl_6().pt_1().child(
                    Switch::new("favicon-ios-1024")
                        .checked(spec.platforms.ios_1024)
                        .label("1024 marketing icon")
                        .xsmall()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.model.update(cx, |m, cx| m.toggle_ios_1024(cx));
                        })),
                ),
            )
        })
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
        // The font select follows the spec (a source switch resets the
        // family) and the All-fonts toggle.
        self.model
            .update(cx, |model, cx| model.sync_font_list(window, cx));

        let m = self.model.read(cx);
        let view = View {
            spec: m.spec.clone(),
            widgets: m.widget_refs(),
            source_index: m.source_index(),
            weight_index: m.weight_index(),
            svg_error: m.svg_error.clone(),
            all_fonts: m.all_fonts,
            palette_dark: m.palette_dark,
            stage_theme: m.stage_theme,
            previews: m.previews.clone(),
            exporting: m.exporting,
        };

        // The breakpoint (§12 L1) measures this page's own width — the
        // content width: the window minus the sidebar and shell frame,
        // whatever the sidebar's state. A flip re-renders once.
        let wide = self.wide.get();
        let measured = self.wide.clone();
        let measure = canvas(
            move |bounds, window, _| {
                let now = bounds.size.width >= px(RAIL_BREAKPOINT);
                if measured.get() != now {
                    measured.set(now);
                    window.refresh();
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();

        let inspector = self.inspector(&view, cx);
        let stage = self.stage(&view, wide, cx);
        let rail = self.rail(&view, wide, cx);
        let body = h_flex().flex_1().min_h_0().w_full().items_start().gap_4();
        let body = if wide {
            body.child(inspector).child(stage).child(rail)
        } else {
            body.child(inspector).child(
                v_flex()
                    .id("favicon-stage-column")
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .overflow_y_scroll()
                    .gap_6()
                    .child(stage)
                    .child(rail),
            )
        };

        page_canvas()
            .id("favicon-page")
            .relative()
            .size_full()
            .child(measure)
            .child(page_title("Favicon generator"))
            .child(body)
    }
}
