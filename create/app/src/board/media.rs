//! Board pictures are decoded by the visible-work pool. Only the UI owns their textures.
use makepad_town_core::event::{Id, TownId};
use makepad_widgets::makepad_platform::thread::{Class, TaskHandle, TaskPool};
use makepad_widgets::{decode_image_from_data, log, Cx, ImageBuffer, Texture, TextureFormat};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;

pub const MEDIA_CACHE_MAX_BYTES: usize = 128 * 1024 * 1024;
pub const MEDIA_MAX_IN_FLIGHT: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaSlotState {
    Missing,
    Queued,
    Decoding,
    Ready { w: u32, h: u32 },
    Failed,
}

type Key = (TownId, Id);

struct MediaEntry {
    state: MediaSlotState,
    texture: Option<Texture>,
    bytes: usize,
    last_draw: u64,
}

#[derive(Default)]
pub struct BoardMediaCache {
    entries: HashMap<Key, MediaEntry>,
    in_flight: Vec<(Key, TaskHandle<Option<ImageBuffer>>)>,
    queued: VecDeque<(Key, PathBuf)>,
    lru_tick: u64,
    bytes: usize,
    over_budget: bool,
}

/// Total texel bytes held by an installed image texture, including every mip level
/// when the texture carries a mip chain.
fn texture_bytes(cx: &mut Cx, texture: &Texture) -> usize {
    match texture.get_format(cx) {
        TextureFormat::VecMipBGRAu8_32 { width, height, data, max_level, .. } => {
            let cpu = data.as_ref().map_or(0, |d| d.len() * size_of::<u32>());
            let (mut w, mut h, mut chain) = (*width, *height, 0usize);
            if w > 0 && h > 0 {
                let levels = max_level.map_or(1, |m| m + 1);
                for _ in 0..levels {
                    chain = chain.saturating_add(w.saturating_mul(h).saturating_mul(size_of::<u32>()));
                    if w == 1 && h == 1 { break }
                    w = (w / 2).max(1);
                    h = (h / 2).max(1);
                }
            }
            cpu.max(chain)
        }
        TextureFormat::VecBGRAu8_32 { width, height, data, .. } => {
            data.as_ref().map_or_else(
                || width.saturating_mul(*height).saturating_mul(size_of::<u32>()),
                |d| d.len() * size_of::<u32>(),
            )
        }
        _ => 0,
    }
}

/// Drops the CPU pixel/mip backing and retires the GPU allocation when this cache
/// owns the last handle (matching `evict_image_from_cache`).
fn release_texture(cx: &mut Cx, texture: Texture) {
    if texture.readers() == 1 {
        match texture.get_format(cx) {
            TextureFormat::VecBGRAu8_32 { data, .. } => *data = None,
            TextureFormat::VecMipBGRAu8_32 { data, .. } => *data = None,
            _ => {}
        }
        texture.release(cx);
    }
}

/// Drops retained draw-list texture bindings before releasing CPU backing and GPU storage
/// for every texture leaving this cache.
fn release_textures(cx: &mut Cx, textures: Vec<Texture>) {
    if textures.is_empty() { return }
    let ids: Vec<_> = textures.iter().map(Texture::texture_id).collect();
    cx.draw_lists.release_bindings(&ids, &[]);
    for texture in textures {
        release_texture(cx, texture);
    }
}

impl BoardMediaCache {
    pub fn state(&self, town: TownId, blob_id: Id) -> MediaSlotState {
        self.entries.get(&(town, blob_id)).map_or(MediaSlotState::Missing, |entry| entry.state)
    }

    /// A path is needed only for the first request after a blob arrives or its texture is evicted.
    pub fn request(&mut self, cx: &mut Cx, town: TownId, blob_id: Id, path: Option<PathBuf>) -> Option<&Texture> {
        let key = (town, blob_id);
        self.lru_tick += 1;
        match self.entries.entry(key) {
            std::collections::hash_map::Entry::Occupied(mut slot) => {
                slot.get_mut().last_draw = self.lru_tick;
                slot.into_mut().texture.as_ref()
            }
            std::collections::hash_map::Entry::Vacant(slot) => {
                let path = path?;
                let entry = slot.insert(MediaEntry { state: MediaSlotState::Queued, texture: None, bytes: 0, last_draw: self.lru_tick });
                if self.in_flight.len() < MEDIA_MAX_IN_FLIGHT {
                    Self::start(cx, key, path, entry, &mut self.in_flight);
                } else {
                    if self.queued.is_empty() { log!("Board pictures are waiting for a decode slot"); }
                    self.queued.push_back((key, path));
                }
                None
            }
        }
    }

