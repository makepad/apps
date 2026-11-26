//! The kit's draw shaders. One Rust draw type, [`DrawKit`], carries eight
//! texture slots and the blend mode; [`KitDraws`] holds one instance per
//! pass kind, each overriding `pixel` and declaring its own named, typed
//! instance fields (`threshold: instance(0.0)`), which `kit.rs` sets by name
//! ([`DrawKit::set`]). Every texture is premultiplied BGRA, sampled with
//! `sample`, and every pass is drawn at dpi 1 so `self.pos * self.rect_size`
//! is the output pixel.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets_internal.*

    let KitBlend = set_type_default() do #(KitBlend::script_api(vm))

    set_type_default() do #(DrawKit::script_shader(vm)){
        ..mod.draw.DrawQuad
        t0: texture_2d(float)
        t1: texture_2d(float)
        t2: texture_2d(float)
        t3: texture_2d(float)
        t4: texture_2d(float)
        t5: texture_2d(float)
        t6: texture_2d(float)
        t7: texture_2d(float)
        blend: instance(KitBlend.Normal)

        // Input 0 at a uv, transparent outside the frame (edge 0) or the
        // edge extended (edge 1).
        in0: fn(uv: vec2, edge: float) -> vec4 {
            let inside = step(0.0, uv.x) * step(uv.x, 1.0) * step(0.0, uv.y) * step(uv.y, 1.0)
            let c = self.t0.sample(clamp(uv, vec2(0.0, 0.0), vec2(1.0, 1.0)))
            return c * max(inside, edge)
        }

        luma: fn(c: vec3) -> float {
            return dot(c, vec3(0.2126, 0.7152, 0.0722))
        }

        unpremul: fn(c: vec4) -> vec3 {
            return c.xyz / max(c.w, 0.00001)
        }

        // Hash without sine (stable across GPUs): 2D/3D in, 0..1 out.
        hash21: fn(p: vec2) -> float {
            let mut q = fract(vec3(p.x, p.y, p.x) * 0.1031)
            q = q + dot(q, vec3(q.y, q.z, q.x) + 33.33)
            return fract((q.x + q.y) * q.z)
        }

        hash31: fn(p: vec3) -> float {
            let mut q = fract(p * 0.1031)
            q = q + dot(q, vec3(q.z, q.y, q.x) + 31.32)
            return fract((q.x + q.y) * q.z)
        }

        hash33: fn(p: vec3) -> vec3 {
            let mut q = fract(p * vec3(0.1031, 0.1030, 0.0973))
            q = q + dot(q, vec3(q.y, q.x, q.z) + 33.33)
            return fract((vec3(q.x, q.x, q.y) + vec3(q.y, q.z, q.z)) * vec3(q.z, q.y, q.x))
        }

        // Value noise in 3D, smooth, 0..1.
        value3: fn(p: vec3) -> float {
            let i = floor(p)
            let f = fract(p)
            let u = f * f * (3.0 - 2.0 * f)
            let a = mix(self.hash31(i), self.hash31(i + vec3(1.0, 0.0, 0.0)), u.x)
            let b = mix(self.hash31(i + vec3(0.0, 1.0, 0.0)), self.hash31(i + vec3(1.0, 1.0, 0.0)), u.x)
            let c = mix(self.hash31(i + vec3(0.0, 0.0, 1.0)), self.hash31(i + vec3(1.0, 0.0, 1.0)), u.x)
            let d = mix(self.hash31(i + vec3(0.0, 1.0, 1.0)), self.hash31(i + vec3(1.0, 1.0, 1.0)), u.x)
            return mix(mix(a, b, u.y), mix(c, d, u.y), u.z)
        }

        grad3: fn(i: vec3, f: vec3) -> float {
            let g = self.hash33(i) * 2.0 - 1.0
            return dot(normalize(g + vec3(0.0001, 0.0002, 0.0003)), f)
        }

        // Gradient (Perlin) noise in 3D, about 0..1.
        perlin3: fn(p: vec3) -> float {
            let i = floor(p)
            let f = fract(p)
            let u = f * f * f * (f * (f * 6.0 - 15.0) + 10.0)
            let x00 = mix(self.grad3(i, f), self.grad3(i + vec3(1.0, 0.0, 0.0), f - vec3(1.0, 0.0, 0.0)), u.x)
            let x10 = mix(self.grad3(i + vec3(0.0, 1.0, 0.0), f - vec3(0.0, 1.0, 0.0)), self.grad3(i + vec3(1.0, 1.0, 0.0), f - vec3(1.0, 1.0, 0.0)), u.x)
            let x01 = mix(self.grad3(i + vec3(0.0, 0.0, 1.0), f - vec3(0.0, 0.0, 1.0)), self.grad3(i + vec3(1.0, 0.0, 1.0), f - vec3(1.0, 0.0, 1.0)), u.x)
            let x11 = mix(self.grad3(i + vec3(0.0, 1.0, 1.0), f - vec3(0.0, 1.0, 1.0)), self.grad3(i + vec3(1.0, 1.0, 1.0), f - vec3(1.0, 1.0, 1.0)), u.x)
            return mix(mix(x00, x10, u.y), mix(x01, x11, u.y), u.z) * 0.9 + 0.5
        }

        // Worley F1 in 2D with feature points circling over z (time), 0..1.
        cellular: fn(p: vec3) -> float {
            let i = floor(vec2(p.x, p.y))
            let f = vec2(p.x, p.y) - i
            let mut best = 8.0
            for k in 0..9 {
                let o = vec2(modf(f32(k), 3.0) - 1.0, floor(f32(k) / 3.0) - 1.0)
                let h = self.hash33(vec3(i.x + o.x, i.y + o.y, 17.0))
                let a = h.z * 6.2831 + p.z * (0.6 + h.x)
                let point = o + vec2(0.5, 0.5) + vec2(cos(a), sin(a)) * 0.35
                best = min(best, length(point - f))
            }
            return clamp(best, 0.0, 1.0)
        }

        // Octave sum. kind: 0 value, 1 perlin, 2 fbm, 3 turbulent, 4 ridged,
        // 5 cellular. o = (octaves, lacunarity, gain, _).
        noise: fn(p: vec3, kind: float, o: vec4) -> float {
            if kind < 0.5 {
                return self.value3(p)
            }
            if kind < 1.5 {
                return self.perlin3(p)
            }
            if kind > 4.5 {
                return self.cellular(p)
            }
            let mut sum = 0.0
            let mut norm = 0.0
            let mut amp = 1.0
            let mut q = p
            for k in 0..8 {
                if f32(k) >= o.x {
                    break
                }
                let n = self.perlin3(q + vec3(f32(k) * 17.13, f32(k) * 3.71, 0.0))
                let mut v = n
                if kind > 3.5 {
                    v = 1.0 - abs(n * 2.0 - 1.0)
                    v = v * v
                } else if kind > 2.5 {
                    v = abs(n * 2.0 - 1.0)
                }
                sum = sum + v * amp
                norm = norm + amp
                amp = amp * o.z
                q = vec3(q.x * o.y, q.y * o.y, q.z * 1.3)
            }
            return sum / max(norm, 0.0001)
        }

        // W3C compositing: premultiplied `s` (source layer) over
        // premultiplied `b` (backdrop) in `self.blend`.
        blend_px: fn(b: vec4, s: vec4) -> vec4 {
            let cb = self.unpremul(b)
            let cs = self.unpremul(s)
            let mut m = cs
            match self.blend {
                KitBlend.Multiply => { m = cb * cs }
                KitBlend.Screen => { m = cb + cs - cb * cs }
                KitBlend.Overlay => { m = self.hard_light(cs, cb) }
                KitBlend.Darken => { m = min(cb, cs) }
                KitBlend.Lighten => { m = max(cb, cs) }
                KitBlend.ColorDodge => {
                    m = min(vec3(1.0, 1.0, 1.0), cb / max(vec3(1.0, 1.0, 1.0) - cs, vec3(0.00001, 0.00001, 0.00001)))
                    m = m * step(vec3(0.00001, 0.00001, 0.00001), cb)
                }
                KitBlend.ColorBurn => {
                    m = vec3(1.0, 1.0, 1.0) - min(vec3(1.0, 1.0, 1.0), (vec3(1.0, 1.0, 1.0) - cb) / max(cs, vec3(0.00001, 0.00001, 0.00001)))
                    m = mix(m, vec3(1.0, 1.0, 1.0), step(vec3(0.99999, 0.99999, 0.99999), cb))
                }
                KitBlend.HardLight => { m = self.hard_light(cb, cs) }
                KitBlend.SoftLight => { m = self.soft_light(cb, cs) }
                KitBlend.Difference => { m = abs(cb - cs) }
                KitBlend.Exclusion => { m = cb + cs - 2.0 * cb * cs }
                KitBlend.Add => { m = min(cb + cs, vec3(1.0, 1.0, 1.0)) }
                KitBlend.Subtract => { m = max(cb - cs, vec3(0.0, 0.0, 0.0)) }
                KitBlend.Divide => { m = min(cb / max(cs, vec3(0.00001, 0.00001, 0.00001)), vec3(1.0, 1.0, 1.0)) }
                KitBlend.Hue => { m = self.set_lum(self.set_sat(cs, self.sat(cb)), self.luma(cb)) }
                KitBlend.Saturation => { m = self.set_lum(self.set_sat(cb, self.sat(cs)), self.luma(cb)) }
                KitBlend.Color => { m = self.set_lum(cs, self.luma(cb)) }
                KitBlend.Luminosity => { m = self.set_lum(cb, self.luma(cs)) }
                _ => { m = cs }
            }
            let rgb = s.xyz * (1.0 - b.w) + b.xyz * (1.0 - s.w) + s.w * b.w * m
            return vec4(rgb, s.w + b.w * (1.0 - s.w))
        }

        // B(cb, cs) of hard light: multiply or screen by the source.
        hard_light: fn(cb: vec3, cs: vec3) -> vec3 {
            let mult = cb * 2.0 * cs
            let scr = vec3(1.0, 1.0, 1.0) - (vec3(1.0, 1.0, 1.0) - cb) * (vec3(1.0, 1.0, 1.0) - (2.0 * cs - 1.0))
            return mix(mult, scr, step(vec3(0.5, 0.5, 0.5), cs))
        }

        soft_light: fn(cb: vec3, cs: vec3) -> vec3 {
            let d = mix(((16.0 * cb - 12.0) * cb + 4.0) * cb, sqrt(cb), step(vec3(0.25, 0.25, 0.25), cb))
            let dark = cb - (vec3(1.0, 1.0, 1.0) - 2.0 * cs) * cb * (vec3(1.0, 1.0, 1.0) - cb)
            let light = cb + (2.0 * cs - 1.0) * (d - cb)
            return mix(dark, light, step(vec3(0.5, 0.5, 0.5), cs))
        }

        clip_color: fn(c: vec3) -> vec3 {
            let l = self.luma(c)
            let n = min(min(c.x, c.y), c.z)
            let x = max(max(c.x, c.y), c.z)
            let mut o = c
            if n < 0.0 {
                o = vec3(l, l, l) + (o - l) * l / max(l - n, 0.00001)
            }
            if x > 1.0 {
                o = vec3(l, l, l) + (o - l) * (1.0 - l) / max(x - l, 0.00001)
            }
            return o
        }

        set_lum: fn(c: vec3, l: float) -> vec3 {
            let d = l - self.luma(c)
            return self.clip_color(c + vec3(d, d, d))
        }

        sat: fn(c: vec3) -> float {
            return max(max(c.x, c.y), c.z) - min(min(c.x, c.y), c.z)
        }

        set_sat: fn(c: vec3, s: float) -> vec3 {
            let n = min(min(c.x, c.y), c.z)
            let x = max(max(c.x, c.y), c.z)
            if x - n < 0.00001 {
                return vec3(0.0, 0.0, 0.0)
            }
            return (c - n) * s / (x - n)
        }

        // The colour passes' common tail: `o` (straight rgb) clamped,
        // premultiplied by the source alpha and mixed over the source.
        grade_out: fn(src: vec4, o: vec3, amt: float) -> vec4 {
            let c = clamp(o, vec3(0.0, 0.0, 0.0), vec3(1.0, 1.0, 1.0))
            return mix(src, vec4(c * src.w, src.w), amt)
        }

        pixel: fn() {
            return self.t0.sample(self.pos)
        }
    }

    mod.widgets.RenderKitDraws = set_type_default() do #(KitDraws::script_component(vm)){
        // opacity; src_rect = source uv rect (x, y, w, h).
        copy +: {
            opacity: instance(0.0)
            src_rect: instance(vec4(0.0, 0.0, 0.0, 0.0))
            pixel: fn() {
                let uv = self.src_rect.xy + self.pos * self.src_rect.zw
                return self.t0.sample(clamp(uv, vec2(0.0, 0.0), vec2(1.0, 1.0))) * self.opacity
            }
        }

        // t0 the input, t1 the effect's result; amount = mix.
        mix +: {
            amount: instance(0.0)
            pixel: fn() {
                return mix(self.t0.sample(self.pos), self.t1.sample(self.pos), self.amount)
            }
        }

        // One separable gaussian pass. axis = (dir.x, dir.y), sigma in
        // texels of t0, edge; taps in bilinear pairs, weights normalised.
        gauss +: {
            axis: instance(vec2(0.0, 0.0))
            sigma: instance(0.0)
            edge: instance(0.0)
            pixel: fn() {
                let size = self.t0.size()
                let texel = vec2(1.0 / max(size.x, 1.0), 1.0 / max(size.y, 1.0))
                let dir = self.axis * texel
                let sigma = max(self.sigma, 0.0001)
                let reach = ceil(sigma * 3.0)
                let k2 = -0.5 / (sigma * sigma)
                let mut sum = self.in0(self.pos, self.edge)
                let mut norm = 1.0
                for i in 0..96 {
                    let k = f32(i) * 2.0 + 1.0
                    if k > reach {
                        break
                    }
                    let w1 = exp(k * k * k2)
                    let w2 = exp((k + 1.0) * (k + 1.0) * k2)
                    let w = w1 + w2
                    let o = (k * w1 + (k + 1.0) * w2) / w
                    sum = sum + (self.in0(self.pos + dir * o, self.edge) + self.in0(self.pos - dir * o, self.edge)) * w
                    norm = norm + 2.0 * w
                }
                return sum / norm
            }
        }

        // Taps along a line, an arc or a ray. mode 0 directional / 1 spin /
        // 2 zoom, taps, amount, edge, center (px), axis (dir.x, dir.y).
        // Directional: amount is the length in px along axis; spin: the
        // angle in radians; zoom: the scale fraction.
        line_blur +: {
            mode: instance(0.0)
            taps: instance(0.0)
            amount: instance(0.0)
            edge: instance(0.0)
            center: instance(vec2(0.0, 0.0))
            axis: instance(vec2(0.0, 0.0))
            pixel: fn() {
                let size = self.rect_size
                let p = self.pos * size
                let c = self.center
                let n = max(self.taps, 1.0)
                let mut sum = vec4(0.0, 0.0, 0.0, 0.0)
                for i in 0..256 {
                    if f32(i) >= n {
                        break
                    }
                    let u = (f32(i) + 0.5) / n - 0.5
                    let mut q = p
                    if self.mode < 0.5 {
                        q = p + self.axis * (u * self.amount)
                    } else if self.mode < 1.5 {
                        let ang = u * self.amount
                        let d = p - c
                        q = c + vec2(d.x * cos(ang) - d.y * sin(ang), d.x * sin(ang) + d.y * cos(ang))
                    } else {
                        q = c + (p - c) * (1.0 - (u + 0.5) * self.amount)
                    }
                    sum = sum + self.in0(q / size, self.edge)
                }
                return sum / n
            }
        }

        // Bright pass. threshold, knee, tint.
        threshold +: {
            threshold: instance(0.0)
            knee: instance(0.0)
            tint: instance(vec4(0.0, 0.0, 0.0, 0.0))
            pixel: fn() {
                let c = self.t0.sample(self.pos)
                let l = self.luma(self.unpremul(c))
                let gate = smoothstep(self.threshold - self.knee * 0.5, self.threshold + self.knee * 0.5 + 0.00001, l)
                return vec4(c.xyz * gate * self.tint.xyz, c.w * gate)
            }
        }

        // Light added back: t0 base, t1..t4 glow levels with weights;
        // screen = mode (0 add, 1 screen). Alpha grows with the light.
        add_light +: {
            weights: instance(vec4(0.0, 0.0, 0.0, 0.0))
            screen: instance(0.0)
            pixel: fn() {
                let base = self.t0.sample(self.pos)
                let g = self.t1.sample(self.pos) * self.weights.x + self.t2.sample(self.pos) * self.weights.y
                    + self.t3.sample(self.pos) * self.weights.z + self.t4.sample(self.pos) * self.weights.w
                let mut rgb = min(base.xyz + g.xyz, vec3(1.0, 1.0, 1.0))
                if self.screen > 0.5 {
                    rgb = base.xyz + g.xyz - base.xyz * min(g.xyz, vec3(1.0, 1.0, 1.0))
                }
                let a = max(base.w, min(max(max(rgb.x, rgb.y), rgb.z), 1.0))
                return vec4(min(rgb, vec3(a, a, a)), a)
            }
        }

        // A shadow's shape: the input's alpha at an offset (px), hardened
        // by spread, optionally inverted (inner shadow), coloured.
        // offset, spread, invert, tint (premultiplied, opacity in).
        shadow_mask +: {
            offset: instance(vec2(0.0, 0.0))
            spread: instance(0.0)
            invert: instance(0.0)
            tint: instance(vec4(0.0, 0.0, 0.0, 0.0))
            pixel: fn() {
                let size = self.rect_size
                let uv = (self.pos * size - self.offset) / size
                let mut alpha = self.in0(uv, 0.0).w
                if self.invert > 0.5 {
                    alpha = 1.0 - alpha
                }
                alpha = clamp(alpha / max(1.0 - self.spread, 0.001), 0.0, 1.0)
                return self.tint * alpha
            }
        }

        // t0 the layer, t1 the blurred shadow. mode: 0 layer over the
        // shadow, 1 the shadow only, 2 an inner shadow over the layer.
        shadow_comp +: {
            mode: instance(0.0)
            pixel: fn() {
                let l = self.t0.sample(self.pos)
                let s = self.t1.sample(self.pos)
                if self.mode > 1.5 {
                    let inner = s * l.w
                    return vec4(inner.xyz + l.xyz * (1.0 - inner.w), l.w)
                }
                if self.mode > 0.5 {
                    return s
                }
                return l + s * (1.0 - l.w)
            }
        }

        // Jump flooding for exact outlines: a seed texel stores its own
        // pixel position in 16 bits per axis (x in r, g; y in b, a), "no
        // seed" is all ones. Seed: outside = 0 seeds where alpha >= 0.5 (the
        // inside), 1 where it is below (the outside).
        jfa_seed +: {
            color_format: @Bgra8NoBlend
            outside: instance(0.0)
            pixel: fn() {
                let p = floor(self.pos * self.rect_size)
                let a = self.t0.sample(self.pos).w
                let seeded = mix(step(0.5, a), 1.0 - step(0.5, a), self.outside)
                if seeded < 0.5 {
                    return vec4(1.0, 1.0, 1.0, 1.0)
                }
                return vec4(floor(p.x / 256.0) / 255.0, modf(p.x, 256.0) / 255.0, floor(p.y / 256.0) / 255.0, modf(p.y, 256.0) / 255.0)
            }
        }

        // One flood step of `jump` pixels: the nearest seed of 9 neighbours.
        jfa_step +: {
            color_format: @Bgra8NoBlend
            jump: instance(0.0)
            decode: fn(uv: vec2) -> vec2 {
                let c = floor(self.t0.sample(uv) * 255.0 + 0.5)
                return vec2(c.x * 256.0 + c.y, c.z * 256.0 + c.w)
            }
            pixel: fn() {
                let size = self.rect_size
                let p = floor(self.pos * size)
                let mut best = vec2(65535.0, 65535.0)
                let mut dist = 1.0e20
                for k in 0..9 {
                    let o = vec2(modf(f32(k), 3.0) - 1.0, floor(f32(k) / 3.0) - 1.0) * self.jump
                    let q = p + o
                    if q.x >= 0.0 && q.y >= 0.0 && q.x < size.x && q.y < size.y {
                        let s = self.decode((q + 0.5) / size)
                        if s.x < 65535.0 {
                            let d = dot(s - p, s - p)
                            if d < dist {
                                dist = d
                                best = s
                            }
                        }
                    }
                }
                if best.x >= 65535.0 {
                    return vec4(1.0, 1.0, 1.0, 1.0)
                }
                return vec4(floor(best.x / 256.0) / 255.0, modf(best.x, 256.0) / 255.0, floor(best.y / 256.0) / 255.0, modf(best.y, 256.0) / 255.0)
            }
        }

        // The outline from the flood fields. t0 layer, t1 nearest inside
        // seed, t2 nearest outside seed. width, position 0 outside /
        // 1 inside / 2 center, tint (premultiplied, opacity in).
        stroke_comp +: {
            width: instance(0.0)
            position: instance(0.0)
            tint: instance(vec4(0.0, 0.0, 0.0, 0.0))
            decode1: fn(uv: vec2) -> vec2 {
                let c = floor(self.t1.sample(uv) * 255.0 + 0.5)
                return vec2(c.x * 256.0 + c.y, c.z * 256.0 + c.w)
            }
            decode2: fn(uv: vec2) -> vec2 {
                let c = floor(self.t2.sample(uv) * 255.0 + 0.5)
                return vec2(c.x * 256.0 + c.y, c.z * 256.0 + c.w)
            }
            pixel: fn() {
                let p = floor(self.pos * self.rect_size)
                let l = self.t0.sample(self.pos)
                let si = self.decode1(self.pos)
                let so = self.decode2(self.pos)
                // Distance to the edge, about half a pixel short of the
                // nearest seed on the other side; the layer's own alpha
                // gives the sub-pixel part at the edge.
                let d_out = mix(length(si - p) - 0.5, 1.0e6, step(65535.0, si.x))
                let d_in = mix(length(so - p) - 0.5, 1.0e6, step(65535.0, so.x))
                let w = self.width
                if self.position < 0.5 {
                    let cov = clamp(w - d_out + 0.5, 0.0, 1.0) * (1.0 - l.w) + l.w
                    return l + self.tint * cov * (1.0 - l.w)
                }
                if self.position < 1.5 {
                    let cov = clamp(w - d_in + 0.5, 0.0, 1.0) * l.w
                    return vec4(self.tint.xyz * cov + l.xyz * (1.0 - self.tint.w * cov), l.w)
                }
                let outside = clamp(w * 0.5 - d_out + 0.5, 0.0, 1.0) * (1.0 - l.w) + l.w
                let inside = clamp(w * 0.5 - d_in + 0.5, 0.0, 1.0) * l.w
                let under = l + self.tint * outside * (1.0 - l.w)
                let cov = max(inside, clamp(w * 0.5 - d_out + 0.5, 0.0, 1.0) * (1.0 - l.w))
                return vec4(self.tint.xyz * cov + under.xyz * (1.0 - self.tint.w * cov), under.w)
            }
        }

        // Bevel lighting. t0 layer, t1 blurred alpha (the height). depth,
        // light_dir, highlight, shadow (premultiplied, opacity in),
        // extent = size px.
        bevel +: {
            depth: instance(0.0)
            light_dir: instance(vec3(0.0, 0.0, 0.0))
            highlight: instance(vec4(0.0, 0.0, 0.0, 0.0))
            shadow: instance(vec4(0.0, 0.0, 0.0, 0.0))
            extent: instance(0.0)
            pixel: fn() {
                let size = self.t1.size()
                let texel = vec2(1.0 / size.x, 1.0 / size.y)
                let l = self.t0.sample(self.pos)
                let hx = self.t1.sample(self.pos + vec2(texel.x, 0.0)).w - self.t1.sample(self.pos - vec2(texel.x, 0.0)).w
                let hy = self.t1.sample(self.pos + vec2(0.0, texel.y)).w - self.t1.sample(self.pos - vec2(0.0, texel.y)).w
                let k = self.depth * max(self.extent, 1.0) * 0.5
                let n = normalize(vec3(-hx * k, -hy * k, 1.0))
                let light = self.light_dir
                let shade = dot(n, light)
                let flat = light.z
                let hi = clamp((shade - flat) / max(1.0 - flat, 0.001), 0.0, 1.0) * l.w
                let lo = clamp((flat - shade) / max(flat, 0.001), 0.0, 1.0) * l.w
                let cl = self.unpremul(l)
                let hcol = self.highlight.xyz / max(self.highlight.w, 0.00001)
                let scol = self.shadow.xyz / max(self.shadow.w, 0.00001)
                let mut rgb = mix(cl, cl + hcol - cl * hcol, hi * self.highlight.w)
                rgb = mix(rgb, rgb * scol, lo * self.shadow.w)
                return vec4(rgb * l.w, l.w)
            }
        }

        // A layer (premultiplied t1) composited with the backdrop t0 in
        // `blend`. opacity, clip: clip keeps the result inside the
        // backdrop's alpha.
        composite +: {
            opacity: instance(0.0)
            clip: instance(0.0)
            pixel: fn() {
                let b = self.t0.sample(self.pos)
                let mut s = self.t1.sample(self.pos) * self.opacity
                if self.clip > 0.5 {
                    s = s * b.w
                }
                let o = self.blend_px(b, s)
                if self.clip > 0.5 {
                    return o * (b.w / max(o.w, 0.00001)) * step(0.00001, o.w)
                }
                return o
            }
        }

        // A solid colour: fill (premultiplied).
        solid +: {
            fill: instance(vec4(0.0, 0.0, 0.0, 0.0))
            pixel: fn() {
                return self.fill
            }
        }

        // A gradient from the 256×1 ramp in t1 (straight colours).
        // kind 0 linear / 1 radial / 2 conic / 3 diamond, wrap 0 pad /
        // 1 repeat / 2 mirror, dither, start_px, end_px.
        gradient +: {
            kind: instance(0.0)
            wrap: instance(0.0)
            dither: instance(0.0)
            start_px: instance(vec2(0.0, 0.0))
            end_px: instance(vec2(0.0, 0.0))
            pixel: fn() {
                let p = self.pos * self.rect_size
                let s = self.start_px
                let e = self.end_px
                let d = e - s
                let mut u = 0.0
                if self.kind < 0.5 {
                    u = dot(p - s, d) / max(dot(d, d), 0.0001)
                } else if self.kind < 1.5 {
                    u = length(p - s) / max(length(d), 0.0001)
                } else if self.kind < 2.5 {
                    let a0 = atan2(d.y, d.x)
                    let a1 = atan2(p.y - s.y, p.x - s.x)
                    u = fract((a1 - a0) / 6.2831853 + 1.0)
                } else {
                    let q = abs(p - s)
                    u = (q.x + q.y) / max(abs(d.x) + abs(d.y), 0.0001)
                }
                if self.wrap > 1.5 {
                    u = 1.0 - abs(u - floor(u * 0.5) * 2.0 - 1.0)
                } else if self.wrap > 0.5 {
                    u = fract(u)
                }
                u = clamp(u, 0.0, 1.0)
                let c = self.t1.sample(vec2(u * 255.0 / 256.0 + 0.5 / 256.0, 0.5))
                // Triangular dither of about one 8-bit step.
                let n = (self.hash21(p) + self.hash21(p + vec2(17.3, 5.1)) - 1.0) / 255.0 * self.dither
                let rgb = clamp(c.xyz + vec3(n, n, n), vec3(0.0, 0.0, 0.0), vec3(1.0, 1.0, 1.0))
                return vec4(rgb * c.w, c.w)
            }
        }

        // Procedural noise. kind, scale px, evolve (time z), seed,
        // octaves, lacunarity, gain, contrast, brightness, offset px,
        // color_a, color_b (straight).
        noise_gen +: {
            kind: instance(0.0)
            scale: instance(0.0)
            evolve: instance(0.0)
            seed: instance(0.0)
            octaves: instance(0.0)
            lacunarity: instance(0.0)
            gain: instance(0.0)
            contrast: instance(0.0)
            brightness: instance(0.0)
            offset: instance(vec2(0.0, 0.0))
            color_a: instance(vec4(0.0, 0.0, 0.0, 0.0))
            color_b: instance(vec4(0.0, 0.0, 0.0, 0.0))
            pixel: fn() {
                let p = (self.pos * self.rect_size + self.offset) / max(self.scale, 0.001)
                let q = vec3(p.x + self.seed * 0.618, p.y + self.seed * 0.379, self.evolve)
                let mut v = self.noise(q, self.kind, vec4(self.octaves, self.lacunarity, self.gain, self.contrast))
                v = clamp((v - 0.5) * self.contrast + 0.5 + self.brightness, 0.0, 1.0)
                let col = mix(self.color_a, self.color_b, v)
                return vec4(col.xyz * col.w, col.w)
            }
        }

        // Displacement by a map (t1): 128/255 is no move. amount (px),
        // channel_x, channel_y, edge; channels 0 r, 1 g, 2 b, 3 a, 4 luma.
        displace +: {
            amount: instance(vec2(0.0, 0.0))
            channel_x: instance(0.0)
            channel_y: instance(0.0)
            edge: instance(0.0)
            channel: fn(m: vec4, k: float) -> float {
                let c = self.unpremul(m)
                if k < 0.5 { return c.x }
                if k < 1.5 { return c.y }
                if k < 2.5 { return c.z }
                if k < 3.5 { return m.w }
                return self.luma(c)
            }
            pixel: fn() {
                let size = self.rect_size
                let m = self.t1.sample(self.pos)
                // 8-bit maps: 128 is still (as in After Effects), 1 and 255
                // the full move either way.
                let dx = clamp((self.channel(m, self.channel_x) - 128.0 / 255.0) / (127.0 / 255.0), -1.0, 1.0) * self.amount.x
                let dy = clamp((self.channel(m, self.channel_y) - 128.0 / 255.0) / (127.0 / 255.0), -1.0, 1.0) * self.amount.y
                return self.in0(self.pos + vec2(dx, dy) / size, self.edge)
            }
        }

        // Slit-scan strips. phase in the wave 0..1, axis 0 vertical /
        // 1 horizontal / 2 diagonal, wave index, count (strips), amp (as a
        // fraction of the height), spread, strip_len, seed,
        // edge (0 wrap, 1 transparent, 2 extend).
        slices +: {
            phase: instance(0.0)
            axis: instance(0.0)
            wave: instance(0.0)
            count: instance(0.0)
            amp: instance(0.0)
            spread: instance(0.0)
            strip_len: instance(0.0)
            seed: instance(0.0)
            edge: instance(0.0)
            pixel: fn() {
                let size = self.rect_size
                let asp = size.x / max(size.y, 1.0)
                let uv = self.pos
                let mut s = uv.x
                let mut m = vec2(0.0, 1.0)
                if self.axis > 0.5 {
                    s = uv.y
                    m = vec2(1.0 / asp, 0.0)
                }
                if self.axis > 1.5 {
                    s = (uv.x * asp + uv.y) / (asp + 1.0)
                    m = vec2(0.70710678 / asp, 0.0 - 0.70710678)
                }
                let k = floor(s * self.count)
                let hk = fract(sin(k * 12.9898 + self.wave * 4.1414 + self.seed * 0.37) * 43758.5453)
                let sgn = step(0.5, hk) * 2.0 - 1.0
                let mag = 0.35 + 0.65 * fract(hk * 7.13)
                let local = self.phase - self.spread * k / max(self.count, 1.0)
                let env = smoothstep(0.0, 0.05, local) * (1.0 - smoothstep(0.05, 0.05 + self.strip_len, local))
                let src = uv - m * (self.amp * sgn * mag * env)
                if self.edge < 0.5 {
                    return self.t0.sample(fract(src))
                }
                return self.in0(src, self.edge - 1.0)
            }
        }

        // Turbulent displacement. amount (px), cell (px), octaves, clock,
        // seed, edge.
        turbulence +: {
            amount: instance(0.0)
            cell: instance(0.0)
            octaves: instance(0.0)
            clock: instance(0.0)
            seed: instance(0.0)
            edge: instance(0.0)
            pixel: fn() {
                let size = self.rect_size
                let p = self.pos * size / max(self.cell, 0.001) + vec2(self.seed * 0.618, self.seed * 0.377)
                let o = vec4(self.octaves, 2.0, 0.5, 0.0)
                let nx = self.noise(vec3(p.x, p.y, self.clock), 2.0, o) * 2.0 - 1.0
                let ny = self.noise(vec3(p.x + 31.7, p.y + 11.3, self.clock), 2.0, o) * 2.0 - 1.0
                return self.in0(self.pos + vec2(nx, ny) * self.amount / size, self.edge)
            }
        }

        // Ripples. center (px), wavelength, amplitude, phase (px),
        // decay (per 1000 px), edge.
        ripple +: {
            center: instance(vec2(0.0, 0.0))
            wavelength: instance(0.0)
            amplitude: instance(0.0)
            phase: instance(0.0)
            decay: instance(0.0)
            edge: instance(0.0)
            pixel: fn() {
                let size = self.rect_size
                let p = self.pos * size
                let d = p - self.center
                let r = length(d)
                let dir = d / max(r, 0.0001)
                let amp = self.amplitude * exp(-self.decay * r / 1000.0)
                let wave = sin((r - self.phase) / max(self.wavelength, 0.001) * 6.2831853) * amp
                return self.in0((p + dir * wave) / size, self.edge)
            }
        }

        // A shape mask. shape 0 rect / 1 rounded / 2 ellipse, center (px),
        // rotation (rad), half_size (px), radius (corner), feather (sigma),
        // expand, invert, opacity.
        shape_mask +: {
            shape: instance(0.0)
            center: instance(vec2(0.0, 0.0))
            rotation: instance(0.0)
            half_size: instance(vec2(0.0, 0.0))
            radius: instance(0.0)
            feather: instance(0.0)
            expand: instance(0.0)
            invert: instance(0.0)
            opacity: instance(0.0)
            erf: fn(x: float) -> float {
                // Abramowitz–Stegun 7.1.26, |error| < 1.5e-7.
                let s = sign(x)
                let a = abs(x)
                let t = 1.0 / (1.0 + 0.3275911 * a)
                let y = 1.0 - (((((1.061405429 * t - 1.453152027) * t) + 1.421413741) * t - 0.284496736) * t + 0.254829592) * t * exp(-a * a)
                return s * y
            }
            pixel: fn() {
                let l = self.t0.sample(self.pos)
                let p0 = self.pos * self.rect_size - self.center
                let cr = cos(-self.rotation)
                let sr = sin(-self.rotation)
                let p = vec2(p0.x * cr - p0.y * sr, p0.x * sr + p0.y * cr)
                let half = max(self.half_size + vec2(self.expand, self.expand), vec2(0.0, 0.0))
                let mut d = 0.0
                if self.shape < 0.5 {
                    let q = abs(p) - half
                    d = length(max(q, vec2(0.0, 0.0))) + min(max(q.x, q.y), 0.0)
                } else if self.shape < 1.5 {
                    let r = min(self.radius, min(half.x, half.y))
                    let q = abs(p) - half + vec2(r, r)
                    d = length(max(q, vec2(0.0, 0.0))) + min(max(q.x, q.y), 0.0) - r
                } else {
                    let k = max(half, vec2(0.0001, 0.0001))
                    let e = length(p / k)
                    d = (e - 1.0) * min(k.x, k.y)
                }
                let sigma = self.feather
                let mut cov = clamp(0.5 - d, 0.0, 1.0)
                if sigma > 0.01 {
                    cov = 0.5 - 0.5 * self.erf(d / (sigma * 1.41421356))
                }
                if self.invert > 0.5 {
                    cov = 1.0 - cov
                }
                return l * mix(1.0, cov, self.opacity)
            }
        }

        // A track matte: t1's alpha or luma. mode 0 alpha / 1 luma, invert.
        matte +: {
            mode: instance(0.0)
            invert: instance(0.0)
            pixel: fn() {
                let l = self.t0.sample(self.pos)
                let m = self.t1.sample(self.pos)
                let mut k = m.w
                if self.mode > 0.5 {
                    k = self.luma(m.xyz)
                }
                if self.invert > 0.5 {
                    k = 1.0 - k
                }
                return l * k
            }
        }

        // A layer plane through the camera: row_a, row_b, row_c = rows of
        // the inverse homography (screen px → layer px, w = 1 / depth).
        // Edges are anti-aliased by the pixel's footprint on the plane; a
        // 2×2 supersample keeps minified planes from shimmering.
        plane +: {
            row_a: instance(vec3(0.0, 0.0, 0.0))
            row_b: instance(vec3(0.0, 0.0, 0.0))
            row_c: instance(vec3(0.0, 0.0, 0.0))
            layer_uv: fn(s: vec2) -> vec3 {
                let q = vec3(dot(self.row_a, vec3(s.x, s.y, 1.0)), dot(self.row_b, vec3(s.x, s.y, 1.0)), dot(self.row_c, vec3(s.x, s.y, 1.0)))
                return vec3(q.x / q.z, q.y / q.z, q.z)
            }
            // Coverage-weighted layer colour at screen point s, and the
            // footprint of one screen pixel on the layer (in w).
            tap: fn(s: vec2) -> vec4 {
                let q = self.layer_uv(s)
                if q.z <= 0.0 {
                    return vec4(0.0, 0.0, 0.0, 0.0)
                }
                let size = self.rect_size
                let uv = vec2(q.x, q.y) / size
                let edge = min(min(q.x, size.x - q.x), min(q.y, size.y - q.y))
                let cov = clamp(edge / max(self.footprint(s), 0.0001) + 0.5, 0.0, 1.0)
                return self.t0.sample(clamp(uv, vec2(0.0, 0.0), vec2(1.0, 1.0))) * cov
            }
            footprint: fn(s: vec2) -> float {
                let q = self.layer_uv(s)
                let q1 = self.layer_uv(s + vec2(1.0, 0.0))
                let q2 = self.layer_uv(s + vec2(0.0, 1.0))
                return max(length(vec2(q1.x - q.x, q1.y - q.y)), length(vec2(q2.x - q.x, q2.y - q.y)))
            }
            pixel: fn() {
                let s = self.pos * self.rect_size
                // 1:1 or magnified: one exact tap; minified: 2×2.
                if self.footprint(s) < 1.25 {
                    return self.tap(s)
                }
                return (self.tap(s + vec2(-0.25, -0.25)) + self.tap(s + vec2(0.25, -0.25)) + self.tap(s + vec2(-0.25, 0.25)) + self.tap(s + vec2(0.25, 0.25))) * 0.25
            }
        }

        // Depth of field over a plane: t0 sharp, t1..t3 blurred with sigma
        // sigma_1, sigma_2, sigma_3. Per pixel the depth (1 / w of the
        // inverse homography row row_c) gives the blur: coc =
        // aperture·|1 - focus/z|, sigma = coc / 2.
        dof +: {
            row_c: instance(vec3(0.0, 0.0, 0.0))
            sigma_1: instance(0.0)
            sigma_2: instance(0.0)
            sigma_3: instance(0.0)
            aperture: instance(0.0)
            focus: instance(0.0)
            pixel: fn() {
                let s = self.pos * self.rect_size
                let w = dot(self.row_c, vec3(s.x, s.y, 1.0))
                let z = 1.0 / max(w, 0.0000001)
                let sigma = self.aperture * abs(1.0 - self.focus / z) * 0.5
                let l0 = self.t0.sample(self.pos)
                let l1 = self.t1.sample(self.pos)
                let l2 = self.t2.sample(self.pos)
                let l3 = self.t3.sample(self.pos)
                if sigma <= self.sigma_1 {
                    return mix(l0, l1, clamp(sigma / max(self.sigma_1, 0.0001), 0.0, 1.0))
                }
                if sigma <= self.sigma_2 {
                    return mix(l1, l2, clamp((sigma - self.sigma_1) / max(self.sigma_2 - self.sigma_1, 0.0001), 0.0, 1.0))
                }
                return mix(l2, l3, clamp((sigma - self.sigma_2) / max(self.sigma_3 - self.sigma_2, 0.0001), 0.0, 1.0))
            }
        }

        // One particle. center (px), diameter (px), rotation, tint
        // (straight), velocity (px/s), shape, additive, birth (px),
        // check (emitter alpha in t0), frame (px).
        particle +: {
            center: instance(vec2(0.0, 0.0))
            diameter: instance(0.0)
            rotation: instance(0.0)
            tint: instance(vec4(0.0, 0.0, 0.0, 0.0))
            velocity: instance(vec2(0.0, 0.0))
            shape: instance(0.0)
            additive: instance(0.0)
            birth: instance(vec2(0.0, 0.0))
            check: instance(0.0)
            frame: instance(vec2(0.0, 0.0))
            pixel: fn() {
                if self.check > 0.5 {
                    let e = self.t0.sample(self.birth / max(self.frame, vec2(1.0, 1.0)))
                    if e.w < 0.5 {
                        return vec4(0.0, 0.0, 0.0, 0.0)
                    }
                }
                let p0 = self.pos * self.rect_size - self.rect_size * 0.5
                let cr = cos(self.rotation)
                let sr = sin(self.rotation)
                let mut p = vec2(p0.x * cr + p0.y * sr, -p0.x * sr + p0.y * cr)
                let r = max(self.diameter * 0.5, 0.25)
                let mut cov = 0.0
                if self.shape < 0.5 {
                    // Soft disc: solid core, soft rim.
                    cov = 1.0 - smoothstep(r * 0.55, r + 0.5, length(p))
                } else if self.shape < 1.5 {
                    let q = abs(p) - vec2(r, r)
                    cov = clamp(0.5 - max(q.x, q.y), 0.0, 1.0)
                } else if self.shape < 2.5 {
                    // Spark: a streak along the velocity.
                    let v = self.velocity
                    let len = length(v)
                    let dir = v / max(len, 0.0001)
                    let along = dot(p0, dir)
                    let across = dot(p0, vec2(-dir.y, dir.x))
                    let half = max(r, min(len * 0.03, self.rect_size.x * 0.5 - 1.0))
                    cov = (1.0 - smoothstep(half * 0.3, half, abs(along))) * (1.0 - smoothstep(r * 0.3, r * 0.5 + 0.5, abs(across)))
                } else if self.shape < 3.5 {
                    cov = clamp(0.5 - abs(length(p) - r * 0.8) + r * 0.12, 0.0, 1.0)
                } else {
                    let ang = atan2(p.y, p.x)
                    let k = r * (0.55 + 0.45 * cos(ang * 5.0))
                    cov = clamp(0.5 - (length(p) - k), 0.0, 1.0)
                }
                let a = self.tint.w * cov
                return vec4(self.tint.xyz * a, a * (1.0 - self.additive))
            }
        }

        // One stroke segment: a capsule from seg_a to seg_b whose radius
        // goes from radius_a to radius_b. butt (caps), fill (premultiplied).
        segment +: {
            seg_a: instance(vec2(0.0, 0.0))
            seg_b: instance(vec2(0.0, 0.0))
            radius_a: instance(0.0)
            radius_b: instance(0.0)
            butt: instance(0.0)
            fill: instance(vec4(0.0, 0.0, 0.0, 0.0))
            pixel: fn() {
                let p = self.rect_pos + self.pos * self.rect_size
                let pa = p - self.seg_a
                let ba = self.seg_b - self.seg_a
                let h0 = dot(pa, ba) / max(dot(ba, ba), 0.0001)
                let h = clamp(h0, 0.0, 1.0)
                let r = mix(self.radius_a, self.radius_b, h)
                let d = length(pa - ba * h) - r
                let mut cov = clamp(0.5 - d, 0.0, 1.0) * clamp(r * 2.0, 0.0, 1.0)
                if self.butt > 0.5 {
                    let len = length(ba)
                    let along = h0 * len
                    cov = cov * clamp(along + 0.5, 0.0, 1.0) * clamp(len - along + 0.5, 0.0, 1.0)
                }
                return self.fill * cov
            }
        }

        // Up to eight samples reduced into one. weights_lo, weights_hi =
        // weights of t0..t3, t4..t7; mode 0 weighted sum / 1 over, newest
        // t0 on top / 2 add / 3 screen.
        accum +: {
            weights_lo: instance(vec4(0.0, 0.0, 0.0, 0.0))
            weights_hi: instance(vec4(0.0, 0.0, 0.0, 0.0))
            mode: instance(0.0)
            pixel: fn() {
                let s0 = self.t0.sample(self.pos) * self.weights_lo.x
                let s1 = self.t1.sample(self.pos) * self.weights_lo.y
                let s2 = self.t2.sample(self.pos) * self.weights_lo.z
                let s3 = self.t3.sample(self.pos) * self.weights_lo.w
                let s4 = self.t4.sample(self.pos) * self.weights_hi.x
                let s5 = self.t5.sample(self.pos) * self.weights_hi.y
                let s6 = self.t6.sample(self.pos) * self.weights_hi.z
                let s7 = self.t7.sample(self.pos) * self.weights_hi.w
                if self.mode < 0.5 {
                    return s0 + s1 + s2 + s3 + s4 + s5 + s6 + s7
                }
                if self.mode < 1.5 {
                    let mut o = s7
                    o = s6 + o * (1.0 - s6.w)
                    o = s5 + o * (1.0 - s5.w)
                    o = s4 + o * (1.0 - s4.w)
                    o = s3 + o * (1.0 - s3.w)
                    o = s2 + o * (1.0 - s2.w)
                    o = s1 + o * (1.0 - s1.w)
                    return s0 + o * (1.0 - s0.w)
                }
                if self.mode < 2.5 {
                    return min(s0 + s1 + s2 + s3 + s4 + s5 + s6 + s7, vec4(1.0, 1.0, 1.0, 1.0))
                }
                let one = vec4(1.0, 1.0, 1.0, 1.0)
                return one - (one - s0) * (one - s1) * (one - s2) * (one - s3) * (one - s4) * (one - s5) * (one - s6) * (one - s7)
            }
        }

        // The velocity blur: sample the frame along the path between two
        // affine maps of the pixel (shutter open and close, relative to the
        // frame's own time). map_open = (a, b, c, d), map_open_t = (tx, ty) of the
        // open map; map_close, map_close_t the same for the close map; taps.
        velocity +: {
            map_open: instance(vec4(0.0, 0.0, 0.0, 0.0))
            map_open_t: instance(vec2(0.0, 0.0))
            map_close: instance(vec4(0.0, 0.0, 0.0, 0.0))
            map_close_t: instance(vec2(0.0, 0.0))
            taps: instance(0.0)
            pixel: fn() {
                let size = self.rect_size
                let p = self.pos * size
                let q0 = vec2(self.map_open.x * p.x + self.map_open.z * p.y + self.map_open_t.x, self.map_open.y * p.x + self.map_open.w * p.y + self.map_open_t.y)
                let q1 = vec2(self.map_close.x * p.x + self.map_close.z * p.y + self.map_close_t.x, self.map_close.y * p.x + self.map_close.w * p.y + self.map_close_t.y)
                let n = max(self.taps, 1.0)
                let mut sum = vec4(0.0, 0.0, 0.0, 0.0)
                for i in 0..128 {
                    if f32(i) >= n {
                        break
                    }
                    let u = (f32(i) + 0.5) / n
                    sum = sum + self.in0(mix(q0, q1, u) / size, 0.0)
                }
                return sum / n
            }
        }

        // Colour operations: one pass kind each; `amount` is the effect's
        // mix. t0 the input, t1 the curve / LUT table where there is one.

        // in_black, in_white, gamma (a power of 1 / gamma), out_black,
        // out_white, channels (1 where the channel is levelled).
        levels +: {
            amount: instance(0.0)
            in_black: instance(0.0)
            in_white: instance(0.0)
            gamma: instance(0.0)
            out_black: instance(0.0)
            out_white: instance(0.0)
            channels: instance(vec3(0.0, 0.0, 0.0))
            pixel: fn() {
                let src = self.t0.sample(self.pos)
                let c = self.unpremul(src)
                let lin = clamp((c - self.in_black) / max(self.in_white - self.in_black, 0.00001), vec3(0.0, 0.0, 0.0), vec3(1.0, 1.0, 1.0))
                let g = pow(lin, vec3(1.0 / self.gamma, 1.0 / self.gamma, 1.0 / self.gamma))
                let o = mix(c, self.out_black + g * (self.out_white - self.out_black), self.channels)
                return self.grade_out(src, o, self.amount)
            }
        }

        // hue (turns), saturation (a scale), lightness, colorize,
        // colorize_hue (turns), colorize_sat.
        hue_saturation +: {
            amount: instance(0.0)
            hue: instance(0.0)
            saturation: instance(0.0)
            lightness: instance(0.0)
            colorize: instance(0.0)
            colorize_hue: instance(0.0)
            colorize_sat: instance(0.0)
            hsl: fn(c: vec3) -> vec3 {
                let x = max(max(c.x, c.y), c.z)
                let n = min(min(c.x, c.y), c.z)
                let l = (x + n) * 0.5
                let d = x - n
                if d < 0.00001 {
                    return vec3(0.0, 0.0, l)
                }
                let s = d / (1.0 - abs(2.0 * l - 1.0) + 0.00001)
                let mut h = 0.0
                if x == c.x {
                    h = modf((c.y - c.z) / d + 6.0, 6.0)
                } else if x == c.y {
                    h = (c.z - c.x) / d + 2.0
                } else {
                    h = (c.x - c.y) / d + 4.0
                }
                return vec3(h / 6.0, s, l)
            }
            rgb: fn(h: vec3) -> vec3 {
                // f(n) = l - a·max(-1, min(k - 3, 9 - k, 1)), k = (n + 12h) mod 12.
                let a = h.y * min(h.z, 1.0 - h.z)
                let n = vec3(0.0, 8.0, 4.0) + h.x * 12.0
                let k = n - floor(n / 12.0) * 12.0
                return vec3(h.z, h.z, h.z) - a * max(min(min(k - 3.0, vec3(9.0, 9.0, 9.0) - k), vec3(1.0, 1.0, 1.0)), vec3(-1.0, -1.0, -1.0))
            }
            pixel: fn() {
                let src = self.t0.sample(self.pos)
                let c = self.unpremul(src)
                let mut h = self.hsl(c)
                if self.colorize > 0.5 {
                    h = vec3(self.colorize_hue, self.colorize_sat, h.z)
                } else {
                    h = vec3(fract(h.x + self.hue + 1.0), clamp(h.y * self.saturation, 0.0, 1.0), h.z)
                }
                let mut r = self.rgb(h)
                if self.lightness > 0.0 {
                    r = r + (vec3(1.0, 1.0, 1.0) - r) * self.lightness
                } else {
                    r = r * (1.0 + self.lightness)
                }
                return self.grade_out(src, r, self.amount)
            }
        }

        // gains = per-channel multipliers (white balance).
        gains +: {
            amount: instance(0.0)
            gains: instance(vec3(0.0, 0.0, 0.0))
            pixel: fn() {
                let src = self.t0.sample(self.pos)
                let c = self.unpremul(src)
                return self.grade_out(src, c * self.gains, self.amount)
            }
        }

        // scale (2^exposure), offset, power (1 / gamma).
        exposure +: {
            amount: instance(0.0)
            scale: instance(0.0)
            offset: instance(0.0)
            power: instance(0.0)
            pixel: fn() {
                let src = self.t0.sample(self.pos)
                let c = self.unpremul(src)
                let e = max(c * self.scale + self.offset, vec3(0.0, 0.0, 0.0))
                return self.grade_out(src, pow(e, vec3(self.power, self.power, self.power)), self.amount)
            }
        }

        brightness_contrast +: {
            amount: instance(0.0)
            brightness: instance(0.0)
            contrast: instance(0.0)
            pixel: fn() {
                let src = self.t0.sample(self.pos)
                let c = self.unpremul(src)
                let k = tan((self.contrast + 1.0) * 0.785398)
                return self.grade_out(src, (c + self.brightness - 0.5) * k + 0.5, self.amount)
            }
        }

        // black, white (straight colours the luma is mapped between),
        // strength.
        tint +: {
            amount: instance(0.0)
            black: instance(vec4(0.0, 0.0, 0.0, 0.0))
            white: instance(vec4(0.0, 0.0, 0.0, 0.0))
            strength: instance(0.0)
            pixel: fn() {
                let src = self.t0.sample(self.pos)
                let c = self.unpremul(src)
                return self.grade_out(src, mix(c, mix(self.black.xyz, self.white.xyz, self.luma(c)), self.strength), self.amount)
            }
        }

        invert +: {
            amount: instance(0.0)
            pixel: fn() {
                let src = self.t0.sample(self.pos)
                let c = self.unpremul(src)
                return self.grade_out(src, vec3(1.0, 1.0, 1.0) - c, self.amount)
            }
        }

        // strength (negative lightens), midpoint, roundness, feather,
        // edge_color (straight).
        vignette +: {
            amount: instance(0.0)
            strength: instance(0.0)
            midpoint: instance(0.0)
            roundness: instance(0.0)
            feather: instance(0.0)
            edge_color: instance(vec4(0.0, 0.0, 0.0, 0.0))
            pixel: fn() {
                let size = self.rect_size
                let src = self.t0.sample(self.pos)
                let c = self.unpremul(src)
                let mut o = c
                let q = (self.pos - 0.5) * 2.0
                let round = (self.roundness + 1.0) * 0.5
                let aspect = size.x / max(size.y, 1.0)
                let v = vec2(q.x * mix(1.0, aspect, round) / mix(1.0, sqrt(aspect * aspect * 0.5 + 0.5), round), q.y / mix(1.0, sqrt(aspect * aspect * 0.5 + 0.5), round))
                let d = length(v) / 1.41421356
                let k = smoothstep(self.midpoint, self.midpoint + max(self.feather, 0.001) * (1.0 - self.midpoint + 0.001), d)
                if self.strength >= 0.0 {
                    o = mix(c, self.edge_color.xyz, k * self.strength)
                } else {
                    o = mix(c, vec3(1.0, 1.0, 1.0), -k * self.strength)
                }
                return self.grade_out(src, o, self.amount)
            }
        }

        // strength, grain_size (px), colored (0 mono / 1 per channel),
        // seed (the frame's).
        grain +: {
            amount: instance(0.0)
            strength: instance(0.0)
            grain_size: instance(0.0)
            colored: instance(0.0)
            seed: instance(0.0)
            pixel: fn() {
                let size = self.rect_size
                let src = self.t0.sample(self.pos)
                let c = self.unpremul(src)
                let cell = floor(self.pos * size / max(self.grain_size, 0.5))
                let n0 = self.hash31(vec3(cell.x, cell.y, self.seed)) + self.hash31(vec3(cell.x + 11.0, cell.y + 7.0, self.seed)) - 1.0
                let mut n = vec3(n0, n0, n0)
                if self.colored > 0.5 {
                    n = vec3(n0, self.hash31(vec3(cell.x, cell.y, self.seed + 3.1)) * 2.0 - 1.0, self.hash31(vec3(cell.x, cell.y, self.seed + 7.7)) * 2.0 - 1.0)
                }
                let l = self.luma(c)
                return self.grade_out(src, c + n * self.strength * (0.3 + 2.8 * l * (1.0 - l)), self.amount)
            }
        }

        // strength, center (px), linear (0 radial from center / 1 along
        // axis), axis (dir.x, dir.y), corner (px, the radial scale).
        chroma +: {
            amount: instance(0.0)
            strength: instance(0.0)
            center: instance(vec2(0.0, 0.0))
            linear: instance(0.0)
            axis: instance(vec2(0.0, 0.0))
            corner: instance(0.0)
            pixel: fn() {
                let size = self.rect_size
                let src = self.t0.sample(self.pos)
                let p = self.pos * size
                let mut off = self.axis * self.strength
                if self.linear < 0.5 {
                    off = (p - self.center) / max(self.corner, 1.0) * self.strength
                }
                let r = self.in0((p + off) / size, 1.0)
                let b = self.in0((p - off) / size, 1.0)
                let a = max(max(r.w, src.w), b.w)
                let px = vec4(r.x, src.y, b.z, a)
                return mix(src, px, self.amount)
            }
        }

        // t1 the 256x1 curves table (master folded in).
        curves +: {
            amount: instance(0.0)
            pixel: fn() {
                let src = self.t0.sample(self.pos)
                let c = self.unpremul(src)
                let u = clamp(c, vec3(0.0, 0.0, 0.0), vec3(1.0, 1.0, 1.0)) * (255.0 / 256.0) + 0.5 / 256.0
                let o = vec3(self.t1.sample(vec2(u.x, 0.5)).x, self.t1.sample(vec2(u.y, 0.5)).y, self.t1.sample(vec2(u.z, 0.5)).z)
                return self.grade_out(src, o, self.amount)
            }
        }

        // t1 the LUT atlas; lut_size (cube edge), strength.
        lut +: {
            amount: instance(0.0)
            lut_size: instance(0.0)
            strength: instance(0.0)
            lut_at: fn(c: vec3) -> vec3 {
                let n = self.lut_size
                let x = clamp(c, vec3(0.0, 0.0, 0.0), vec3(1.0, 1.0, 1.0)) * (n - 1.0)
                let z0 = min(floor(x.z), n - 2.0)
                let fz = x.z - z0
                let u0 = vec2((z0 * n + x.x + 0.5) / (n * n), (x.y + 0.5) / n)
                let u1 = vec2(((z0 + 1.0) * n + x.x + 0.5) / (n * n), (x.y + 0.5) / n)
                return mix(self.t1.sample(u0).xyz, self.t1.sample(u1).xyz, fz)
            }
            pixel: fn() {
                let src = self.t0.sample(self.pos)
                let c = self.unpremul(src)
                return self.grade_out(src, mix(c, self.lut_at(c), self.strength), self.amount)
            }
        }
    }
}

