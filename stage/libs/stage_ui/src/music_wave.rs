//! The donor DJ waveform renderer for both the scrolling lanes and overview.
//!
//! Its frequency bands, normalized envelope and genuine stem shares arrive as
//! worker-prepared pyramids. The original stack, ghost, glow and LOD equations
//! are retained from donors/stage/src/music_view.rs::DrawWaveLane. A deck owns
//! one uploaded texture pair, shared by both views; playback changes uniforms.

use crate::deck_strip::DeckStripState;
use makepad_widgets::*;
use makepad_stage_audio_wave::{levels_for, WaveData, WavePyramid};
use std::{rc::Rc, sync::Arc};

const WINDOW_SECONDS: f64 = 8.0;
const SCRUB_DRAG_POINTS: f64 = 4.0;
// The host samples at 30 Hz. Extrapolate only briefly between fresh samples;
// a stalled host freezes the picture instead of inventing continued playback.
const MAX_PREDICTION_SECONDS: f64 = 0.1;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*
    let stage = mod.stage

    set_type_default() do #(DrawMusicWaveLane::script_shader(vm)){
        ..mod.draw.DrawQuad
        tiles: texture_2d(float)
        stem_tiles: texture_2d(float)
        // A partly measured source: which of `tiles`' columns are measured
        // (red: the mixed wave, green: its stem colours), wrapped into rows
        // like the tiles; and the coarse overview with its own mask, shown
        // where this picture has no measurement.
        known_tiles: texture_2d(float)
        ov_tiles: texture_2d(float)
        ov_known_tiles: texture_2d(float)

        color_bg: uniform(stage.wave_bg)
        light_theme: uniform(0.0)
        // Before separation a wave is grey peaks and nothing else: colour
        // in this view always means a real separated stem.
        color_grey: uniform(stage.wave_muted)
        // How much colour a band reading may carry. Mirrored in Rust as
        // BAND_CHROMA, where the reasoning and the measurements live.
        band_chroma: uniform(0.24)
        color_grid: uniform(stage.wave_grid)
        color_grid_bar: uniform(stage.wave_bar)
        // The running loop, in the app-wide accent. Low alpha: a wash the
        // waveform stays readable through, not a fill that replaces it.
        color_loop: uniform(stage.wave_accent_wash)
        // The loop's span as (start_col, end_col, _, _). A UNIFORM and not
        // two `#[live]` fields: those become per-instance VERTEX INPUTS,
        // and this lane already sits on the vs_5_0 limit of 32 — two more
        // attributes fail the shader compile outright (X4506). Nothing
        // here varies per instance anyway.
        loop_span: uniform(#x00000000)
        // How hard the end-of-track warning is showing, 0..1. A UNIFORM
        // for the same reason `loop_span` is one: this lane sits ON the
        // vs_5_0 limit of 32 vertex inputs, and one more `#[live]` field
        // is one more per-instance attribute and no waveform at all on
        // Windows. It varies per LANE rather than per instance, and each
        // lane is its own draw, so a uniform carries it honestly.
        // The eye's gain on the envelope. 1.0 is the picture as it was
        // always drawn, which is what every surface that never sets it
        // goes on getting.
        wave_gain: uniform(1.0)
        // The three tone controls, unity until a deck says otherwise. The
        // strip never says otherwise: it is the reference picture.
        eq_gain: uniform(#xffffff00)
        warn: uniform(0.0)
        color_warn: uniform(stage.wave_warn)
        // A loop drag's would-be landing, same encoding as `loop_span`.
        // Drawn dimmer beside the ghost so the operator sees both where
        // the loop IS and where release will put it.
        preview_span: uniform(#x00000000)
        // (mask width, mask rows, on, _): when on, `known_tiles` says which
        // columns are measured; an unmeasured column draws the overview, or a
        // dashed rule, never a silent flat wave. Uniforms for the same vertex
        // input limit as `loop_span`.
        known_span: uniform(#x00000000)
        // The overview fallback: (texture width, texture rows, columns, on)
        // and (mask width, mask rows, overview columns per column of this
        // picture, overview column of this picture's column 0).
        ov_tex: uniform(#x00000000)
        ov_map: uniform(#x00000000)
        color_head: uniform(stage.wave_ink)
        // Where the playhead sits across the lane's width, 0..1. A
        // uniform, not an instance: this shader is already at D3D11's
        // vs_5_0 32-input ceiling (see the stem palette below), and one
        // more instance field is "no waveform at all on Windows" again.
        head_fraction: uniform(0.5)
        // The stem palette, pushed from STEM_COLORS every draw so the
        // waveform and the knobs cannot disagree. Uniforms, not instances:
        // they are per-draw constants, and as instances they blew the
        // D3D11 vs_5_0 32-input limit.
        color_vocals: uniform(#fff)
        color_drums: uniform(#fff)
        color_bass: uniform(#fff)
        color_other: uniform(#fff)

        mask_at: fn(column: float) -> vec4 {
            let c = floor(column)
            let wrap = floor(c / self.known_span.x)
            let u = (c - wrap * self.known_span.x + 0.5) / self.known_span.x
            return self.known_tiles.sample(vec2(u, (wrap + 0.5) / self.known_span.y))
        }

        ov_mask_at: fn(column: float) -> vec4 {
            let c = floor(column)
            let wrap = floor(c / self.ov_map.x)
            let u = (c - wrap * self.ov_map.x + 0.5) / self.ov_map.x
            return self.ov_known_tiles.sample(vec2(u, (wrap + 0.5) / self.ov_map.y))
        }

        // The overview's finest level at one of its columns.
        ov_at: fn(column: float) -> vec4 {
            let c = clamp(floor(column), 0.0, max(self.ov_tex.z - 1.0, 0.0))
            let wrap = floor(c / self.ov_tex.x)
            let u = (c - wrap * self.ov_tex.x + 0.5) / self.ov_tex.x
            return self.ov_tiles.sample(vec2(u, (wrap + 0.5) / self.ov_tex.y))
        }

        // One column of one pyramid level. Each level is stored as its own
        // block of rows in the same texture, so a level is a row offset and
        // a column count — hand-encoded mips. `xyz` are the bands, `w` is
        // the column's level against the whole track: its height.
        level_at: fn(column: float, base_row: float, level_cols: float, scale: float) -> vec4 {
            let c = clamp(floor(column / scale), 0.0, max(level_cols - 1.0, 0.0))
            let wrap = floor(c / self.tex_w)
            let u = (c - wrap * self.tex_w + 0.5) / self.tex_w
            let v = (base_row + wrap + 0.5) / self.tex_h
            return self.tiles.sample(vec2(u, v))
        }

        // What this PIXEL covers, alias-free at any zoom. Each level is a
        // max-reduction of the one below, so a transient never disappears
        // as the view pulls back; the two levels either side of the current
        // scale are blended so zooming does not pop.
        tile_span: fn(column: float) -> vec4 {
            let lo = self.level_at(column, self.lo_row, self.lo_cols, self.lo_scale)
            if self.lod_blend <= 0.0 {
                if self.lo_scale <= 1.0 {
                    // Zoomed past one column per pixel: interpolate along
                    // the finest level so the envelope stays smooth. A
                    // column MEASURES a hop, so its value belongs at the
                    // hop's centre — sampling on column boundaries instead
                    // slides the whole waveform half a column (5 ms) left of
                    // the beat grid and of the stem colours, which are drawn
                    // by the un-interpolated path just below.
                    let base = floor(column - 0.5)
                    let f = column - 0.5 - base
                    let a = self.level_at(base, self.lo_row, self.lo_cols, 1.0)
                    let b = self.level_at(base + 1.0, self.lo_row, self.lo_cols, 1.0)
                    return a * (1.0 - f) + b * f
                }
                return lo
            }
            let hi = self.level_at(column, self.hi_row, self.hi_cols, self.hi_scale)
            return lo * (1.0 - self.lod_blend) + hi * self.lod_blend
        }

        // The same column of the stem pyramid — identical layout, so the
        // level selection above serves both.
        stem_level_at: fn(column: float, base_row: float, level_cols: float, scale: float) -> vec4 {
            let c = clamp(floor(column / scale), 0.0, max(level_cols - 1.0, 0.0))
            let wrap = floor(c / self.tex_w)
            let u = (c - wrap * self.tex_w + 0.5) / self.tex_w
            let v = (base_row + wrap + 0.5) / self.tex_h
            return self.stem_tiles.sample(vec2(u, v))
        }

        stem_span: fn(column: float) -> vec4 {
            let lo = self.stem_level_at(column, self.lo_row, self.lo_cols, self.lo_scale)
            if self.lod_blend <= 0.0 {
                return lo
            }
            let hi = self.stem_level_at(column, self.hi_row, self.hi_cols, self.hi_scale)
            return lo * (1.0 - self.lod_blend) + hi * self.lod_blend
        }

        // Beat and bar rulings, drawn UNDER the waveform.
        grid_at: fn(column: float) -> vec4 {
            if self.beat_cols < 0.5 {
                return vec4(0.0, 0.0, 0.0, 0.0)
            }
            let b = (column - self.beat_phase) / self.beat_cols
            let nb = floor(b + 0.5)
            let d = abs(b - nb) * self.beat_cols / max(self.cols_per_px, 0.0001)
            let is_bar = step(modf(nb + 4096.0, 4.0), 0.5)
            let half = mix(0.45, 1.0, is_bar)
            let a = 1.0 - smoothstep(half, half + 1.0, d)
            let c = self.color_grid.mix(self.color_grid_bar, is_bar)
            return vec4(c.x, c.y, c.z, c.w * a)
        }

        // The running loop: a translucent wash plus one-pixel bracket
        // edges. The short top/bottom caps make IN and OUT read as a pair,
        // not as two unrelated grid rules.
        loop_at: fn(column: float) -> float {
            if self.loop_span.y <= self.loop_span.x {
                return 0.0
            }
            let inside = step(self.loop_span.x, column) * step(column, self.loop_span.y)
            let scale = max(self.cols_per_px, 0.0001)
            let from_in = (column - self.loop_span.x) / scale
            let from_out = (self.loop_span.y - column) / scale
            let edge_in = 1.0 - smoothstep(0.5, 1.0, abs(from_in))
            let edge_out = 1.0 - smoothstep(0.5, 1.0, abs(from_out))
            let y = min(self.pos.y, 1.0 - self.pos.y) * self.rect_size.y
            let cap_y = 1.0 - smoothstep(0.5, 1.0, y)
            let cap_in = step(0.0, from_in) * step(from_in, 6.0)
            let cap_out = step(0.0, from_out) * step(from_out, 6.0)
            let bracket = max(max(edge_in, edge_out), cap_y * max(cap_in, cap_out))
            return max(inside * self.color_loop.w, bracket)
        }

        // A drag's would-be landing: the same band at reduced weight, so
        // the ghost (the loop still playing) stays the louder of the two.
        preview_at: fn(column: float) -> float {
            if self.preview_span.y <= self.preview_span.x {
                return 0.0
            }
            let inside = step(self.preview_span.x, column) * step(column, self.preview_span.y)
            let de = min(abs(column - self.preview_span.x), abs(column - self.preview_span.y))
            let edge = 1.0 - smoothstep(0.5, 1.8, de / max(self.cols_per_px, 0.0001))
            return max(inside * self.color_loop.w, edge * 0.7) * 0.6
        }

        pixel: fn() {
            let px = self.pos.x * self.rect_size.x
            let column = self.centre_col + (px - self.rect_size.x * self.head_fraction) * self.cols_per_px
            let bg = self.color_bg
            // No track: a quiet centre rule where the waveform will be.
            if self.cols < 1.0 {
                let y0 = abs(self.pos.y - 0.5) * self.rect_size.y
                return bg.mix(self.color_grid, 1.0 - smoothstep(0.5, 1.5, y0))
            }
            if self.known_span.z > 0.5 {
                let inside = column >= 0.0 && column < self.cols
                let measured = inside && self.mask_at(column).x > 0.5
                if !measured {
                    let y0 = abs(self.pos.y - 0.5) * self.rect_size.y
                    // The overview at this same time, where it is measured:
                    // a plain grey envelope of its level.
                    if self.ov_tex.w > 0.5 {
                        let oc = column * self.ov_map.z + self.ov_map.w
                        if oc >= 0.0 && oc < self.ov_tex.z && self.ov_mask_at(oc).x > 0.5 {
                            let h = self.ov_at(oc).w * self.rect_size.y * 0.5
                            return bg.mix(self.color_grey, 1.0 - smoothstep(h - 0.5, h + 0.5, y0))
                        }
                    }
                    let dash = step(0.5, fract(px / 8.0))
                    return bg.mix(self.color_grid, (1.0 - smoothstep(0.5, 1.5, y0)) * dash)
                }
            }
            if column < 0.0 || column >= self.cols {
                return bg
            }
            let t = self.tile_span(column)
            // What the TONE CONTROLS have left of this column. The three
            // band readings are already in this texture and were only ever
            // used for colour; they are shares of one column, so the share
            // of them the EQ lets through is the share of the column that
            // is still being heard. Cutting the bass shortens a bass-heavy
            // column and leaves a bright one where it was, which is the
            // difference a broadband scale cannot show.
            let bands = t.x + t.y + t.z
            let kept = t.x * self.eq_gain.x + t.y * self.eq_gain.y + t.z * self.eq_gain.z
            let eq_w = clamp(mix(1.0, kept / max(bands, 0.0001), step(0.004, bands)), 0.0, 1.0)
            // THE HEIGHT OF A COLUMN IS HOW LOUD THE TRACK IS THERE. The
            // level channel was normalized once, against the whole track,
            // when the tiles were built. A quiet intro draws short and a
            // drop draws tall, in the grey region and in the separated one
            // alike, so the seam between them is invisible in height and
            // only the colouring changes.
            //
            // One hand may raise it and it is the operator's: the visual
            // gain is the EYE's and not the mix's, it moves nothing that
            // can be heard, and it is clamped before the envelope so a
            // lifted column saturates flat at the half-lane rather than
            // drawing outside it. At 1.0 this line is what it always was.
            let ungained = clamp(t.w * self.wave_gain, 0.0, 1.0) * 0.78
            let level = ungained * eq_w

            // A column the separator has reached is coloured by WHAT it is;
            // one it has not is a single honest grey. Both are the same
            // mirrored, layered envelope, so the picture only gains meaning
            // as the separation catches up — it never jumps.
            let raw = self.stem_span(column)
            let present = raw.x + raw.y + raw.z + raw.w
            let separated = step(0.004, present) * self.has_stems

            // The stems PARTITION that height in proportion to what each
            // one contributes to the column — they never scale it up. A
            // killed stem takes its share away with it, so the shape of the
            // wave is the shape of what the deck will play; the overview
            // strip passes ones here and stays the reference picture.
            let inverse = 1.0 / max(present, 0.0001)
            let grey_h = level
            let s_bass = level * raw.z * inverse * self.gain_bass
            let s_drums = level * raw.y * inverse * self.gain_drums
            let s_vocals = level * raw.x * inverse * self.gain_vocals
            let s_other = level * raw.w * inverse * self.gain_other

            let e0 = mix(grey_h, s_bass, separated)
            let e1 = mix(grey_h, s_bass + s_drums, separated)
            let e2 = mix(grey_h, s_bass + s_drums + s_vocals, separated)
            let e3 = mix(grey_h, s_bass + s_drums + s_vocals + s_other, separated)

            // A column the separator has NOT reached is coloured by its
            // three bands, which have been in this texture all along and
            // ignored: red is low, green is mid, blue is high. The balance
            // of the three picks a hue; the hue is all it says.
            //
            // Held at the grey's own brightness, because brightness in this
            // lane already means played-or-coming and active-or-parked, and
            // capped well short of the stem palette's colourfulness, because
            // the one thing this must never do is let an unseparated record
            // pass for a separated one. The other half of that is structural
            // and stronger: a separated column is a STACK of up to four
            // colours with edges, and this is one flat tone from the centre
            // to the tip.
            let top = max(t.x, max(t.y, t.z))
            let unit = vec3(t.x, t.y, t.z) / max(top, 0.0001)
            let mid = unit.x * 0.299 + unit.y * 0.587 + unit.z * 0.114
            let off = unit - vec3(mid, mid, mid)
            let spread = max(off.x, max(off.y, off.z)) - min(off.x, min(off.y, off.z))
            let grey_y = self.color_grey.x * 0.299
                + self.color_grey.y * 0.587
                + self.color_grey.z * 0.114
            let toned = vec3(grey_y, grey_y, grey_y)
                + off * min(1.0, self.band_chroma / max(spread, 0.0001))
            // Nothing measured, nothing to say: the old grey stands.
            let measured = step(0.004, top) * step(0.000001, spread)
            let plain = vec4(
                mix(self.color_grey.x, toned.x, measured),
                mix(self.color_grey.y, toned.y, measured),
                mix(self.color_grey.z, toned.z, measured),
                1.0
            )

            let c0 = plain.mix(self.color_bass, separated)
            let c1 = plain.mix(self.color_drums, separated)
            let c2 = plain.mix(self.color_vocals, separated)
            let c3 = plain.mix(self.color_other, separated)

            // Half-pixel feathering: the envelope edge stays smooth while
            // the whole thing scrolls, instead of crawling pixel to pixel.
            let y = abs(self.pos.y - 0.5) * 2.0
            let feather = 2.0 / max(self.rect_size.y, 2.0)
            // The GHOST: where a knob has taken height away, the envelope
            // that would have been drawn stands behind the live one. It is
            // the only way to see what a cut is doing rather than only
            // what is left, and it costs nothing when nothing is cut --
            // with every knob up it is exactly under the live envelope and
            // its own term is zero.
            let in_ghost = 1.0 - smoothstep(ungained - feather, ungained + feather, y)
            let in0 = 1.0 - smoothstep(e0 - feather, e0 + feather, y)
            let in1 = 1.0 - smoothstep(e1 - feather, e1 + feather, y)
            let in2 = 1.0 - smoothstep(e2 - feather, e2 + feather, y)
            let in3 = 1.0 - smoothstep(e3 - feather, e3 + feather, y)

            let band = c0 * in0
                + c1 * (in1 - in0)
                + c2 * (in2 - in1)
                + c3 * (in3 - in2)
            let cover = clamp(max(in3, in2), 0.0, 1.0)

            // Loud passages glow on dark glass and deepen on a light ground.
            let energy = clamp(e3 * 1.35, 0.0, 1.0)
            let glow = energy * energy * (1.0 - smoothstep(0.0, e3 + 0.001, y)) * mix(0.18, 0.45, separated)
            let lit = (band.xyz + vec3(glow)).mix(band.xyz * (1.0 - glow), self.light_theme)

            // Behind the playhead the music has been played; ahead of it is
            // what is coming, and that reads brighter. WHERE the playhead
            // is depends on the surface: the zoomed lanes scroll so it sits
            // at the window centre, but the overview strip fits the whole
            // track and carries its own head — using centre_col there dimmed
            // a fixed left half of the song whatever was playing.
            let played_ref = mix(self.centre_col, self.head_col, self.head_on)
            let played = step(column, played_ref)
            let level = mix(1.0, 0.58, played) * mix(0.45, 1.0, self.active)

            let g = self.grid_at(column)
            let under = bg.mix(vec4(g.x, g.y, g.z, 1.0), g.w)
            let wave_rgb = (lit * level).mix(bg.xyz.mix(lit, level), self.light_theme)
            let wave = vec4(wave_rgb, 1.0)
            // The ghost goes under the live envelope and is drawn in the
            // column's own uncoloured tone: a silhouette says WHERE the
            // wave was, and saying it in the stem colours would have it
            // read as a second, quieter mix.
            let ghost_only = clamp(in_ghost - cover, 0.0, 1.0)
            let ghost_rgb = (plain.xyz * level).mix(bg.xyz.mix(plain.xyz, level), self.light_theme)
            let ghost = vec4(ghost_rgb, 1.0)
            let grounded = under.mix(ghost, ghost_only * 0.30)
            let body = grounded.mix(wave, cover)
            // A whisper of the rulings survives on top, so the two decks
            // can be read against each other through a loud passage.
            let ruled = body.mix(vec4(g.x, g.y, g.z, 1.0), g.w * 0.30)
            // The overview and a stationary loop lane carry an in-shader
            // playhead; normally the zoomed lanes share the centre overlay.
            // The loop sits over the picture, under the playhead: you have
            // to be able to see the band through a loud passage.
            let la = max(self.loop_at(column), self.preview_at(column))
            let banded = ruled.mix(vec4(self.color_loop.x, self.color_loop.y, self.color_loop.z, 1.0), la)
            // The end-of-track warning, UNDER the playhead so the head
            // stays crisp, and capped well short of opaque: the last
            // thirty seconds of a record are exactly when the picture
            // most needs reading, so this has to be impossible to miss
            // without painting over the thing being watched.
            let warned = banded.mix(self.color_warn, self.warn * 0.38)
            let hd = abs(column - self.head_col) / max(self.cols_per_px, 0.0001)
            let ha = (1.0 - smoothstep(0.5, 1.8, hd)) * self.head_on
            return warned.mix(self.color_head, ha)
        }
    }


    mod.widgets.StageMusicWaveBase = #(MusicWave::register_widget(vm))
    mod.widgets.StageMusicWave = set_type_default() do mod.widgets.StageMusicWaveBase{
        width: Fill height: Fill
        draw_bg +: {color: stage.wave_ruler_bg}
        draw_text +: {
            color: stage.wave_muted
            text_style: theme.font_bold{font_size: 8}
        }
        // The donor's two-point core and quiet halo, shared by both lanes.
        draw_head +: {
            color: uniform(stage.wave_head)
            glow: uniform(stage.wave_glow)
            pixel: fn() {
                let d = abs((self.pos.x - 0.5) * self.rect_size.x)
                let core = 1.0 - smoothstep(0.8, 1.6, d)
                let halo = (1.0 - smoothstep(1.5, 7.0, d)) * 0.30
                let a = clamp(core + halo, 0.0, 1.0)
                let c = self.glow.mix(self.color, core)
                return vec4(c.x * a, c.y * a, c.z * a, a)
            }
        }
    }
}

/// One zoomed lane. Only `#[live]` instance fields sit after the `#[deref]`,
/// per the draw-shader layout law.
#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawMusicWaveLane {
    #[deref]
    pub draw_super: DrawQuad,
    /// Tile-texture dimensions, in texels.
    #[live(1.0)]
    pub tex_w: f32,
    #[live(1.0)]
    pub tex_h: f32,
    /// Valid tile columns for this track, at the finest level.
    #[live]
    pub cols: f32,
    /// Finer of the two pyramid levels in play: first row, column count,
    /// and how many finest-level columns each of its columns covers.
    #[live]
    pub lo_row: f32,
    #[live(1.0)]
    pub lo_cols: f32,
    #[live(1.0)]
    pub lo_scale: f32,
    /// The next level up, and the blend between them.
    #[live]
    pub hi_row: f32,
    #[live(1.0)]
    pub hi_cols: f32,
    #[live(2.0)]
    pub hi_scale: f32,
    #[live]
    pub lod_blend: f32,
    /// The tile column under the playhead.
    #[live]
    pub centre_col: f32,
    /// Zoom: tile columns per screen pixel.
    #[live(1.0)]
    pub cols_per_px: f32,
    /// Beat period in tile columns; 0 = no grid yet.
    #[live]
    pub beat_cols: f32,
    /// A column that is a downbeat, so bars rule where the music does.
    #[live]
    pub beat_phase: f32,
    /// 1 for a deck that is playing, less for a parked one.
    #[live(1.0)]
    pub active: f32,
    /// 1 once a stem texture is bound; 0 keeps the band colouring.
    ///
    /// The stem palette lives as four UNIFORMS in the script registration
    /// (`color_vocals`..`color_other`), pushed via `set_uniform` every
    /// draw — as instance inputs they tipped this shader over D3D11's
    /// vs_5_0 limit of 32 vertex inputs (36 > 32: no waveform at all on
    /// Windows). They are per-draw constants, so uniforms are their
    /// honest storage class anyway.
    #[live]
    pub has_stems: f32,
    /// The stem knobs, so the wave shows the mix that will play.
    #[live(1.0)]
    pub gain_vocals: f32,
    #[live(1.0)]
    pub gain_drums: f32,
    #[live(1.0)]
    pub gain_bass: f32,
    #[live(1.0)]
    pub gain_other: f32,
    /// Playhead column and whether to draw it in-shader (the overview does;
    /// the zoomed lanes share one drawn across both).
    #[live]
    pub head_col: f32,
    #[live]
    pub head_on: f32,
}


/// The immutable uploaded picture of one source. Its two consumers share
/// these handles; resizing, scrolling and reapplying a theme upload nothing.
/// Rc expresses that GPU objects stay on the UI thread. Only WaveData crosses
/// from the analysis worker, through the parent's existing Arc channel.
/// Where a partly measured picture's column 0 sits on its absolute column
/// grid (its `cols_per_second`), and every run of columns measured there:
/// the mixed wave and, separately, its stem colours. Half-open runs in
/// absolute columns, as the analysis reports them.
#[derive(Clone, Debug)]
pub struct WaveCoverage {
    pub origin_col: f64,
    pub known: Arc<[(u64, u64)]>,
    pub stems_known: Arc<[(u64, u64)]>,
}

/// Mask rows are this wide; a picture of 8192 columns takes 8 rows.
const MASK_WIDTH: usize = 1024;

/// A partly measured picture uploaded once: its pyramids, its coverage mask
/// and where it sits. Shared by every view that shows the same report.
pub(crate) struct PlacedWave {
    pub(crate) textures: Rc<MusicWaveTextures>,
    pub(crate) coverage: WaveCoverage,
    pub(crate) mask: Texture,
    pub(crate) mask_w: usize,
    pub(crate) mask_h: usize,
}

impl PlacedWave {
    pub(crate) fn upload(cx: &mut Cx, source: Arc<WaveData>, coverage: WaveCoverage) -> Option<Rc<Self>> {
        let cols = source.bands.levels.first().map_or(0, |level| level.cols);
        let textures = MusicWaveTextures::upload(cx, source)?;
        let mask_w = cols.clamp(1, MASK_WIDTH);
        let mask_h = cols.div_ceil(mask_w).max(1);
        let mut data = vec![0xff00_0000u32; mask_w * mask_h];
        let origin = coverage.origin_col.max(0.0) as u64;
        for (runs, bit) in [(&coverage.known, 0x00ff_0000u32), (&coverage.stems_known, 0x0000_ff00u32)] {
            for &(start, end) in runs.iter() {
                let start = start.saturating_sub(origin).min(cols as u64) as usize;
                let end = end.saturating_sub(origin).min(cols as u64) as usize;
                for texel in &mut data[start..end.max(start)] {
                    *texel |= bit;
                }
            }
        }
        let mask = Texture::new_with_format(cx, TextureFormat::VecBGRAu8_32 {
            width: mask_w, height: mask_h, data: Some(data), updated: TextureUpdated::Full,
        });
        Some(Rc::new(Self { textures, coverage, mask, mask_w, mask_h }))
    }

    pub(crate) fn shows(&self, source: &Arc<WaveData>, coverage: &WaveCoverage) -> bool {
        Arc::ptr_eq(&self.textures.source, source)
            && Arc::ptr_eq(&self.coverage.known, &coverage.known)
            && Arc::ptr_eq(&self.coverage.stems_known, &coverage.stems_known)
            && self.coverage.origin_col == coverage.origin_col
    }
}

pub(crate) struct MusicWaveTextures {
    pub(crate) source: Arc<WaveData>,
    pub(crate) bands: Texture,
    pub(crate) stems: Option<Texture>,
}

/// A UI-thread handle to one prepared source's uploaded pyramids. Cloning it
/// shares textures; it neither samples audio nor uploads another copy.
#[derive(Clone)]
pub struct PreparedWaveHandle(pub(crate) Rc<MusicWaveTextures>);

impl PreparedWaveHandle {
    pub fn same_source(&self, other: &Self) -> bool { Rc::ptr_eq(&self.0, &other.0) }
    pub fn has_stems(&self) -> bool { self.0.stems.is_some() }
}

impl MusicWaveTextures {
    pub(crate) fn upload(cx: &mut Cx, source: Arc<WaveData>) -> Option<Rc<Self>> {
        let bands = &source.bands;
        if !valid_pyramid(bands) || !source.cols_per_second.is_finite() || source.cols_per_second <= 0.0 {
            return None;
        }
        let upload = |cx: &mut Cx, pyramid: &WavePyramid| Texture::new_with_format(cx,
            TextureFormat::VecBGRAu8_32 {
                width: pyramid.width,
                height: pyramid.height,
                data: Some(pyramid.data.clone()),
                updated: TextureUpdated::Full,
            },
        );
        let band_texture = upload(cx, bands);
        // A stem pyramid must occupy exactly the same rows and hops. An
        // incompatible optional sidecar cannot lend a different instant's
        // colour to this track; the measured frequency-band picture remains.
        let stems = source.stems.as_ref().filter(|stems| {
            valid_pyramid(stems) && stems.width == bands.width && stems.height == bands.height
                && stems.levels == bands.levels
        }).map(|stems| upload(cx, stems));
        Some(Rc::new(Self { source, bands: band_texture, stems }))
    }
}

fn valid_pyramid(pyramid: &WavePyramid) -> bool {
    pyramid.width > 0 && pyramid.height > 0
        && pyramid.width.checked_mul(pyramid.height) == Some(pyramid.data.len())
        && pyramid.levels.first().is_some_and(|level| level.cols > 0)
        && pyramid.levels.iter().all(|level| {
            level.cols > 0 && level.base_row.checked_add(level.cols.div_ceil(pyramid.width))
                .is_some_and(|end| end <= pyramid.height)
        })
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum MusicWaveAction {
    #[default]
    None,
    Grabbed,
    Seeked(f64),
    /// The captured press lost its source or could not produce a finite time.
    Cancelled,
    /// The scrolling lane's content is under the hand: it holds the record
    /// at this many seconds, either way, from the first drag step on.
    Scratch(f64),
    /// The hand let go of a scratch (or it was cancelled): no seek follows.
    ScratchEnd,
}

#[derive(Clone, Copy, Default, PartialEq)]
struct WaveState {
    deck: usize,
    loaded: bool,
    ready: bool,
    playing: bool,
    duration: f64,
    position: f64,
    speed: f64,
    loop_span: Option<(f64, f64)>,
    paged: Option<crate::deck_strip::PagedTransportStamp>,
    beat: Option<crate::deck_strip::DeckBeat>,
}

#[derive(Clone, Copy)]
struct WaveDrag {
    // A drag uses the picture it pressed, even while its preview recentres.
    centre: f64,
    down_x: f64,
    seconds_per_point: f64,
    click_target: f64,
    travelled: bool,
    target: f64,
    /// The paged source the hand took hold of: a refreshed picture of the
    /// same growing recording keeps the gesture, another source ends it.
    source: Option<u64>,
}

#[derive(Clone, Copy)]
struct PendingWaveSeek {
    target: f64,
    started: f64,
    // A repeated pre-release position is not evidence that the callback
    // applied the cue, even when it is very near the requested time.
    from_position: f64,
    // A paged source at release, and the exact seek request the application
    // issued for this preview: only that request's settlement ends it, never
    // timing, duration or another request's serial.
    paged: Option<(u64, Option<u64>)>,
}

#[derive(Script, ScriptHook, Widget)]
pub struct MusicWave {
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    #[layout]
    layout: Layout,
    #[redraw]
    #[area]
    area: Area,
    #[live]
    draw_bg: DrawColor,
    #[live]
    draw_lane: DrawMusicWaveLane,
    #[live]
    draw_rule: DrawColor,
    #[live]
    overview: bool,
    #[live]
    draw_head: DrawQuad,
    #[live]
    draw_text: DrawText,
    #[rust]
    state: WaveState,
    #[rust]
    textures: Option<Rc<MusicWaveTextures>>,
    /// A partly measured source's picture, and the overview this lane falls
    /// back to where the picture has no measurement; None for a whole track
    /// analysed from its start.
    #[rust]
    placed: Option<Rc<PlacedWave>>,
    #[rust]
    fallback: Option<Rc<PlacedWave>>,
    #[rust]
    stamp: f64,
    #[rust]
    drawn_centre: f64,
    #[rust]
    drag: Option<WaveDrag>,
    // Releasing a captured drag precedes the audio owner's next observation.
    // Keep that final picture until telemetry reaches it, or the host refuses.
    #[rust]
    pending_seek: Option<PendingWaveSeek>,
    #[rust]
    next_frame: NextFrame,
    #[rust]
    disabled: bool,
    #[rust]
    stem_levels: Option<[f32; 4]>,
}

impl MusicWave {
    /// Accepted effective gains in vocals/drums/bass/other order. This only
    /// paints the observed mix; the live flow bindings own all audio writes.
    pub fn set_stem_levels(&mut self, cx: &mut Cx, levels: [f32; 4]) {
        let levels = levels.map(|level| if level.is_finite() { level.clamp(0.0, 1.5) } else { 1.0 });
        if self.stem_levels != Some(levels) {
            self.stem_levels = Some(levels);
            self.area.redraw(cx);
        }
    }

    pub fn set_state(&mut self, cx: &mut Cx, deck: usize, state: &DeckStripState) {
        let duration = finite_nonnegative(state.duration);
        let position = finite_nonnegative(state.position).min(duration);
        let speed = finite_nonnegative(state.speed);
        let loop_span = (state.loop_enabled
            && state.loop_in.is_finite()
            && state.loop_out.is_finite()
            && state.loop_in >= 0.0
            && state.loop_out > state.loop_in
            && state.loop_out <= duration)
            .then_some((state.loop_in, state.loop_out));
        let next = WaveState {
            deck: deck.min(1),
            loaded: state.has_track,
            ready: state.ready,
            playing: state.playing && state.has_track,
            duration,
            position,
            speed,
            loop_span,
            paged: state.paged,
            beat: state.beat.filter(|beat| beat.beat_secs.is_finite() && beat.beat_secs > 1e-3 && beat.downbeat_secs.is_finite()),
        };
        if let Some((source, _)) = self.pending_seek.and_then(|pending| pending.paged) {
            // Paged: settled only by its own request (`settle_seek`).
            // Another source, eject or loss of the deck clears it; a growing
            // duration or any serial change does not.
            let replaced = next.paged.is_none_or(|now| now.source != source);
            if replaced || !next.loaded || !next.ready || next.deck != self.state.deck {
                self.pending_seek = None;
                self.area.redraw(cx);
            }
        } else if let Some(pending) = self.pending_seek {
            let elapsed = (cx.seconds_since_app_start() - pending.started).max(0.0);
            // cue_at is a control-bank write, while cue is a queued callback
            // packet. Its admission can precede several identical position
            // observations. Reconcile only a changed source observation on
            // the forward trajectory of the requested cue, not an old sample
            // anywhere in an expanding symmetric distance around it.
            let target = if let Some((start, end)) = next.loop_span {
                start + (pending.target - start).rem_euclid(end - start)
            } else { pending.target };
            let advance = if next.playing { elapsed * speed } else { 0.0 };
            let delta = position - target;
            let precision = 0.005; // source positions and cue_at are f32 seconds
            let forward = if let Some((start, end)) = next.loop_span {
                delta.rem_euclid(end - start)
            } else { delta };
            let reached = position != pending.from_position
                && (delta.abs() <= precision || (forward >= 0.0 && forward <= advance + precision));
            if reached || elapsed > 0.75 || !next.loaded || !next.ready
                || next.deck != self.state.deck || next.duration != self.state.duration
            {
                self.pending_seek = None;
                // Clearing a preview changes the picture even if the owner
                // sample itself stayed unchanged (paused/no-op cue or timeout).
                self.area.redraw(cx);
            }
        }
        // The hand holds exactly the source it took hold of: a replacement
        // (another paged source, or a clip on this deck changing) ends it.
        if let Some(drag) = self.drag {
            let replaced = match drag.source {
                Some(source) => next.paged.is_none_or(|now| now.source != source),
                None => next.paged.is_some() || next.duration != self.state.duration,
            };
            if replaced || next.deck != self.state.deck {
                self.cancel_drag(cx);
            }
        }
        if self.state != next {
            // Repeated unchanged samples are not fresh clock evidence.
            self.stamp = cx.seconds_since_app_start();
            self.state = next;
            self.area.redraw(cx);
        }
        if !self.state.loaded || !self.state.ready {
            self.cancel_drag(cx);
        }
    }

    /// Show a partly measured picture (its textures replace the whole-track
    /// ones), with an optional overview fallback.
    pub(crate) fn set_placed(&mut self, cx: &mut Cx, placed: Option<Rc<PlacedWave>>, fallback: Option<Rc<PlacedWave>>) {
        let same = |a: &Option<Rc<PlacedWave>>, b: &Option<Rc<PlacedWave>>| match (a, b) {
            (Some(a), Some(b)) => Rc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        if same(&self.placed, &placed) && same(&self.fallback, &fallback) {
            return;
        }
        let textures = placed.as_ref().map(|placed| placed.textures.clone());
        self.placed = placed;
        self.fallback = fallback;
        self.set_textures(cx, textures);
        self.redraw(cx);
    }

    pub(crate) fn set_textures(&mut self, cx: &mut Cx, textures: Option<Rc<MusicWaveTextures>>) {
        let same = match (&self.textures, &textures) {
            (Some(previous), Some(next)) => Rc::ptr_eq(previous, next),
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        self.textures = textures;
        self.pending_seek = None;
        // A growing recording's picture is refreshed while it records: the
        // hand stays on it. A different source (or a clip's new picture) ends it.
        let same = self.drag.is_some_and(|drag| drag.source.is_some() && drag.source == self.state.paged.map(|stamp| stamp.source));
        if !same {
            self.cancel_drag(cx);
        }
        self.area.redraw(cx);
    }

    fn cancel_drag(&mut self, cx: &mut Cx) {
        if let Some(drag) = self.drag.take() {
            // A scratch always gets its release, whatever ended the gesture.
            if !self.overview && drag.travelled {
                cx.widget_action(self.widget_uid(), MusicWaveAction::ScratchEnd);
            }
            cx.widget_action(self.widget_uid(), MusicWaveAction::Cancelled);
        }
    }

    fn position_at(&self, now: f64) -> f64 {
        if let Some(drag) = self.drag {
            return drag.target;
        }
        if let Some(pending) = self.pending_seek {
            // Handoff belongs to set_state's observation/reconciliation, not
            // a draw clock that can expire between an old and a fresh sample.
            return pending.target;
        }
        let state = self.state;
        // A paged source shows its accepted observation only: buffering,
        // faults and expiry must never paint drifting progress.
        if !state.playing || !state.ready || state.paged.is_some() {
            return state.position;
        }
        let elapsed = (now - self.stamp).clamp(0.0, MAX_PREDICTION_SECONDS);
        let ahead = state.position + elapsed * state.speed;
        // Like the donor, a loop captures only a sampled position already
        // inside it. A position approaching a loop must not teleport into it.
        if let Some((start, end)) = state.loop_span {
            if state.position >= start && state.position < end {
                return start + (ahead - start).rem_euclid(end - start);
            }
        }
        ahead.min(state.duration)
    }

    fn can_seek(&self) -> bool {
        !self.disabled && self.state.ready && self.state.loaded && self.state.duration > 0.0
    }

    fn time_at(&self, rect: Rect, abs: DVec2, centre: f64) -> Option<f64> {
        if !rect.contains(abs) || rect.size.x <= 0.0 {
            return None;
        }
        let fraction = (abs.x - rect.pos.x) / rect.size.x;
        let secs = if self.overview {
            fraction * self.state.duration
        } else {
            centre + (fraction - 0.5) * WINDOW_SECONDS
        };
        (secs.is_finite() && (0.0..=self.state.duration).contains(&secs)).then_some(secs)
    }

    fn drag_time_at(&self, rect: Rect, abs: DVec2, drag: WaveDrag) -> Option<f64> {
        if rect.size.x <= 0.0 || !abs.x.is_finite() { return None; }
        if self.overview {
            // A captured timeline gesture remains valid outside its visual
            // bounds. Pulling past either end lands exactly at that end.
            let fraction = ((abs.x - rect.pos.x) / rect.size.x).clamp(0.0, 1.0);
            return Some(fraction * self.state.duration);
        }
        let delta = abs.x - drag.down_x;
        let seconds = if drag.travelled || delta.abs() >= SCRUB_DRAG_POINTS {
            // Move the record under the fixed head: pulling the content
            // right brings an EARLIER sample to the centre, like the donor.
            drag.centre - delta * drag.seconds_per_point
        } else {
            drag.click_target
        };
        seconds.is_finite().then(|| seconds.clamp(0.0, self.state.duration))
    }

    fn draw_wave(&mut self, cx: &mut Cx2d, rect: Rect, position: f64) {
        let data = self.textures.as_ref().filter(|_| self.state.loaded);
        let cols = data.and_then(|data| data.source.bands.levels.first()).map_or(0, |level| level.cols);
        let cols_per_second = data.map_or(100.0, |data| data.source.cols_per_second);
        // A placed texture starts at `origin` columns of its grid; the
        // overview then spans the source's whole duration on that grid.
        let placement = self.placed.clone();
        let origin = placement.as_ref().map_or(0.0, |placed| placed.coverage.origin_col);
        let span_cols = self.state.duration * cols_per_second;
        let cols_per_px = if self.overview && placement.is_some() {
            span_cols / rect.size.x.max(1.0)
        } else if self.overview {
            cols as f64 / rect.size.x.max(1.0)
        } else {
            WINDOW_SECONDS * cols_per_second / rect.size.x.max(1.0)
        };
        let lane = &mut self.draw_lane;
        if let Some(data) = data {
            let bands = &data.source.bands;
            lane.draw_vars.set_texture(0, &data.bands);
            lane.tex_w = bands.width.max(1) as f32;
            lane.tex_h = bands.height.max(1) as f32;
            let (lo, lo_scale, hi, hi_scale, blend) = levels_for(&bands.levels, cols_per_px);
            lane.lo_row = lo.base_row as f32;
            lane.lo_cols = lo.cols.max(1) as f32;
            lane.lo_scale = lo_scale as f32;
            lane.hi_row = hi.base_row as f32;
            lane.hi_cols = hi.cols.max(1) as f32;
            lane.hi_scale = hi_scale as f32;
            lane.lod_blend = blend as f32;
            if let Some(stems) = &data.stems {
                lane.draw_vars.set_texture(1, stems);
                lane.has_stems = 1.0;
            } else {
                lane.draw_vars.empty_texture(1);
                lane.has_stems = 0.0;
            }
        } else {
            lane.draw_vars.empty_texture(0);
            lane.draw_vars.empty_texture(1);
            lane.tex_w = 1.0;
            lane.tex_h = 1.0;
            lane.lo_row = 0.0;
            lane.lo_cols = 1.0;
            lane.lo_scale = 1.0;
            lane.hi_row = 0.0;
            lane.hi_cols = 1.0;
            lane.hi_scale = 1.0;
            lane.lod_blend = 0.0;
            lane.has_stems = 0.0;
        }
        lane.cols = cols as f32;
        lane.cols_per_px = cols_per_px as f32;
        lane.centre_col = if self.overview && placement.is_some() {
            (span_cols * 0.5 - origin) as f32
        } else if self.overview {
            cols as f32 * 0.5
        } else {
            (position * cols_per_second - origin) as f32
        };
        // The overview spans exactly the source duration, including its last
        // partial analysis hop; the scrolling lane uses the analysis timebase.
        lane.head_col = if self.overview && placement.is_none() {
            (position / self.state.duration.max(f64::EPSILON) * cols as f64) as f32
        } else {
            (position * cols_per_second - origin) as f32
        };
        // Coverage: every measured run, and the overview where this picture
        // has none (the lane only; an overview is its own fallback).
        match &placement {
            Some(placed) => {
                lane.draw_vars.set_texture(2, &placed.mask);
                lane.draw_vars.set_uniform(cx, live_id!(known_span), &[placed.mask_w as f32, placed.mask_h as f32, 1.0, 0.0]);
            }
            None => {
                lane.draw_vars.empty_texture(2);
                lane.draw_vars.set_uniform(cx, live_id!(known_span), &[1.0, 1.0, 0.0, 0.0]);
            }
        }
        let fallback = self.fallback.as_ref().filter(|_| !self.overview && placement.is_some());
        match fallback {
            Some(ov) => {
                let ov_bands = &ov.textures.source.bands;
                let ov_cols = ov_bands.levels.first().map_or(0, |level| level.cols);
                let ratio = ov.textures.source.cols_per_second / cols_per_second.max(f64::EPSILON);
                lane.draw_vars.set_texture(3, &ov.textures.bands);
                lane.draw_vars.set_texture(4, &ov.mask);
                lane.draw_vars.set_uniform(cx, live_id!(ov_tex), &[ov_bands.width.max(1) as f32, ov_bands.height.max(1) as f32, ov_cols as f32, 1.0]);
                lane.draw_vars.set_uniform(cx, live_id!(ov_map), &[
                    ov.mask_w as f32, ov.mask_h as f32, ratio as f32, (origin * ratio - ov.coverage.origin_col) as f32,
                ]);
            }
            None => {
                lane.draw_vars.empty_texture(3);
                lane.draw_vars.empty_texture(4);
                lane.draw_vars.set_uniform(cx, live_id!(ov_tex), &[1.0, 1.0, 0.0, 0.0]);
                lane.draw_vars.set_uniform(cx, live_id!(ov_map), &[1.0, 1.0, 0.0, 0.0]);
            }
        }
        lane.head_on = if self.overview && cols > 0 { 1.0 } else { 0.0 };
        // The saved beat grid rules the scrolling lanes; across a whole
        // track in the overview its beats would be a solid wash. There is
        // no control-backed EQ yet.
        match self.state.beat.filter(|_| !self.overview && data.is_some()) {
            Some(beat) => {
                lane.beat_cols = (beat.beat_secs * cols_per_second) as f32;
                lane.beat_phase = (beat.downbeat_secs * cols_per_second - origin) as f32;
            }
            None => {
                lane.beat_cols = 0.0;
                lane.beat_phase = 0.0;
            }
        }
        lane.active = if self.state.playing { 1.0 } else if self.overview { 0.7 } else { 0.55 };
        let gains = if self.overview { [1.0; 4] } else { self.stem_levels.unwrap_or([1.0; 4]) };
        lane.gain_vocals = gains[0];
        lane.gain_drums = gains[1];
        lane.gain_bass = gains[2];
        lane.gain_other = gains[3];
        lane.draw_vars.set_uniform(cx, live_id!(wave_gain), &[1.0]);
        lane.draw_vars.set_uniform(cx, live_id!(eq_gain), &[1.0, 1.0, 1.0, 0.0]);
        lane.draw_vars.set_uniform(cx, live_id!(head_fraction), &[0.5]);
        lane.draw_vars.set_uniform(cx, live_id!(preview_span), &[0.0; 4]);
        lane.draw_vars.set_uniform(cx, live_id!(warn), &[0.0]);
        let palette = makepad_look::palette::get(cx);
        lane.draw_vars.set_uniform(cx, live_id!(light_theme), &[palette.wave_is_light() as u8 as f32]);
        for (id, rgba) in [
            (live_id!(color_bg), palette.wave_bg),
            (live_id!(color_grey), palette.wave_muted),
            (live_id!(color_grid), palette.wave_grid),
            (live_id!(color_grid_bar), palette.wave_bar),
            (live_id!(color_warn), palette.wave_warn),
            (live_id!(color_head), palette.wave_ink),
            (live_id!(color_vocals), palette.wave_vocals),
            (live_id!(color_drums), palette.wave_drums),
            (live_id!(color_bass), palette.wave_bass),
            (live_id!(color_other), palette.wave_other),
        ] {
            let color = Vec4f::from_u32(rgba);
            lane.draw_vars.set_uniform(cx, id, &[color.x, color.y, color.z, color.w]);
        }
        let accent = Vec4f::from_u32(if self.state.deck == 0 { palette.wave_accent } else { makepad_look::palette::on_wave(cx, palette.link) });
        lane.draw_vars.set_uniform(cx, live_id!(color_loop), &[accent.x, accent.y, accent.z, 0.18]);
        let loop_scale = if self.overview && placement.is_none() {
            cols as f64 / self.state.duration.max(f64::EPSILON)
        } else {
            cols_per_second
        };
        let (loop_in, loop_out) = self.state.loop_span.filter(|_| self.state.loaded)
            .map_or((0.0, 0.0), |(start, end)| (start * loop_scale - origin, end * loop_scale - origin));
        lane.draw_vars.set_uniform(cx, live_id!(loop_span), &[loop_in as f32, loop_out as f32, 0.0, 0.0]);
        lane.draw_abs(cx, rect);
    }

    fn draw_seconds(&mut self, cx: &mut Cx2d, rect: Rect, body: Rect, centre: f64) {
        let left = centre - WINDOW_SECONDS * 0.5;
        let step = if rect.size.x < 320.0 { 2.0 } else { 1.0 };
        let mut second = (left.max(0.0) / step).ceil() * step;
        let last = (centre + WINDOW_SECONDS * 0.5).min(self.state.duration);
        while second <= last {
            let x = time_x(rect, centre, second);
            self.draw_rule.draw_abs(cx, Rect {
                pos: dvec2(x, body.pos.y + body.size.y),
                size: dvec2(1.0, 4.0),
            });
            if x + 32.0 < rect.pos.x + rect.size.x {
                self.draw_text.draw_abs(
                    cx,
                    dvec2(x + 3.0, body.pos.y + body.size.y + 2.0),
                    &format!("{second:.0}s"),
                );
            }
            second += step;
        }
    }
}

fn finite_nonnegative(value: f64) -> f64 {
    if value.is_finite() { value.max(0.0) } else { 0.0 }
}

// WaveLane::mark_x in source seconds: the head stays at half the lane.
fn time_x(rect: Rect, centre: f64, seconds: f64) -> f64 {
    rect.pos.x + rect.size.x * (0.5 + (seconds - centre) / WINDOW_SECONDS)
}

impl MusicWaveRef {
    pub fn set_stem_levels(&self, cx: &mut Cx, levels: [f32; 4]) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.set_stem_levels(cx, levels);
        }
    }

    pub fn set_state(&self, cx: &mut Cx, deck: usize, state: &DeckStripState) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.set_state(cx, deck, state);
        }
    }

    pub(crate) fn set_placed(&self, cx: &mut Cx, placed: Option<Rc<PlacedWave>>, fallback: Option<Rc<PlacedWave>>) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.set_placed(cx, placed, fallback);
        }
    }

    pub(crate) fn set_textures(&self, cx: &mut Cx, textures: Option<Rc<MusicWaveTextures>>) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.set_textures(cx, textures);
        }
    }

    pub fn grabbed(&self, actions: &Actions) -> bool {
        actions.filter_widget_actions_cast::<MusicWaveAction>(self.widget_uid())
            .any(|action| matches!(action, MusicWaveAction::Grabbed))
    }

    pub fn cancelled(&self, actions: &Actions) -> bool {
        actions.filter_widget_actions_cast::<MusicWaveAction>(self.widget_uid())
            .any(|action| matches!(action, MusicWaveAction::Cancelled))
    }

    /// Where the hand holds the record now, the newest of this batch.
    pub fn scratched(&self, actions: &Actions) -> Option<f64> {
        actions.filter_widget_actions_cast::<MusicWaveAction>(self.widget_uid())
            .filter_map(|action| match action {
                MusicWaveAction::Scratch(seconds) => Some(seconds),
                _ => None,
            })
            .last()
    }

    pub fn scratch_ended(&self, actions: &Actions) -> bool {
        actions.filter_widget_actions_cast::<MusicWaveAction>(self.widget_uid())
            .any(|action| matches!(action, MusicWaveAction::ScratchEnd))
    }

    pub fn seeked(&self, actions: &Actions) -> Option<f64> {
        actions.filter_widget_actions_cast::<MusicWaveAction>(self.widget_uid())
            .find_map(|action| match action {
                MusicWaveAction::Seeked(seconds) => Some(seconds),
                _ => None,
            })
    }

    /// End the gesture in progress, as a lost focus would.
    pub fn end_gesture(&self, cx: &mut Cx) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.cancel_drag(cx);
        }
    }

    /// Called when the host refuses a seek. Accepted position immediately
    /// resumes authority; there is no pending preview left to time out.
    pub fn reject_seek(&self, cx: &mut Cx) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.pending_seek = None;
            inner.area.redraw(cx);
        }
    }

    /// Bind the paged preview released on `source` to the application's
    /// exact seek `request`. A preview already bound keeps its request.
    pub fn assign_seek(&self, source: u64, request: u64) {
        if let Some(mut inner) = self.borrow_mut() {
            if let Some(pending) = inner.pending_seek.as_mut() {
                if pending.paged == Some((source, None)) {
                    pending.paged = Some((source, Some(request)));
                }
            }
        }
    }

    /// `request` on `source` was accepted, refused or superseded: its own
    /// preview ends and the accepted position shows. Any other request's
    /// settlement leaves a newer preview alone.
    pub fn settle_seek(&self, cx: &mut Cx, source: u64, request: u64) {
        if let Some(mut inner) = self.borrow_mut() {
            if inner.pending_seek.is_some_and(|pending| pending.paged == Some((source, Some(request)))) {
                inner.pending_seek = None;
                inner.area.redraw(cx);
            }
        }
    }
}

