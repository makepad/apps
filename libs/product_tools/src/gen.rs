//! The generation tools (`gen_presets`, `gen_run`, `gen_job`,
//! `gen_choose`, `gen_cancel`, `gen_estimate`, `gen_save_preset`) over the
//! product's generation studio (makepad-flow-studio's API): Stage's FLOW
//! page, Story's video maker.
//!
//! A product implements [`GenTools`], carries [`GenWait`] in its own wait
//! type, and answers it from its waits' pump with [`pending`] and
//! [`answer`].

use crate::{done, fail};
use makepad_app_shell::tool_host::{Ctx, Handler, Outcome, ToolProduct};
use makepad_flow_studio::{FlowStudio, JobPhase};
use makepad_studio_api::{Registry, ToolError};
use makepad_strict_json::{self as json, Value as Json};
use makepad_widgets::Cx;

/// How long `gen_run` and `gen_job` with `wait` hold the call.
const GEN_WAIT_SECS: f64 = 600.0;

/// How long a call waits for the studio's preset library and first hub probe.
const READY_WAIT_SECS: f64 = 15.0;

/// What the generation tools need from a product: its studio, and its wait
/// type carrying theirs.
pub trait GenTools: ToolProduct {
    fn studio(app: &mut Self::App) -> &mut FlowStudio;
    fn studio_ref(app: &Self::App) -> &FlowStudio;
    fn gen_wait(wait: GenWait<Self>) -> Self::Wait;
}

/// What a generation call waits for.
pub enum GenWait<P: ToolProduct> {
    /// A generation job, until it ends or stops at a choice, or the deadline.
    Job { job: u64, deadline: f64 },
    /// The studio's preset library and first hub probe (a full LAN beacon
    /// interval), then the call again.
    Ready { deadline: f64, handler: Handler<P>, args: Json },
}

impl<P: ToolProduct> Clone for GenWait<P> {
    fn clone(&self) -> Self {
        match self {
            GenWait::Job { job, deadline } => GenWait::Job { job: *job, deadline: *deadline },
            GenWait::Ready { deadline, handler, args } => GenWait::Ready { deadline: *deadline, handler: *handler, args: args.clone() },
        }
    }
}

impl<P: ToolProduct> GenWait<P> {
    /// The generation job a call waits on, if it waits on one.
    pub fn job(&self) -> Option<u64> {
        match self {
            GenWait::Job { job, .. } => Some(*job),
            GenWait::Ready { .. } => None,
        }
    }
}

/// Register the generation tools on a product's registry.
pub fn register<P: GenTools>(registry: &mut Registry<Handler<P>>) {
    let tools: &[(&str, Handler<P>)] = &[
        ("gen_presets", gen_presets::<P>),
        ("gen_run", gen_run::<P>),
        ("gen_job", gen_job::<P>),
        ("gen_choose", gen_choose::<P>),
        ("gen_cancel", gen_cancel::<P>),
        ("gen_estimate", gen_estimate::<P>),
        ("gen_save_preset", gen_save_preset::<P>),
    ];
    for (name, handler) in tools {
        registry.register(name, *handler);
    }
}

/// The wait is not answered yet at `now`.
pub fn pending<P: GenTools>(app: &P::App, wait: &GenWait<P>, now: f64) -> bool {
    let studio = P::studio_ref(app);
    match wait {
        GenWait::Job { job, deadline } => match studio.job(*job) {
            Some(snapshot) => matches!(snapshot.phase, JobPhase::Preparing | JobPhase::Running) && now < *deadline,
            None => true,
        },
        GenWait::Ready { deadline, .. } => !studio.api_ready() && now < *deadline,
    }
}

/// The answer to a wait [`pending`] no longer holds: the job's view, or the
/// call run again now that the studio is ready (which may wait again).
/// `None`: nothing to answer with yet.
pub fn answer<P: GenTools>(ctx: &mut Ctx<P>, wait: GenWait<P>) -> Option<Outcome<P::Wait>> {
    match wait {
        GenWait::Job { job, .. } => {
            let view = P::studio_ref(ctx.app).gen_view(job)?;
            Some(done(makepad_gen::tools::job_json(&view)))
        }
        GenWait::Ready { handler, args, .. } => Some(handler(ctx, &args)),
    }
}

/// How far a generation job is: its stage and progress in permille, for a
/// caller's progress line.
pub fn progress(studio: &FlowStudio, job: u64) -> Option<(String, u16)> {
    let view = studio.gen_view(job)?;
    let stage = if view.stage.is_empty() { "running".to_string() } else { view.stage };
    Some((stage, (view.progress.clamp(0.0, 1.0) * 1000.0) as u16))
}

