//! Parts printed in full colour from the loaded filaments: a picture tile (`m.picture`) and the
//! swatch that measures a filament set (`m.colour_swatch`).
//!
//! A picture's file lives beside its script ([`Pictures`]); the lowering reads only its size.
//! The paint compiles to a raster mix over the part ([`Spec`] says what makes it), and the build
//! makes the raster once it knows the filaments ([`grid`]): the tile's dither
//! (`fab_colour::tile`) or the swatch's layout (`fab_colour::swatch`), each print layer a grid
//! of filaments one bead apart. The part prints with the settings that grid needs
//! ([`settings`]): every layer solid, its lines a bead apart along x then y, and its first layer
//! painted too.

use fab_colour::{dither::Rules, palette::Filament, swatch, tile, view, FilamentSet, Image};
use fab_paint_field::raster::{RasterGrid, RasterSpec};
use makepad_model_script::lang;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

/// A picture file beside a script: its bytes (saved with the project) and the picture read.
#[derive(Debug, PartialEq)]
pub struct Picture {
    pub bytes: Arc<Vec<u8>>,
    pub image: Arc<Image>,
}

impl Picture {
    pub fn new(bytes: Vec<u8>) -> Result<Picture, String> {
        let image = Image::decode(&bytes)?;
        Ok(Picture { bytes: Arc::new(bytes), image: Arc::new(image) })
    }
}

/// A picture's copy in a crash copy's meshes (`photo.jpg.picture`): its file's bytes as they are.
pub const PICTURE_EXTENSION: &str = "picture";

/// A document's pictures, by file name.
pub type Pictures = BTreeMap<String, Arc<Picture>>;

/// The picture files a script names (`m.picture({image: "photo.jpg" ..})`), each once.
pub fn picture_files(source: &str) -> Vec<String> {
    let Ok(names) = lang::names::parse(source) else { return Vec::new() };
    let mut out: Vec<String> = Vec::new();
    names.syntax.walk(&mut |_, node| {
        if let Some(("m.picture", args)) = node.as_call() {
            if let Some(f) = args.first().and_then(|a| a.field("image")).and_then(|f| f.value.string()) {
                if !out.iter().any(|o| o == f) {
                    out.push(f.to_string());
                }
            }
        }
    });
    out
}

/// The pictures a script names, read from `dir` (one missing: its part says so).
pub fn load(dir: &Path, source: &str, out: &mut Pictures) -> Result<(), String> {
    for f in picture_files(source) {
        if out.contains_key(&f) || f.contains(['/', '\\']) {
            continue;
        }
        if let Ok(bytes) = std::fs::read(dir.join(&f)) {
            out.insert(f.clone(), Arc::new(Picture::new(bytes).map_err(|e| format!("{}: {e}", dir.join(&f).display()))?));
        }
    }
    Ok(())
}

/// The pictures' sizes in pixels, as the lowering reads them.
pub fn sizes(p: &Pictures) -> lang::Images {
    p.iter().map(|(k, v)| (k.clone(), [v.image.width as u32, v.image.height as u32])).collect()
}

/// What makes a picture's or a swatch's paint: the builder's fields, its picture, the materials
/// of a first guess (name and linear colour) and where its box's middle sits (model mm).
#[derive(Debug)]
pub struct Spec {
    key: u64,
    pub arg: Arg,
    pub image: Option<Arc<Image>>,
    pub filaments: Vec<(String, [f32; 3])>,
    pub centre: [f64; 3],
}

/// A picture builder's fields (mm).
#[derive(Clone, Debug, PartialEq)]
pub struct Arg {
    pub swatch: bool,
    pub size: [f64; 2],
    pub layers: usize,
    pub layer: f64,
    pub face_down: bool,
    pub min_run: usize,
    pub smooth: f64,
    pub set: Option<String>,
}

