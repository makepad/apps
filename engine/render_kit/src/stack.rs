//! Effect stacks: an ordered list of effects, each with every parameter
//! keyable, resolved at `t` into plain values for the GPU.
//!
//! A host builds an [`Effect`] from what its author wrote, a name and named
//! values (`Effect::build("glow", [("radius", 24.0.into())])`), and gets
//! teaching diagnostics back: unknown effects and parameters with the
//! closest real name, wrong kinds with the syntax expected, and values
//! out of range (clamped, with a warning).

use crate::anim::{Anim, Value};
use crate::catalog::{self, closest, coerce_anim, EffectInfo, EffectKind, ParamKind, EFFECTS};

/// One effect with its parameters, all keyable, in catalog order.
#[derive(Clone, Debug, PartialEq)]
pub struct Effect {
    pub kind: EffectKind,
    pub params: Vec<Anim>,
    /// Off: the stack skips it (an author's bypass).
    pub enabled: bool,
}

/// What building an effect said.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Diagnostics {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

impl Effect {
    /// The effect with every parameter at its default.
    pub fn new(kind: EffectKind) -> Self {
        let info = catalog::info(kind);
        Self { kind, params: info.params.iter().map(|p| Anim::Const(p.kind.default_value())).collect(), enabled: true }
    }

    pub fn info(&self) -> &'static EffectInfo {
        catalog::info(self.kind)
    }

    /// An effect by name with named parameters. Errors name the fix:
    /// the closest effect or parameter, or the syntax a parameter takes.
    pub fn build<S: AsRef<str>>(name: &str, params: impl IntoIterator<Item = (S, Anim)>) -> Result<(Self, Vec<String>), Diagnostics> {
        let mut diagnostics = Diagnostics::default();
        let Some(info) = catalog::by_name(name) else {
            let hint = closest(name.trim_start_matches('@'), EFFECTS.iter().map(|e| e.name))
                .map(|c| format!(" Did you mean `{c}`?"))
                .unwrap_or_default();
            diagnostics.errors.push(format!(
                "there is no effect `{name}`.{hint} Effects: {}",
                EFFECTS.iter().map(|e| e.name).collect::<Vec<_>>().join(", ")
            ));
            return Err(diagnostics);
        };
        let mut effect = Effect::new(info.kind);
        for (param, anim) in params {
            let param = param.as_ref();
            if param == "enabled" {
                if let Anim::Const(Value::Bool(on)) = anim {
                    effect.enabled = on;
                    continue;
                }
            }
            match info.param(param) {
                Some((index, p)) => match coerce_anim(p, anim, &mut diagnostics.warnings) {
                    Ok(anim) => effect.params[index] = anim,
                    Err(error) => diagnostics.errors.push(format!("{}: {error}", info.name)),
                },
                None => {
                    let hint = closest(param, info.params.iter().map(|p| p.name)).map(|c| format!(" Did you mean `{c}`?")).unwrap_or_default();
                    diagnostics.errors.push(format!(
                        "{} has no parameter `{param}`.{hint} Its parameters: {}",
                        info.name,
                        info.params.iter().map(|p| p.name).collect::<Vec<_>>().join(", ")
                    ));
                }
            }
        }
        if diagnostics.errors.is_empty() {
            Ok((effect, diagnostics.warnings))
        } else {
            Err(diagnostics)
        }
    }

    /// Set one parameter (checked like [`Effect::build`]).
    pub fn set(&mut self, name: &str, anim: impl Into<Anim>) -> Result<Vec<String>, String> {
        let info = self.info();
        let (index, p) = info.param(name).ok_or_else(|| format!("{} has no parameter `{name}`", info.name))?;
        let mut warnings = Vec::new();
        self.params[index] = coerce_anim(p, anim.into(), &mut warnings)?;
        Ok(warnings)
    }

    /// Builder form of [`Effect::set`] for code that knows its names are
    /// right (tests, fixtures); panics otherwise.
    pub fn with(mut self, name: &str, anim: impl Into<Anim>) -> Self {
        if let Err(error) = self.set(name, anim) {
            panic!("{error}");
        }
        self
    }

    pub fn is_temporal(&self) -> bool {
        self.info().temporal
    }

    /// Every parameter's value at `t`, numbers clamped to their range.
    pub fn at(&self, t: f64) -> ResolvedEffect {
        let info = self.info();
        let values = self
            .params
            .iter()
            .zip(info.params)
            .map(|(anim, p)| clamp_value(&p.kind, anim.at(t)))
            .collect();
        ResolvedEffect { kind: self.kind, values, enabled: self.enabled }
    }

    /// True when no parameter changes with time.
    pub fn is_static(&self) -> bool {
        self.params.iter().all(Anim::is_static)
    }
}

