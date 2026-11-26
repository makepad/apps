//! `makepad-story-player`: a finished film played smoothly and scrubbably,
//! natively and in the browser.
//!
//! ```text
//! makepad-story-player [--film <bundle dir>] [--play] [--at <seconds>]
//! makepad-story-player --film <dir> --gpu-report <out dir> [--gpu-size 1920x1080] [--gpu-step <s>]   (GPU memory per plate)
//! cargo makepad wasm --link-table build -p makepad-story-player --release   (the browser build; the bundle at `film/` beside index.html)
//! ```
//!
//! The film is a bundle ([`bundle`]); its plates are Motion documents drawn
//! by Motion's renderer at the film's time ([`film`]); the song is the
//! clock while it plays ([`audio`]). The frame is letterboxed in the
//! window, drawn at a resolution scale that follows the frame time.

#[cfg(not(target_arch = "wasm32"))]
mod analyse;
#[cfg(not(target_arch = "wasm32"))]
mod gpu_report;
mod audio;
mod bundle;
mod edit_audio;
mod film;
mod job;
mod kernels;
mod player;
mod scratch;

use makepad_widgets::*;

// The faces the films set their type in, packaged with the app (the
// browser fetches them beside the page).
app_main!(App, font_assets: [INTER_FONT_ASSET, ROBOTO_FLEX_FONT_ASSET]);

/// The film this player is built around (the film builder's pack).
#[cfg(feature = "film")]
pub static FILM_PACK: &[u8] = include_bytes!(env!("MAKEPAD_FILM_PACK"));

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    mod.widgets.FilmPlayerBase = #(player::FilmPlayer::register_widget(vm))
    mod.widgets.FilmPlayer = set_type_default() do mod.widgets.FilmPlayerBase{
        width: Fill
        height: Fill
        draw_text +: {
            color: #xeee9df
            text_style: theme.font_regular{font_size: 11.0}
        }
        draw_title +: {
            color: #x0a0a0b
            text_style: theme.font_bold{font_size: 30.0}
        }
        draw_note +: {
            color: #x0a0a0b
            text_style: theme.font_regular{font_size: 10.0}
        }
    }

    startup() do #(App::script_component(vm)){
        ui: Root{
            main_window := Window{
                window.title: "Film"
                window.inner_size: vec2(1280, 720)
                pass.clear_color: #x000000
                body +: {
                    player := mod.widgets.FilmPlayer{}
                }
            }
        }
    }
}

#[derive(Script, ScriptHook)]
pub struct App {
    #[live]
    ui: WidgetRef,
}

impl MatchEvent for App {
    fn handle_startup(&mut self, cx: &mut Cx) {
        if let Some(mut p) = self.ui.widget(cx, ids!(player)).borrow_mut::<player::FilmPlayer>() {
            p.start(cx);
        }
    }
}

impl AppMain for App {
    fn script_mod(vm: &mut ScriptVm) -> ScriptValue {
        // The analysis records every shader from the first one applied.
        #[cfg(not(target_arch = "wasm32"))]
        analyse::early(vm);
        // The film's prebuilt shaders, before the first one is applied (the
        // UI's own shaders apply while the app starts).
        #[cfg(feature = "film")]
        if let Some(bytes) = makepad_story_film_pack::Pack::parse(FILM_PACK).ok().and_then(|p| p.get(makepad_story_film_pack::names::SHADERS)) {
            if let Err(e) = vm.host.cx_mut().install_shader_pack(bytes) {
                error!("player: the film's shaders: {e:?}");
            }
        }
        // The film's own copies of the app's resources (subset UI fonts),
        // by their web paths, before the theme asks for them.
        #[cfg(feature = "film")]
        bundle::provide_pack_resources(vm.host.cx_mut(), FILM_PACK);
        makepad_widgets::script_mod(vm);
        makepad_content_packs::script_mod(vm);
        makepad_motion_draw::script_mod(vm);
        player::script_mod(vm);
        self::script_mod(vm)
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        self.match_event(cx, event);
        self.ui.handle_event(cx, event, &mut Scope::empty());
    }
}
