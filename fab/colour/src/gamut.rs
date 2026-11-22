//! The colours a palette prints: its inks side by side mix to any colour in their hull in linear
//! light. A picture is brought into it keeping what the eye holds on to most: its lightness
//! stays as it is through the middle of the range the inks print as greys, and only its darkest
//! and lightest are eased into that range's ends ([`Tone`]); then a colour's chroma, never its
//! hue, gives way where the inks cannot reach it, eased so colours past the edge still differ.
//! The hull's size is measured in OKLab, the space where equal distances look equal, with what an
//! extra filament would add.

use crate::hull::{Hull, P};
use crate::oklab::{self, Lab, Rgb};
use crate::palette::{Filament, FilamentSet, Palette, StackModel};

fn to64(c: Rgb) -> P {
    c.map(|x| x as f64)
}
fn to32(c: P) -> Rgb {
    c.map(|x| x as f32)
}

/// A palette's gamut: the hull of its inks, and down its lightness the printable colour with the
/// least chroma (the nearest it comes to grey), from which a colour's chroma gives way.
#[derive(Clone, Debug)]
pub struct Gamut {
    hull: Hull,
    /// The least-chroma printable colour (OKLab) at each of [`AXIS`] lightnesses from the darkest
    /// ink to the lightest.
    axis: Vec<Lab>,
    /// The lightnesses a picture's black and white print at: the darkest and the lightest
    /// near-grey the inks mix.
    floor: f32,
    top: f32,
}

/// Steps of the least-chroma table.
const AXIS: usize = 128;
/// How far from grey a colour may be (OKLab chroma) and still serve as the picture's black or
/// white.
const NEAR_GREY: f32 = 0.04;
/// The share of the print's lightness range at each end that a picture's darker or lighter
/// lightnesses are eased into; between, lightness is kept.
const L_KNEE: f32 = 0.2;
/// The share of the way to the gamut's edge a colour's chroma is kept whole; past it, the
/// colours out to the screen's edge are eased into the rest.
const C_KNEE: f32 = 0.75;
/// The share of a picture's pixels darker than its black (and lighter than its white): a few
/// specks do not set its range.
const OUTLIERS: f64 = 0.005;

/// How a picture's lightness prints: kept as it is between the knees, its darker and lighter
/// ends eased into the gamut's grey range. A picture narrower than the range is moved into it
/// whole.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tone {
    /// The picture's darkest and lightest (OKLab L, few outliers aside), after the move.
    black: f32,
    white: f32,
    /// Its move.
    shift: f32,
    floor: f32,
    top: f32,
}

impl Tone {
    /// The lightness `l` prints at.
    pub fn l(&self, l: f32) -> f32 {
        let l = l + self.shift;
        let knee = L_KNEE * (self.top - self.floor);
        let (lo, hi) = (self.floor + knee, self.top - knee);
        // eased: a curve leaving the knee at slope one that reaches the range's end at the
        // picture's own end, `room` short of it in `span`
        let ease = |u: f32, span: f32| -> f32 {
            if span <= knee {
                return u;
            }
            let k = knee * span / (span - knee);
            u / (1.0 + u / k)
        };
        let out = if l < lo {
            lo - ease(lo - l, lo - self.black)
        } else if l > hi {
            hi + ease(l - hi, self.white - hi)
        } else {
            l
        };
        out.clamp(self.floor, self.top)
    }
}

impl Gamut {
    pub fn new(colours: &[Rgb]) -> Gamut {
        let l = |c: &Rgb| oklab::oklab(*c)[0];
        let dark = *colours.iter().min_by(|a, b| l(a).total_cmp(&l(b))).expect("a gamut of no colours");
        let light = *colours.iter().max_by(|a, b| l(a).total_cmp(&l(b))).unwrap();
        let hull = Hull::new(&colours.iter().map(|c| to64(*c)).collect::<Vec<_>>());
        let (ld, ll) = (l(&dark), l(&light));
        let axis: Vec<Lab> = (0..AXIS)
            .map(|i| {
                let target = ld + (ll - ld) * i as f32 / (AXIS - 1) as f32;
                // on the line from the darkest ink to the lightest (it lies in the hull), then as far
                // toward grey at the same lightness as the hull holds
                let on_line = oklab::oklab(line_at(dark, light, target));
                let grey: Lab = [on_line[0], 0.0, 0.0];
                let at = |s: f32| -> Lab { std::array::from_fn(|k| on_line[k] + s * (grey[k] - on_line[k])) };
                at(reach(&|s| inside(&hull, at(s)), 0.0, 1.0))
            })
            .collect();
        let grey = |c: &&Lab| c[1].hypot(c[2]) <= NEAR_GREY;
        let floor = axis.iter().find(grey).map_or(ld, |c| c[0]);
        let top = axis.iter().rev().find(grey).map_or(ll, |c| c[0]).max(floor);
        Gamut { hull, axis, floor, top }
    }

