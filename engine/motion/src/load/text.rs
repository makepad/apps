//! The text layers: Text and KineticText, read from their components.

use crate::plain::TextProps;
use super::{check_path_data, At, Reader};
use super::did_you_mean;
use crate::layers::{Field, KineticText, Text};
use crate::model::*;
use crate::obj::Look;
use crate::Ease;
use makepad_script::ScriptNew;
use std::sync::Arc;

impl Text {
    pub(super) fn read(&self, c: &mut Reader, at: &At) -> LayerKind {
        let number_source = self.number.is_set();
        let source = match (self.text.is_set(), self.source.look(c)) {
            _ if number_source => {
                if self.text.is_set() || self.source.is_set() {
                    c.err(at, "number", "a Text shows `text`, `source: voice(@vo)` or `number`, one of them".into(), None);
                }
                // `number: @time | @doc | @edit` reads a clock.
                let clock = match self.number.look(c) {
                    Some(Look::Id(id)) => match id.as_str() {
                        "time" => Some(Clock::Layer),
                        "doc" => Some(Clock::Doc),
                        "edit" => Some(Clock::Edit),
                        other => {
                            c.err(at, "number", format!("`number: @{other}`: a clock is @time (the layer's), @doc or @edit"), None);
                            None
                        }
                    },
                    _ => None,
                };
                let value = if clock.is_some() {
                    Prop::Const([0.0])
                } else {
                    let number = self.number.look(c);
                    c.prop_of::<1>(at, "number", number, [0.0])
                };
                let format = match self.format.look(c) {
                    None => TimeFormat::Plain,
                    Some(Look::Id(id)) => match id.as_str() {
                        "timecode" => TimeFormat::Timecode,
                        "mmss" => TimeFormat::MmSs,
                        "hhmmss" => TimeFormat::HhMmSs,
                        "seconds" => TimeFormat::Seconds { total: self.total.look(c).and_then(|v| v.num()) },
                        "escalate" => TimeFormat::Escalate,
                        other => {
                            c.err(at, "format", format!("`@{other}` is not a time format.{}", did_you_mean(other, crate::layers::TIME_FORMAT)), Some("@timecode (00:00:00:12), @mmss (45:10), @hhmmss, @seconds (3.5 / 15.0 s with `total`)"));
                            TimeFormat::Plain
                        }
                    },
                    Some(other) => {
                        c.err(at, "format", format!("`format` is @timecode, @mmss, @hhmmss or @seconds, not {}", other.kind()), None);
                        TimeFormat::Plain
                    }
                };
                let decimals = match self.decimals.look(c) {
                    None => 0,
                    Some(value) => match c.num_const(at, "decimals", &value) {
                        Some(d) if (0.0..=10.0).contains(&d) => d.round() as u32,
                        _ => {
                            c.err(at, "decimals", "`decimals` is 0 to 10".into(), None);
                            0
                        }
                    },
                };
                let grouping = match self.grouping.look(c) {
                    None | Some(Look::Bool(false)) => None,
                    Some(Look::Bool(true)) => Some(",".to_string()),
                    Some(Look::Str(sep)) => Some(sep.clone()),
                    Some(other) => {
                        c.err(at, "grouping", format!("`grouping` is true (\",\") or the separator as a string, not {}", other.kind()), None);
                        None
                    }
                };
                let prefix = self.prefix.text(c, at, "prefix").unwrap_or_default();
                let suffix = self.suffix.text(c, at, "suffix").unwrap_or_default();
                let tabular = self.tabular.is_true(c);
                // `log: true`: the value is the power of ten (reaching past
                // f64: 1e1000); `overflow: @wall`: the full digits tile the frame.
                let log = self.log.is_true(c);
                let wall = matches!(self.overflow.look(c), Some(Look::Id(o)) if o == "wall");
                TextSource::Number { value, decimals, grouping, prefix, suffix, tabular, clock, format, log, wall }
            }
            (true, None) => TextSource::Text(self.text.text(c, at, "text").unwrap_or_default()),
            (false, Some(voice)) if voice.is("Voice") => TextSource::Voice(c.voice_id(&voice).unwrap_or_default()),
            (false, Some(other)) => {
                c.err(at, "source", format!("`source` is `voice(@node)`, a voiceover's words, not {}", other.kind()), None);
                TextSource::Text(String::new())
            }
            (true, Some(_)) => {
                c.err(at, "source", "a Text has `text` or `source: voice(@vo)`, not both".into(), None);
                TextSource::Text(String::new())
            }
            (false, None) => {
                c.err(at, "text", "a Text needs `text: \"…\"` (or `source: voice(@vo)` for a voiceover's words)".into(), None);
                TextSource::Text(String::new())
            }
        };
        let face = self.font.face_or(c, at, "font", "Inter");
        let font = face.family.to_string();
        // The glyphs this text may show beyond those a build sees drawn
        // (`glyphs: @all` for open text); a number shows any digit.
        if let Some(value) = self.glyphs.look(c) {
            let spec = match &value {
                Look::Id(id) => Some(id.to_string()),
                other => c.string(at, "glyphs", other),
            };
            if let Some(spec) = spec {
                match makepad_doc_fonts::check_coverage(&spec) {
                    Ok(()) => makepad_doc_fonts::declare_coverage(&font, &spec),
                    Err(e) => c.err(at, "glyphs", e, None),
                }
            }
        }
        if number_source {
            makepad_doc_fonts::declare_coverage(&font, "digits");
        }
        let weight = face.weight;
        if !(1.0..=1000.0).contains(&weight) {
            c.err(at, "font", format!("the Font's `weight` is 100..900 (400 regular, 700 bold), not {}", crate::value::fmt_num(weight)), None);
        }
        let weight = weight.clamp(1.0, 1000.0);
        let by = self.by.pick(c, at, "by", TextBy::Whole);
        let voice_bound = matches!(source, TextSource::Voice(_));
        if by == TextBy::Syllable && !voice_bound {
            c.err(at, "by", "`by: @syllable` needs the syllables of a voiceover: `source: voice(@vo)`".into(), None);
        }
        let stagger = self.stagger.stagger(c, at, "stagger");
        if by == TextBy::Whole && stagger.step.value > 0.0 {
            c.warn(at, "stagger", "`stagger` needs `by: @letter`, `@word` or `@line`: the text animates as one piece".into(), None);
        }
        let mut voice = VoiceStyle::default();
        let behaviours = [
            ("show", self.show.is_set()),
            ("pop", self.pop.is_set()),
            ("highlight", self.highlight.is_set()),
            ("highlight_color", self.highlight_color.is_set()),
            ("wave", self.wave.is_set()),
            ("ghost", self.ghost.is_set()),
            ("line", self.line.is_set()),
            ("sweep", self.sweep.is_set()),
            ("karaoke", self.karaoke.is_set()),
        ];
        for (field, set) in behaviours {
            if set && !voice_bound {
                c.err(at, field, format!("`{field}` is a voiceover behaviour: it needs `source: voice(@vo)`"), None);
            }
        }
        if voice_bound {
            voice.show = self.show.pick(c, at, "show", VoiceShow::All);
            let pop = self.pop.look(c);
            voice.pop = matches!(&pop, Some(Look::Id(id)) if id == "on_word") || matches!(pop, Some(Look::Bool(true)));
            if let Some(value) = pop {
                if !matches!(&value, Look::Id(id) if id == "on_word") && !matches!(value, Look::Bool(_)) {
                    c.err(at, "pop", format!("`pop` is `@on_word`, not {}", value.kind()), None);
                }
            }
            voice.highlight = match self.highlight.look(c) {
                None => None,
                Some(Look::Id(id)) if id == "active" => Some(VoiceShow::Active),
                Some(Look::Id(id)) if id == "spoken" => Some(VoiceShow::Spoken),
                Some(other) => {
                    c.err(at, "highlight", format!("`highlight` is @active (the word being said) or @spoken (every word said so far), not {}", other.kind()), None);
                    None
                }
            };
            if self.ghost.is_set() {
                voice.ghost = self.ghost.konst(c, at, "ghost");
            }
            if self.line.is_set() {
                voice.line = self.line.konst_opt(c, at, "line").map(|n| n.max(0.0) as u32);
            }
            voice.sweep = self.sweep.is_true(c);
            voice.karaoke = match self.karaoke.look(c) {
                None => None,
                Some(Look::Obj(k, _)) => {
                    let color = |c: &mut Reader, name: &str, default: Option<Color>| match c.get(k, name) {
                        None | Some(Look::Nil) => default,
                        Some(value) => c.color_const(at, "karaoke", &value).or(default),
                    };
                    let number = |c: &mut Reader, name: &str, default: f64| match c.get(k, name) {
                        None | Some(Look::Nil) => default,
                        Some(value) => c.num_const(at, "karaoke", &value).unwrap_or(default),
                    };
                    let sung = color(c, "sung", Some(voice.highlight_color)).unwrap_or(voice.highlight_color);
                    Some(Karaoke {
                        unsung: color(c, "unsung", None),
                        sung,
                        cooled: color(c, "cooled", Some(sung)).unwrap_or(sung),
                        anticipate: number(c, "anticipate", 0.4).max(0.0),
                        cool: number(c, "cool", 1.5).max(0.0),
                        syllables: matches!(c.get(k, "by"), Some(Look::Id(id)) if id == "syllable"),
                        rtl: matches!(c.get(k, "dir"), Some(Look::Id(id)) if id == "rtl"),
                    })
                }
                Some(other) => {
                    c.err(at, "karaoke", format!("`karaoke` is `{{sung: #ffb070 cooled: #eeeae4 unsung: #ffffff40 anticipate: 0.4 cool: 1.5}}`, not {}", other.kind()), None);
                    None
                }
            };
            if self.highlight_color.is_set() {
                voice.highlight_color = self.highlight_color.konst(c, at, "highlight_color").unwrap_or(voice.highlight_color);
            }
            voice.stretch = matches!(self.stretch.look(c), Some(Look::Id(id)) if id == "vowels");
            voice.wave = match self.wave.look(c) {
                None => false,
                Some(Look::Id(id)) if id == "pitch" => true,
                Some(other) => {
                    c.err(at, "wave", format!("`wave` is `@pitch` (words rise and fall with the voice), not {}", other.kind()), None);
                    false
                }
            };
        }
        let size = self.size.prop(c, at, "size", 72.0);
        let wrap = self.wrap.width(c, at, "wrap");
        let fit = self.fit.width(c, at, "fit");
        let line_height = self.line_height.konst(c, at, "line_height", 1.2);
        let tracking = self.tracking.prop(c, at, "tracking", 0.0);
        let italic = face.italic;
        // Variable-font axes: the Font's own, then the layer's keyable
        // `wdth`, `wght`, `slnt` and `axes: {TAG: value}` over them; the
        // font is read at the values.
        let mut axes: Vec<(String, Prop<1>)> = face.axes.iter().map(|(tag, v)| (tag.clone(), Prop::Const([*v]))).collect();
        match self.stretch.look(c) {
            None => {}
            Some(Look::Id(id)) if id == "vowels" => {
                if !voice_bound {
                    c.err(at, "stretch", "`stretch: @vowels` is a voiceover behaviour: it needs `source: voice(@vo)`".into(), None);
                }
            }
            Some(other) => c.err(at, "stretch", format!("`stretch` is `@vowels` (a voiceover behaviour), not {}; the width axis is `wdth: 125` or the Font's `axes: {{wdth: 125}}`", other.kind()), None),
        }
        for (tag, field) in [("wdth", &self.wdth), ("wght", &self.wght), ("slnt", &self.slnt)] {
            if field.is_set() {
                let prop = field.prop(c, at, tag, 0.0);
                axes.retain(|(t, _)| t != tag);
                axes.push((tag.into(), prop));
            }
        }
        match self.axes.look(c) {
            None => {}
            Some(Look::Obj(o, _)) => {
                for (tag, value) in c.fields(o) {
                    if tag.is_empty() || tag.len() > 4 {
                        c.err(at, "axes", format!("`{tag}` is not an axis tag (up to four letters: wdth, wght, slnt, opsz, GRAD, ...)"), None);
                        continue;
                    }
                    let Some(prop) = c.prop_value::<1>(at, "axes", &value) else { continue };
                    axes.retain(|(t, _)| *t != tag);
                    axes.push((tag, prop));
                }
            }
            Some(other) => c.err(at, "axes", format!("`axes` is an object of axis values, e.g. `{{wdth: 125 GRAD: 50}}`, not {}", other.kind()), None),
        }
        let ranges = self.ranges(c, at, by, &font, weight, italic);
        let plate = if !self.plate.is_set() {
            let set = [
                ("plate_pad", self.plate_pad.is_set()),
                ("plate_radius", self.plate_radius.is_set()),
                ("plate_stroke", self.plate_stroke.is_set()),
                ("plate_width", self.plate_width.is_set()),
                ("plate_reveal", self.plate_reveal.is_set()),
                ("plate_each", self.plate_each.is_set()),
            ];
            for (field, set) in set {
                if set {
                    c.warn(at, field, format!("`{field}` does nothing without `plate: <colour>`"), None);
                }
            }
            None
        } else {
            let color = self.plate.prop(c, at, "plate", [0.0, 0.0, 0.0, 1.0]);
            let pad = match self.plate_pad.look(c) {
                None => [16.0, 8.0],
                Some(Look::Num(n)) => [n, n],
                Some(value) => c.vec2_const(at, "plate_pad", &value).unwrap_or([16.0, 8.0]),
            };
            Some(Plate {
                color,
                pad,
                radius: self.plate_radius.konst(c, at, "plate_radius", 0.0),
                stroke: self.plate_stroke.opt(c, at, "plate_stroke"),
                width: self.plate_width.konst(c, at, "plate_width", 1.0),
                reveal: self.plate_reveal.prop(c, at, "plate_reveal", 1.0),
                each: self.plate_each.is_true(c),
                underline: matches!(self.plate_style.look(c), Some(Look::Id(s)) if s == "underline"),
            })
        };
        let reveal = self.reveal.opt(c, at, "reveal");
        let caret = if !self.caret.is_set() {
            None
        } else {
            let opacity = self.caret.prop(c, at, "caret", 1.0);
            let color = if !self.caret_color.is_set() {
                self.color.prop(c, at, "color", [1.0, 1.0, 1.0, 1.0])
            } else {
                self.caret_color.prop(c, at, "caret_color", [1.0, 1.0, 1.0, 1.0])
            };
            let block = match self.caret_style.look(c) {
                None => false,
                Some(Look::Id(s)) if s == "bar" => false,
                Some(Look::Id(s)) if s == "block" => true,
                Some(other) => {
                    c.err(at, "caret_style", format!("`caret_style` is @bar or @block, not {}", other.kind()), None);
                    false
                }
            };
            Some((opacity, color, block))
        };
        LayerKind::Text {
            source,
            props: TextProps {
                font,
                weight,
                italic,
                size,
                color: self.color.prop(c, at, "color", [1.0, 1.0, 1.0, 1.0]),
                align: self.align.pick(c, at, "align", Align::Left),
                valign: self.valign.pick(c, at, "valign", VAlign::Top),
                by,
                stagger,
                line_height,
                tracking,
                wrap,
                fit,
                reveal,
            },
            ranges,
            voice,
            plate,
            caret,
            axes,
            path: match self.path.look(c) {
                None => None,
                Some(Look::Str(data)) => match check_path_data(&data) {
                    Ok(()) => Some((crate::geometry::parse_path(&data).into(), self.path_offset.prop(c, at, "path_offset", 0.0))),
                    Err(message) => {
                        c.err(at, "path", message, None);
                        None
                    }
                },
                // `path: layer("loss")` / `layer("chart", "series")`: another
                // layer's path, bound once every layer is loaded.
                Some(Look::Obj(o, Some("LayerPath"))) => {
                    let name = match c.get(o, "name") { Some(Look::Str(n)) => n, _ => String::new() };
                    let series = match c.get(o, "series") { Some(Look::Str(s)) => Some(s), _ => None };
                    c.pending_paths.push((at.path.clone(), name, series, at.loc));
                    Some((Arc::default(), self.path_offset.prop(c, at, "path_offset", 0.0)))
                }
                Some(other) => {
                    c.err(at, "path", format!("a Text's `path` is path data (\"M 0 200 C …\") its letters ride, or `layer(\"name\")`, not {}", other.kind()), None);
                    None
                }
            },
            flicker: self.flicker(c, at),
            only: match self.only.look(c) {
                None => None,
                Some(Look::Arr(items)) => Some(items.iter().filter_map(Look::num).map(|n| n.max(0.0) as usize).collect()),
                Some(value) => match value.num() {
                    Some(n) => Some(vec![n.max(0.0) as usize]),
                    None => {
                        c.err(at, "only", format!("`only` is a letter's index or a list of them, not {}", value.kind()), None);
                        None
                    }
                },
            },
            stroke: self.stroke.opt(c, at, "stroke").map(|color| (color, self.width.prop(c, at, "width", 2.0))),
        }
    }