    fn start(cx: &mut Cx, key: Key, path: PathBuf, entry: &mut MediaEntry,
             in_flight: &mut Vec<(Key, TaskHandle<Option<ImageBuffer>>)>) {
        match cx.task_pool().submit_named(Class::Visible, "board media decode", move || {
            let bytes = std::fs::read(path).ok()?;
            let mut image = decode_image_from_data(&bytes).ok()?;
            image.prepare_for_texture();
            Some(image)
        }) {
            Ok(task) => {
                in_flight.push((key, task));
                entry.state = MediaSlotState::Decoding;
            }
            Err(_) => entry.state = MediaSlotState::Failed,
        }
    }

    /// A completion changes what the card shows, including when its data could not be decoded.
    pub fn poll(&mut self, cx: &mut Cx) -> bool {
        let mut changed = false;
        self.in_flight.retain_mut(|(key, task)| {
            let Some(result) = task.try_take() else { return true };
            let entry = self.entries.get_mut(key).unwrap();
            if let Ok(Some(image)) = result {
                let (w, h) = (image.width as u32, image.height as u32);
                let texture = image.into_new_texture(cx);
                entry.bytes = texture_bytes(cx, &texture);
                self.bytes += entry.bytes;
                entry.state = MediaSlotState::Ready { w, h };
                entry.texture = Some(texture);
            } else {
                entry.state = MediaSlotState::Failed;
            }
            changed = true;
            false
        });
        while self.in_flight.len() < MEDIA_MAX_IN_FLIGHT {
            let Some((key, path)) = self.queued.pop_front() else { break };
            Self::start(cx, key, path, self.entries.get_mut(&key).unwrap(), &mut self.in_flight);
        }
        changed
    }

    pub fn evict_to_budget(&mut self, cx: &mut Cx, keep: &HashSet<Key>) {
        self.evict(cx, keep, MEDIA_CACHE_MAX_BYTES);
    }

    pub(super) fn evict(&mut self, cx: &mut Cx, keep: &HashSet<Key>, budget: usize) {
        if self.bytes > budget {
            let mut candidates: Vec<_> = self.entries.iter()
                .filter(|(key, entry)| entry.texture.is_some() && !keep.contains(*key))
                .map(|(&key, entry)| (entry.last_draw, key)).collect();
            candidates.sort_unstable();
            let mut evicted = Vec::new();
            for (_, key) in candidates {
                let entry = self.entries.remove(&key).unwrap();
                evicted.push(entry.texture.unwrap());
                self.bytes -= entry.bytes;
                if self.bytes <= budget { break }
            }
            release_textures(cx, evicted);
        }
        if self.bytes > budget && !self.over_budget {
            log!("Visible board pictures use {} decoded bytes (budget {})", self.bytes, budget);
        }
        self.over_budget = self.bytes > budget;
    }

    pub fn clear(&mut self, cx: &mut Cx) {
        for (_, task) in self.in_flight.drain(..) { task.cancel(); }
        self.queued.clear();
        let textures: Vec<_> = self.entries.drain().filter_map(|(_, entry)| entry.texture).collect();
        release_textures(cx, textures);
        self.bytes = 0;
        self.lru_tick = 0;
        self.over_budget = false;
    }
}

impl Drop for BoardMediaCache {
    fn drop(&mut self) {
        for (_, task) in &self.in_flight { task.cancel(); }
    }
}

/// Native players can need a filename extension. Each open owns its alias, including
/// a preparation that completes after its window closed.
pub struct BoardMovie {
    pub path: PathBuf,
    cleanup: Option<TaskPool>,
}

impl BoardMovie {
    pub fn prepare(cx: &mut Cx, path: PathBuf) -> Result<TaskHandle<Result<Self, String>>, String> {
        use crate::store_worker::CaptureFormat;
        use std::io::Read;
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let pool = cx.task_pool();
        cx.task_pool().submit_named(Class::Visible, "prepare board movie", move || {
            let mut header = [0; 12];
            std::fs::File::open(&path).and_then(|mut f| f.read_exact(&mut header)).map_err(|e| e.to_string())?;
            let format = CaptureFormat::sniff(&header).filter(|f| f.is_video()).ok_or("Unknown movie format.")?;
            let dir = path.parent().ok_or("Missing recording folder.")?.join("playback");
            if std::fs::create_dir_all(&dir).is_ok() {
                loop {
                    let serial = NEXT.fetch_add(1, Ordering::Relaxed);
                    let link = dir.join(format!("board-{}-{serial}.{}", std::process::id(), format.extension()));
                    match std::fs::hard_link(&path, &link) {
                        Ok(()) => return Ok(Self { path: link, cleanup: Some(pool) }),
                        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                        Err(_) => break,
                    }
                }
            }
            Ok(Self { path, cleanup: None })
        }).map_err(|e| e.to_string())
    }
}

