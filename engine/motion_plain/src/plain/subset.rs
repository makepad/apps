//! Explicit decisions about the Motion vocabulary a peer can draw as data.
//! An unlisted name is unknown, so vocabulary additions require a decision.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Admitted,
    Refused,
}
use Class::{Admitted, Refused};

pub fn layer(name: &str) -> Option<Class> {
    match name {
        "Rect" | "Ellipse" | "Path" | "Text" | "Group" | "Repeat" => Some(Admitted),
        "Svg" | "Vector" | "Image" | "Video" | "Bones" | "Pattern" | "Chart" | "Shader"
        | "KineticText" | "Text3D" | "Seq3D" | "Scene3D" | "Widget" | "Crowd" | "Freeze" | "Comic"
        | "Polaroid" | "Character" | "Hud" | "Words" | "FlyThrough" | "Shorts" | "TokenStream" | "Form"
        | "CodeDraw" | "UiWindow" | "UiButton" | "EaseGraph" | "ParticleText" | "Bounce" | "GridFloor"
        | "Metaballs" | "Rays" | "GridWarp" | "Branches" | "Math" | "Shape" | "MatchMorph" | "Plot"
        | "Graph" | "Parametric" | "Implicit" | "VectorField" | "Riemann" | "Area" | "Tangent" | "Dot"
        | "Draw2D" | "Morph" | "Camera" | "Zoomed" => Some(Refused),
        _ => None,
    }
}

