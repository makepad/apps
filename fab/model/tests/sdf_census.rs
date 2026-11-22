//! Inventory, not a count gate. Run on Linux with spirv-val on PATH.
//! FabModel::compile calls script/compile_with; part_fields is Fab's live-field path.
//! No meshing, slicing, baking, app launch or GPU dispatch is needed for this census.
#![cfg(target_os = "linux")]
use fab_model::FabModel;
use fab_sdf_engine::{kernel::{self, KernelAir, ComputeBake, KernelSlot}, program};
use fab_sdf_engine::makepad_csg_sdf::{self as sdf, SdfNode};
use fab_sdf_engine::makepad_draw::{self as draw, *};
use std::{collections::{BTreeMap, BTreeSet}, path::{Path, PathBuf}};

mod fixtures;
#[path = "fixtures/sources.rs"]
mod sources;
use sources::literals;

#[derive(Default)]
struct Census {
    rows: Vec<[String; 5]>,
    counts: BTreeMap<String, usize>,
    parts: usize,
    failed: usize,
    meshes: usize,
    links: usize,
    lower_errors: usize,
    documents: usize,
    compile_errors: Vec<String>,
    trace_sets: BTreeMap<Vec<(u64, bool)>, Result<(), String>>,
    routes: BTreeSet<u64>,
}

fn estimate(cause: &str) -> &'static str {
    match cause {
        "Links" => "medium: GPU polyline-tube distance, clipping and per-link material slots; spatial indexing for mail",
        "Tubes" => "medium: GPU round-cone distance over the cell grid of piece groups; the CPU answers it as a host leaf",
        "mesh/import" => "large: GPU triangle BVH distance plus winding/sign for open and overlapping meshes",
        "non-Splash kernel" => "per type: express the Rust field as analytic nodes or Splash; arbitrary Rust has no GPU compiler",
        "MAX_KERNEL_COST" => "medium/large: reduce AIR cost or split work into resumable GPU dispatches before raising the watchdog bound",
        "MAX_KERNEL_PARAMS" => "medium: extend the program parameter ABI and shader dispatch beyond four vec4s",
        "MAX_FRAME_WORDS" => "medium: support larger local frames or spill to GPU storage with measured occupancy",
        "empty sculpt" => "small: represent the empty solid explicitly in GPU programs",
        "program stack" => "medium: flatten/split deep programs or extend GPU interpreter stacks",
        "SPIR-V compile" => "compiler work: fix the reported trace shader emission/validation failure",
        "dynamic loop" => "medium: bounded GPU loop lowering with explicit progress for long work",
        "recursion" => "large: eliminate recursion or lower an explicit GPU stack",
        "f64" => "medium/large: portable f32 semantics or a capability-checked f64 implementation",
        "array" => "medium: lower the reported array/frame operation into shader IR",
        "closure" => "medium: specialize captures and inline the callable",
        "string" => "redesign field: resolve text on the host before GPU evaluation",
        "math function" => "medium: implement the missing math operation with the required numeric semantics",
        _ => "inspect the exact diagnostic; add the missing shader IR operation or program lowering",
    }
}

fn classify(error: &str) -> &'static str {
    let e = error.to_lowercase();
    if e.contains("ops per sample") { "MAX_KERNEL_COST" }
    else if e.contains("parameters (at most") { "MAX_KERNEL_PARAMS" }
    else if e.contains("words of local arrays") { "MAX_FRAME_WORDS" }
    else if e.contains("f64") { "f64" }
    else if e.contains("recurs") { "recursion" }
    else if e.contains("loop") || e.contains("bound") { "dynamic loop" }
    else if e.contains("string") { "string" }
    else if e.contains("closure") { "closure" }
    else if e.contains("array") || e.contains("frame") { "array" }
    else if e.contains("math") || e.contains("builtin") { "math function" }
    else { "unsupported language construct" }
}

impl Census {
    fn row(&mut self, doc: &str, part: &str, construct: &str, cause: &str, measured: impl ToString) {
        self.rows.push([doc.into(), part.into(), construct.into(), cause.into(), measured.to_string()]);
        *self.counts.entry(cause.into()).or_default() += 1;
    }