impl Drop for BoardMovie {
    fn drop(&mut self) {
        if let Some(pool) = self.cleanup.take() {
            let path = std::mem::take(&mut self.path);
            match pool.submit_named(Class::Background, "remove board movie link", move || {
                if let Err(e) = std::fs::remove_file(path) { log!("town: removing board movie link: {e}"); }
            }) {
                Ok(task) => task.detach(),
                Err(e) => log!("town: scheduling board movie cleanup: {e}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_ai_services::pictures::{encode_png, RgbaPicture};
    use std::time::{Duration, Instant};

    #[test]
    fn board_media_cache_decodes_off_ui_evicts_lru_and_releases_textures() {
        use makepad_widgets::{image_cache_use_mipmaps, TextureUpdated, TextureWrap};
        let dir = std::env::temp_dir().join(format!("town-board-media-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut cache = BoardMediaCache::default();
        let town = [7; 32];
        let expected_entry_bytes = if image_cache_use_mipmaps() { (16 + 4 + 1) * 4 } else { 16 * 4 };
        for n in 0..6 {
            let path = dir.join(n.to_string());
            let picture = RgbaPicture { width: 4, height: 4, rgba: [n, 0, 0, 255].repeat(16) };
            std::fs::write(&path, encode_png(&picture).unwrap()).unwrap();
            assert!(cache.request(&mut cx, town, [n; 32], Some(path)).is_none());
            cache.request(&mut cx, town, [n; 32], None);
            assert_eq!(cache.state(town, [n; 32]), if n < 4 { MediaSlotState::Decoding } else { MediaSlotState::Queued });
        }
        assert_eq!((cache.in_flight.len(), cache.queued.len(), cache.bytes), (4, 2, 0));
        let deadline = Instant::now() + Duration::from_secs(5);
        while !cache.in_flight.is_empty() {
            cache.poll(&mut cx);
            assert!(Instant::now() < deadline, "board picture workers did not finish");
            std::thread::yield_now();
        }
        std::fs::remove_dir_all(dir).unwrap();
        assert!(!cache.poll(&mut cx), "an idle signal must leave the canvas at rest");
        assert_eq!(cache.bytes, 6 * expected_entry_bytes, "cache counts installed texture bytes, mips included");
        for n in 0..6 { assert_eq!(cache.state(town, [n; 32]), MediaSlotState::Ready { w: 4, h: 4 }); }

        let mip_4x4 = Texture::new_with_format(&mut cx, TextureFormat::VecMipBGRAu8_32 {
            width: 4, height: 4, data: Some(vec![0u32; 21].into()), max_level: Some(2),
            wrap: TextureWrap::ClampToEdge, updated: TextureUpdated::Full,
        });
        assert_eq!(texture_bytes(&mut cx, &mip_4x4), 84, "4x4 mip chain counts 16 + 4 + 1 texels");
        release_texture(&mut cx, mip_4x4);
        let mip_3x3 = Texture::new_with_format(&mut cx, TextureFormat::VecMipBGRAu8_32 {
            width: 3, height: 3, data: Some(vec![0u32; 9].into()), max_level: Some(1),
            wrap: TextureWrap::ClampToEdge, updated: TextureUpdated::Full,
        });
        assert_eq!(texture_bytes(&mut cx, &mip_3x3), 40, "non-power-of-two mip chain counts 9 + 1 texels even if CPU holds level 0");
        release_texture(&mut cx, mip_3x3);

        let touched = cache.request(&mut cx, town, [0; 32], None).expect("touch the oldest picture").clone();
        assert_eq!(texture_bytes(&mut cx, &touched), expected_entry_bytes);
        drop(touched);
        let keep = HashSet::from([(town, [1; 32])]);
        cache.evict(&mut cx, &keep, 2 * expected_entry_bytes);
        assert_eq!(cache.bytes, 2 * expected_entry_bytes);
        assert_eq!(cache.state(town, [0; 32]), MediaSlotState::Ready { w: 4, h: 4 }, "most recently drawn");
        assert_eq!(cache.state(town, [2; 32]), MediaSlotState::Missing, "offscreen texture evicted");
        cache.evict(&mut cx, &keep, 0);
        assert_eq!(cache.bytes, expected_entry_bytes, "the visible picture survives even over budget");
        assert!(cache.request(&mut cx, town, [1; 32], None).is_some());
        cache.clear(&mut cx);
        assert_eq!((cache.bytes, cache.entries.len()), (0, 0), "switching boards releases retained textures");
    }
}