/// W3C blend modes, in the catalog's order.
#[derive(Clone, Copy, Debug, PartialEq, Script, ScriptHook)]
#[repr(u32)]
pub enum KitBlend {
    #[pick]
    Normal = 1,
    Multiply = 2,
    Screen = 3,
    Overlay = 4,
    Darken = 5,
    Lighten = 6,
    ColorDodge = 7,
    ColorBurn = 8,
    HardLight = 9,
    SoftLight = 10,
    Difference = 11,
    Exclusion = 12,
    Add = 13,
    Subtract = 14,
    Divide = 15,
    Hue = 16,
    Saturation = 17,
    Color = 18,
    Luminosity = 19,
}

impl KitBlend {
    pub const ALL: [KitBlend; 19] = [
        Self::Normal, Self::Multiply, Self::Screen, Self::Overlay, Self::Darken, Self::Lighten, Self::ColorDodge, Self::ColorBurn, Self::HardLight,
        Self::SoftLight, Self::Difference, Self::Exclusion, Self::Add, Self::Subtract, Self::Divide, Self::Hue, Self::Saturation, Self::Color,
        Self::Luminosity,
    ];

    /// By the catalog name (`multiply`, `soft_light`, …); normal otherwise.
    pub fn from_name(name: &str) -> Self {
        crate::catalog::BLEND_MODES.iter().position(|m| *m == name.trim_start_matches('@')).map(|i| Self::ALL[i]).unwrap_or(Self::Normal)
    }
}