/// Start the studio for an API call and wait for its catalogue and first
/// hub probe, then run `handler`.
fn when_ready<P: GenTools>(ctx: &mut Ctx<P>, handler: Handler<P>, args: &Json) -> Option<Outcome<P::Wait>> {
    let studio = P::studio(ctx.app);
    if studio.api_ready() {
        return None;
    }
    studio.api_start(ctx.cx);
    Some(Outcome::Wait(P::gen_wait(GenWait::Ready { deadline: Cx::time_now() + READY_WAIT_SECS, handler, args: args.clone() })))
}

fn gen_call<W>(name: &str, args: &Json) -> Result<makepad_gen::tools::GenCall, Outcome<W>> {
    makepad_gen::tools::parse(name, args).map_err(|error| fail(ToolError::invalid(error)))
}

fn gen_job_id<W>(id: &str) -> Result<u64, Outcome<W>> {
    id.trim().parse().map_err(|_| fail(ToolError::not_found(format!("no generation job `{id}`"))))
}

fn gen_job_reply<P: GenTools>(ctx: &mut Ctx<P>, job: u64) -> Outcome<P::Wait> {
    match P::studio_ref(ctx.app).gen_view(job) {
        Some(view) => done(makepad_gen::tools::job_json(&view)),
        None => fail(ToolError::not_found(format!("no generation job `{job}`"))),
    }
}

fn gen_presets<P: GenTools>(ctx: &mut Ctx<P>, args: &Json) -> Outcome<P::Wait> {
    if let Some(wait) = when_ready(ctx, gen_presets_ready::<P>, args) {
        return wait;
    }
    gen_presets_ready(ctx, args)
}

fn gen_presets_ready<P: GenTools>(ctx: &mut Ctx<P>, args: &Json) -> Outcome<P::Wait> {
    let kind = match gen_call("gen_presets", args) {
        Ok(makepad_gen::tools::GenCall::Presets { kind }) => kind,
        Ok(_) => unreachable!(),
        Err(outcome) => return outcome,
    };
    done(json::obj(vec![("presets", Json::Arr(P::studio_ref(ctx.app).gen_presets(kind.as_deref())))]))
}

fn gen_run<P: GenTools>(ctx: &mut Ctx<P>, args: &Json) -> Outcome<P::Wait> {
    if let Some(wait) = when_ready(ctx, gen_run_ready::<P>, args) {
        return wait;
    }
    gen_run_ready(ctx, args)
}

fn gen_run_ready<P: GenTools>(ctx: &mut Ctx<P>, args: &Json) -> Outcome<P::Wait> {
    let (request, wait) = match gen_call("gen_run", args) {
        Ok(makepad_gen::tools::GenCall::Run { request, wait }) => (request, wait),
        Ok(_) => unreachable!(),
        Err(outcome) => return outcome,
    };
    let studio = P::studio(ctx.app);
    let (key, run) = match studio.gen_request(&request) {
        Ok(run) => run,
        Err(error) => return fail(ToolError::refused(error)),
    };
    let job = match studio.run_preset(ctx.cx, &key, run) {
        Ok(job) => job,
        Err(error) => return fail(ToolError::refused(format!("{error} (gen_presets lists the presets, their inputs and params)"))),
    };
    let wait = if wait { GEN_WAIT_SECS } else { 0.0 };
    Outcome::Wait(P::gen_wait(GenWait::Job { job, deadline: Cx::time_now() + wait }))
}

fn gen_job<P: GenTools>(ctx: &mut Ctx<P>, args: &Json) -> Outcome<P::Wait> {
    let (id, wait) = match gen_call("gen_job", args) {
        Ok(makepad_gen::tools::GenCall::Job { id, wait }) => (id, wait),
        Ok(_) => unreachable!(),
        Err(outcome) => return outcome,
    };
    let job = match gen_job_id(&id) { Ok(job) => job, Err(outcome) => return outcome };
    if P::studio_ref(ctx.app).job(job).is_none() {
        return fail(ToolError::not_found(format!("no generation job `{id}`")));
    }
    Outcome::Wait(P::gen_wait(GenWait::Job { job, deadline: Cx::time_now() + if wait { GEN_WAIT_SECS } else { 0.0 } }))
}