fn clamp_value(kind: &ParamKind, value: Value) -> Value {
    match (kind, value) {
        (ParamKind::Num { min, max, default }, Value::Num(v)) => Value::Num(if v.is_finite() { v.clamp(*min, *max) } else { *default }),
        (_, value) => value,
    }
}

/// One effect's values at one time.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedEffect {
    pub kind: EffectKind,
    pub values: Vec<Value>,
    pub enabled: bool,
}

impl ResolvedEffect {
    /// An effect at its defaults, for hosts that set values per frame.
    pub fn new(kind: EffectKind) -> Self {
        Effect::new(kind).at(0.0)
    }

    /// Builder: set a value by name (panics on an unknown name; hosts use
    /// [`Effect::build`] for authored input).
    pub fn with(mut self, name: &str, value: Value) -> Self {
        let info = catalog::info(self.kind);
        let (index, p) = info.param(name).unwrap_or_else(|| panic!("{} has no parameter {name}", info.name));
        let value = catalog::coerce(p, value).unwrap_or_else(|e| panic!("{e}"));
        self.values[index] = clamp_value(&p.kind, value);
        self
    }

    fn value(&self, name: &str) -> &Value {
        let info = catalog::info(self.kind);
        let (index, _) = info.param(name).unwrap_or_else(|| panic!("{} has no parameter {name}", info.name));
        &self.values[index]
    }

    pub fn num(&self, name: &str) -> f64 {
        match self.value(name) {
            Value::Num(v) => *v,
            Value::Bool(v) => f64::from(u8::from(*v)),
            _ => 0.0,
        }
    }

    pub fn f(&self, name: &str) -> f32 {
        self.num(name) as f32
    }

    pub fn vec2(&self, name: &str) -> [f64; 2] {
        match self.value(name) {
            Value::Vec2(v) => *v,
            Value::Num(v) => [*v, *v],
            _ => [0.0, 0.0],
        }
    }

    pub fn vec3(&self, name: &str) -> [f64; 3] {
        match self.value(name) {
            Value::Vec3(v) => *v,
            _ => [0.0, 0.0, 0.0],
        }
    }

    pub fn color(&self, name: &str) -> [f64; 4] {
        match self.value(name) {
            Value::Color(v) => *v,
            _ => [0.0, 0.0, 0.0, 1.0],
        }
    }

    pub fn flag(&self, name: &str) -> bool {
        matches!(self.value(name), Value::Bool(true))
    }

    pub fn choice(&self, name: &str) -> &str {
        match self.value(name) {
            Value::Choice(v) | Value::Text(v) | Value::Layer(v) => v,
            _ => "",
        }
    }

    /// Index of the current choice in the parameter's list (0 if unset).
    pub fn choice_index(&self, name: &str) -> usize {
        let info = catalog::info(self.kind);
        match info.param(name) {
            Some((_, p)) => match p.kind {
                ParamKind::Choice { choices, .. } => choices.iter().position(|c| *c == self.choice(name)).unwrap_or(0),
                _ => 0,
            },
            None => 0,
        }
    }

