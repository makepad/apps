//! The Fabricate proof pane's living test scene: the `ledctl` fixture board exported through the
//! generic profile and drawn as read back from the files. Keys: 1-9, 0 layers (from the top of the
//! stack), W what the factory sees (off: the model as a ghost under the read-back), B panel,
//! T theme, F fit; the wheel zooms, space-drag pans; click focuses an object.
//! Drive it as a person does: press an object (`ui act node=<id> press`), then the keys. The one
//! fixture choice is the input: with the environment variable PROOFBAR_BREAK=1 the export is made from a board whose F.Cu
//! layer was shifted after validation, so the failing check shows as a marker and in the panel.

pub use makepad_widgets;

use fab_elec_board::{resolve_rules, zone::fill_all};
use fab_elec_cam::tables::{fill_generic_tables, PlacementRules};
use fab_elec_cam::{export, ResolvedProfile};
use fab_elec_fixtures::ledctl;
use fab_elec_view::proof::{pane::ProofPane, scene::ProofScene};
use makepad_widgets::*;
use std::sync::Arc;

app_main!(App);

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    startup() do #(App::script_component(vm)){
        ui: Root{
            main_window := Window{
                window.title: "Proof bar"
                window.inner_size: vec2(1400, 860)
                body +: {
                    proof := ProofPane{width: Fill height: Fill}
                }
            }
        }
    }
}

#[derive(Script, ScriptHook)]
pub struct App {
    #[live]
    ui: WidgetRef,
    #[rust]
    loaded: bool,
}

impl AppMain for App {
    fn script_mod(vm: &mut ScriptVm) -> ScriptValue {
        crate::makepad_widgets::script_mod(vm);
        fab_elec_view::live::script_mod(vm);
        fab_elec_view::canvas::draw::script_mod(vm);
        fab_elec_view::board::draw::script_mod(vm);
        fab_elec_view::proof::pane::script_mod(vm);
        self::script_mod(vm)
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        if !self.loaded {
            if let Some(mut pane) = self.ui.widget(cx, ids!(proof)).borrow_mut::<ProofPane>() {
                pane.set_scene(cx, Arc::new(scene()));
                self.loaded = true;
            }
        }
        self.ui.handle_event(cx, event, &mut Scope::empty());
    }
}

fn scene() -> ProofScene {
    let d = ledctl();
    let u = &d.units[0];
    let (board, circuit) = (u.board.as_ref().expect("a board unit"), &u.circuit);
    let fills = fill_all(board, &resolve_rules(board, None));
    let mut profile = ResolvedProfile::standard_2_layer("ledctl", "A");
    fill_generic_tables(&mut profile, board, circuit, &PlacementRules::default());
    let mut e = export(board, circuit, &fills, &profile).expect("ledctl exports");
    if std::env::var("PROOFBAR_BREAK").is_ok() {
        // a shifted copper layer: the files say something the model does not
        let mut shifted = (*e.model).clone();
        if let Some(items) = shifted.layers.get_mut(&fab_elec_core::LayerId::F_CU) {
            for it in items.iter_mut() {
                if let fab_elec_cam::Geom::Shape(s) = &mut it.geom {
                    *s = s.translated(fab_elec_core::P::mm(0.6, 0.0));
                }
            }
        }
        let files = fab_elec_cam::render(&shifted, &profile).expect("renders");
        e.validation = fab_elec_cam::validate(&files, &profile, &e.model);
        e.read_back = fab_elec_cam::read_back(&files, &profile);
        e.files = files;
    }
    ProofScene::build(&e)
}