    pub fn of(palette: &Palette) -> Gamut {
        Gamut::new(&palette.inks.iter().map(|i| i.colour).collect::<Vec<_>>())
    }

    pub fn contains(&self, c: Rgb) -> bool {
        self.hull.contains(to64(c))
    }

    /// The lightness range a picture prints in: its black and its white, the darkest and the
    /// lightest near-grey the inks mix.
    pub fn range(&self) -> (f32, f32) {
        (self.floor, self.top)
    }

    /// How `picture`'s colours (linear) print their lightness.
    pub fn tone(&self, picture: &[Rgb]) -> Tone {
        let mut ls: Vec<f32> = picture.iter().map(|c| oklab::oklab(c.map(|x| x.clamp(0.0, 1.0)))[0]).collect();
        ls.sort_by(|a, b| a.total_cmp(b));
        let at = |q: f64| ls.get(((ls.len().max(1) - 1) as f64 * q).round() as usize).copied();
        let (black, white) = (at(OUTLIERS).unwrap_or(0.0), at(1.0 - OUTLIERS).unwrap_or(1.0));
        // a picture that fits moves in whole; a wider one stays where it is and is eased
        let shift = if white - black <= self.top - self.floor { (self.floor - black).max(0.0) + (self.top - white).min(0.0) } else { 0.0 };
        Tone { black: black + shift, white: white + shift, shift, floor: self.floor, top: self.top }
    }

    /// A tone for colours with no picture round them: black to white.
    pub fn full(&self) -> Tone {
        self.tone(&[[0.0; 3], [1.0; 3]])
    }

    /// The least-chroma printable colour at lightness `l`.
    fn anchor(&self, l: f32) -> Lab {
        let (lo, hi) = (self.axis[0][0], self.axis[AXIS - 1][0]);
        let t = ((l - lo) / (hi - lo).max(1e-9)).clamp(0.0, 1.0) * (AXIS - 1) as f32;
        let (i, f) = ((t.floor() as usize).min(AXIS - 2), t - t.floor().min((AXIS - 2) as f32));
        std::array::from_fn(|k| self.axis[i][k] + f * (self.axis[i + 1][k] - self.axis[i][k]))
    }

    /// The printable colour a wanted one (linear) aims at: its lightness by `tone`, then from the
    /// grey-most printable colour at that lightness out toward the wanted colour (so its hue
    /// stays), all the way while well inside the gamut; nearer the edge the stretch out to the
    /// screen's edge along that line is eased into the rest of the way to the gamut's.
    pub fn map(&self, c: Rgb, tone: &Tone) -> Rgb {
        let lab = oklab::oklab(c.map(|x| x.clamp(0.0, 1.0)));
        let want: Lab = [tone.l(lab[0]), lab[1], lab[2]];
        let anchor = self.anchor(want[0]);
        let at = |s: f32| -> Lab { std::array::from_fn(|i| anchor[i] + s * (want[i] - anchor[i])) };
        let on_screen = |s: f32| oklab::linear(at(s)).iter().all(|x| (-1e-4..=1.0 + 1e-4).contains(x));
        // how far out the screen's colours go along the line (the wanted one at least), and the gamut
        let screen = if on_screen(1.0) { reach(&on_screen, 1.0, 8.0) } else { 1.0 };
        let edge = reach(&|s| inside(&self.hull, at(s)), 0.0, screen);
        let knee = C_KNEE * edge;
        let s = if screen <= edge || 1.0 <= knee { 1.0 } else { knee + (1.0 - knee) * (edge - knee) / (screen - knee) };
        to32(self.hull.onto_span(to64(oklab::linear(at(s)))))
    }