impl Arg {
    /// A builder's fields as the lowering read them.
    pub fn of(a: &lang::PictureArg) -> Arg {
        Arg { swatch: a.image.is_none(), size: a.size, layers: a.layers as usize, layer: a.layer, face_down: a.face_down, min_run: a.min_run as usize, smooth: a.smooth, set: a.set.clone() }
    }
}

impl Spec {
    pub fn new(arg: Arg, image: Option<(Arc<Picture>, &str)>, filaments: Vec<(String, [f32; 3])>, centre: [f64; 3]) -> Spec {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut eat = |b: &[u8]| {
            for &x in b {
                h = (h ^ x as u64).wrapping_mul(0x100_0000_01b3);
            }
        };
        eat(format!("{arg:?}|{filaments:?}|{centre:?}").as_bytes());
        if let Some((p, name)) = &image {
            eat(name.as_bytes());
            eat(&p.bytes);
        }
        Spec { key: h | 1, arg, image: image.map(|(p, _)| p.image.clone()), filaments, centre }
    }
}

impl RasterSpec for Spec {
    fn key(&self) -> u64 {
        self.key
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// The bead every line of a picture prints (mm) on a nozzle: one width for its walls and its
/// layers, so its lines lie one spacing apart and the grid's pitch is that spacing.
pub fn bead(nozzle: f64) -> f64 {
    nozzle * 1.05
}

/// The grid's pitch (mm): the spacing of beads of [`bead`] at the layer height.
pub fn cell(nozzle: f64, layer: f64) -> f64 {
    makepad_fab_toolpath::flow::spacing(bead(nozzle), layer)
}

/// The print settings a picture part needs, under the placement's own: its layers, all solid in
/// lines a bead apart running along x then y on one grid through its middle (its cells'), one
/// wall, and its paint printed on the first layer and exactly all through.
pub fn settings(arg: &lang::PictureArg, nozzle: f64) -> Vec<(String, String)> {
    let (h, w) = (format!("{}", arg.layer), format!("{:.3}", bead(nozzle)));
    let n = arg.layers.to_string();
    let mut out: Vec<(String, String)> = [("layer_height", h.as_str()), ("initial_layer_print_height", h.as_str()), ("wall_loops", "1"), ("top_shell_layers", n.as_str()), ("bottom_shell_layers", n.as_str()), ("infill_direction", "0"), ("bottom_surface_pattern", "monotonic"), ("top_surface_pattern", "monotonic"), ("internal_solid_infill_pattern", "monotonic"), ("ironing_type", "no ironing"), ("fab_paint_first_layer", "1"), ("fab_paint_inside_exact", "1"), ("fab_solid_on_grid", "1")]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    for key in ["line_width", "initial_layer_line_width", "outer_wall_line_width", "inner_wall_line_width", "internal_solid_infill_line_width", "top_surface_line_width", "sparse_infill_line_width"] {
        out.push((key.to_string(), w.clone()));
    }
    out
}

/// The filament set a picture prints with: the calibrated set it names, else its first
/// guess's materials, else the filaments loaded (in slot order) as a set kept for them at its
/// layer height measured them ([`crate::colour_sets::matching`]), or as first guessed.
pub fn set_of(spec: &Spec, loaded: &[(String, [f32; 3])], calibrated: &crate::colour_sets::Sets) -> Result<FilamentSet, String> {
    if let Some(name) = &spec.arg.set {
        return match calibrated.get(name) {
            Some(set) => Ok((**set).clone()),
            None => Err(format!("no calibrated filament set '{name}': print the colour swatch and read its photo first, or give filaments: [..] for a first guess")),
        };
    }
    if spec.filaments.is_empty() {
        if let Some(set) = crate::colour_sets::matching(calibrated, loaded, spec.arg.layer) {
            return Ok((**set).clone());
        }
    }
    let list = if spec.filaments.is_empty() { loaded } else { &spec.filaments[..] };
    if list.is_empty() {
        return Err("no filaments to print the picture with: link the printer with its filaments loaded, or give filaments: [Material{..} ..] for a first guess".into());
    }
    if list.len() > 16 {
        return Err(format!("a picture prints with at most 16 filaments, not {}", list.len()));
    }
    let name = list.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join("+");
    Ok(FilamentSet::new(&name, list.iter().map(|(n, c)| Filament::guess_linear(n, *c)).collect()))
}

/// A swatch part's photo read back ([`fab_colour::read`]): its filament set as it printed
/// ([`set_of`]), named `name`, with each of its stacks as the photo shows it, and the reading.
pub fn read_swatch(spec: &Spec, loaded: &[(String, [f32; 3])], nozzle: f64, photo: &[u8], name: &str) -> Result<(FilamentSet, fab_colour::read::Reading), String> {
    if !spec.arg.swatch {
        return Err("that part is a picture: read the photo of a colour swatch (m.colour_swatch)".into());
    }
    // the filaments as first guessed: the swatch's layout is theirs, its reading replaces any
    // calibration's
    let mut set = set_of(spec, loaded, &Default::default())?;
    let reading = fab_colour::read::read(&Image::decode(photo)?, &set, &tile_spec(spec, nozzle))?;
    set.name = name.to_string();
    set.measured = reading.measured.clone();
    Ok((set, reading))
}

/// The layers from the face a tile dithers (the rest are its backing).
fn depth(spec: &Spec) -> usize {
    2.min(spec.arg.layers)
}

/// The tile's layout on the grid: its pitch, size and rules.
fn tile_spec(spec: &Spec, nozzle: f64) -> tile::TileSpec {
    let c = cell(nozzle, spec.arg.layer);
    // an odd count, so a cell's middle is the part's, where the solid lines' grid runs through
    let count = |mm: f64| ((mm / c).floor() as usize).max(1).saturating_sub(1) | 1;
    let size = [count(spec.arg.size[0]), count(spec.arg.size[1])];
    tile::TileSpec {
        cell: c,
        size,
        layers: spec.arg.layers,
        layer: spec.arg.layer,
        face: if spec.arg.face_down { tile::Face::Down } else { tile::Face::Up },
        depth: depth(spec),
        rules: Rules { min_run: spec.arg.min_run, switch: (spec.arg.smooth * 0.1) as f32, lightness: 2.0 },
        first_lines: tile::Axis::X,
        background: [0.8; 3],
        viewing: view::VIEWING,
    }
}

/// A picture's or swatch's raster in `set`'s filaments (places: the set's filaments in order),
/// and the tile it came from.
pub fn grid(spec: &Spec, set: &FilamentSet, nozzle: f64) -> (Arc<RasterGrid>, tile::Tile) {
    let ts = tile_spec(spec, nozzle);
    let t = match &spec.image {
        Some(image) if !spec.arg.swatch => tile::make(image, set, &ts),
        _ => swatch::tile(set, &ts).0,
    };
    (raster(spec, &t, &t.layers), t)
}

/// What a picture's or swatch's part looks like rather than what it prints: its raster with
/// the dithered layers holding, past the `filaments` places, a place per colour its stacks show
/// from the face (the backing its filaments), and those colours (linear). At most `room`
/// colours: past that many, a cell takes the nearest of the commonest.
pub fn face_view(spec: &Spec, t: &tile::Tile, filaments: usize, room: usize) -> (Arc<RasterGrid>, Vec<[f32; 3]>) {
    let n = t.layers.len();
    let dithered: Vec<usize> = (0..depth(spec).min(n)).map(|d| if spec.arg.face_down { d } else { n - 1 - d }).collect();
    let key = |c: &[f32; 3]| c.map(f32::to_bits);
    let mut count: std::collections::HashMap<[u32; 3], (usize, [f32; 3])> = std::collections::HashMap::new();
    for c in &t.shown {
        count.entry(key(c)).or_insert((0, *c)).0 += 1;
    }
    let mut colours: Vec<(usize, [f32; 3])> = count.into_values().collect();
    colours.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.map(f32::to_bits).cmp(&b.1.map(f32::to_bits))));
    colours.truncate(room.min(256 - filaments));
    let kept: Vec<[f32; 3]> = colours.into_iter().map(|(_, c)| c).collect();
    let lab: Vec<_> = kept.iter().map(|c| fab_colour::oklab::oklab(*c)).collect();
    let mut place = std::collections::HashMap::new();
    let mut layers = t.layers.clone();
    for (cell, c) in t.shown.iter().enumerate() {
        let k = *place.entry(key(c)).or_insert_with(|| {
            let l = fab_colour::oklab::oklab(*c);
            (0..kept.len()).min_by(|&a, &b| fab_colour::oklab::delta_e(lab[a], l).total_cmp(&fab_colour::oklab::delta_e(lab[b], l))).unwrap_or(0)
        });
        for &l in &dithered {
            layers[l][cell] = (filaments + k) as u8;
        }
    }
    (raster(spec, t, &layers), kept)
}