    fn ranges(&self, c: &mut Reader, at: &At, by: TextBy, font: &str, weight: f64, italic: bool) -> Vec<TextRange> {
        if !self.range.is_set() && !self.ranges.is_set() {
            return Vec::new();
        }
        if !matches!(by, TextBy::Letter | TextBy::Word | TextBy::Line) {
            c.err(at, if self.ranges.is_set() { "ranges" } else { "range" }, "ranges need by: @letter, @word or @line".into(), None);
        }
        if self.range.is_set() && self.ranges.is_set() {
            c.err(at, "ranges", "use `range: Range{…}` or `ranges: [Range{…}, …]`, not both".into(), None);
        }
        let mut ranges = Vec::new();
        let mut read = |c: &mut Reader, value: Look, field: &str| {
            match value {
                Look::Obj(obj, Some("Range")) => {
                    let at = c.at(obj, at.field(field), at.loc);
                    ranges.push(crate::layers::Range::script_from_value(c.vm, obj.into()).read(c, &at, by));
                }
                other => c.err(at, field, format!("`{field}` expects Range{{…}}, not {}", other.kind()), None),
            }
        };
        if let Some(value) = self.range.look(c) {
            read(c, value, "range");
        }
        match self.ranges.look(c) {
            None => {}
            Some(Look::Arr(values)) => {
                for (i, value) in values.into_iter().enumerate() {
                    read(c, value, &format!("ranges[{i}]"));
                }
            }
            Some(other) => c.err(at, "ranges", format!("`ranges` expects [Range{{…}}, …], not {}", other.kind()), None),
        }
        // Font metadata is load work, and only needed for axis targets.
        if ranges.iter().any(|r| !r.axes.is_empty()) {
            match crate::type_fns::font_axes(font, weight, italic) {
                Ok(axes) => {
                    for range in &mut ranges {
                        let loc = range.loc;
                        let path = &range.path;
                        range.axes.retain_mut(|axis| {
                            if let Some(info) = axes.iter().find(|a| a.tag == axis.tag) {
                                axis.default = if axis.tag == "wght" { weight } else { info.default as f64 };
                                true
                            } else {
                                c.diags.push(Diagnostic::warning(loc, format!("{path}.axes.{}", axis.tag), format!("Range axis `{}` is not in font \"{font}\"; that target is ignored", axis.tag)));
                                false
                            }
                        });
                    }
                }
                Err(error) => {
                    c.warn(at, "ranges", format!("cannot read Range axis defaults for font \"{font}\": {error}; axis targets are ignored"), None);
                    for range in &mut ranges {
                        range.axes.clear();
                    }
                }
            }
        }
        ranges
    }