#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawKit {
    #[deref]
    pub draw_super: DrawQuad,
    #[live]
    pub blend: KitBlend,
}

impl DrawKit {
    /// Back to the default blend; a pass sets every field it declares.
    pub fn reset(&mut self) {
        self.blend = KitBlend::Normal;
    }

    /// Sets the pass's instance field `name` for the next draw. A name the
    /// pass does not declare is logged once.
    pub fn set(&mut self, cx: &Cx, name: LiveId, v: &[f32]) {
        if !self.draw_vars.set_dyn_instance(cx, name, v) {
            static MISSING: std::sync::Mutex<Vec<u64>> = std::sync::Mutex::new(Vec::new());
            if let Ok(mut seen) = MISSING.lock() {
                if !seen.contains(&name.0) {
                    seen.push(name.0);
                    log!("render-kit: a pass has no instance field `{name}`");
                }
            }
        }
    }

    pub fn set_f32(&mut self, cx: &Cx, name: LiveId, v: f32) {
        self.set(cx, name, &[v]);
    }

    pub fn set_vec2(&mut self, cx: &Cx, name: LiveId, v: [f32; 2]) {
        self.set(cx, name, &v);
    }

    pub fn set_vec3(&mut self, cx: &Cx, name: LiveId, v: [f32; 3]) {
        self.set(cx, name, &v);
    }