fn gen_choose<P: GenTools>(ctx: &mut Ctx<P>, args: &Json) -> Outcome<P::Wait> {
    let (id, pick) = match gen_call("gen_choose", args) {
        Ok(makepad_gen::tools::GenCall::Choose { id, pick }) => (id, pick),
        Ok(_) => unreachable!(),
        Err(outcome) => return outcome,
    };
    let job = match gen_job_id(&id) { Ok(job) => job, Err(outcome) => return outcome };
    match P::studio(ctx.app).choose_job(job, pick) {
        Ok(()) => gen_job_reply(ctx, job),
        Err(error) => fail(ToolError::refused(error)),
    }
}

fn gen_cancel<P: GenTools>(ctx: &mut Ctx<P>, args: &Json) -> Outcome<P::Wait> {
    let id = match gen_call("gen_cancel", args) {
        Ok(makepad_gen::tools::GenCall::Cancel { id }) => id,
        Ok(_) => unreachable!(),
        Err(outcome) => return outcome,
    };
    let job = match gen_job_id(&id) { Ok(job) => job, Err(outcome) => return outcome };
    let studio = P::studio(ctx.app);
    if studio.job(job).is_none() {
        return fail(ToolError::not_found(format!("no generation job `{id}`")));
    }
    studio.cancel_job(job);
    gen_job_reply(ctx, job)
}

/// Save a preset to the library with the given prompt, parameters and
/// seed: the function the FLOW page's Save as preset button used to call.
fn gen_save_preset<P: GenTools>(ctx: &mut Ctx<P>, args: &Json) -> Outcome<P::Wait> {
    if let Some(wait) = when_ready(ctx, gen_save_preset_ready::<P>, args) {
        return wait;
    }
    gen_save_preset_ready(ctx, args)
}

fn gen_save_preset_ready<P: GenTools>(ctx: &mut Ctx<P>, args: &Json) -> Outcome<P::Wait> {
    let text = |name: &str| args.get(name).and_then(Json::as_str).map(str::to_string);
    let Some(preset) = text("preset").filter(|preset| !preset.trim().is_empty()) else { return fail(ToolError::invalid("`preset` is required (gen_presets lists them)")) };
    let params = match args.get("params") {
        Some(Json::Obj(fields)) => fields.clone(),
        _ => Vec::new(),
    };
    let seed = match args.get("seed") {
        Some(Json::Int(seed)) if *seed >= 0 => Some(*seed as u64),
        _ => None,
    };
    let run = makepad_gen::tools::RunRequest { preset, prompt: text("prompt"), inputs: Vec::new(), params, seed };
    match P::studio(ctx.app).save_as_preset(&run, text("name").as_deref()) {
        Ok(name) => done(json::obj(vec![("saving", Json::Str(name))])),
        Err(error) => fail(ToolError::refused(error)),
    }
}

fn gen_estimate<P: GenTools>(ctx: &mut Ctx<P>, args: &Json) -> Outcome<P::Wait> {
    if let Some(wait) = when_ready(ctx, gen_estimate_ready::<P>, args) {
        return wait;
    }
    gen_estimate_ready(ctx, args)
}

fn gen_estimate_ready<P: GenTools>(ctx: &mut Ctx<P>, args: &Json) -> Outcome<P::Wait> {
    let (preset, params) = match gen_call("gen_estimate", args) {
        Ok(makepad_gen::tools::GenCall::Estimate { preset, params }) => (preset, params),
        Ok(_) => unreachable!(),
        Err(outcome) => return outcome,
    };
    let request = makepad_gen::tools::RunRequest { preset: preset.clone(), params, ..Default::default() };
    let studio = P::studio(ctx.app);
    let run = match studio.gen_request(&request) {
        Ok((_, run)) => run,
        Err(error) => return fail(ToolError::refused(error)),
    };
    match studio.estimate(&makepad_flow_studio::gen_key(&preset), &run) {
        Some(estimate) => {
            let mut fields = vec![
                ("preset", json::s(preset)),
                ("generators", Json::Arr(estimate.steps.into_iter().map(|(node, model, hub, eta)| json::obj(vec![
                    ("stage", json::s(node)), ("model", json::s(model)), ("node", json::s(hub)), ("eta_seconds", Json::Int((eta / 1000) as i64)),
                ])).collect())),
            ];
            if estimate.unavailable.is_empty() {
                fields.push(("eta_seconds", Json::Int((estimate.total_ms / 1000) as i64)));
            } else {
                fields.push(("missing", Json::Arr(estimate.unavailable.into_iter().map(json::s).collect())));
            }
            done(json::obj(fields))
        }
        None => fail(ToolError::not_found(format!("no estimate for `{preset}`: gen_presets lists the presets"))),
    }
}
