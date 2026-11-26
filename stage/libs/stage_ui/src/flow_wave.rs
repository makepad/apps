//! Shared GPU material for source pictures on cards and routed cables.
//! Prepared sources keep their existing textures. Short measured observations
//! share one atlas; every cable only supplies geometry and shader parameters.

use crate::music_wave::PreparedWaveHandle;
use makepad_stage_audio_wave::levels_for;
use makepad_widgets::*;
use std::rc::Rc;

/// Unthemed identity colours for measured sources. Drawing resolves these
/// against the current audio ground, alongside prepared stem materials.
pub const STEM_COLORS: [u32; 4] = makepad_look::palette::Palette::HOUSE.stem_colors();
const WIRE_UNITS_PER_SECOND: f64 = 96.0;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*
    let stage = mod.stage

    mod.widgets.StageFlowWaveDraw = set_type_default() do #(DrawFlowWave::script_shader(vm)){
        ..mod.draw.DrawQuad
        tiles: texture_2d(float)
        layers: texture_2d(float)
        size: uniform(vec4(1.0, 1.0, 0.0, 0.0))
        lo: uniform(vec4(0.0, 1.0, 1.0, 0.0))
        hi: uniform(vec4(0.0, 1.0, 1.0, 0.0))
        gains: uniform(vec4(1.0, 1.0, 1.0, 1.0))
        // overall gain, genuine layers, snapshot, played/source column
        state: uniform(vec4(1.0, 0.0, 0.0, 0.0))
        color: uniform(stage.info)
        active: uniform(1.0)
        opacity: uniform(1.0)
        light_theme: uniform(0.0)
        c0: uniform(stage.wave_vocals)
        c1: uniform(stage.wave_drums)
        c2: uniform(stage.wave_bass)
        c3: uniform(stage.wave_other)

        tile_at: fn(column: float, info: vec4) -> vec4 {
            let col = clamp(floor(column / info.z), 0.0, max(info.y - 1.0, 0.0))
            let row = floor(col / self.size.x)
            return self.tiles.sample(vec2((col - row * self.size.x + 0.5) / self.size.x, (info.x + row + 0.5) / self.size.y))
        }
        layer_at: fn(column: float, info: vec4) -> vec4 {
            let col = clamp(floor(column / info.z), 0.0, max(info.y - 1.0, 0.0))
            let row = floor(col / self.size.x)
            return self.layers.sample(vec2((col - row * self.size.x + 0.5) / self.size.x, (info.x + row + 0.5) / self.size.y))
        }
        pixel: fn() {
            let point = self.pos * self.rect_size
            let start = self.segment.xy
            let delta = self.segment.zw - start
            let length2 = max(dot(delta, delta), 0.0001)
            let direction = delta / sqrt(length2)
            let begin = dot(point - start, self.join.xy) / max(dot(direction, self.join.xy), 0.01)
            let end = dot(self.segment.zw - point, self.join.zw) / max(dot(direction, self.join.zw), 0.01)
            // Shared miter planes close the outside of every bend. Distance
            // between those planes keeps the endpoint source column identical
            // on both sides of a join, without repeated caps or phase jumps.
            if begin < 0.0 || end < 0.0 { return vec4(0.0) }
            let t = begin / max(begin + end, 0.0001)
            let column = mix(self.along.x, self.along.y, t)
            if column < 0.0 || column >= self.size.z { return vec4(0.0) }
            let distance = abs(delta.x * (point.y - start.y) - delta.y * (point.x - start.x)) / sqrt(length2)
            let y = distance / max(self.radius, 0.1)
            let feather = 1.0 / max(self.radius, 1.0)
            let base = floor(column - 0.5)
            let fraction = column - 0.5 - base
            let fine = self.tile_at(base, self.lo).mix(self.tile_at(base + 1.0, self.lo), fraction)
            let coarse = self.tile_at(column, self.lo).mix(self.tile_at(column, self.hi), self.lo.w)
            let tile = coarse.mix(fine, step(self.lo.z, 1.0) * (1.0 - step(0.0001, self.lo.w)))
            let raw = self.layer_at(column, self.lo).mix(self.layer_at(column, self.hi), self.lo.w)
            let total = raw.x + raw.y + raw.z + raw.w
            let separated = self.state.y * step(0.004, total)
            // Prepared level is the donor's normalized source envelope.
            // Snapshots contain measured peak magnitude with no AGC.
            let level = clamp(tile.w * self.state.x, 0.0, 1.0) * 0.78
            let share = raw * (level / max(total, 0.0001)) * self.gains
            let e0 = mix(level, share.z, separated)
            let e1 = mix(level, share.z + share.y, separated)
            let e2 = mix(level, share.z + share.y + share.x, separated)
            let e3 = mix(level, share.z + share.y + share.x + share.w, separated)
            let a0 = 1.0 - smoothstep(e0 - feather, e0 + feather, y)
            let a1 = 1.0 - smoothstep(e1 - feather, e1 + feather, y)
            let a2 = 1.0 - smoothstep(e2 - feather, e2 + feather, y)
            let a3 = 1.0 - smoothstep(e3 - feather, e3 + feather, y)
            let measured = tile.xyz / max(max(tile.x, max(tile.y, tile.z)), 0.0001)
            let grey = dot(measured, vec3(0.299, 0.587, 0.114))
            let tint = self.color.xyz + (measured - vec3(grey)) * 0.24 * (1.0 - self.state.z)
            let plain = vec4(tint, 1.0)
            let band = plain.mix(self.c2, separated) * a0
                + plain.mix(self.c1, separated) * (a1 - a0)
                + plain.mix(self.c0, separated) * (a2 - a1)
                + plain.mix(self.c3, separated) * (a3 - a2)
            let cover = clamp(a3, 0.0, 1.0) * step(0.000001, e3)
            let energy = clamp(e3 * 1.35, 0.0, 1.0)
            let glow = energy * energy * (1.0 - smoothstep(0.0, e3 + 0.001, y)) * mix(0.18, 0.45, separated)
            let played = mix(1.0, 0.58, step(column, self.state.w))
            let brightness = played * mix(0.45, 1.0, self.active)
            let lit = (band.xyz + vec3(glow)).mix(band.xyz * (1.0 - glow), self.light_theme)
            // The material stays transparent on cables. Light-theme played
            // and paused traces fade into their ground instead of going black.
            let alpha = cover * self.opacity * mix(1.0, brightness, self.light_theme)
            return vec4(lit * brightness * self.opacity * step(0.000001, e3), alpha)
        }
    }
    mod.widgets.StageFlowWave = set_type_default() do #(FlowWave::register_widget(vm)){
        width: Fill height: Fill
        draw_wave: mod.widgets.StageFlowWaveDraw{}
    }
}