/// Fields are classified in the context of their owner. Even a known field
/// on a refused layer cannot make that layer data.
pub fn field(owner: &str, name: &str) -> Option<Class> {
    match (owner, name) {
        ("Motion", "label" | "brief" | "tags" | "size" | "duration" | "fps" | "bpm" | "background" | "inputs"
        | "layers" | "stops") => Some(Admitted),
        ("Motion", "style" | "camera" | "color" | "post" | "motion_blur") => Some(Refused),
        ("Font", "family" | "weight" | "italic") => Some(Admitted),
        ("Font", "axes") => Some(Refused),
        ("Common", "name" | "at" | "x" | "y" | "scale" | "rotate" | "anchor" | "opacity" | "blend" | "span"
        | "blur" | "feather" | "feather_side" | "visible" | "locked" | "hard_cut") => Some(Admitted),
        ("Common", "data_layer" | "effects" | "exit" | "exit_at" | "exit_duration" | "exit_to" | "onion"
        | "slices" | "tear" | "droste" | "rigid") => Some(Refused),
        ("Rect", "size" | "radius" | "fill" | "stroke" | "width" | "trim_start" | "trim_end" | "cap") => Some(Admitted),
        ("Ellipse", "size" | "fill" | "stroke" | "width" | "trim_start" | "trim_end" | "cap") => Some(Admitted),
        ("Path", "svg" | "points" | "closed" | "smooth" | "fill" | "stroke" | "width" | "trim_start"
        | "trim_end" | "cap" | "join") => Some(Admitted),
        ("Svg", "src" | "svg" | "size") => Some(Refused),
        ("Vector", "src" | "svg" | "icon" | "size" | "parts" | "draw_on" | "stagger" | "trace" | "fill"
        | "stroke") => Some(Refused),
        ("Text", "text" | "font" | "size" | "color" | "align" | "valign" | "by" | "stagger" | "line_height"
        | "tracking" | "wrap" | "fit" | "reveal") => Some(Admitted),
        ("Text", "source" | "range" | "ranges" | "show" | "pop" | "highlight" | "highlight_color" | "karaoke"
        | "stretch" | "wdth" | "wght" | "slnt" | "axes" | "wave" | "ghost" | "line" | "sweep" | "number"
        | "decimals" | "grouping" | "prefix" | "suffix" | "tabular" | "format" | "total" | "log"
        | "overflow" | "plate" | "plate_pad" | "plate_radius" | "plate_stroke" | "plate_width"
        | "plate_reveal" | "plate_each" | "plate_style" | "path" | "path_offset" | "animate" | "start"
        | "speed" | "words" | "hold" | "layout" | "inside" | "stroke" | "width" | "caret"
        | "caret_color" | "caret_style" | "cps" | "flicker" | "fail" | "role" | "only" | "window"
        | "morph" | "mirror" | "collapse" | "rows" | "glyphs") => Some(Refused),
        ("Image", "src" | "size" | "fit" | "zoom" | "focus" | "mesh") => Some(Refused),
        ("Video", "src" | "size" | "fit" | "in" | "speed") => Some(Refused),
        ("Group", "layers" | "mask" | "mask_mode" | "stagger" | "size") => Some(Admitted),
        ("Group", "mask_feather" | "post" | "glyphs") => Some(Refused),
        ("Bones", "bones" | "ik" | "follow" | "layers") => Some(Refused),
        ("Repeat", "layer" | "count" | "layout" | "columns" | "spacing" | "radius" | "start" | "sweep" | "face"
        | "delay" | "step" | "jitter" | "seed") => Some(Admitted),
        ("Repeat", "source" | "path") => Some(Refused),
        ("Pattern", "kind" | "color" | "color2" | "cell" | "angle" | "weight" | "size" | "speed" | "seed") => Some(Refused),
        ("Chart", "type" | "data" | "csv" | "json" | "split" | "next" | "switch" | "every" | "size" | "theme"
        | "colors" | "color" | "accent" | "label_color" | "font" | "title_font" | "label_size"
        | "format" | "decimals" | "prefix" | "suffix" | "currency" | "start" | "stagger" | "grow"
        | "ease" | "land" | "axes" | "grid" | "values" | "labels" | "legend" | "title" | "subtitle"
        | "source" | "annotate" | "highlight" | "links" | "layout" | "thickness" | "max" | "min"
        | "orient" | "mode" | "sort" | "top" | "smooth" | "center" | "x_title" | "y_title" | "root"
        | "payoff" | "dots" | "series" | "regions" | "face" | "shape" | "callout" | "style" | "clock"
        | "total" | "rider" | "rider_rotate" | "rider_offset" | "review" | "y_scale" | "x_scale"
        | "window" | "activate" | "header" | "top_k" | "overflow") => Some(Refused),
        ("Shader", "effect" | "pixel" | "helpers" | "size" | "params" | "p1" | "p2" | "p3" | "p4" | "speed"
        | "input" | "data") => Some(Refused),
        ("KineticText", "preset" | "text" | "source" | "size" | "p1" | "p2" | "p3" | "p4" | "speed" | "fit" | "wdth"
        | "wght" | "slnt" | "backdrop" | "font") => Some(Refused),
        ("Text3D", "text" | "preset" | "font" | "material" | "color" | "accent" | "background" | "depth"
        | "bevel" | "bevel_type" | "tracking" | "line_height" | "align" | "by" | "order" | "stagger"
        | "duration" | "word_times" | "words" | "line" | "ghost" | "highlight_color" | "pulse" | "music"
        | "fit" | "size" | "environment" | "floor" | "motion_blur" | "seed") => Some(Refused),
        ("Seq3D", "template" | "text" | "subtitle" | "color" | "accent" | "background" | "logo" | "image"
        | "video" | "product" | "font" | "bpm" | "duration" | "seed" | "from" | "to" | "points" | "size"
        | "phone_image" | "phone_video" | "fit" | "crop" | "phone_crop" | "phone_aspect") => Some(Refused),
        ("Scene3D", "model" | "size" | "camera" | "lights" | "nodes" | "materials" | "background" | "exposure"
        | "textures" | "videos" | "layers") => Some(Refused),
        ("Widget", "ui" | "size" | "props") => Some(Refused),
        ("Crowd", "count" | "rows" | "size" | "seed" | "variants" | "shape" | "color" | "color2" | "outline"
        | "eyes" | "eyes2" | "bob" | "stagger" | "react" | "hop" | "nod" | "arms" | "cheer" | "signs"
        | "sign" | "sign_up" | "sign_color" | "sign_ink" | "start" | "member") => Some(Refused),
        ("Freeze", "time" | "size") => Some(Refused),
        ("Comic", "panels" | "size" | "layout" | "columns" | "gutter" | "border" | "ink" | "paper" | "halftone"
        | "seed") => Some(Refused),
        ("Polaroid", "time" | "size" | "caption" | "caption_time" | "tilt" | "flash" | "mono" | "burst" | "ink"
        | "card") => Some(Refused),
        ("Character", "model" | "look" | "svg" | "src" | "height" | "facing" | "pose" | "expression" | "costume"
        | "mouth" | "walk" | "float" | "emote" | "nod" | "bob" | "blink" | "seed" | "sway" | "vary"
        | "lip_sync" | "lead" | "parts" | "reach") => Some(Refused),
        ("Hud", "size" | "title" | "tag" | "chapters" | "bpm" | "timecode" | "clock" | "ink" | "inset"
        | "brackets" | "label_size") => Some(Refused),
        ("Words", "words" | "every" | "start" | "size" | "font" | "text_size" | "color" | "bg" | "look" | "hold") => Some(Refused),
        ("FlyThrough", "layers" | "into" | "letter" | "time" | "duration" | "zoom") => Some(Refused),
        ("Shorts", "tag" | "headline" | "accent" | "inset" | "inset_size" | "meta" | "clip" | "total" | "floor"
        | "ink" | "accent_color" | "background") => Some(Refused),
        ("TokenStream", "prompt" | "tokens" | "times" | "every" | "start" | "words" | "line" | "header" | "candidates"
        | "decoys" | "top_k" | "accent" | "ink" | "panel" | "size" | "enter") => Some(Refused),
        ("Form", "title" | "subtitle" | "fields" | "checks" | "signature" | "stamp" | "stamp2" | "start"
        | "every" | "size" | "ink" | "paper" | "stamp_color") => Some(Refused),
        ("CodeDraw", "code" | "layers" | "every" | "start" | "split" | "size" | "ink" | "muted" | "panel") => Some(Refused),
        ("UiWindow", "title" | "lines" | "size" | "start" | "cps" | "accent" | "ink" | "panel" | "button") => Some(Refused),
        ("UiButton", "label" | "click" | "size" | "accent" | "ink" | "cursor") => Some(Refused),
        ("EaseGraph", "curve" | "size" | "color" | "accent" | "ink" | "start" | "duration" | "label" | "dots"
        | "font" | "fill_at") => Some(Refused),
        ("ParticleText", "text" | "stages" | "hold" | "move" | "font" | "size" | "box" | "count" | "radius" | "seed"
        | "start" | "converge" | "release" | "wind" | "swirl" | "colors" | "glow") => Some(Refused),
        ("Bounce", "size" | "color" | "radius" | "height" | "bounces" | "restitution" | "squash" | "start"
        | "duration" | "baseline" | "labels" | "ink" | "font") => Some(Refused),
        ("GridFloor", "size" | "color" | "horizon" | "cells" | "speed" | "glow" | "fade" | "width" | "sky") => Some(Refused),
        ("Metaballs", "size" | "color" | "colors" | "satellites" | "radius" | "orbit" | "k" | "speed" | "wobble"
        | "background" | "caption" | "ink" | "font") => Some(Refused),
        ("Rays", "size" | "color" | "center" | "count" | "spread" | "speed" | "reach" | "burst" | "intensity") => Some(Refused),
        ("GridWarp", "size" | "color" | "center" | "cells" | "mass" | "mode" | "ring" | "speed" | "width") => Some(Refused),
        ("Branches", "size" | "style" | "from" | "angle" | "length" | "depth" | "spread" | "shrink" | "seed"
        | "color" | "width" | "start" | "grow" | "labels" | "ink" | "font") => Some(Refused),
        ("Math", "tex" | "size" | "color" | "display" | "align" | "font" | "parts" | "paths" | "chars") => Some(Refused),
        ("Shape", "op" | "layers" | "fill" | "stroke" | "width" | "trim_start" | "trim_end" | "cap" | "join") => Some(Refused),
        ("MatchMorph", "steps" | "times" | "durations" | "by" | "math" | "display" | "font" | "size" | "tracking"
        | "color" | "colors" | "enter_color" | "settle" | "parts" | "paths" | "chars" | "align"
        | "duration" | "ease") => Some(Refused),
        ("Plot", "size" | "layers" | "grid" | "labels" | "x_label" | "y_label" | "color" | "grid_color"
        | "label_size" | "width" | "reveal" | "map" | "map_at" | "fps") => Some(Refused),
        ("Graph", "f" | "from" | "to" | "color" | "stroke" | "fill" | "width" | "trim_start" | "trim_end"
        | "cap" | "join" | "fps" | "samples") => Some(Refused),
        ("Parametric", "f" | "from" | "to" | "color" | "stroke" | "fill" | "width" | "trim_start" | "trim_end"
        | "cap" | "join" | "fps" | "samples") => Some(Refused),
        ("Implicit", "f" | "cells" | "color" | "stroke" | "fill" | "width" | "trim_start" | "trim_end" | "cap"
        | "join" | "fps" | "samples") => Some(Refused),
        ("VectorField", "f" | "step" | "color" | "stroke" | "fill" | "width" | "trim_start" | "trim_end" | "cap"
        | "join" | "fps" | "samples") => Some(Refused),
        ("Riemann", "f" | "from" | "to" | "n" | "side" | "color" | "stroke" | "fill" | "width" | "trim_start"
        | "trim_end" | "cap" | "join" | "fps" | "samples") => Some(Refused),
        ("Area", "f" | "g" | "from" | "to" | "color" | "stroke" | "fill" | "width" | "trim_start" | "trim_end"
        | "cap" | "join" | "fps" | "samples") => Some(Refused),
        ("Tangent", "f" | "length" | "dot" | "color" | "stroke" | "fill" | "width" | "trim_start" | "trim_end"
        | "cap" | "join" | "fps" | "samples") => Some(Refused),
        ("Dot", "radius" | "label" | "tex" | "label_size" | "color" | "stroke" | "fill" | "width"
        | "trim_start" | "trim_end" | "cap" | "join" | "fps" | "samples") => Some(Refused),
        ("Draw2D", "draw" | "size") => Some(Refused),
        ("Morph", "from" | "to" | "time" | "duration") => Some(Refused),
        ("Camera", "center" | "zoom" | "roll" | "screen" | "layers") => Some(Refused),
        ("Zoomed", "of" | "region" | "region_at" | "region_size" | "size" | "frame" | "width" | "connect"
        | "background" | "radius") => Some(Refused),
        _ => match layer(owner)? {
            Admitted => field("Common", name),
            Refused => field("Common", name).map(|_| Refused),
        },
    }
}

