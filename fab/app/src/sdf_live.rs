//! Live distance fields: while a parameter of a distance-field part is dragged, the program is
//! compiled at the live value on the task pool and the parts' new fields go straight to
//! the SDF view, which traces tiles for the updated field. Nothing is meshed: the committed
//! evaluation (meshes for export and slicing) follows on release as before. A custom kernel's
//! numbers are kernel parameters, so a drag compiles no kernel and no shader. The same compile
//! gives every parameter's handle at the live value, so a handle that depends on the dragged value
//! moves with it and arrives with the fields it belongs to.
//!
//! Newest request wins: a drag never queues stale fields.

use fab::api::{LiveSdf, PartSdf};
use fab_model::params::{self, PValue, PartParams};
use makepad_widgets::*;
use std::sync::Arc;
use makepad_widgets::makepad_platform::thread::{Class, Strand, Version};
use std::time::Instant;

pub struct LiveRequest {
    pub gen: u64,
    pub program: Arc<fab_model::program::Compiled>,
    pub model: Arc<Vec<PartParams>>,
    /// The plate shown: its frames take the handles to the printer, as `parts`' do the fields.
    pub ev: Arc<fab_model::eval::Evaluated>,
    pub key: String,
    pub value: PValue,
    /// The change that writes the value when no parameter of the model holds it (a command's at a
    /// pick: a round); none: `key`'s own.
    pub change: Option<fab_model::Change>,
    /// The plate's distance-field parts: (object, its model's whole placement on the printer).
    pub parts: Vec<(String, [[f64; 4]; 3])>,
    pub asked: Instant,
    /// The grown parts' runs: their coarse levels follow the value.
    pub grow_runs: Arc<crate::grow_live::Runs>,
}

#[derive(Debug)]
pub struct LiveAction {
    pub gen: u64,
    pub fields: Vec<LiveSdf>,
    /// The parts' parameters and handles at the live value.
    pub params: Vec<Arc<fab::params::PartParamsView>>,
    pub key: String,
}

pub struct LiveWorker {
    strand: Strand,
    version: Version,
}

impl LiveWorker {
    pub fn start(cx: &Cx) -> LiveWorker {
        LiveWorker { strand: Strand::new(&cx.task_pool(), "fab sdf live"), version: Version::new() }
    }

    /// Compile the newest queued request; a running field finishes publishing.
    pub fn request(&self, req: LiveRequest) {
        self.strand.submit(Class::Interactive, self.version.next(), move |_| {
            let t = Instant::now();
            match build(&req) {
                Ok((fields, params)) => {
                    let compile_ms = t.elapsed().as_secs_f64() * 1000.0;
                    if std::env::var("FAB_PARAM_LOG").is_ok() {
                        log!("fab param live {}: {compile_ms:.1} ms", req.key);
                    }
                    let fields = fields.into_iter().map(|(object, sdf)| LiveSdf { object, sdf, asked: req.asked, compile_ms }).collect();
                    Cx::post_action(LiveAction { gen: req.gen, fields, params, key: req.key.clone() });
                }
                Err(e) => {
                    log!("fab sdf live: {e}");
                    // no new shape: slicing continues with the shape it has
                    crate::live_slice::shape_published();
                }
            }
        }).detach();
    }
}

#[allow(clippy::type_complexity)]
fn build(req: &LiveRequest) -> Result<(Vec<(String, PartSdf)>, Vec<Arc<fab::params::PartParamsView>>), String> {
    let inst = fab_model::program::split_id(&req.key).and_then(|(name, _)| req.program.instance(name)).ok_or_else(|| format!("no instance for '{}'", req.key))?;
    let live = Arc::new(match &req.change {
        Some(change) => params::with_change(inst, change)?,
        None => params::with_value(&req.program, &req.model, &req.key, &req.value)?,
    });
    drag_grown(req, inst, &live);
    let fields = fab_model::eval::part_fields(inst, &live, &req.program.strokes);
    let model = params::model_with(&req.program, &req.ev, &req.model, &inst.name, &live);
    let views = crate::param_view::views(&model, &req.ev);
    let fields = req
        .parts
        .iter()
        .filter_map(|(object, to_printer)| {
            let (_, shape, key) = fields.iter().find(|(o, ..)| o == object)?;
            Some((object.clone(), PartSdf::placed(shape.clone(), to_printer, *key)?))
        })
        .collect();
    Ok((fields, views))
}

/// What the grown parts of the instance dragged ask at the live value: their study's loads and
/// their mass, for the coarse levels to follow.
fn drag_grown(req: &LiveRequest, inst: &fab_model::program::Instance, live: &fab_model::program::Script) {
    if live.program.grow_requests.is_empty() {
        return;
    }
    for (i, part) in req.ev.plate.parts.iter().enumerate() {
        let Some((_, obj)) = req.program.object(&part.object).filter(|(of, _)| of.name == inst.name) else { continue };
        let field = req.ev.fields.get(i).and_then(Option::as_ref);
        let to_plate = req.ev.model_to_plate.get(i);
        let study = live.study.get(&obj.name).filter(|s| s.posed());
        let (Some(field), Some(to_plate), Some(study)) = (field, to_plate, study) else { continue };
        for r in &live.program.grow_requests {
            if let Some(loads) = crate::grow_live::loads_of(field, to_plate, &r.part, study) {
                req.grow_runs.drag(req.gen, &(live.file.clone(), r.part.clone()), loads, r.mass);
            }
        }
        // the instances of a script grow as one
        return;
    }
}