    pub fn set_vec4(&mut self, cx: &Cx, name: LiveId, v: Vec4f) {
        self.set(cx, name, &[v.x, v.y, v.z, v.w]);
    }
}

/// One draw per pass kind.
#[derive(Script, ScriptHook)]
pub struct KitDraws {
    #[live]
    pub copy: DrawKit,
    #[live]
    pub mix: DrawKit,
    #[live]
    pub gauss: DrawKit,
    #[live]
    pub line_blur: DrawKit,
    #[live]
    pub threshold: DrawKit,
    #[live]
    pub add_light: DrawKit,
    #[live]
    pub shadow_mask: DrawKit,
    #[live]
    pub shadow_comp: DrawKit,
    #[live]
    pub jfa_seed: DrawKit,
    #[live]
    pub jfa_step: DrawKit,
    #[live]
    pub stroke_comp: DrawKit,
    #[live]
    pub bevel: DrawKit,
    #[live]
    pub composite: DrawKit,
    #[live]
    pub solid: DrawKit,
    #[live]
    pub gradient: DrawKit,
    #[live]
    pub noise_gen: DrawKit,
    #[live]
    pub displace: DrawKit,
    #[live]
    pub turbulence: DrawKit,
    #[live]
    pub slices: DrawKit,
    #[live]
    pub ripple: DrawKit,
    #[live]
    pub shape_mask: DrawKit,
    #[live]
    pub matte: DrawKit,
    #[live]
    pub plane: DrawKit,
    #[live]
    pub dof: DrawKit,
    #[live]
    pub particle: DrawKit,
    #[live]
    pub segment: DrawKit,
    #[live]
    pub accum: DrawKit,
    #[live]
    pub velocity: DrawKit,
    #[live]
    pub levels: DrawKit,
    #[live]
    pub hue_saturation: DrawKit,
    #[live]
    pub gains: DrawKit,
    #[live]
    pub exposure: DrawKit,
    #[live]
    pub brightness_contrast: DrawKit,
    #[live]
    pub tint: DrawKit,
    #[live]
    pub invert: DrawKit,
    #[live]
    pub vignette: DrawKit,
    #[live]
    pub grain: DrawKit,
    #[live]
    pub chroma: DrawKit,
    #[live]
    pub curves: DrawKit,
    #[live]
    pub lut: DrawKit,
}