/// Constructor position matters: e.g. `Rect{}` and `keys(...)`.
pub fn is_data_object(name: &str) -> bool {
    matches!(name, "Font" | "Motion") || layer(name) == Some(Admitted)
}

pub fn is_data_call(name: &str) -> bool {
    matches!(name, "vec2" | "keys" | "tween" | "repeat" | "in" | "input")
}

/// Types, calls, and the other names exported by the document vocabulary.
pub fn constructor(name: &str) -> Option<Class> {
    match name {
        name if is_data_object(name) || is_data_call(name) => Some(Admitted),
        "cabs" | "carg" | "cdiv" | "cexp" | "clog" | "cmul" | "conj" | "csqrt" => Some(Refused),
        "Along" | "Anchor" | "Asset" | "Bar" | "Beat" | "Beats" | "Bezier" | "Blend" | "Blend2"
        | "Bone" | "Downbeat" | "Edit" | "Effect" | "Follow" | "IK" | "In" | "Input" | "Item" | "Kernel"
        | "Keys" | "LayerPath" | "Line" | "Loop" | "MissingMedia" | "Music" | "Onset" | "Playhead"
        | "Progress" | "Range" | "Rate" | "Section" | "Sequence" | "Sim" | "Spring" | "Stagger"
        | "Tween" | "Voice" | "Word" | "aa_fill" | "aa_step" | "align_paths" | "along" | "anchor"
        | "asset" | "bar" | "bayer4" | "beat" | "beat_at" | "beats" | "bezier" | "blend" | "blend2"
        | "color" | "cpow" | "double_smooth" | "downbeat" | "drum_pos" | "ease" | "ease_id" | "edit" | "emit"
        | "engrave_helpers" | "exponential_decay" | "fit_text" | "fixed" | "frame_idx" | "glyph_x"
        | "hash01" | "hash12" | "hash22" | "hashn" | "here" | "hsv2rgb" | "icon" | "icon_path"
        | "implicit_curve" | "import" | "inside" | "isoline" | "item" | "knob" | "lagged" | "lang"
        | "layer" | "line" | "linear_rgb" | "linear_to_srgb" | "linear_to_srgb1" | "lingering" | "live"
        | "luma" | "lyric_line" | "lyric_line_after" | "lyric_word" | "map_points" | "memo"
        | "mix_paths" | "modulate" | "modules" | "music" | "none" | "not_quite_there" | "onset"
        | "palette" | "path_boolean" | "playhead" | "plot_ticks" | "prog" | "progress" | "pulse"
        | "rand_seq" | "remap" | "rgb" | "rgba" | "rot2" | "run" | "running_start" | "sample_curve"
        | "sd_box" | "sd_circle" | "sd_ring" | "sd_round_box" | "sd_segment" | "section" | "sequence"
        | "snoise2" | "snoise_mod289" | "snoise_permute" | "spring" | "spring_step" | "squish"
        | "srgb_to_linear" | "srgb_to_linear1" | "stagger" | "stroke_head" | "stroke_path"
        | "stroke_text" | "svg_path" | "text_layout" | "text_outline" | "text_path" | "text_points"
        | "then" | "there_and_back" | "there_and_back_with_pause" | "time_of_beat" | "vnoise" | "voice"
        | "wiggle" | "window01" | "word" | "wrap" | "wrap10" => Some(Refused),
        _ => layer(name),
    }
}