    pub fn points(&self, name: &str) -> &[[f64; 2]] {
        match self.value(name) {
            Value::Points(v) => v,
            _ => &[],
        }
    }

    pub fn stops(&self, name: &str) -> &[(f64, [f64; 4])] {
        match self.value(name) {
            Value::Stops(v) => v,
            _ => &[],
        }
    }

    /// The layers this effect reads from the host (matte, map, emitter).
    pub fn layer_refs(&self) -> Vec<&str> {
        let info = catalog::info(self.kind);
        info.params
            .iter()
            .zip(&self.values)
            .filter_map(|(p, v)| match (p.kind, v) {
                (ParamKind::Layer, Value::Layer(name)) if !name.is_empty() => Some(name.as_str()),
                _ => None,
            })
            .collect()
    }
}

/// An ordered list of effects: the first applies to the layer, each next
/// to the result before it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EffectStack {
    pub effects: Vec<Effect>,
}

impl EffectStack {
    pub fn new(effects: Vec<Effect>) -> Self {
        Self { effects }
    }

    pub fn push(&mut self, effect: Effect) {
        self.effects.push(effect);
    }

    pub fn is_empty(&self) -> bool {
        self.effects.iter().all(|e| !e.enabled)
    }

    /// Every enabled effect's values at `t`.
    pub fn at(&self, t: f64) -> Vec<ResolvedEffect> {
        self.effects.iter().filter(|e| e.enabled).map(|e| e.at(t)).collect()
    }

    /// Needs rendering at other times (motion blur, echo).
    pub fn is_temporal(&self) -> bool {
        self.effects.iter().any(|e| e.enabled && e.is_temporal())
    }

    /// No parameter changes with time and nothing reads other times.
    pub fn is_static(&self) -> bool {
        self.effects.iter().all(|e| !e.enabled || (e.is_static() && !e.is_temporal()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anim::{Ease, Key};

    #[test]
    fn build_resolves_keys_and_clamps() {
        let keys = Anim::keys(vec![
            Key { t: 0.0, v: Value::Num(0.0), ease: Ease::Linear },
            Key { t: 1.0, v: Value::Num(40.0), ease: Ease::Linear },
        ])
        .unwrap();
        let (effect, warnings) = Effect::build("gaussian_blur", [("radius", keys), ("direction", Anim::Const(Value::Choice("horizontal".into())))]).unwrap();
        assert!(warnings.is_empty());
        assert_eq!(effect.at(0.5).num("radius"), 20.0);
        assert_eq!(effect.at(0.5).choice("direction"), "horizontal");
        assert_eq!(effect.at(0.5).choice_index("direction"), 1);
        assert!(!effect.is_static());
        let (effect, warnings) = Effect::build("glow", [("intensity", Anim::from(99.0))]).unwrap();
        assert_eq!(warnings.len(), 1);
        assert_eq!(effect.at(0.0).num("intensity"), 20.0);
    }

    #[test]
    fn build_errors_teach() {
        let err = Effect::build("gausian_blur", Vec::<(&str, Anim)>::new()).unwrap_err();
        assert!(err.errors[0].contains("Did you mean `gaussian_blur`"), "{err:?}");
        let err = Effect::build("drop_shadow", [("distanse", Anim::from(4.0))]).unwrap_err();
        assert!(err.errors[0].contains("Did you mean `distance`"), "{err:?}");
        assert!(err.errors[0].contains("Its parameters: color, opacity"), "{err:?}");
    }

    #[test]
    fn stacks_know_what_they_need() {
        let mut stack = EffectStack::default();
        stack.push(Effect::new(EffectKind::Glow));
        assert!(stack.is_static() && !stack.is_temporal());
        stack.push(Effect::new(EffectKind::MotionBlur));
        assert!(stack.is_temporal() && !stack.is_static());
        let matte = ResolvedEffect::new(EffectKind::Matte).with("layer", Value::Layer("@logo".into()));
        assert_eq!(matte.layer_refs(), vec!["logo"]);
    }
}