    // Each tree occurrence has a path; repeated references in different parts count separately.
    // Walk all children even after lower_with has returned its first error.
    fn walk(&mut self, doc: &str, part: &str, path: &str, n: &SdfNode, native_type: Option<&str>) {
        match n {
            SdfNode::Links(l) => {
                let p = program::lower(n).expect("Links lower");
                self.links += 1;
                println!("[sdf-census] LINKS {doc} {part} {path}: {} loops, {} segments, {} buffer bytes",
                    l.loops.len(), l.loops.iter().map(|l| l.pts.len()).sum::<usize>(), p.texels.len() * 16);
            },
            SdfNode::Tubes(t) => self.row(doc, part, &format!("{path}: Tubes"), "Tubes", format!("{} curves, {} pieces", t.curves.len(), t.piece_count())),
            SdfNode::Sketch(s) => self.row(doc, part, &format!("{path}: Sketch"), "Sketch", format!("{} edges", s.outline.edge_count())),
            SdfNode::Mesh(m) => { self.meshes += 1; println!("[sdf-census] MESH {doc} {part} {path}: {} triangles", m.data.triangles.len()); },
            SdfNode::Kernel(k) => match &k.splash {
                None => self.row(doc, part, &format!("{path}: {}", native_type.expect("identify every native fixture's Rust type")), "non-Splash kernel", "SdfKernel.splash=None"),
                Some(s) => match KernelAir::new(s) {
                    Ok(a) => { self.routes.insert(a.key); println!("[sdf-census] route {doc} {part} {path}: cost={} params={}", a.cost, a.params); }
                    Err(e) => self.row(doc, part, &format!("{path}: Splash kernel"), classify(&e), e),
                },
            },
            SdfNode::Combine { of, .. } => for (i, child) in of.iter().enumerate() { self.walk(doc, part, &format!("{path}/{i}"), child, native_type); },
            SdfNode::Material { within, of, .. } => {
                if let Some(w) = within { self.walk(doc, part, &format!("{path}/within"), w, native_type); }
                self.walk(doc, part, &format!("{path}/of"), of, native_type);
            }
            SdfNode::Sculpt { strokes, of } => {
                if sdf::bounds::sculpt_in(strokes, of, &([f64::NEG_INFINITY; 3], [f64::INFINITY; 3])).is_none() {
                    self.row(doc, part, &format!("{path}: Sculpt"), "empty sculpt", "a sculpt with nothing left of its solid");
                }
                self.walk(doc, part, &format!("{path}/of"), of, native_type);
            }
            SdfNode::Shell { of, .. } | SdfNode::Round { of, .. } | SdfNode::Transform { of, .. }
            | SdfNode::Instance { of, .. } | SdfNode::Bone { of, .. } | SdfNode::Deform { of, .. }
            | SdfNode::Id { of, .. } | SdfNode::Texture { of, .. } => self.walk(doc, part, &format!("{path}/of"), of, native_type),
            SdfNode::Sphere { .. } | SdfNode::Box { .. } | SdfNode::Cylinder { .. } | SdfNode::Torus { .. }
            | SdfNode::Capsule { .. } | SdfNode::Ellipsoid { .. } | SdfNode::Gyroid { .. } => {}
        }
    }

    fn part(&mut self, cx: &mut Cx, doc: &str, part: &str, n: &SdfNode, native_type: Option<&str>) {
        self.parts += 1;
        let start = self.rows.len();
        let mut slots = Vec::new();
        self.walk(doc, part, "root", n, native_type);
        if let Err(error) = kernel::collect(n, &mut slots) {
            if self.rows.len() == start { self.row(doc, part, "program", "lowering", format!("{error:?}")); }
            self.lower_errors += 1;
            self.failed += 1;
            return;
        }
        match program::lower_with(n, &slots, 1.0) {
            Err(e) => {
                self.lower_errors += 1;
                println!("[sdf-census] lower error {doc} {part}: {e:?}");
                if self.rows.len() == start { self.row(doc, part, "program", "lowering", format!("{e:?}")); }
            }
            Ok(p) => {
                if p.value_depth > program::MAX_VALUE_DEPTH || p.point_depth > program::MAX_POINT_DEPTH {
                    self.row(doc, part, "program", "program stack", format!("values={}/{} points={}/{}", p.value_depth, program::MAX_VALUE_DEPTH, p.point_depth, program::MAX_POINT_DEPTH));
                }
            }
        }
        if !slots.is_empty() {
            let key = slots.iter().map(|s| (s.key, !s.is_mesh())).collect();
            let result = self.trace_sets.entry(key).or_insert_with(|| compile_trace(cx, &slots)).clone();
            if let Err(e) = result {
                // Attribute a failed set to every routed construct in that set.
                for (i, s) in slots.iter().enumerate() {
                    self.row(doc, part, &format!("kernel slot {i} ({:016x})", s.key), "SPIR-V compile", &e);
                }
            }
        }
        self.failed += usize::from(self.rows.len() > start);
    }

