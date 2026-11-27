//! Makepad Town's app crate: the model the window and the AI tools share ([`model`]), the AI
//! service manifest and its action table ([`tools`]), and the window's lists ([`lists`]).

pub mod ai_trace;
pub mod app_link;
pub mod board;
pub mod call;
pub mod consent;
pub mod docs;
pub mod link_worker;
pub mod lists;
pub mod model;
pub mod publish;
pub mod record;
pub mod store_worker;
mod theme;
pub mod tools;
pub mod transcript;
pub mod tweak;
pub mod service_worker;
mod ui;
pub use ui::connect::{offer_line, pair_failure, ADDRESS_FAILED, DECLINED, PAIR_FAILED};
pub use ui::picture_view;
pub mod view;

/// The caption bar's height on every platform. The window pins it (on macOS the traffic lights
/// centre on it instead of the system's shorter title bar).
pub const CAPTION_HEIGHT: f64 = 42.0;

pub fn script_mod(vm: &mut makepad_widgets::ScriptVm) {
    #[cfg(test)]
    vm.capture_errors_nested();
    makepad_code_editor::script_mod(vm);
    makepad_markdown_editor::script_mod(vm);
    theme::script_mod(vm);
    makepad_canvas::script_mod(vm);
    board::canvas::script_mod(vm);
    lists::script_mod(vm);
    ui::navigation::script_mod(vm);
    ui::tasks::script_mod(vm);
    call::script_mod(vm);
    ui::chat::script_mod(vm);
    ui::picture_view::script_mod(vm);
    ui::docs::script_mod(vm);
    ui::connect::script_mod(vm);
    ui::pages::script_mod(vm);
    ui::publish::script_mod(vm);
    ui::live::script_mod(vm);
    ui::talk::script_mod(vm);
    #[cfg(test)]
    {
        let mut diagnostics = vm.take_errors();
        diagnostics.extend(vm.take_warnings());
        vm.capture_errors_restore();
        assert!(diagnostics.is_empty(), "Town Splash registration:\n{}", diagnostics.join("\n"));
    }
}