/// A tile's `layers` (plan, x right and y to the back) as a raster in the model frame (x, and
/// z = -y): a grid row from the least z is a plan row from the back.
fn raster(spec: &Spec, t: &tile::Tile, layers: &[Vec<u8>]) -> Arc<RasterGrid> {
    let [nx, ny] = t.size;
    let places = layers
        .iter()
        .map(|layer| (0..nx * ny).map(|i| {
            let (x, r) = (i % nx, i / nx);
            layer[(ny - 1 - r) * nx + x]
        }).collect())
        .collect();
    let thick = spec.arg.layers as f64 * spec.arg.layer;
    let origin = [spec.centre[0] - 0.5 * nx as f64 * t.cell, spec.centre[1] - 0.5 * thick, spec.centre[2] - 0.5 * ny as f64 * t.cell];
    Arc::new(RasterGrid::new(origin, t.cell, spec.arg.layer, [nx, ny], places))
}

/// A compiled script's picture and swatch parts: what makes each one's paint.
pub fn specs(script: &crate::program::Script) -> Vec<&Spec> {
    script.paint.values().flat_map(|paint| raster_specs(&paint.program)).collect()
}

/// What makes each picture or swatch a paint program prints, in its layers' order.
pub fn raster_specs(program: &fab_paint_field::Program) -> impl Iterator<Item = &Spec> {
    program.layers.iter().filter_map(|layer| {
        let fab_paint_field::Op::Mix { mix, .. } = &layer.op else { return None };
        let fab_paint_field::MixField::Raster(r) = &mix.field else { return None };
        r.spec.as_any().downcast_ref::<Spec>()
    })
}