#[derive(Script, ScriptHook)]
#[repr(C)]
struct DrawFlowWave {
    #[deref] draw_super: DrawQuad,
    #[live] segment: Vec4f,
    #[live] along: Vec2f,
    #[live] join: Vec4f,
    #[live(1.0)] radius: f32,
}

/// One GPU upload for every measured output in a sampling round. Materials
/// share rows of this atlas; a fanout never uploads the same observation twice.
pub struct FlowWaveAtlas {
    texture: Texture,
    columns: usize,
    rows: usize,
}

impl FlowWaveAtlas {
    pub fn new(cx: &mut Cx, columns: usize, rows: usize) -> Rc<Self> {
        let columns = columns.max(1);
        let rows = rows.max(1);
        Rc::new(Self { rows, columns, texture: Texture::new_with_format(cx, TextureFormat::VecRGBAf32 {
            width: columns, height: rows, data: Some(vec![0.0; rows * columns * 4]), updated: TextureUpdated::Full,
        }) })
    }

    pub fn update<const N: usize>(&self, cx: &mut Cx, rows: &[[[f32; 2]; N]]) {
        let mut data = self.texture.take_vec_f32(cx);
        data.resize(self.rows * self.columns * 4, 0.0);
        data.fill(0.0);
        for (row, bins) in rows.iter().take(self.rows).enumerate() {
            for (column, pair) in bins.iter().take(self.columns).enumerate() {
                let magnitude = pair.iter().filter(|value| value.is_finite()).fold(0.0_f32, |peak, value| peak.max(value.abs())).min(1.0);
                data[(row * self.columns + column) * 4 + 3] = magnitude;
            }
        }
        self.texture.put_back_vec_f32(cx, data, None);
    }
}

#[derive(Clone)]
enum WaveSource { Prepared(PreparedWaveHandle), Snapshot { atlas: Rc<FlowWaveAtlas>, row: usize, seconds: f64 } }

#[derive(Clone)]
pub struct FlowWaveMaterial {
    source: WaveSource,
    pub position: f64,
    pub window: f64,
    pub gains: [f32; 4],
    pub gain: f32,
    pub playing: bool,
    pub color: Vec4f,
}

