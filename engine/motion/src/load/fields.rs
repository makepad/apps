//! Typed field reads: each kind of [`Field`] reads its value with the
//! checks its kind carries (a number or an animation, a colour, a moment,
//! a length, an `@name` of its choices, …). `name` is the field's name, for
//! the diagnostic's place.

use super::{At, Reader};
use crate::layers::{kind, Field, Kind};
use crate::model::*;
use crate::obj::Look;

/// An `@name` kind: its choices and what each reads as.
/// Public only because `Field<K>` (pub) is implemented for it; the module keeps it crate-internal.
pub trait Choice: Kind {
    type T: Copy + PartialEq;
    fn names() -> &'static [&'static str];
    fn parse(id: &str) -> Option<Self::T>;
}

macro_rules! choice {
    ($kind:ident, $t:ty, $names:ident, { $($id:pat => $v:expr),* $(,)? }) => {
        impl Choice for kind::$kind {
            type T = $t;
            fn names() -> &'static [&'static str] {
                crate::layers::$names
            }
            fn parse(id: &str) -> Option<$t> {
                Some(match id { $($id => $v,)* _ => return None })
            }
        }
    };
}

choice!(Cap, Cap, CAP, { "round" => Cap::Round, "butt" => Cap::Butt, "square" => Cap::Square });
choice!(Join, Join, JOIN, { "round" => Join::Round, "miter" => Join::Miter, "bevel" => Join::Bevel });
choice!(FeatherSide, FeatherSide, FEATHER_SIDE, { "both" => FeatherSide::Both, "inner" => FeatherSide::Inner, "outer" => FeatherSide::Outer });
choice!(Blend, Blend, BLEND, { "normal" => Blend::Normal, "add" => Blend::Add, "screen" => Blend::Screen, "multiply" => Blend::Multiply });
choice!(Fit, Fit, FIT, { "fill" => Fit::Fill, "fit" => Fit::Fit, "stretch" => Fit::Stretch, "none" => Fit::None });
choice!(Align, Align, ALIGN, { "left" => Align::Left, "center" | "centre" => Align::Center, "right" => Align::Right });
choice!(VAlign, VAlign, VALIGN, { "top" => VAlign::Top, "center" | "centre" | "middle" => VAlign::Center, "bottom" => VAlign::Bottom });
choice!(TextBy, TextBy, TEXT_BY, { "whole" => TextBy::Whole, "word" => TextBy::Word, "letter" | "char" => TextBy::Letter, "line" => TextBy::Line, "syllable" => TextBy::Syllable });
choice!(MaskMode, MaskMode, MASK_MODE, { "alpha" => MaskMode::Alpha, "inverse" | "alpha_inverse" => MaskMode::Inverse, "luma" => MaskMode::Luma, "luma_inverse" => MaskMode::LumaInverse });
choice!(VoiceShow, VoiceShow, VOICE_SHOW, { "all" => VoiceShow::All, "spoken" => VoiceShow::Spoken, "active" => VoiceShow::Active, "line" => VoiceShow::Line });

impl<K: Kind> Field<K> {
    /// The value as written (`None`: not written, or `nil`).
    pub(super) fn look(&self, c: &Reader) -> Option<Look> {
        if self.0.is_nil() {
            return None;
        }
        Some(c.look(self.0))
    }

    /// Whether the document wrote this field.
    pub(super) fn is_set(&self) -> bool {
        !self.0.is_nil()
    }

    /// Whether the field is `true` (anything else, written or not, is not).
    pub(super) fn is_true(&self, c: &Reader) -> bool {
        matches!(self.look(c), Some(Look::Bool(true)))
    }

    /// The `@name` or string written, as is (`None` otherwise).
    pub(super) fn ident(&self, c: &Reader) -> Option<String> {
        match self.look(c) {
            Some(Look::Id(s)) | Some(Look::Str(s)) => Some(s),
            _ => None,
        }
    }
}

impl<K: Choice> Field<K> {
    /// `@name`, one of the kind's choices (`default` when not written).
    pub(super) fn pick(&self, c: &mut Reader, at: &At, name: &str, default: K::T) -> K::T {
        at.note(name, || K::names().iter().find(|n| K::parse(n) == Some(default)).map(|n| format!("@{n}")), None);
        let value = self.look(c);
        c.id_enum_of(at, name, value, K::parse, K::names(), default)
    }
}

impl Field<kind::Num> {
    /// A number or an animation of one.
    pub(super) fn prop(&self, c: &mut Reader, at: &At, name: &str, default: f64) -> Prop<1> {
        let value = self.look(c);
        c.prop_of::<1>(at, name, value, [default])
    }

    pub(super) fn opt(&self, c: &mut Reader, at: &At, name: &str) -> Option<Prop<1>> {
        let value = self.look(c)?;
        c.prop_value::<1>(at, name, &value)
    }

    /// A constant number (`None` when not written or wrong).
    pub(super) fn konst_opt(&self, c: &mut Reader, at: &At, name: &str) -> Option<f64> {
        let value = self.look(c)?;
        c.num_const(at, name, &value)
    }

    /// A constant number (`default` when not written or wrong).
    pub(super) fn konst(&self, c: &mut Reader, at: &At, name: &str, default: f64) -> f64 {
        at.note(name, || Some(super::splash_default([default])), None);
        match self.look(c) {
            None => default,
            Some(value) => c.num_const(at, name, &value).unwrap_or(default),
        }
    }
}

