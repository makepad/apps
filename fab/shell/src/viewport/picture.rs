//! The project's picture: the settled composite shrunk on the GPU to [`PICTURE_SIDE`] before it is
//! read back. Each picture pixel averages the composite pixels it covers, the box the plate list's
//! CPU shrink takes (`fab_ai::plates::thumbnail_png`), so the picture is the same and only its
//! pixels cross to the CPU: a 1243 x 1705 view reads back 0.3 MB, not 8.5 MB.

use super::*;

script_mod! {
    use mod.pod.*
    use mod.math.*
    use mod.shader.*
    use mod.draw

    mod.draw.DrawFabPicture = mod.std.set_type_default() do #(DrawFabPicture::script_shader(vm)){
        ..mod.draw.DrawQuad
        color_format: @Bgra8NoBlend
        src: texture_2d(float)
        // xy: the composite's pixels, zw: the picture's
        u_size: uniform(vec4(1.0, 1.0, 1.0, 1.0))
        depth_clip: 0.0

        vertex: fn() {
            self.pos = self.geom.pos
            self.world = vec4(self.geom.pos.x, self.geom.pos.y, 0.0, 1.0)
            self.vertex_pos = vec4(
                self.geom.pos.x * 2.0 - 1.0
                1.0 - self.geom.pos.y * 2.0
                0.0
                1.0
            )
        }

        // Picture pixel o covers composite pixels [o * s / d, (o + 1) * s / d) in whole pixels
        // (at least one), the mean of their 8-bit values rounded down, opaque. The half added
        // before each division keeps an exact multiple from landing a hair under its integer.
        pixel: fn() -> vec4f {
            let s = self.u_size.xy
            let d = self.u_size.zw
            let o = floor(self.pos * d)
            let lo = floor((o * s + vec2(0.5, 0.5)) / d)
            let hi = min(max(floor(((o + vec2(1.0, 1.0)) * s + vec2(0.5, 0.5)) / d), lo + vec2(1.0, 1.0)), s)
            var sum = vec3(0.0, 0.0, 0.0)
            var y = lo.y
            while y < hi.y {
                var x = lo.x
                while x < hi.x {
                    let c = self.src.sample_nearest(vec2(x + 0.5, y + 0.5) / s)
                    sum = sum + floor(c.xyz * 255.0 + vec3(0.5, 0.5, 0.5))
                    x = x + 1.0
                }
                y = y + 1.0
            }
            let n = max((hi.x - lo.x) * (hi.y - lo.y), 1.0)
            let mean = floor((sum + vec3(0.5, 0.5, 0.5)) / n) / 255.0
            return vec4(mean.x, mean.y, mean.z, 1.0)
        }
    }

    // rig (the 9x reference's `vgrab`): the view copied pixel for pixel into a band of rows (the
    // quad is the whole view, placed so the band's rows fall in the pass)
    mod.draw.DrawFabViewBand = mod.std.set_type_default() do #(DrawFabViewBand::script_shader(vm)){
        ..mod.draw.DrawQuad
        color_format: @Bgra8NoBlend
        src: texture_2d(float)
        depth_clip: 0.0

        pixel: fn() -> vec4f {
            let c = self.src.sample_nearest(self.pos)
            return vec4(c.x, c.y, c.z, 1.0)
        }
    }
}

#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawFabPicture {
    #[deref]
    pub draw_super: DrawQuad,
}

/// What a picture or a band that was not asked in a draw waits for.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum DrawWait {
    /// the draw cannot be made or the readback request limit is reached: retry next frame
    #[default]
    Frame,
    /// its shader compiles: `Event::DrawShadersReady` says when to ask again
    Pipeline,
}

/// The picture's pass, target and draw, made on first use.
#[derive(Default)]
pub struct Picture {
    pass: Option<(DrawPass, DrawList, Texture)>,
    draw: Option<Box<DrawFabPicture>>,
    wait: DrawWait,
    readback_wait_logged: bool,
}

/// The picture's size for a composite of `sw` x `sh` pixels: its longest side [`PICTURE_SIDE`],
/// never larger than the composite, as the plate list rounds it.
fn picture_size(sw: usize, sh: usize) -> (usize, usize) {
    let k = (PICTURE_SIDE as f64 / sw.max(sh) as f64).min(1.0);
    (((sw as f64 * k).round() as usize).max(1), ((sh as f64 * k).round() as usize).max(1))
}

impl Picture {
    /// What the last `take` that gave `Ok(None)` waits for: a frame when the draw
    /// cannot be made or the readback request limit is reached, otherwise its shader.
    pub fn wait(&self) -> DrawWait {
        self.wait
    }