impl FlowWaveMaterial {
    pub fn prepared(source: PreparedWaveHandle, position: f64, window: f64) -> Self {
        Self { source: WaveSource::Prepared(source), position, window, gains: [1.0; 4], gain: 1.0, playing: true, color: Vec4f::from_u32(0x8995a7ff) }
    }
    pub fn snapshot(atlas: Rc<FlowWaveAtlas>, row: usize, seconds: f64, color: Vec4f) -> Self {
        Self { source: WaveSource::Snapshot { atlas, row, seconds }, position: seconds, window: seconds, gains: [1.0; 4], gain: 1.0, playing: true, color }
    }
    pub fn same_source(&self, other: &Self) -> bool {
        match (&self.source, &other.source) {
            (WaveSource::Prepared(a), WaveSource::Prepared(b)) => a.same_source(b),
            (WaveSource::Snapshot {atlas:a, row:ar, ..}, WaveSource::Snapshot {atlas:b, row:br, ..}) => Rc::ptr_eq(a,b) && ar == br,
            _ => false,
        }
    }
    pub fn has_stems(&self) -> bool { matches!(&self.source, WaveSource::Prepared(handle) if handle.has_stems()) }
    fn columns_per_second(&self) -> f64 {
        match &self.source {
            WaveSource::Prepared(handle) => handle.0.source.cols_per_second,
            WaveSource::Snapshot {atlas,seconds, ..} => atlas.columns as f64 / seconds.max(0.000001),
        }
    }
}

impl DrawFlowWave {
    fn bind(&mut self, cx: &mut Cx2d, material: &FlowWaveMaterial, columns_per_pixel: f64) {
        let (width,height,cols,lo,hi,has_layers,snapshot) = match &material.source {
            WaveSource::Prepared(handle) => {
                let data = &handle.0;
                let pyramid = &data.source.bands;
                self.draw_vars.set_texture(0, &data.bands);
                self.draw_vars.set_texture(1, data.stems.as_ref().unwrap_or(&data.bands));
                let (a,sa,b,sb,mix) = levels_for(&pyramid.levels, columns_per_pixel);
                (pyramid.width,pyramid.height,pyramid.levels[0].cols,
                    [a.base_row as f32,a.cols as f32,sa as f32,mix as f32],
                    [b.base_row as f32,b.cols as f32,sb as f32,0.0],data.stems.is_some(),false)
            }
            WaveSource::Snapshot {atlas,row,..} => {
                self.draw_vars.set_texture(0, &atlas.texture);
                self.draw_vars.set_texture(1, &atlas.texture);
                (atlas.columns,atlas.rows,atlas.columns,[*row as f32,atlas.columns as f32,1.0,0.0],[*row as f32,atlas.columns as f32,1.0,0.0],false,true)
            }
        };
        self.draw_vars.set_uniform(cx, live_id!(size), &[width as f32,height as f32,cols as f32,0.0]);
        self.draw_vars.set_uniform(cx, live_id!(lo), &lo);
        self.draw_vars.set_uniform(cx, live_id!(hi), &hi);
        let finite = |v:f32| if v.is_finite() {v.clamp(0.0, 8.0)} else {0.0};
        self.draw_vars.set_uniform(cx, live_id!(gains), &material.gains.map(finite));
        self.draw_vars.set_uniform(cx, live_id!(state), &[finite(material.gain),has_layers as u8 as f32,snapshot as u8 as f32,(material.position * material.columns_per_second()) as f32]);
        let palette = makepad_look::palette::get(cx);
        let color = if snapshot { makepad_look::palette::on_wave(cx, material.color.to_u32()) } else if palette.wave_is_light() { palette.wave_muted } else { material.color.to_u32() };
        for (id, rgba) in [(live_id!(color), color), (live_id!(c0), palette.wave_vocals), (live_id!(c1), palette.wave_drums), (live_id!(c2), palette.wave_bass), (live_id!(c3), palette.wave_other)] {
            let color = Vec4f::from_u32(rgba);
            self.draw_vars.set_uniform(cx, id, &[color.x,color.y,color.z,color.w]);
        }
        self.draw_vars.set_uniform(cx, live_id!(light_theme), &[palette.wave_is_light() as u8 as f32]);
        self.draw_vars.set_uniform(cx, live_id!(active), &[if material.playing {1.0} else {0.55}]);
        self.draw_vars.set_uniform(cx, live_id!(opacity), &[1.0]);
    }

    fn segment(&mut self, cx: &mut Cx2d, a: DVec2, b: DVec2, from_column: f64, to_column: f64, radius: f64) {
        let direction=(b-a)/(b-a).length().max(0.0001);
        self.joined_segment(cx,a,b,from_column,to_column,radius,direction,direction);
    }