impl Field<kind::Vec2> {
    /// `vec2(x, y)` (a number is both) or an animation of one.
    pub(super) fn prop(&self, c: &mut Reader, at: &At, name: &str, default: [f64; 2]) -> Prop<2> {
        let value = self.look(c);
        c.prop_of::<2>(at, name, value, default)
    }

    pub(super) fn opt(&self, c: &mut Reader, at: &At, name: &str) -> Option<Prop<2>> {
        let value = self.look(c)?;
        c.prop_value::<2>(at, name, &value)
    }

    pub(super) fn konst(&self, c: &mut Reader, at: &At, name: &str, default: [f64; 2]) -> [f64; 2] {
        at.note(name, || Some(super::splash_default(default)), None);
        match self.look(c) {
            None => default,
            Some(value) => c.vec2_const(at, name, &value).unwrap_or(default),
        }
    }

    /// A constant `vec2` (`None` when not written or wrong).
    pub(super) fn konst_opt(&self, c: &mut Reader, at: &At, name: &str) -> Option<[f64; 2]> {
        let value = self.look(c)?;
        c.vec2_const(at, name, &value)
    }

    /// A layer's `size` when written (a negative constant is an error).
    pub(super) fn size(&self, c: &mut Reader, at: &At, name: &str) -> Option<Prop<2>> {
        let value = self.look(c);
        c.opt_size_of(at, name, value)
    }
}

impl Field<kind::Color> {
    /// A colour or an animation of one.
    pub(super) fn prop(&self, c: &mut Reader, at: &At, name: &str, default: [f64; 4]) -> Prop<4> {
        let value = self.look(c);
        c.prop_of::<4>(at, name, value, default)
    }

    pub(super) fn opt(&self, c: &mut Reader, at: &At, name: &str) -> Option<Prop<4>> {
        let value = self.look(c)?;
        c.prop_value::<4>(at, name, &value)
    }

    pub(super) fn konst(&self, c: &mut Reader, at: &At, name: &str) -> Option<Color> {
        let value = self.look(c)?;
        c.color_const(at, name, &value)
    }
}

impl Field<kind::Any> {
    /// A width in document units, or `@safe`.
    pub(super) fn width(&self, c: &mut Reader, at: &At, name: &str) -> Option<f64> {
        let value = self.look(c);
        c.width_of(at, name, value)
    }
}

impl Field<kind::Font> {
    /// The face written, a `Font{family weight italic axes}` (`None`: not
    /// written, or an error said what is wrong with it).
    pub(super) fn face(&self, c: &mut Reader, at: &At, name: &str) -> Option<crate::font_spec::FontSpec> {
        if !self.is_set() {
            return None;
        }
        match crate::font_spec::font_spec(c.vm, self.0) {
            Ok(face) => Some(face),
            Err(error) => {
                c.err(at, name, error, None);
                None
            }
        }
    }

    /// The face written, or `family` at 400 when not written (or wrong).
    pub(super) fn face_or(&self, c: &mut Reader, at: &At, name: &str, family: &'static str) -> crate::font_spec::FontSpec {
        at.note(name, || Some(format!("\"{family}\"")), None);
        self.face(c, at, name).unwrap_or(crate::font_spec::FontSpec { family, weight: 400.0, italic: false, axes: Vec::new() })
    }
}

impl Field<kind::Words> {
    pub(super) fn text(&self, c: &mut Reader, at: &At, name: &str) -> Option<String> {
        let value = self.look(c)?;
        c.string(at, name, &value)
    }
}

impl Field<kind::Prose> {
    pub(super) fn text(&self, c: &mut Reader, at: &At, name: &str) -> Option<String> {
        let value = self.look(c)?;
        c.string(at, name, &value)
    }
}

impl Field<kind::Bool> {
    /// `true` or `false` (`default` when not written).
    pub(super) fn flag(&self, c: &mut Reader, at: &At, name: &str, default: bool) -> bool {
        at.note(name, || Some(default.to_string()), None);
        match self.look(c) {
            None => default,
            Some(Look::Bool(b)) => b,
            Some(other) => {
                c.err(at, name, format!("`{name}` is true or false, not {}", other.kind()), None);
                default
            }
        }
    }
}

impl Field<kind::Time> {
    /// A moment: seconds or `beat(n)`, `word(@vo, i)`, ….
    pub(super) fn time(&self, c: &mut Reader, at: &At, name: &str) -> Option<Time> {
        let value = self.look(c)?;
        c.time(at, name, &value)
    }
}

impl Field<kind::Length> {
    /// A length: seconds or `beats(x)`.
    pub(super) fn length(&self, c: &mut Reader, at: &At, name: &str) -> Option<Dur> {
        let value = self.look(c)?;
        c.duration_of(at, name, &value)
    }
}

impl Field<kind::Length> {
    /// A stagger: a step length, or `Stagger{step order}`.
    pub(super) fn stagger(&self, c: &mut Reader, at: &At, name: &str) -> Stagger {
        let value = self.look(c);
        c.stagger_of(at, name, value)
    }
}


impl Field<kind::Media> {
    /// `asset(id, rev)`, `in(@name)`, or (with `markup`) SVG markup.
    pub(super) fn media(&self, c: &mut Reader, at: &At, name: &str, markup: bool) -> Option<Media> {
        let value = self.look(c)?;
        c.media(at, name, &value, markup)
    }
}