    /// `flicker: 0..1` (a neon tube's sputter, per letter) and `fail: 3`
    /// or `fail: [3, 7]` (the letters, by character index, that keep
    /// failing).
    fn flicker(&self, c: &mut Reader, at: &At) -> Option<(Prop<1>, Vec<usize>)> {
        let fail = match self.fail.look(c) {
            None => Vec::new(),
            Some(Look::Arr(items)) => items.iter().filter_map(Look::num).map(|n| n.max(0.0) as usize).collect(),
            Some(value) => match value.num() {
                Some(n) => vec![n.max(0.0) as usize],
                None => {
                    c.err(at, "fail", format!("`fail` is a letter's index (`fail: 3`) or several (`fail: [3, 7]`), not {}", value.kind()), None);
                    Vec::new()
                }
            },
        };
        match self.flicker.is_set() {
            false if fail.is_empty() => None,
            false => Some((Prop::Const([0.0]), fail)),
            true => Some((self.flicker.prop(c, at, "flicker", 0.0), fail)),
        }
    }
}

impl crate::layers::Range {
    fn read(&self, c: &mut Reader, at: &At, text_by: TextBy) -> TextRange {
        let by = match self.by.look(c) {
            None => text_by,
            Some(Look::Id(id)) if id == "letter" => TextBy::Letter,
            Some(Look::Id(id)) if id == "word" => TextBy::Word,
            Some(Look::Id(id)) if id == "line" => TextBy::Line,
            Some(other) => {
                c.err(at, "by", format!("Range `by` expects @letter, @word or @line, not {}", other.kind()), None);
                text_by
            }
        };
        if matches!((text_by, by), (TextBy::Word, TextBy::Letter) | (TextBy::Line, TextBy::Letter | TextBy::Word)) {
            c.err(at, "by", "Range `by` is finer than the Text's units; use the Text's `by` or a coarser selection (@letter → @word → @line)".into(), None);
        }
        let from = self.from.prop(c, at, "from", 0.0);
        let to = self.to.prop(c, at, "to", 1.0);
        if let (Some([from]), Some([to])) = (from.constant(), to.constant()) {
            if to < from {
                c.err(at, "to", "Range `to` must be greater than or equal to `from`".into(), None);
            }
        }
        let soft = self.soft.prop(c, at, "soft", 0.1);
        if soft.constant().is_some_and(|[v]| v < 0.0) {
            c.err(at, "soft", "Range `soft` is a non-negative fraction (0 gives hard edges)".into(), None);
        }
        let ease = self.ease.look(c).map_or(Ease::InOut(crate::Curve::Quad), |v| c.ease(at, "ease", &v));
        let invert = match self.invert.look(c) {
            None => false,
            Some(Look::Bool(value)) => value,
            Some(other) => {
                c.err(at, "invert", format!("Range `invert` expects true or false, not {}", other.kind()), None);
                false
            }
        };
        let mut axes = Vec::new();
        for (tag, field) in [("wght", &self.wght), ("wdth", &self.wdth), ("slnt", &self.slnt)] {
            if let Some(value) = field.opt(c, at, tag) {
                axes.push(TextRangeAxis { tag: tag.into(), value, default: 0.0 });
            }
        }
        match self.axes.look(c) {
            None => {}
            Some(Look::Obj(obj, None)) => {
                for (tag, value) in c.fields(obj) {
                    let field = format!("axes.{tag}");
                    if tag.is_empty() || tag.len() > 4 || !tag.is_ascii() {
                        c.err(at, &field, format!("Range axis `{tag}` expects an axis tag of up to four ASCII characters (wght, wdth, slnt, GRAD, …)"), None);
                        continue;
                    }
                    if let Some(value) = c.prop_value::<1>(at, &field, &value) {
                        axes.retain(|a| a.tag != tag);
                        axes.push(TextRangeAxis { tag, value, default: 0.0 });
                    }
                }
            }
            Some(other) => c.err(at, "axes", format!("Range `axes` expects an object of keyable axis values, e.g. {{GRAD: 50}}, not {}", other.kind()), None),
        }
        TextRange {
            loc: at.loc,
            path: at.path.clone(),
            by, from, to, soft, ease, invert,
            slide: self.slide.prop(c, at, "slide", 0.0),
            x: self.x.opt(c, at, "x"),
            y: self.y.opt(c, at, "y"),
            rotate: self.rotate.opt(c, at, "rotate"),
            blur: self.blur.opt(c, at, "blur"),
            scale: self.scale.opt(c, at, "scale"),
            opacity: self.opacity.opt(c, at, "opacity"),
            color: self.color.opt(c, at, "color"),
            axes,
        }
    }
}