    fn joined_segment(&mut self, cx: &mut Cx2d, a: DVec2, b: DVec2, from_column: f64, to_column: f64, radius: f64, begin:DVec2,end:DVec2) {
        if (b-a).length() < 0.01 || !from_column.is_finite() || !to_column.is_finite() { return; }
        let pad = radius * 2.0 + 1.0;
        let origin = dvec2(a.x.min(b.x)-pad,a.y.min(b.y)-pad);
        let size = dvec2((a.x-b.x).abs()+pad*2.0,(a.y-b.y).abs()+pad*2.0);
        self.segment = Vec4f {x:(a.x-origin.x) as f32,y:(a.y-origin.y) as f32,z:(b.x-origin.x) as f32,w:(b.y-origin.y) as f32};
        self.along = Vec2f {x:from_column as f32,y:to_column as f32};
        self.join=Vec4f {x:begin.x as f32,y:begin.y as f32,z:end.x as f32,w:end.y as f32};
        self.radius = radius as f32;
        self.draw_abs(cx, Rect {pos:origin,size});
    }

    fn material_rect(&mut self, cx: &mut Cx2d, rect: Rect, material: &FlowWaveMaterial) {
        if rect.size.x <= 0.0 || rect.size.y <= 0.0 { return; }
        let rate = material.columns_per_second();
        let (start,span) = match &material.source {
            WaveSource::Snapshot {atlas,..} => (0.0,atlas.columns as f64),
            _ => ((material.position-material.window*0.5)*rate,material.window*rate),
        };
        self.bind(cx, material, span / rect.size.x.max(1.0));
        self.segment(cx,rect.pos+dvec2(0.0,rect.size.y*0.5),rect.pos+dvec2(rect.size.x,rect.size.y*0.5),start,start+span,rect.size.y*0.5);
    }
}

pub struct FlowWaveRenderer { draw: DrawFlowWave }

impl FlowWaveRenderer {
    pub fn new(cx: &mut Cx) -> Self {
        cx.with_vm(|vm| {
            let value = script_eval!(vm, { mod.widgets.StageFlowWaveDraw{} });
            Self { draw: DrawFlowWave::script_from_value(vm, value) }
        })
    }
    pub fn draw_rect(&mut self, cx: &mut Cx2d, rect: Rect, material: &FlowWaveMaterial) { self.draw.material_rect(cx,rect,material); }
    /// Points and cumulative lengths are authoritative render-local route
    /// geometry. Time scale is source-anchored, never stretched to wire length.
    pub fn draw_wire(&mut self, cx: &mut Cx2d, points: impl IntoIterator<Item=(DVec2,f64)>, material: &FlowWaveMaterial, radius: f64) {
        let radius = radius.clamp(1.0,4.0);
        let rate = material.columns_per_second();
        let columns_per_unit = rate / WIRE_UNITS_PER_SECOND;
        self.draw.bind(cx,material,columns_per_unit);
        let head = material.position * rate;
        let mut points = points.into_iter().peekable();
        let Some((mut previous,mut length)) = points.next() else {return};
        let mut incoming=None;
        while let Some((point,next_length))=points.next() {
            let direction=(point-previous)/(point-previous).length().max(0.0001);
            let outgoing=points.peek().map_or(direction,|(next,_)|(*next-point)/(*next-point).length().max(0.0001));
            let normal=|v:DVec2|v/v.length().max(0.0001);
            let begin=incoming.map_or(direction,|incoming|normal(incoming+direction));
            let end=normal(direction+outgoing);
            self.draw.joined_segment(cx,previous,point,head-length*columns_per_unit,head-next_length*columns_per_unit,radius,begin,end);
            incoming=Some(direction);
            previous=point; length=next_length;
        }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct FlowWave {
    #[uid] uid: WidgetUid,
    #[source] source: ScriptObjectRef,
    #[walk] walk: Walk,
    #[redraw] #[area] area: Area,
    #[live] draw_wave: DrawFlowWave,
    #[rust] material: Option<FlowWaveMaterial>,
}
impl FlowWaveRef {
    pub fn set_material(&self, cx: &mut Cx, material: Option<FlowWaveMaterial>) {
        if let Some(mut inner) = self.borrow_mut() { inner.material=material; inner.area.redraw(cx); }
    }
}
impl Widget for FlowWave {
    fn handle_event(&mut self, _cx: &mut Cx, _event: &Event, _scope: &mut Scope) {}
    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        let rect = cx.walk_turtle_with_area(&mut self.area,walk);
        if let Some(material) = &self.material { self.draw_wave.material_rect(cx,rect,material); }
        DrawStep::done()
    }
}
