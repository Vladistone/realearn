use crate::base::{blocking_lock, non_blocking_lock};
use crate::domain::{
    AdditionalTransformationInput, EelMidiSourceScript, EelTransformation, LuaMidiSourceScript,
    SafeLua,
};
use crate::infrastructure::ui::bindings::root;
use crate::infrastructure::ui::util::{open_in_browser, open_in_text_editor};
use crate::infrastructure::ui::{ScriptEditorInput, ScriptEngine};
use baseview::WindowHandle;
use derivative::Derivative;
use egui::plot::{Line, Plot, PlotPoint, PlotPoints};
use egui::{CentralPanel, Sense, Style, TextEdit, Ui, Visuals};
use helgoboss_learn::{
    AbsoluteValue, FeedbackStyle, FeedbackValue, MidiSourceScript, NumericFeedbackValue,
    Transformation, TransformationInput, TransformationInputMetaData, UnitValue,
};
use reaper_low::{raw, Swell};
use semver::Version;
use std::cell::RefCell;
use std::error::Error;
use std::ptr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use swell_ui::{Dimensions, SharedView, View, ViewContext, Window};

pub type SharedContent = Arc<Mutex<String>>;

pub struct ScriptTemplateGroup {
    pub name: &'static str,
    pub templates: &'static [ScriptTemplate],
}

pub struct ScriptTemplate {
    pub name: &'static str,
    pub content: &'static str,
    pub description: &'static str,
    pub min_realearn_version: Option<Version>,
}

#[derive(Derivative)]
#[derivative(Debug)]
pub struct AdvancedScriptEditorPanel {
    view: ViewContext,
    content: SharedContent,
    #[derivative(Debug = "ignore")]
    apply: Box<dyn Fn(String)>,
    #[derivative(Debug = "ignore")]
    toolbox: RefCell<Option<Toolbox>>,
}

impl AdvancedScriptEditorPanel {
    pub fn new(
        input: ScriptEditorInput<impl Fn(String) + 'static, EelTransformation>,
        script_template_groups: &'static [ScriptTemplateGroup],
    ) -> Self {
        Self {
            view: Default::default(),
            content: Arc::new(Mutex::new(input.initial_content)),
            apply: Box::new(input.apply),
            toolbox: {
                let toolbox = Toolbox {
                    engine: input.engine,
                    help_url: input.help_url,
                    script_template_groups,
                };
                RefCell::new(Some(toolbox))
            },
        }
    }

    fn apply(&self) {
        let content = blocking_lock(&self.content);
        (self.apply)(content.clone());
    }
}

impl View for AdvancedScriptEditorPanel {
    fn dialog_resource_id(&self) -> u32 {
        root::ID_EMPTY_PANEL
    }

    fn view_context(&self) -> &ViewContext {
        &self.view
    }

    fn opened(self: SharedView<Self>, window: Window) -> bool {
        let window_size = window.size();
        let dpi_factor = window.dpi_scaling_factor();
        let window_width = window_size.width.get() as f64 / dpi_factor;
        let window_height = window_size.height.get() as f64 / dpi_factor;
        let toolbox = self.toolbox.take().expect("toolbox already in use");
        let state = State::new(self.content.clone(), toolbox);
        let settings = baseview::WindowOpenOptions {
            title: "Script editor".into(),
            size: baseview::Size::new(window_width, window_height),
            scale: baseview::WindowScalePolicy::SystemScaleFactor,
            gl_config: Some(Default::default()),
        };
        egui_baseview::EguiWindow::open_parented(
            &self.view.require_window(),
            settings,
            state,
            |ctx: &egui::Context, _queue: &mut egui_baseview::Queue, _state: &mut State| {
                let mut style: egui::Style = (*ctx.style()).clone();
                #[cfg(any(target_os = "macos", target_os = "windows"))]
                {
                    style.visuals = if Window::dark_mode_is_enabled() {
                        Visuals::dark()
                    } else {
                        Visuals::light()
                    };
                }
                #[cfg(target_os = "linux")]
                {
                    style.visuals = Visuals::light();
                }
                ctx.set_style(style);
            },
            |ctx: &egui::Context, _queue: &mut egui_baseview::Queue, state: &mut State| {
                run_ui(ctx, state);
            },
        );
        true
    }

    fn closed(self: SharedView<Self>, _window: Window) {
        self.apply();
    }

