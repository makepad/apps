//! Makepad Create's app crate: the model the window and the AI tools share ([`model`]), the AI
//! service manifest and its action table ([`tools`]), and the window's lists ([`lists`]).

pub mod board;
pub mod call;
pub mod docs;
pub mod link_worker;
pub mod lists;
pub mod model;
pub mod publish;
mod theme;
pub mod tools;
mod ui;
pub mod view;

/// The caption bar's height on every platform. The window pins it (on macOS the traffic lights
/// centre on it instead of the system's shorter title bar), and the caption's connection control
/// sits inside it with room above and below.
pub const CAPTION_HEIGHT: f64 = 42.0;

pub fn script_mod(vm: &mut makepad_widgets::ScriptVm) {
    makepad_code_editor::script_mod(vm);
    makepad_markdown_editor::script_mod(vm);
    theme::script_mod(vm);
    makepad_canvas::script_mod(vm);
    board::canvas::script_mod(vm);
    lists::script_mod(vm);
    ui::navigation::script_mod(vm);
    ui::tasks::script_mod(vm);
    ui::chat::script_mod(vm);
    ui::docs::script_mod(vm);
    ui::pages::script_mod(vm);
    ui::publish::script_mod(vm);
    ui::connect::script_mod(vm);
    ui::talk::script_mod(vm);
}