impl Widget for MusicWave {
    fn set_disabled(&mut self, cx: &mut Cx, disabled: bool) {
        if self.disabled != disabled {
            self.disabled = disabled;
            if disabled {
                self.cancel_drag(cx);
            }
            self.area.redraw(cx);
        }
    }

    fn disabled(&self, _cx: &Cx) -> bool {
        self.disabled
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        if self.next_frame.is_event(event).is_some() {
            self.area.redraw(cx);
        }
        // Leaving the window (or the app) mid-gesture: the release may happen
        // elsewhere and never reach us, so the hand lets go now.
        if matches!(event, Event::WindowLostFocus(_) | Event::Pause | Event::Background) {
            self.cancel_drag(cx);
            self.area.redraw(cx);
        }
        if !self.can_seek() {
            return;
        }
        match event.hits(cx, self.area) {
            Hit::FingerHoverIn(_) | Hit::FingerHoverOver(_) => cx.set_cursor(MouseCursor::Hand),
            Hit::FingerDown(fe) if fe.is_primary_hit() => {
                if let Some(target) = self.time_at(fe.rect, fe.abs, self.drawn_centre) {
                    self.pending_seek = None;
                    self.drag = Some(WaveDrag {
                        centre: self.drawn_centre,
                        down_x: fe.abs.x,
                        seconds_per_point: WINDOW_SECONDS / fe.rect.size.x,
                        click_target: target,
                        travelled: false,
                        // The scrolling picture does not jump to the click
                        // before we know whether this hand will drag it.
                        target: if self.overview { target } else { self.drawn_centre },
                        source: self.state.paged.map(|stamp| stamp.source),
                    });
                    cx.widget_action(self.widget_uid(), MusicWaveAction::Grabbed);
                    self.area.redraw(cx);
                }
            }
            Hit::FingerMove(fe) => {
                if let Some(mut drag) = self.drag {
                    drag.travelled |= (fe.abs.x - drag.down_x).abs() >= SCRUB_DRAG_POINTS;
                    if self.overview || drag.travelled {
                        if let Some(target) = self.drag_time_at(fe.rect, fe.abs, drag) {
                            drag.target = target;
                            // The lane is the record: dragging it scratches.
                            if !self.overview {
                                cx.widget_action(self.widget_uid(), MusicWaveAction::Scratch(target));
                            }
                        }
                    }
                    self.drag = Some(drag);
                    self.area.redraw(cx);
                }
            }
            Hit::FingerUp(fe) if fe.is_primary_hit() => {
                if let Some(drag) = self.drag.take() {
                    if !self.overview && drag.travelled {
                        // Letting go of the record: it plays on from where
                        // the hand left it, no seek.
                        cx.widget_action(self.widget_uid(), MusicWaveAction::ScratchEnd);
                        self.area.redraw(cx);
                        return;
                    }
                    // A cancelled release is never a seek.
                    let target = if fe.cancelled { None } else { self.drag_time_at(fe.rect, fe.abs, drag) };
                    let action = match target {
                        Some(seconds) => {
                            self.pending_seek = Some(PendingWaveSeek {
                                target: seconds, started: cx.seconds_since_app_start(),
                                from_position: self.state.position,
                                paged: self.state.paged.map(|stamp| (stamp.source, None)),
                            });
                            MusicWaveAction::Seeked(seconds)
                        }
                        None => MusicWaveAction::Cancelled,
                    };
                    cx.widget_action(self.widget_uid(), action);
                    self.area.redraw(cx);
                }
            }
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        let rect = cx.walk_turtle_with_area(&mut self.area, walk);
        let palette = makepad_look::palette::get(cx);
        self.draw_bg.color = Vec4f::from_u32(palette.wave_ruler_bg);
        self.draw_text.color = Vec4f::from_u32(palette.wave_muted);
        self.draw_rule.color = Vec4f::from_u32(palette.wave_grid);
        for (id, rgba) in [(live_id!(color), palette.wave_head), (live_id!(glow), palette.wave_glow)] {
            let color = Vec4f::from_u32(rgba);
            self.draw_head.draw_vars.set_uniform(cx, id, &[color.x, color.y, color.z, color.w]);
        }
        self.draw_bg.draw_abs(cx, rect);
        if rect.size.x <= 0.0 || rect.size.y <= 0.0 {
            return DrawStep::done();
        }
        let now = cx.seconds_since_app_start();
        let centre = self.position_at(now);
        self.drawn_centre = centre;
        let ruler_height = if !self.overview && rect.size.y >= 40.0 { 16.0 } else { 0.0 };
        let body = Rect { pos: rect.pos, size: dvec2(rect.size.x, rect.size.y - ruler_height) };
        cx.push_clip_rect(rect);
        self.draw_wave(cx, body, centre);
        if self.state.loaded && ruler_height > 0.0 {
            self.draw_seconds(cx, rect, body, centre);
        }
        if !self.overview {
            self.draw_head.draw_abs(cx, Rect {
                pos: dvec2(rect.pos.x + rect.size.x * 0.5 - 6.0, rect.pos.y),
                size: dvec2(12.0, rect.size.y),
            });
        }
        cx.pop_clip_rect();
        if self.state.playing && self.state.ready && self.state.paged.is_none() && self.drag.is_none()
            && now - self.stamp < MAX_PREDICTION_SECONDS
        {
            self.next_frame = cx.new_next_frame();
        }
        DrawStep::done()
    }
}
