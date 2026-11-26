//! `makepad-story`: Makepad Story (CUT and DECK). With `--headless` or
//! `--check` it runs a workflow package, with `--export-web` it exports a
//! document as a web page, and with `--panel-effects` it checks that every
//! panel control changes the film; all without a window.

fn main() -> std::process::ExitCode {
    story::story_main()
}