/// A picture or swatch part as it will look.
pub struct Preview {
    /// A picture: the picture as the tile's cells take it and the face it prints above (as
    /// the eye sees it, turned over), the face from [`view::VIEWING`] and the ΔE map against
    /// the picture below ([`view::compare`]); a swatch: its face alone.
    pub image: Image,
    /// A picture's ΔE from [`view::VIEWING`] (mean, 95th percentile): in all, what the gamut
    /// cost, what the dither cost.
    pub delta: Option<[(f32, f32); 3]>,
}

/// A compiled script's picture and swatch parts, each as a [`Preview`] `scale` pixels a cell,
/// in the colours the filament set shows (its measured stacks', else the first guess's).
pub fn previews(script: &crate::program::Script, loaded: &[(String, [f32; 3])], calibrated: &crate::colour_sets::Sets, nozzle: f64, scale: usize) -> Vec<Result<Preview, String>> {
    specs(script)
        .into_iter()
        .map(|spec| {
            let set = set_of(spec, loaded, calibrated)?;
            let (_, t) = grid(spec, &set, nozzle);
            let face = if spec.arg.face_down { tile::Face::Down } else { tile::Face::Up };
            Ok(match &spec.image {
                Some(image) if !spec.arg.swatch => {
                    let picture = Image { width: t.size[0], height: t.size[1], pixels: image.cells(t.size[0], t.size[1]) };
                    let c = view::compare(&picture, &t.as_seen(face, &t.wanted), &t.face(face), t.cell, scale);
                    Preview { image: c.image, delta: Some([c.total, c.gamut, c.dither]) }
                }
                _ => Preview { image: t.face(face).scaled(scale), delta: None },
            })
        })
        .collect()
}

