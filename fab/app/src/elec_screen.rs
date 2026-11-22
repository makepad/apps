//! The nominal device's screen readers become the view's persistent panel textures.

use fab_elec_core::CompRef;
use fab_elec_fw::ScreenSink;
use fab_elec_mcu_parts::screen::ScreenReader;
use fab_elec_view::{live::sim::LiveScreen, ElecLive};
use makepad_widgets::makepad_platform::thread::ToUIReceiver;
use makepad_widgets::{Cx, Texture, TextureFormat, TextureUpdated};
use std::sync::Arc;

struct Panel {
    reader: ScreenReader,
    generation: u64,
    texture: Texture,
    staging: Vec<u32>,
}

/// One host's readers, replaced together whenever a nominal device is made.
#[derive(Default)]
pub struct Screens {
    sets: ToUIReceiver<Vec<(CompRef, ScreenReader)>>,
    panels: Vec<Panel>,
    clear: bool,
}

impl Screens {
    /// The maker sends only after building a device. Dropping this receiver rejects late sends.
    pub fn sink(&self) -> ScreenSink {
        let tx = self.sets.sender();
        Arc::new(move |readers| { let _ = tx.send(readers); })
    }

    /// A removed device clears the view on the next frame and cannot hand over late readers.
    pub fn clear(&mut self) {
        *self = Self { clear: true, ..Self::default() };
    }

    /// Copies only new generations, reusing each panel's texture and staging allocation.
    /// Assumed: the panel's brightness byte maps linearly to emitted light, byte / 255.
    pub fn frame(&mut self, cx: &mut Cx) -> bool {
        let mut newest = self.clear.then(Vec::new);
        self.clear = false;
        while let Ok(readers) = self.sets.try_recv() {
            newest = Some(readers);
        }
        let mut changed = false;
        if let Some(readers) = newest {
            self.panels.clear();
            let mut screens = Vec::with_capacity(readers.len());
            for (comp, reader) in readers {
                let (width, height) = reader.size();
                let texture = Texture::new_with_format(cx, TextureFormat::VecBGRAu8_32 {
                    width, height, data: Some(vec![0xff00_0000; width * height]), updated: TextureUpdated::Full,
                });
                screens.push(LiveScreen {
                    comp, texture: texture.clone(), size: (width as u32, height as u32),
                    glass: reader.glass().map(|n| n as u32),
                    on: false, brightness: 0.0, mean: [0.0; 3], t: 0.0,
                });
                self.panels.push(Panel {
                    reader, generation: 0, texture, staging: Vec::with_capacity(width * height),
                });
            }
            let live = &mut cx.get_global::<ElecLive>().sim;
            changed = !live.screens.is_empty() || !screens.is_empty();
            if changed { live.screens = Arc::new(screens); }
        }
        for (index, panel) in self.panels.iter_mut().enumerate() {
            let frame = panel.reader.latest();
            if frame.generation == panel.generation { continue; }
            panel.staging.clear();
            panel.staging.extend_from_slice(&frame.bgra);
            // A backend may have released its CPU data; swap accepts that and returns an empty Vec.
            panel.texture.swap_vec_u32(cx, &mut panel.staging);
            let live = &mut cx.get_global::<ElecLive>().sim;
            let screen = &mut Arc::make_mut(&mut live.screens)[index];
            screen.on = frame.on;
            screen.brightness = frame.brightness as f32 / 255.0;
            screen.mean = frame.mean.map(|channel| channel * screen.brightness);
            screen.t = frame.t;
            panel.generation = frame.generation;
            changed = true;
        }
        if changed { cx.get_global::<ElecLive>().sim.screens_key += 1; }
        changed
    }
}