    /// Shrink `comp` (what `comp_pass` draws, `size` its logical size) into the picture in this
    /// draw and queue its read-back: the picture's pass paints after the composite and before
    /// what the composite paints for. `Ok(None)` asks for another frame when the shrink's draw
    /// cannot be made or the readback request limit is reached, or waits for the shader when
    /// it is still compiling (it would paint nothing). [`Picture::wait`] records which.
    pub fn take(&mut self, cx: &mut Cx2d, comp_pass: &DrawPass, comp: &Texture, size: DVec2) -> Result<Option<ReadbackTicket>, ReadbackError> {
        if self.pass.is_none() {
            let target = Texture::new_with_format(cx.cx, TextureFormat::RenderBGRAu8 { size: TextureSize::Auto, initial: true });
            self.pass = Some((DrawPass::new_with_name(cx.cx, "fab_picture"), DrawList::new(cx.cx), target));
        }
        if self.draw.is_none() {
            self.draw = cx.cx.try_with_vm(|vm| Box::new(DrawFabPicture::script_new_with_default(vm)));
        }
        let Some(draw) = self.draw.as_mut() else {
            self.wait = DrawWait::Frame;
            return Ok(None);
        };
        let shader = draw.draw_super.draw_vars.draw_shader_id;
        if !shader.is_some_and(|shader| cx.cx.draw_shader_ready(shader, true)) {
            self.wait = DrawWait::Pipeline;
            return Ok(None);
        }
        let dpi = cx.passes[comp_pass.draw_pass_id()].dpi_factor.unwrap_or(1.0);
        let (sw, sh) = Cx::pass_pixel_size(size, dpi);
        if sw == 0 || sh == 0 {
            return Err(ReadbackError::InvalidSize);
        }
        let (w, h) = picture_size(sw, sh);
        {
            let dv = &mut draw.draw_super.draw_vars;
            dv.set_texture(0, comp);
            dv.set_uniform(cx.cx, live_id!(u_size), &[sw as f32, sh as f32, w as f32, h as f32]);
        }
        let (pass, list, target) = self.pass.as_mut().unwrap();
        pass.insert_after(cx.cx, comp_pass.draw_pass_id());
        let picture = dvec2(w as f64, h as f64);
        pass.clear_color_textures(cx.cx);
        pass.set_color_texture(cx, target, DrawPassClearColor::ClearWith(vec4(0.0, 0.0, 0.0, 0.0)));
        pass.clear_depth_texture(cx.cx);
        cx.begin_pass(pass, Some(1.0));
        pass.set_size(cx, picture);
        list.begin_always(cx);
        let pass_size = cx.current_pass_size();
        cx.begin_root_turtle(pass_size, Layout::flow_overlay());
        draw.draw_abs(cx, Rect { pos: dvec2(0.0, 0.0), size: picture });
        cx.end_pass_sized_turtle();
        list.end(cx);
        cx.end_pass(pass);
        match target.read_back(cx.cx, ReadbackRequest::default()) {
            Ok(ticket) => {
                self.readback_wait_logged = false;
                Ok(Some(ticket))
            }
            Err(ReadbackError::Backpressure) => {
                if !self.readback_wait_logged {
                    log!("fab: context readback request limit reached; project picture waits");
                    self.readback_wait_logged = true;
                }
                self.wait = DrawWait::Frame;
                Ok(None)
            }
            Err(error) => {
                self.readback_wait_logged = false;
                Err(error)
            }
        }
    }
}

#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawFabViewBand {
    #[deref]
    pub draw_super: DrawQuad,
}

/// rig (the 9x reference's `vgrab`): a view read back whole at its own pixels, a band of rows at a
/// time: one read-back of a 3x view (2343 x 2814) is over the platform's [`TEXTURE_READBACK_MAX_BYTES`]
/// with its rows padded, so each band is read back alone, the next asked once the last is in.
#[derive(Default)]
pub struct ViewGrab {
    pass: Option<(DrawPass, DrawList, Texture)>,
    draw: Option<Box<DrawFabViewBand>>,
    wait: DrawWait,
    readback_wait_logged: bool,
    /// The view's pixels and its RGBA so far, the next band's first row, the band in flight (its
    /// ticket, first row and rows).
    size: (usize, usize),
    rgba: Vec<u8>,
    row: usize,
    band: Option<(ReadbackTicket, usize, usize)>,
    active: bool,
    /// Bands of dropped grabs still in flight: taken and let go as they come in.
    stale: Vec<ReadbackTicket>,
}

impl ViewGrab {
    /// A grab is under way (begun, not yet whole or dropped).
    pub fn active(&self) -> bool {
        self.active
    }

    /// What the last `step` that gave `Ok(false)` waits for: a frame when the draw
    /// cannot be made or the readback request limit is reached, otherwise its shader.
    pub fn wait(&self) -> DrawWait {
        self.wait
    }

    /// Begin a grab: the next [`ViewGrab::step`]s read the view back from its first row.
    pub fn begin(&mut self) {
        self.readback_wait_logged = false;
        self.active = true;
        self.size = (0, 0);
        self.rgba.clear();
        self.row = 0;
        self.band = None;
    }

    /// Drop the grab under way (the view changed).
    pub fn drop_grab(&mut self) {
        self.active = false;
        self.stale.extend(self.band.take().map(|b| b.0));
        self.rgba = Vec::new();
    }