impl KineticText {
    /// `KineticText{preset: "321_typewriter" text: "…" | source: voice(@vo) p1..p4 speed size font wdth wght slnt backdrop}`:
    /// a kinetic type kit (a text pack's, such as Stage's kinetic text
    /// pack) showing the document's words.
    pub(super) fn read(&self, c: &mut Reader, at: &At) -> LayerKind {
        let preset = match self.preset.look(c) {
            Some(Look::Str(name)) | Some(Look::Id(name)) if !name.trim().is_empty() => {
                let name = name.trim();
                if name.contains('/') { name.to_string() } else { format!("vjfx/{name}") }
            }
            Some(other) => {
                c.err(at, "preset", format!("`preset` names a kinetic type preset (\"321_typewriter\", \"339_karaoke_sweep\", …), not {}", other.kind()), None);
                String::new()
            }
            None => {
                c.err(at, "preset", "a KineticText names its `preset` (a kinetic type kit)".into(), Some("`KineticText{preset: \"318_letter_flip\" text: \"Hello\"}`; `makepad-motion kinetic` lists them"));
                String::new()
            }
        };
        let text = match (self.text.is_set(), self.source.look(c)) {
            (true, None) => KineticSource::Text(self.text.text(c, at, "text").unwrap_or_default()),
            (false, Some(voice)) if voice.is("Voice") => KineticSource::Voice(c.voice_id(&voice).unwrap_or_default()),
            (false, Some(other)) => {
                c.err(at, "source", format!("`source` is `voice(@node)`, not {}", other.kind()), None);
                KineticSource::Text(String::new())
            }
            (true, Some(_)) => {
                c.err(at, "text", "a KineticText has `text` or `source: voice(@vo)`, not both".into(), None);
                KineticSource::Text(String::new())
            }
            (false, None) => {
                c.err(at, "text", "a KineticText needs `text: \"…\"` or `source: voice(@vo)`".into(), None);
                KineticSource::Text(String::new())
            }
        };
        let mut params = Vec::new();
        dials(c, at, &mut params, [("p1", &self.p1), ("p2", &self.p2), ("p3", &self.p3), ("p4", &self.p4)]);
        // The kit font's variable axes (keyable): width, weight, slant.
        for (p, field) in [("wdth", &self.wdth), ("wght", &self.wght), ("slnt", &self.slnt)] {
            if field.is_set() {
                params.push((p.to_string(), field.prop(c, at, p, 0.0)));
            }
        }
        // `backdrop: false`: the letters alone over the layers under them
        // (a game's banner over play); the kit's whole frame by default.
        // `font:` any document font (bundled or installed external) in
        // place of the kit's own (a Font; its `italic: true` the italic).
        let face = self.font.face(c, at, "font");
        if face.as_ref().is_some_and(|f| f.italic) {
            params.push(("italic".to_string(), Prop::Const([1.0])));
        }
        let font = face.map(|f| f.family.to_string());
        if let Some(value) = self.backdrop.look(c) {
            match value {
                Look::Bool(on) => params.push(("backdrop".to_string(), Prop::Const([if on { 1.0 } else { 0.0 }]))),
                other => c.err(at, "backdrop", format!("`backdrop` is true or false, not {}", other.kind()), None),
            }
        }
        let speed = self.speed.konst(c, at, "speed", 1.0);
        LayerKind::Shader { program: ShaderProgram::Preset(preset), size: self.size.size(c, at, "size"), params, speed, below: false, data: None, text: Some(text), font }
    }
}

/// The written dials `p1`..`p4` (each 0..1; -1 where a kit reads its own default).
pub(super) fn dials(c: &mut Reader, at: &At, params: &mut Vec<(String, Prop<1>)>, fields: [(&str, &Field<crate::layers::kind::Num>); 4]) {
    for (name, field) in fields {
        if field.is_set() {
            params.push((name.to_string(), field.prop(c, at, name, -1.0)));
        }
    }
}
