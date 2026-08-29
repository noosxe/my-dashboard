//! Notes sample module (docs/0003 §5.6, persistence per docs/0004 §4.4).
//!
//! Proves the sub-page path AND the module-data persistence path: notes
//! live in a single state entity (loaded at init, debounced-saved to
//! `<data>/modules/notes/notes.json` on every mutation), and both views
//! observe it per the module contract.

use std::sync::Arc;
use std::time::Duration;

use gpui::*;

use gpui::prelude::FluentBuilder as _;

use gpui_component::{
    ActiveTheme, IconName,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputEvent, InputState},
    v_flex,
};

use serde::{Deserialize, Serialize};

use dashboard_core::Module;
use dashboard_core::module::{ModuleEntry, ModuleIcon, ModuleMeta};
use dashboard_core::nav::{Crumb, Navigator};
use dashboard_core::paths::AppPaths;
use dashboard_core::store::{DebouncedSave, load_json};

pub struct NotesModule;

impl Module for NotesModule {
    fn meta() -> ModuleMeta {
        ModuleMeta {
            id: "notes".into(),
            title: "Notes".into(),
            icon: Some(ModuleIcon::BuiltIn(IconName::BookOpen)),
        }
    }

    fn init(
        nav: &Entity<Navigator>,
        window: &mut Window,
        paths: &AppPaths,
        cx: &mut App,
    ) -> ModuleEntry {
        // 1. State entity, seeded from disk (docs/0004 §4.4). Missing or
        //    corrupt file → defaults (store handles quarantine).
        let data_dir = paths.module_data_dir("notes");
        let notes_file = data_dir.join("notes.json");
        let loaded: NotesFile = load_json(&notes_file)
            .inspect_err(|err| eprintln!("dashboard: notes load failed: {err}"))
            .ok()
            .flatten()
            .unwrap_or_default();

        let save = DebouncedSave::new(notes_file, Duration::from_millis(500));

        let state = cx.new(|_| NotesState {
            notes: loaded.notes,
            next_id: loaded.next_id,
            save,
        });

        // 2. Two views over the same entity (module contract).
        ModuleEntry {
            meta: Self::meta(),
            overview: cx.new(|cx| NotesOverview::new(state.clone(), cx)).into(),
            main_page: cx
                .new(|cx| NotesPage::new(state, nav.clone(), window, cx))
                .into(),
        }
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Debug)]
pub struct Note {
    pub id: u64,
    pub text: String,
}

/// On-disk shape (`<data>/modules/notes/notes.json`).
#[derive(Serialize, Deserialize, Default)]
struct NotesFile {
    notes: Vec<Note>,
    next_id: u64,
}

/// The single state entity; mutations schedule the debounced save.
struct NotesState {
    notes: Vec<Note>,
    next_id: u64,
    save: DebouncedSave,
}

impl NotesState {
    fn snapshot(&self) -> NotesFile {
        NotesFile {
            notes: self.notes.clone(),
            next_id: self.next_id,
        }
    }

    fn add(&mut self, text: String, cx: &mut Context<Self>) {
        let note = Note {
            id: self.next_id,
            text,
        };
        self.next_id += 1;
        self.notes.insert(0, note); // newest first
        self.save.schedule(self.snapshot(), cx);
        cx.notify();
    }

    fn remove(&mut self, id: u64, cx: &mut Context<Self>) {
        self.notes.retain(|n| n.id != id);
        self.save.schedule(self.snapshot(), cx);
        cx.notify();
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Screen {
    Main,
    Details,
}

/// Contract-compliant overview: observes the state entity, summarizes it.
struct NotesOverview {
    state: Entity<NotesState>,
}

impl NotesOverview {
    fn new(state: Entity<NotesState>, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        Self { state }
    }
}

impl Render for NotesOverview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self.state.read(cx).notes.len();
        let summary = match count {
            0 => "No notes yet — open to jot the first one.".to_string(),
            1 => "1 note — open to read it.".to_string(),
            n => format!("{n} notes — open to read them."),
        };
        v_flex()
            .size_full()
            .gap_2()
            .justify_center()
            .child(div().text_color(cx.theme().muted_foreground).child(summary))
    }
}

struct NotesPage {
    state: Entity<NotesState>,
    input: Entity<InputState>,
    nav: Entity<Navigator>,
    screen: Screen,
}