    fn button_clicked(self: SharedView<Self>, resource_id: u32) {
        match resource_id {
            // Escape key
            raw::IDCANCEL => self.close(),
            _ => {}
        }
    }
}

struct State {
    content: SharedContent,
    last_build_outcome: BuildOutcome,
    template_in_preview: Option<TemplateInPreview>,
    toolbox: Toolbox,
}

struct TemplateInPreview {
    template: &'static ScriptTemplate,
    build_outcome: BuildOutcome,
}

#[derive(Default)]
struct BuildOutcome {
    plot_points: Vec<PlotPoint>,
    error: String,
}

struct Toolbox {
    engine: Box<dyn ScriptEngine<Script = EelTransformation>>,
    help_url: &'static str,
    script_template_groups: &'static [ScriptTemplateGroup],
}

impl Toolbox {
    fn build(&self, content: &str) -> BuildOutcome {
        let (plot_points, error) = match self.engine.compile(&content) {
            Ok(script) => {
                let uses_time = script.wants_to_be_polled();
                let sample_count = if uses_time {
                    // 301 samples from 0 to 10 seconds
                    // TODO-high Check what happens to first invocation. Maybe not in time domain?
                    301
                } else {
                    // 101 samples from 0.0 to 1.0
                    101
                };
                let points = (0..sample_count)
                    .filter_map(|i| {
                        let (x, rel_time_millis) = if uses_time {
                            // TODO-high This is not enough. We must also increase the x axis bounds
                            //  to reflect the seconds.
                            (1.0, 33 * i)
                        } else {
                            (0.01 * i as f64, 0)
                        };
                        let input = TransformationInput::new(
                            UnitValue::new_clamped(x),
                            TransformationInputMetaData {
                                rel_time: Duration::from_millis(rel_time_millis),
                            },
                        );
                        let additional_input = AdditionalTransformationInput { y_last: 0.0 };
                        let output = script
                            .transform_continuous(input, UnitValue::MIN, additional_input)
                            .ok()?;
                        let plot_x = if uses_time {
                            rel_time_millis as f64 / 10_000.0
                        } else {
                            x
                        };
                        let y = output.value()?;
                        Some(PlotPoint::new(plot_x, y.get()))
                    })
                    .collect();
                (points, "".to_string())
            }
            Err(e) => (vec![], e.to_string()),
        };
        BuildOutcome { plot_points, error }
    }
}

impl State {
    pub fn new(content: SharedContent, toolbox: Toolbox) -> Self {
        let mut state = State {
            content,
            last_build_outcome: Default::default(),
            template_in_preview: None,
            toolbox,
        };
        state.invalidate();
        state
    }

    pub fn invalidate(&mut self) {
        let content = blocking_lock(&self.content);
        self.last_build_outcome = self.toolbox.build(&*content);
    }
}