    /// The hull's volume in OKLab, counted in cubes `step` on a side.
    pub fn volume(&self, step: f32) -> f64 {
        if self.hull.dimension() < 3 {
            return 0.0;
        }
        let n = |lo: f32, hi: f32| ((hi - lo) / step).ceil() as i32;
        let mut count = 0u64;
        for i in 0..n(0.0, 1.0) {
            for j in 0..n(-0.4, 0.4) {
                for k in 0..n(-0.4, 0.4) {
                    let lab = [(i as f32 + 0.5) * step, -0.4 + (j as f32 + 0.5) * step, -0.4 + (k as f32 + 0.5) * step];
                    let c = oklab::linear(lab);
                    // a mix of real colours is a real colour
                    if c.iter().all(|x| (-1e-4..=1.0 + 1e-4).contains(x)) && self.contains(c) {
                        count += 1;
                    }
                }
            }
        }
        count as f64 * (step as f64).powi(3)
    }
}

/// The colour on the line from `dark` to `light` (linear) whose lightness is `l`.
fn line_at(dark: Rgb, light: Rgb, l: f32) -> Rgb {
    let mix = |t: f32| -> Rgb { std::array::from_fn(|k| dark[k] + t * (light[k] - dark[k])) };
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..30 {
        let mid = 0.5 * (lo + hi);
        if oklab::oklab(mix(mid))[0] < l {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    mix(0.5 * (lo + hi))
}

/// Whether the inks mix a colour (OKLab).
fn inside(hull: &Hull, c: Lab) -> bool {
    hull.contains(hull.onto_span(to64(oklab::linear(c))))
}

/// How far along a line from `from`, where `holds` holds, toward `to` it still holds.
fn reach(holds: &dyn Fn(f32) -> bool, from: f32, to: f32) -> f32 {
    if holds(to) {
        return to;
    }
    let (mut lo, mut hi) = (from, to);
    for _ in 0..16 {
        let mid = 0.5 * (lo + hi);
        if holds(mid) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
}

/// The gamut of a set and what each candidate filament would add to it.
#[derive(Clone, Debug)]
pub struct Report {
    /// OKLab volume of the set's palette (its stacks counted).
    pub volume: f64,
    /// The same of every colour a screen shows (the sRGB cube), for scale.
    pub srgb: f64,
    /// Each candidate's name and the volume the set gains with it loaded too.
    pub adds: Vec<(String, f64)>,
}

/// Typical spool colours (sRGB) by letter and name: first guesses for sets named before they
/// are loaded.
const SPOOLS: [(char, &str, [u8; 3]); 10] = [
    ('C', "Cyan", [0, 163, 224]),
    ('M', "Magenta", [214, 0, 126]),
    ('Y', "Yellow", [255, 212, 0]),
    ('K', "Black", [26, 26, 26]),
    ('W', "White", [242, 242, 240]),
    ('R', "Red", [200, 16, 46]),
    ('O', "Orange", [255, 106, 19]),
    ('G', "Green", [0, 150, 57]),
    ('B', "Blue", [0, 51, 160]),
    ('V', "Violet", [95, 37, 159]),
];

/// A typical spool's first guess by its colour's name or letter (C M Y K W R O G B V).
pub fn spool(name: &str) -> Option<Filament> {
    let letter = |c: char| name.len() == 1 && name.eq_ignore_ascii_case(&c.to_string());
    SPOOLS.iter().find(|(c, n, _)| letter(*c) || n.eq_ignore_ascii_case(name)).map(|(_, n, rgb)| Filament::guess(n, *rgb))
}

/// Filaments worth weighing as a set's extra slots: first guesses from typical spool colours.
pub fn candidates() -> Vec<Filament> {
    ["Red", "Orange", "Green", "Blue", "Violet", "Black", "White"].into_iter().filter_map(spool).collect()
}

/// `set`'s gamut in `model` and what each of `extra` would add (a backing filament keeps its
/// place: the candidates come after the set's own).
pub fn report(set: &FilamentSet, model: &StackModel, extra: &[Filament], step: f32) -> Report {
    let volume = Gamut::of(&Palette::new(set, model)).volume(step);
    let cube: Vec<Rgb> = (0..8).map(|i| [(i & 1) as f32, ((i >> 1) & 1) as f32, ((i >> 2) & 1) as f32]).collect();
    let srgb = Gamut::new(&cube).volume(step);
    let adds = extra
        .iter()
        .map(|f| {
            let mut more = set.clone();
            more.filaments.push(f.clone());
            (f.name.clone(), Gamut::of(&Palette::new(&more, model)).volume(step) - volume)
        })
        .collect();
    Report { volume, srgb, adds }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palette::Backing;

    fn set(names: &str) -> FilamentSet {
        FilamentSet::new(names, names.chars().map(|c| spool(&c.to_string()).unwrap()).collect())
    }

    fn model(s: &FilamentSet) -> StackModel {
        let backing = match s.white() {
            Some(index) => Backing::Filament { index, mm: 0.32 },
            None => Backing::Repeat { levels: vec![0, 1], layer: 0.16 },
        };
        StackModel { levels: vec![0.16, 0.16], backing, background: [0.8; 3] }
    }

    /// A colour in the gamut stays; one outside comes in at its own lightness with its hue, only
    /// its chroma given up; through the middle of the range a lightness prints as it is, and a
    /// picture narrower than the range moves into it whole, its contrast kept; white maps to the
    /// lightest near-grey (of CMYK too, which has no white filament: not its yellow); a set of
    /// greys maps every colour to a grey; and the stacks add volume: two levels of four
    /// filaments hold more than the four alone, and every candidate adds nothing negative.
    #[test]
    fn colours_come_into_the_gamut_and_stacks_add_volume() {
        let cmyw = set("CMYW");
        let g = Gamut::of(&Palette::new(&cmyw, &model(&cmyw)));
        let full = g.full();
        let inside = g.map([0.3, 0.3, 0.3], &full);
        assert!(g.contains(inside));
        let (floor, top) = g.range();
        let knee = L_KNEE * (top - floor);
        for l in [floor + knee + 0.01, 0.5 * (floor + top), top - knee - 0.01] {
            assert!((full.l(l) - l).abs() < 1e-6, "{l} prints as it is");
        }
        let src = oklab::oklab([0.8, 0.02, 0.01]);
        let red = oklab::oklab(g.map([0.8, 0.02, 0.01], &full));
        let hue = |c: Lab| c[2].atan2(c[1]);
        assert!(g.contains(oklab::linear(red)) && (red[0] - full.l(src[0])).abs() < 0.01 && (hue(red) - hue(src)).abs() < 0.1 && red[1] > 0.03, "red keeps its lightness and hue: {red:?} of {src:?}");
        let white = oklab::oklab(g.map([1.0; 3], &full));
        assert!((white[0] - top).abs() < 0.01 && white[1].hypot(white[2]) < NEAR_GREY + 0.01, "{white:?}");
        let black = oklab::oklab(g.map([0.0; 3], &full));
        assert!(black[1].hypot(black[2]) <= NEAR_GREY + 0.01, "black prints near grey: {black:?}");
        // a narrow picture: its lightnesses keep their differences
        let narrow: Vec<Rgb> = [0.2f32, 0.3, 0.4].iter().map(|l| oklab::linear([*l, 0.0, 0.0])).collect();
        let t = g.tone(&narrow);
        assert!(((t.l(0.4) - t.l(0.2)) - 0.2).abs() < 1e-4 && t.l(0.2) >= floor - 1e-6, "{} {}", t.l(0.2), t.l(0.4));
        let cmyk = set("CMYK");
        let gk = Gamut::of(&Palette::new(&cmyk, &model(&cmyk)));
        let w = oklab::oklab(gk.map([1.0; 3], &gk.full()));
        assert!(w[1].hypot(w[2]) < NEAR_GREY + 0.01 && w[0] > gk.range().0 + 0.2, "CMYK's white is its lightest grey: {w:?}");
        let greys = FilamentSet::new("KW", vec![spool("Black").unwrap(), spool("White").unwrap()]);
        let gg = Gamut::of(&Palette::new(&greys, &model(&greys)));
        let grey = oklab::oklab(gg.map([0.9, 0.1, 0.1], &gg.full()));
        assert!(grey[1].abs() < 0.02 && grey[2].abs() < 0.02, "{grey:?}");
        let step = 0.025;
        let flat = Gamut::new(&cmyw.filaments.iter().map(|f| f.colour).collect::<Vec<_>>()).volume(step);
        let r = report(&cmyw, &model(&cmyw), &candidates(), step);
        assert!(r.volume > flat * 1.05, "stacks add volume: {} over {flat}", r.volume);
        assert!(r.volume < r.srgb && r.srgb > 0.0);
        let k = r.adds.iter().find(|(n, _)| n == "Black").unwrap().1;
        assert!(k > 0.0 && r.adds.iter().all(|(_, a)| *a >= -1e-9), "{:?}", r.adds);
    }
}