impl NotesPage {
    fn new(
        state: Entity<NotesState>,
        nav: Entity<Navigator>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Jot something…"));

        // Enter submits (docs/0004 §4.4). Subscribe callbacks carry no
        // `&mut Window`, but `InputState::set_value` needs one to clear
        // the field — the AnyWindowHandle::update route from the clock
        // module (0003 §3.12) bridges that.
        let window_handle = window.window_handle();
        cx.subscribe(&input, move |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                let text = this.input.read(cx).value().trim().to_string();
                if !text.is_empty() {
                    this.state.update(cx, |s, cx| s.add(text, cx));
                    let _ = window_handle.update(cx, |_, window, cx| {
                        this.input
                            .update(cx, |input, cx| input.set_value("", window, cx));
                    });
                }
            }
        })
        .detach();

        Self {
            state,
            input,
            nav,
            screen: Screen::Main,
        }
    }
}

impl NotesPage {
    fn add_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().trim().to_string();
        if text.is_empty() {
            return; // ignore empty submits
        }
        self.state.update(cx, |s, cx| s.add(text, cx));
        self.input
            .update(cx, |input, cx| input.set_value("", window, cx));
    }

    fn show_main(&mut self, cx: &mut Context<Self>) {
        if self.screen == Screen::Main {
            return;
        }
        self.screen = Screen::Main;
        self.nav.update(cx, |nav, cx| nav.set_crumbs(vec![], cx));
        cx.notify();
    }

    fn show_details(&mut self, cx: &mut Context<Self>) {
        if self.screen == Screen::Details {
            return;
        }
        self.screen = Screen::Details;
        let me = cx.entity();
        self.nav.update(cx, |nav, cx| {
            nav.set_crumbs(
                vec![Crumb {
                    label: "Details".into(),
                    on_click: Some(Arc::new(move |_, cx| {
                        me.update(cx, |page, cx| page.show_main(cx));
                    })),
                }],
                cx,
            );
        });
        cx.notify();
    }
}

impl Render for NotesPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        match self.screen {
            Screen::Main => self.render_main(cx).into_any_element(),
            Screen::Details => self.render_details(cx).into_any_element(),
        }
    }
}

impl NotesPage {
    fn render_main(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let notes: Vec<(u64, String)> = self
            .state
            .read(cx)
            .notes
            .iter()
            .map(|n| (n.id, n.text.clone()))
            .collect();

        let input = self.input.clone();

        v_flex()
            .p_4()
            .gap_3()
            .child(div().text_size(px(18.)).child("Notes"))
            .child(
                // Input row: single-line editor + Add (docs/0004 §4.4).
                h_flex()
                    .gap_2()
                    .child(div().flex_1().child(Input::new(&input)))
                    .child(
                        Button::new("add-note")
                            .label("Add")
                            .primary()
                            .on_click(cx.listener(|this, _, window, cx| this.add_note(window, cx))),
                    ),
            )
            .children(notes.into_iter().map(|(id, text)| {
                h_flex()
                    .id(SharedString::from(format!("note-{id}")))
                    .gap_2()
                    .items_center()
                    .py_1()
                    .child(div().flex_1().child(text))
                    .child(
                        Button::new(SharedString::from(format!("del-{id}")))
                            .ghost()
                            .icon(IconName::Close)
                            .tooltip("Delete note")
                            .on_click({
                                cx.listener(move |this, _, _, cx| {
                                    this.state.update(cx, |s, cx| s.remove(id, cx))
                                })
                            }),
                    )
            }))
            .child(
                // Sub-page proof (docs/0003 §5.6): breadcrumb crumbs.
                Button::new("open-details")
                    .label("View details")
                    .ghost()
                    .on_click(cx.listener(|this, _, _, cx| this.show_details(cx))),
            )
            .when(self.state.read(cx).notes.is_empty(), |this| {
                // Designed empty state ([DG]): tell the user what to do.
                this.child(
                    div()
                        .text_color(cx.theme().muted_foreground)
                        .child("Nothing here yet — type above and press Enter."),
                )
            })
    }

    fn render_details(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .p_4()
            .gap_3()
            .child(div().text_size(px(18.)).child("Notes — details"))
            .child(div().child(
                "You navigated into a sub-page: the breadcrumb grew a \
                     Details crumb, and clicking it (or Notes) brings you back.",
            ))
            .child(
                Button::new("back-to-main")
                    .label("Back")
                    .on_click(cx.listener(|this, _, _, cx| this.show_main(cx))),
            )
    }
}