    fn document(&mut self, cx: &mut Cx, doc: &str, source: &str, files: &BTreeMap<String, String>, project: bool) {
        self.documents += 1;
        let model = if project { FabModel::load("census", source, files.clone()) }
        else { Ok(FabModel::with_script_files("census", "census.cad.splash", source, files.clone(), BTreeMap::new())) };
        let compiled = model.and_then(|mut m| {
            if doc.contains("app/tests/sdf_field.rs:") && source.contains("m.import(\"mesh_query_cube\")") {
                // This runtime fixture imports its generated STL before applying
                // the script. Give the extracted document that same asset.
                m.assets = std::sync::Arc::new(fab_model::program::Assets::from([("mesh_query_cube".into(), fixtures::imported_stl_cube())]));
            }
            m.compile()
        });
        match compiled {
            Err(e) => {
                let message = format!("{doc}: {e}");
                println!("[sdf-census] document error {message}");
                self.compile_errors.push(message);
            }
            Ok(c) => {
                let before = self.parts;
                let mut non_fields = 0;
                for inst in &c.instances {
                    let fields = fab_model::eval::part_fields(inst, &inst.script, &c.strokes);
                    non_fields += inst.placed.len() - fields.len();
                    for (name, shape, _) in fields { self.part(cx, doc, &name, &shape.root, None); }
                }
                println!("[sdf-census] document {doc}: {} SDF parts, {non_fields} non-SDF parts", self.parts - before);
            }
        }
    }
}

// Compile the production trace shader with its Eval and mesh bindings.
fn compile_trace(cx: &mut Cx, slots: &[KernelSlot]) -> Result<(), String> {
    cx.with_vm(|vm| {
        use makepad_script::shader_ir_compute::{ComputeTarget, KernelCode};
        let mut splice = ComputeBake::new(vm, slots).ok_or("ComputeBake::new returned None")?;
        let module = vm.heap().module(id!(sdf));
        let base = vm.value(module, id!(TraceCoarse)).as_object().ok_or("TraceCoarse missing")?;
        let result = splice.draw_variant(vm, base).ok_or("trace shader variant".to_string())
            .and_then(|object| makepad_script::shader_ir_compute::compile_compute_kernel(vm, object, ComputeTarget::Spirv));
        splice.release(vm);
        let kernel = result?;
        let KernelCode::Spirv(words) = kernel.code else { return Err("no SPIR-V output".into()); };
        match makepad_script::shader_ir_spirv::spirv_val(&words) {
            Some(result) => result?,
            None => panic!("spirv-val unavailable: Linux validation is required"),
        }
        println!("[sdf-census] validated kernel set {:?}: compute SPIR-V", slots.iter().map(|s| format!("{:016x}", s.key)).collect::<Vec<_>>());
        Ok(())
    })
}

fn paths(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<_> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
    out.sort(); out
}