fn run_ui(ctx: &egui::Context, state: &mut State) {
    use egui::plot::{Line, Plot, PlotPoints};
    use egui::{
        emath, epaint, pos2, vec2, Color32, Frame, Pos2, Rect, SidePanel, Stroke, TextEdit, Window,
    };
    SidePanel::left("left-panel")
        .default_width(ctx.available_rect().width() / 2.0)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                let response = ui.menu_button("Templates", |ui| {
                    for group in state.toolbox.script_template_groups {
                        ui.menu_button(group.name, |ui| {
                            for template in group.templates {
                                let response = ui.button(template.name);
                                if response.hovered() {
                                    // Preview template
                                    let template_changed = state
                                        .template_in_preview
                                        .as_ref()
                                        .map(|t| !ptr::eq(t.template, template))
                                        .unwrap_or(true);
                                    if template_changed {
                                        let build_outcome = state.toolbox.build(template.content);
                                        let template_in_preview = TemplateInPreview {
                                            template,
                                            build_outcome,
                                        };
                                        state.template_in_preview = Some(template_in_preview);
                                    }
                                    // TODO-high clear template in preview when moving out of
                                    //  menu button area
                                }
                                if response.clicked() {
                                    // Apply template
                                    *blocking_lock(&state.content) = template.content.to_string();
                                    state.invalidate();
                                    ui.close_menu();
                                }
                            }
                        });
                    }
                });
                if response.response.clicked_elsewhere() {
                    // Menu closed
                    state.template_in_preview = None;
                }
                if ui.hyperlink_to("Help", state.toolbox.help_url).clicked() {
                    open_in_browser(state.toolbox.help_url);
                };
            });
            let response = {
                let mut content = blocking_lock(&state.content);
                let text_edit = TextEdit::multiline(&mut *content).code_editor();
                ui.add_sized(ui.available_size(), text_edit)
            };
            if response.changed() {
                state.invalidate();
            }
        });
    CentralPanel::default().show(ctx, |ui| {
        if let Some(template_in_preview) = &state.template_in_preview {
            // A template is being hovered. Show a preview!
            // Description
            ui.label(template_in_preview.template.description);
            // Code preview
            // TODO-high Increase window width
            // TODO-high Make the basics work cross-platform (and check which things work, which
            //  don't)
            // TODO-high Make built-in undo work for German layout
            // TODO-high Or build a dedicated undo/redo working directly on the content
            // TODO-high Make copy/cut work (somehow the C/X keys are eaten when holding command,
            //  they don't arrive in baseview)
            // TODO-high Maybe reuse whatever clipboard code is used in ReaLearn in general
            let mut content = template_in_preview.template.content;
            let output = TextEdit::multiline(&mut content).code_editor().show(ui);
            let anything_selected = output
                .cursor_range
                .map_or(false, |cursor| !cursor.is_empty());
            output.response.context_menu(|ui| {
                if ui
                    .add_enabled(anything_selected, egui::Button::new("Copy"))
                    .clicked()
                {
                    if let Some(text_cursor_range) = output.cursor_range {
                        use egui::TextBuffer as _;
                        let selected_chars = text_cursor_range.as_sorted_char_range();
                        let selected_text = content.char_range(selected_chars);
                        ctx.output().copied_text = selected_text.to_string();
                    }
                }
            });
            // Plot preview
            plot_build_outcome(ui, &template_in_preview.build_outcome);
        } else {
            // Plot our script
            plot_build_outcome(ui, &state.last_build_outcome);
        }
    });
    // Window::new("Hey")
    //     .collapsible(true)
    //     .show(context, |ui| {
    // let color = if ui.visuals().dark_mode {
    //     Color32::from_additive_luminance(196)
    // } else {
    //     Color32::from_black_alpha(240)
    // };
    //
    // Frame::canvas(ui.style()).show(ui, |ui| {
    //     ui.ctx().request_repaint();
    //     let time = ui.input().time;
    //
    //     let desired_size = ui.available_width() * vec2(1.0, 0.35);
    //     let (_id, rect) = ui.allocate_space(desired_size);
    //
    //     let to_screen =
    //         emath::RectTransform::from_to(Rect::from_x_y_ranges(0.0..=1.0, -1.0..=1.0), rect);
    //
    //     let mut shapes = vec![];
    //
    //     for &mode in &[2, 3, 5] {
    //         let mode = mode as f64;
    //         let n = 120;
    //         let speed = 1.5;
    //
    //         let points: Vec<Pos2> = (0..=n)
    //             .map(|i| {
    //                 let t = i as f64 / (n as f64);
    //                 let amp = (time * speed * mode).sin() / mode;
    //                 let y = amp * (t * std::f64::consts::TAU / 2.0 * mode).sin();
    //                 to_screen * pos2(t as f32, y as f32)
    //             })
    //             .collect();
    //
    //         let thickness = 10.0 / mode as f32;
    //         shapes.push(epaint::Shape::line(points, Stroke::new(thickness, color)));
    //     }
    //
    //     ui.painter().extend(shapes);
    // });
    // });
}

fn plot_build_outcome(ui: &mut Ui, build_outcome: &BuildOutcome) {
    if !build_outcome.error.is_empty() {
        ui.colored_label(ui.visuals().error_fg_color, &build_outcome.error);
        return;
    }
    let line = Line::new(PlotPoints::Owned(build_outcome.plot_points.clone()));
    Plot::new("transformation_plot")
        .allow_boxed_zoom(false)
        .allow_drag(false)
        .allow_scroll(false)
        .allow_zoom(false)
        .height(ui.available_height())
        .data_aspect(1.0)
        .view_aspect(1.0)
        .include_x(1.0)
        .include_y(1.0)
        .show_background(false)
        .show(ui, |plot_ui| {
            plot_ui.line(line);
        });
}