    /// In a draw: ask the next band's read-back of `src` (what `comp_pass` draws at `size`, its
    /// logical size), unless one is in flight. `Ok(false)` asks for another frame when the band's
    /// draw cannot be made or the readback request limit is reached, or waits for the shader
    /// while it compiles. [`ViewGrab::wait`] records which.
    pub fn step(&mut self, cx: &mut Cx2d, comp_pass: &DrawPass, src: &Texture, size: DVec2) -> Result<bool, ReadbackError> {
        if !self.active || self.band.is_some() {
            return Ok(true);
        }
        if self.pass.is_none() {
            let target = Texture::new_with_format(cx.cx, TextureFormat::RenderBGRAu8 { size: TextureSize::Auto, initial: true });
            self.pass = Some((DrawPass::new_with_name(cx.cx, "fab_view_grab"), DrawList::new(cx.cx), target));
        }
        if self.draw.is_none() {
            self.draw = cx.cx.try_with_vm(|vm| Box::new(DrawFabViewBand::script_new_with_default(vm)));
        }
        let Some(draw) = self.draw.as_mut() else {
            self.wait = DrawWait::Frame;
            return Ok(false);
        };
        let shader = draw.draw_super.draw_vars.draw_shader_id;
        if !shader.is_some_and(|shader| cx.cx.draw_shader_ready(shader, true)) {
            self.wait = DrawWait::Pipeline;
            return Ok(false);
        }
        let dpi = cx.passes[comp_pass.draw_pass_id()].dpi_factor.unwrap_or(1.0);
        let (w, h) = Cx::pass_pixel_size(size, dpi);
        if w == 0 || h == 0 {
            return Err(ReadbackError::InvalidSize);
        }
        if self.size != (w, h) {
            self.size = (w, h);
            self.rgba = vec![0; w * h * 4];
            self.row = 0;
        }
        // a band's rows padded to 4 KiB, a half of the read-back limit
        let padded = (w * 4).div_ceil(4096) * 4096;
        let rows = (TEXTURE_READBACK_MAX_BYTES / 2 / padded).min(h - self.row).max(1);
        draw.draw_super.draw_vars.set_texture(0, src);
        let (pass, list, target) = self.pass.as_mut().unwrap();
        pass.insert_after(cx.cx, comp_pass.draw_pass_id());
        pass.clear_color_textures(cx.cx);
        pass.set_color_texture(cx, target, DrawPassClearColor::ClearWith(vec4(0.0, 0.0, 0.0, 0.0)));
        pass.clear_depth_texture(cx.cx);
        let band = dvec2(w as f64, rows as f64);
        cx.begin_pass(pass, Some(1.0));
        pass.set_size(cx, band);
        list.begin_always(cx);
        let pass_size = cx.current_pass_size();
        cx.begin_root_turtle(pass_size, Layout::flow_overlay());
        draw.draw_abs(cx, Rect { pos: dvec2(0.0, -(self.row as f64)), size: dvec2(w as f64, h as f64) });
        cx.end_pass_sized_turtle();
        list.end(cx);
        cx.end_pass(pass);
        let ticket = match target.read_back(cx.cx, ReadbackRequest::default()) {
            Ok(ticket) => {
                self.readback_wait_logged = false;
                ticket
            }
            Err(ReadbackError::Backpressure) => {
                if !self.readback_wait_logged {
                    log!("fab: context readback request limit reached; view band waits");
                    self.readback_wait_logged = true;
                }
                self.wait = DrawWait::Frame;
                return Ok(false);
            }
            Err(error) => {
                self.readback_wait_logged = false;
                return Err(error);
            }
        };
        self.band = Some((ticket, self.row, rows));
        Ok(true)
    }

    /// On a signal: the band in flight, if it is in (`None` while it is not): `Ok(None)` when the
    /// next band is to be asked (in a draw), `Ok(Some)` once the view is whole, its size and RGBA,
    /// rows top to bottom (the grab is then over).
    pub fn take(&mut self, cx: &mut Cx) -> Option<Result<Option<(usize, usize, Vec<u8>)>, ReadbackError>> {
        if !self.stale.is_empty() {
            let done: Vec<ReadbackTicket> = cx.try_take_texture_readbacks_for(&self.stale).into_iter().map(|f| f.ticket).collect();
            self.stale.retain(|t| !done.contains(t));
        }
        let (ticket, row, rows) = self.band?;
        let frame = cx.try_take_texture_readbacks_for(&[ticket]).into_iter().next()?;
        self.band = None;
        let w = self.size.0;
        let data = match frame.data {
            Ok(data) if frame.width == w && frame.height == rows && data.len() >= w * rows * 4 => data,
            Ok(_) => {
                self.drop_grab();
                return Some(Err(ReadbackError::InvalidSize));
            }
            Err(e) => {
                self.drop_grab();
                return Some(Err(e));
            }
        };
        self.rgba[row * w * 4..(row + rows) * w * 4].copy_from_slice(&data[..w * rows * 4]);
        self.row = row + rows;
        if self.row < self.size.1 {
            return Some(Ok(None));
        }
        self.active = false;
        Some(Ok(Some((self.size.0, self.size.1, std::mem::take(&mut self.rgba)))))
    }
}