#[test]
fn sdf_census() {
    let fab = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let root = fab.ancestors().nth(3).unwrap();
    let resources = fab.join("app/resources");
    let files: BTreeMap<String, String> = paths(&resources).into_iter().filter(|p| p.extension().is_some_and(|e| e == "splash"))
        .map(|p| (p.file_name().unwrap().to_str().unwrap().into(), std::fs::read_to_string(p).unwrap())).collect();
    let mut cx = Cx::new(Box::new(|_, _| {}));
    cx.with_vm(|vm| {
        draw::makepad_platform::script::script_mod(vm);
        draw::script_mod(vm);
        fab_sdf_engine::compute::script_mod(vm);
        fab_sdf_engine::gpu_jobs::script_mod(vm);
        fab_sdf_engine::trace::script_mod(vm);
    });
    let mut c = Census::default();
    for (name, source) in &files {
        if name.ends_with(".cad.splash") || name.ends_with(".fab.splash") {
            c.document(&mut cx, &format!("apps/apps/fab/app/resources/{name}"), source, &files, name.ends_with(".fab.splash"));
        }
    }
    for crate_dir in paths(fab) {
        let tests = crate_dir.join("tests");
        if !tests.is_dir() { continue; }
        for path in paths(&tests).into_iter().filter(|p| p.extension().is_some_and(|e| e == "rs")) {
            if path.ends_with("model/tests/sdf_census.rs") { continue; }
            let relative = path.strip_prefix(root).unwrap().to_str().unwrap();
            let text = std::fs::read_to_string(&path).unwrap();
            let sources = literals(&text);
            let mut inline_files = files.clone();
            // np_model_field's PROJECT reads the DOME constant from the same Rust file.
            if relative.ends_with("np_model_field.rs") {
                inline_files.insert("dome.cad.splash".into(), sources.iter().find(|(_, s)| s.contains("let dome = m.sdf")).unwrap().1.clone());
            }
            for (line, mut source) in sources {
                if !source.contains("use mod.model") && !(source.contains("use mod.fab") && source.contains("script(\"")) { continue; }
                if source.contains("{side}") { source = source.replace("{side}", "120mm").replace("{bar}", "0.8mm").replace("{{", "{").replace("}}", "}"); }
                let doc = format!("{relative}:{line}");
                if relative.ends_with("jobs.rs") && source.contains("m.no_such_shape") {
                    // This job fixture deliberately asks for an unknown constructor.
                    println!("[sdf-census] expected-invalid document {doc}");
                    let result = FabModel::with_script("invalid", "invalid.cad.splash", &source).compile();
                    assert!(result.is_err(), "revisit expected-invalid fixture {doc}");
                    continue;
                }
                let project = !source.contains("use mod.model");
                c.document(&mut cx, &doc, &source, &inline_files, project);
            }
        }
    }
    // Runtime-generated plate-list documents: same shape under three names; sources live in
    // app/tests/plates_panel.rs. Actual user plates/AI outputs live outside this repository.
    for name in ["lattice", "figure", "saved"] {
        c.document(&mut cx, &format!("apps/apps/fab/app/tests/plates_panel.rs:{name}"), &format!("{}let block = m.box({{size: [10mm 10mm 10mm]}})\nobject(block)\n", fab_model::program::EMPTY), &files, false);
    }
    c.document(&mut cx, "apps/apps/fab/app/src/demo_part.rs:noise_vase", &format!("{}\n{}", files["vase.cad.splash"], files["noise_paint.splash"]), &files, false);
    // The other assembled script sources in slice/tests/painted_by_position.rs::cases.
    let painted = std::fs::read_to_string(fab.join("slice/tests/painted_by_position.rs")).unwrap();
    let materials = literals(&painted).into_iter().find(|(_, s)| s.starts_with("let base = Material")).unwrap().1;
    let belly = |y: f64, a: f64| {
        let r = 21.0 * (1.0 - ((y - 23.8) / 25.2).powi(2)).sqrt();
        format!("[{:.3}mm {y:.3}mm {:.3}mm]", r * a.cos(), r * a.sin())
    };
    for (name, suffix) in [
        ("plain", String::new()),
        ("stroke", format!("{materials}m.stroke([{} {}], 3mm, red)\n", belly(18.0, 0.1), belly(32.0, 0.75))),
        ("flood", format!("{materials}m.fill(Flood{{seed: {} angle: 12deg}}, red)\n", belly(23.8, 0.0))),
        ("noise", files["noise_paint.splash"].clone()),
    ] {
        c.document(&mut cx, &format!("apps/apps/fab/slice/tests/painted_by_position.rs:{name}"), &format!("{}\n{suffix}", files["vase.cad.splash"]), &files, false);
    }
    for (name, source) in [("weave.rs:MAIL", sources::WEAVE_MAIL), ("weave.rs:chainmail_scale", sources::WEAVE_LARGE), ("mechanics.rs:sheet", sources::MECHANICS_SHEET)] {
        c.document(&mut cx, &format!("retained app/tests/{name}"), source, &files, false);
    }
    fixtures::visit(&mut |doc, name, node, ty| c.part(&mut cx, doc, name, &node, ty));
    println!("document | part | construct | cause | measured value / exact diagnostic");
    println!("--- | --- | --- | --- | ---");
    for row in &c.rows { println!("{}", row.join(" | ")); }
    println!("[sdf-census] counts per construct occurrence:");
    for (cause, n) in &c.counts { println!("{cause} | {n} | {}", estimate(cause)); }
    println!("[sdf-census] coverage: {} documents; {} distinct routed kernels; {} kernel sets, {} SPIR-V failures; {} lower_with errors; {} document errors", c.documents, c.routes.len(), c.trace_sets.len(), c.trace_sets.values().filter(|r| r.is_err()).count(), c.lower_errors, c.compile_errors.len());
    // A census reports unavailable documents too; it does not turn either document
    // rejection or GPU support counts into a gate. In particular, the old PROJECT in
    // np_model_field uses the removed `filament` argument; its DOME source is evaluated
    // independently above, unchanged and at defaults. Never silently rewrite a fixture.
    for e in &c.compile_errors { println!("[sdf-census] unevaluated document (excluded from part totals): {e}"); }
    println!("[sdf-census] links: {} lowering through LINKS", c.links);
    println!("[sdf-census] meshes: {} lowering through MESH", c.meshes);
    println!("[sdf-census] lowering: {} parts, {} fail", c.parts, c.failed);
}