/// The swatch's size (mm) for `n` filaments, as `m.colour_swatch({size})` takes it.
pub fn swatch_size(n: usize) -> [f64; 2] {
    swatch::size(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::Evaluator;
    use crate::FabModel;
    use fab_plate::machine::Machine;

    /// A picture red on its left half, blue on its right, as a file beside the script.
    fn halves() -> Arc<Picture> {
        let (w, h) = (24, 16);
        let pixels = (0..w * h).map(|i| if i % w < w / 2 { [0.75, 0.02, 0.03] } else { [0.02, 0.06, 0.6] }).collect();
        Arc::new(Picture { bytes: Arc::new(b"halves".to_vec()), image: Arc::new(Image { width: w, height: h, pixels }) })
    }

    const SCRIPT: &str = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\n\
        let cyan = Material{albedo: #00a0e0 name: \"Cyan\"}\n\
        let magenta = Material{albedo: #e0007a name: \"Magenta\"}\n\
        let yellow = Material{albedo: #ffe000 name: \"Yellow\"}\n\
        let white = Material{albedo: #f2f2f2 name: \"White\"}\n\
        let tile = m.picture({image: \"halves.png\" width: 24mm filaments: [cyan magenta yellow white]})\n\
        object(tile)\n";

    /// A face-down picture tile in a first guess's filaments prints its picture on its first two
    /// layers, mirrored: where the picture's red left half lands (the right as it prints) its
    /// stacks show red, where its blue half lands blue; its backing layers are the set's white,
    /// and the part prints at the tile's layer height with its first layer painted.
    #[test]
    fn a_picture_tile_paints_its_picture_on_the_first_layers() {
        let mut m = FabModel::with_script("tile", "tile.cad.splash", SCRIPT);
        m.pictures = Arc::new(Pictures::from([("halves.png".to_string(), halves())]));
        let mut e = Evaluator::new(Machine::dual_nozzle_350());
        let ev = m.evaluate(&mut e, None).unwrap_or_else(|f| panic!("{f}")).1;
        assert!(ev.warnings.is_empty(), "{:?}", ev.warnings);
        let part = &ev.plate.parts[0];
        let get = |k: &str| part.print.iter().find(|(o, _)| o == k).map(|(_, v)| v.as_str());
        assert_eq!(get("layer_height"), Some("0.16"));
        assert_eq!(get("fab_paint_first_layer"), Some("1"));
        let sp = part.paint.as_ref().expect("the tile's paint");
        let states = sp.states(part.filament);
        let names = ["Cyan", "Magenta", "Yellow", "White"];
        let filament = |p: [f64; 3]| -> usize {
            let slot = (sp.sample_at)(p).slot as usize;
            let n = sp.colors.get(slot).and_then(|c| c.color.clone()).filter(|_| states.get(slot).is_some_and(|s| *s != 0)).unwrap_or_default();
            names.iter().position(|k| *k == n).unwrap_or_else(|| panic!("{p:?} prints '{n}'"))
        };
        let lo = part.mesh.positions.iter().fold([f64::MAX; 3], |a, p| [a[0].min(p[0]), a[1].min(p[1]), a[2].min(p[2])]);
        let hi = part.mesh.positions.iter().fold([f64::MIN; 3], |a, p| [a[0].max(p[0]), a[1].max(p[1]), a[2].max(p[2])]);
        assert!((hi[2] - lo[2] - 0.64).abs() < 1e-6 && (hi[0] - lo[0] - 24.0).abs() < 1e-6, "the tile is {lo:?}..{hi:?}");
        let set = FilamentSet::new("first guess", ["#00a0e0", "#e0007a", "#ffe000", "#f2f2f2"].iter().zip(names).map(|(c, n)| {
            let v = u32::from_str_radix(&c[1..], 16).unwrap();
            Filament::guess(n, [(v >> 16) as u8, (v >> 8) as u8, v as u8])
        }).collect());
        // per half as it prints (x below and above the middle): the mean colour its stacks show
        let mut sum = [[0.0f32; 3]; 2];
        let mut n = [0usize; 2];
        for i in 0..60 {
            for j in 0..40 {
                let x = lo[0] + 0.5 + (hi[0] - lo[0] - 1.0) * i as f64 / 59.0;
                let y = lo[1] + 0.5 + (hi[1] - lo[1] - 1.0) * j as f64 / 39.0;
                if (x - 0.5 * (lo[0] + hi[0])).abs() < 1.0 {
                    continue;
                }
                let stack: Vec<u8> = (0..4).map(|l| filament([x, y, lo[2] + 0.16 * (l as f64 + 0.5)]) as u8).collect();
                assert!(stack[2] == 3 && stack[3] == 3, "the backing at {x:.1},{y:.1} is {stack:?}");
                let (c, _) = set.colour_of(&stack.iter().map(|f| (*f, 0.16)).collect::<Vec<_>>(), [0.8; 3]);
                let half = (x > 0.5 * (lo[0] + hi[0])) as usize;
                n[half] += 1;
                for k in 0..3 {
                    sum[half][k] += c[k];
                }
            }
        }
        let lab = |h: usize| fab_colour::oklab::oklab(sum[h].map(|v| v / n[h] as f32));
        let (blue, red) = (lab(0), lab(1));
        // mirrored: the picture's red (its left) prints on the right half
        assert!(red[1] > 0.1 && red[2] > 0.0, "the red half shows {red:?}");
        assert!(blue[2] < -0.06 && blue[1].abs() < 0.05, "the blue half shows {blue:?}");
    }

    /// The views show a picture's face in the colours its stacks show, not its filaments': its
    /// two dithered layers show each stack's colour, red on the red half (the right as it
    /// prints) and blue on the blue, and the backing above shows the white it prints in.
    #[test]
    fn the_views_show_a_pictures_face_in_its_stacks_colours() {
        let mut m = FabModel::with_script("tile", "tile.cad.splash", SCRIPT);
        m.pictures = Arc::new(Pictures::from([("halves.png".to_string(), halves())]));
        let mut e = Evaluator::new(Machine::dual_nozzle_350());
        let ev = m.evaluate(&mut e, None).unwrap_or_else(|f| panic!("{f}")).1;
        let paint = ev.paint[0].as_ref().expect("the tile's paint");
        let (program, shown) = paint.seen.as_ref().expect("the tile's face as the views show it");
        // a point's colour as the views show it, and whether it is one of the stacks' (past the
        // filaments' slots)
        let seen = |p: [f64; 3]| -> ([f32; 3], bool) {
            let f = &paint.from_printer;
            let q = std::array::from_fn(|r| f[r][0] * p[0] + f[r][1] * p[1] + f[r][2] * p[2] + f[r][3]);
            let slot = program.sample(fab_paint_field::Channel::Color, q).slot as usize;
            (shown[slot], slot >= paint.colors.len())
        };
        let part = &ev.plate.parts[0];
        let lo = part.mesh.positions.iter().fold([f64::MAX; 3], |a, p| [a[0].min(p[0]), a[1].min(p[1]), a[2].min(p[2])]);
        let hi = part.mesh.positions.iter().fold([f64::MIN; 3], |a, p| [a[0].max(p[0]), a[1].max(p[1]), a[2].max(p[2])]);
        let mid = 0.5 * (lo[0] + hi[0]);
        let mut sum = [[0.0f32; 3]; 2];
        let mut n = [0usize; 2];
        for i in 0..60 {
            for j in 0..40 {
                let x = lo[0] + 0.5 + (hi[0] - lo[0] - 1.0) * i as f64 / 59.0;
                let y = lo[1] + 0.5 + (hi[1] - lo[1] - 1.0) * j as f64 / 39.0;
                if (x - mid).abs() < 1.0 {
                    continue;
                }
                let (back, stack) = seen([x, y, lo[2] + 0.16 * 3.5]);
                let back = fab_colour::oklab::oklab(back);
                assert!(!stack && back[0] > 0.9 && back[1].abs() < 0.02, "the backing at {x:.1},{y:.1} shows {back:?}");
                let (face, stack) = seen([x, y, lo[2] + 0.08]);
                assert!(stack && seen([x, y, lo[2] + 0.24]) == (face, true), "the face at {x:.1},{y:.1} shows a filament's colour");
                let c = fab_colour::oklab::oklab(face);
                let half = (x > mid) as usize;
                n[half] += 1;
                for k in 0..3 {
                    sum[half][k] += c[k];
                }
            }
        }
        let (blue, red) = (sum[0].map(|v| v / n[0] as f32), sum[1].map(|v| v / n[1] as f32));
        assert!(red[1] > 0.1 && red[2] > 0.0, "the red half shows {red:?}");
        assert!(blue[2] < -0.06 && blue[1].abs() < 0.05, "the blue half shows {blue:?}");
    }

    /// The swatch for a first guess's four filaments prints every pair of them on its first two
    /// layers, and backs them in white and in the darkest (magenta of these) on the others.
    #[test]
    fn a_swatch_prints_every_pair_over_both_backings() {
        let size = swatch_size(4);
        let src = SCRIPT.replace("m.picture({image: \"halves.png\" width: 24mm filaments: [cyan magenta yellow white]})", &format!("m.colour_swatch({{size: [{}mm {}mm] filaments: [cyan magenta yellow white]}})", size[0], size[1]));
        let m = FabModel::with_script("swatch", "swatch.cad.splash", &src);
        let mut e = Evaluator::new(Machine::dual_nozzle_350());
        let ev = m.evaluate(&mut e, None).unwrap_or_else(|f| panic!("{f}")).1;
        assert!(ev.warnings.is_empty(), "{:?}", ev.warnings);
        let part = &ev.plate.parts[0];
        let sp = part.paint.as_ref().expect("the swatch's paint");
        let lo = part.mesh.positions.iter().fold([f64::MAX; 3], |a, p| [a[0].min(p[0]), a[1].min(p[1]), a[2].min(p[2])]);
        let hi = part.mesh.positions.iter().fold([f64::MIN; 3], |a, p| [a[0].max(p[0]), a[1].max(p[1]), a[2].max(p[2])]);
        let mut seen = vec![std::collections::BTreeSet::new(); 4];
        for (layer, names) in seen.iter_mut().enumerate() {
            for i in 0..200 {
                for j in 0..140 {
                    let p = [lo[0] + (hi[0] - lo[0]) * (i as f64 + 0.5) / 200.0, lo[1] + (hi[1] - lo[1]) * (j as f64 + 0.5) / 140.0, lo[2] + 0.16 * (layer as f64 + 0.5)];
                    let slot = (sp.sample_at)(p).slot as usize;
                    names.insert(sp.colors[slot].color.clone().unwrap_or_default());
                }
            }
        }
        let all: std::collections::BTreeSet<String> = ["Cyan", "Magenta", "Yellow", "White"].iter().map(|s| s.to_string()).collect();
        assert_eq!(seen[0], all, "the first layer");
        assert_eq!(seen[1], all, "the second layer");
        for layer in 2..4 {
            assert!(seen[layer].contains("White") && seen[layer].contains("Magenta"), "layer {} backs in {:?}", layer + 1, seen[layer]);
        }
    }

}
