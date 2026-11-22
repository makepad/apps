//! Character specs from Splash values: `game.character_new`, `game.fps_arms_new` and
//! `m.character` read the script's spec object (or a preset name) straight into the typed
//! [`CharacterSpec`] through the model language's value reader. The named presets are
//! Splash too (`character_presets.splash`, read once per thread).
use crate::lang::{Reader, V};
use makepad_model::character::{CharacterSpec, FpsArms, SpecTree};
use makepad_script::*;

impl SpecTree for V {
    fn get(&self, key: &str) -> Option<Self> {
        match self { V::Obj(o) => o.get(key).filter(|v| !matches!(v, V::Nil)).cloned(), _ => None }
    }
    fn keys(&self) -> Option<Vec<String>> {
        match self { V::Obj(o) if o.items.is_empty() => Some(o.fields.iter().map(|(k, _)| k.clone()).collect()), _ => None }
    }
    fn items(&self) -> Option<Vec<Self>> {
        match self { V::Arr(a) => Some(a.clone()), V::Obj(o) if !o.items.is_empty() => Some(o.items.clone()), _ => None }
    }
    fn num(&self) -> Option<f64> { match self { V::Num(n) => Some(*n), _ => None } }
    fn text(&self) -> Option<String> { match self { V::Str(s) | V::Id(s) => Some(s.clone()), _ => None } }
    fn boolean(&self) -> Option<bool> { match self { V::Bool(b) => Some(*b), _ => None } }
    fn color(&self) -> Option<[f64; 3]> {
        match self {
            V::Color(c) => {
                // The VM's colour channels are f32 (byte / 255), linearised in f64.
                let lin = |byte: u32| { let c = ((byte & 255) as f32 / 255.0) as f64; if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) } };
                Some([lin(c >> 24), lin(c >> 16), lin(c >> 8)])
            }
            V::Str(s) => makepad_model::character::hex_color(s),
            _ => { let a = self.items()?; if a.len() < 3 { return None; } Some([a[0].num()?, a[1].num()?, a[2].num()?]) }
        }
    }
}

const PRESETS_SOURCE: &str = include_str!("character_presets.splash");

thread_local! {
    static PRESETS: Vec<(String, V)> = load_presets();
}

/// The presets file evaluated in a VM of its own: `[name spec]` pairs.
fn load_presets() -> Vec<(String, V)> {
    let mut host = Box::new(ScriptVmHost::new(0i32, ()));
    let mut vm = ScriptVm { host: host.host_context(), bx: Box::new(ScriptVmBase::new()) };
    vm.capture_errors();
    let list = vm.eval(ScriptMod::new("", "", "character_presets.splash", 1, 0, PRESETS_SOURCE, &[]));
    let errors = vm.take_errors();
    assert!(errors.is_empty(), "character_presets.splash: {errors:?}");
    let V::Arr(pairs) = Reader::new(&mut vm, Vec::new()).read(list) else { panic!("character_presets.splash is a list") };
    pairs.into_iter().map(|pair| match pair {
        V::Arr(p) if p.len() == 2 => match (&p[0], &p[1]) { (V::Str(name), spec) => (name.clone(), spec.clone()), _ => panic!("a preset is [name spec]") },
        _ => panic!("a preset is [name spec]"),
    }).collect()
}

/// The preset names, in their file's order.
pub fn preset_names() -> Vec<String> { PRESETS.with(|p| p.iter().map(|(n, _)| n.clone()).collect()) }

fn preset(name: &str) -> Option<V> { PRESETS.with(|p| p.iter().find(|(n, _)| n == name).map(|(_, v)| v.clone())) }

/// A preset by name.
pub fn preset_spec(name: &str) -> Result<CharacterSpec, String> {
    CharacterSpec::read(&V::Str(name.into()), &preset)
}

/// A preset name or a spec object.
pub fn read(vm: &mut ScriptVm, v: ScriptValue) -> Result<CharacterSpec, String> {
    let value = Reader::new(vm, Vec::new()).read(v);
    if value.text().is_none() && value.keys().is_none() {
        return Err("a character spec is a preset name or an object ({preset, body, face, hair, outfit, colors, ...})".into());
    }
    CharacterSpec::read(&value, &preset)
}

/// First-person arms: a spec plus its hold (`{preset, hold, support, ...}` or a preset name).
pub fn read_arms(vm: &mut ScriptVm, v: ScriptValue) -> Result<(CharacterSpec, FpsArms), String> {
    let value = Reader::new(vm, Vec::new()).read(v);
    if value.text().is_none() && value.keys().is_none() {
        return Err("pass {preset, hold, support} or a preset name".into());
    }
    let spec = CharacterSpec::read(&value, &preset)?;
    Ok((spec, FpsArms::read(&value)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_model::ProgramOp;

    fn build(ops: &[ProgramOp]) -> makepad_model::BuiltProgram {
        let limits = makepad_model::Limits::default();
        makepad_model::build_program(ops, limits, &mut |_| Err("no imports".into()), None).unwrap_or_else(|e| panic!("{}", e.message))
    }

    /// Every preset reads and builds (presets on presets included), and first-person arms stay
    /// small. Build time is cold-cache load time, so it is printed (run with --nocapture).
    #[test]
    fn presets_build_and_arms_stay_small() {
        let names = preset_names();
        assert!(names.len() >= 17 && names.iter().any(|n| n == "halcyon_medic"), "{names:?}");
        for name in &names {
            let spec = preset_spec(name).unwrap();
            assert!(spec.warnings.is_empty(), "{name}: {:?}", spec.warnings);
            let t = std::time::Instant::now();
            let built = build(&[ProgramOp::Character(Box::new(spec))]);
            eprintln!("character {name}: {} ms, {} B", t.elapsed().as_millis(), built.compiled.glb.len());
        }
        // A preset on a preset takes its base's fields it does not set.
        let (medic, warden) = (preset_spec("halcyon_medic").unwrap(), preset_spec("halcyon_warden").unwrap());
        assert_eq!((medic.heads, medic.stylize, medic.primary), (warden.heads, warden.stylize, warden.primary));
        assert_ne!(medic.height, warden.height);
        for (preset, hold) in [("halcyon_warden", makepad_model::character::Hold::Rifle), ("kestrel_striker", makepad_model::character::Hold::Pistol)] {
            let arms = FpsArms { hold, support: None, grip_radius: 0.015, grip_front: 0.021, support_radius: 0.022 };
            let bytes = build(&[ProgramOp::FpsArms(Box::new(preset_spec(preset).unwrap()), arms)]).compiled.glb.len();
            assert!(bytes < 1_500_000, "arms {preset}: {bytes} B");
        }
    }
}
