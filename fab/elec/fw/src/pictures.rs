//! The `files` fake (`fake: {files: "<folder>"}`): a folder of the project's pictures, served to a
//! device that asks for them, fitted to the size it asks. It plays on the host end with or without
//! the network, so it is an offline feature as much as a stand-in.
//!
//! The protocol, v1, as a device speaks it on its connection:
//! - At ready the service sends `count <n>`: the pictures in the folder, in name order.
//! - To `get <index> <w> <h> <row>` it answers picture `index` (modulo the count), fitted inside
//!   `w` x `h` (its aspect kept, black margins), from row `row` on. The answer is one binary
//!   message: a 16-byte little-endian header (`'P'`, version 1, format 1 = RGB565 with each pixel
//!   high byte first, 0, then as u16: index, w, h, first row, rows, 0), then the rows.
//! - `none <index>` means no such picture: the folder has none, or it cannot be fitted.
//! - Anything else is skipped.
//!
//! [`Checker`] checks either end's messages against the same table.

use fab_elec_mcu::convert::{decode_picture, image, is_picture, Dither, Fit, ImageOptions, Rotate};
use fab_elec_mcu_rt::net::{Payload, Service};
use makepad_widgets_core::log;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

pub struct Pictures {
    /// The pictures' names and files, in name order.
    files: Vec<(String, Arc<[u8]>)>,
    /// Each picture fitted so far, by (index, w, h): its rows (None: it cannot be fitted). Fitting
    /// gives the same bytes every time, so a run's marks and forks may share them.
    fitted: Mutex<HashMap<(usize, u16, u16), Option<Payload>>>,
}

impl Pictures {
    /// The pictures among `files` (name, bytes); the other files are left out.
    pub fn new(files: &[(String, Arc<[u8]>)]) -> Pictures {
        let mut files: Vec<_> = files.iter().filter(|(name, bytes)| is_picture(name, bytes)).cloned().collect();
        files.sort_by(|a, b| a.0.cmp(&b.0));
        Pictures { files, fitted: Mutex::new(HashMap::new()) }
    }

    /// Picture `index` fitted inside `w` x `h` as RGB565 rows, each pixel high byte first.
    fn fit(&self, index: usize, w: u16, h: u16) -> Option<Payload> {
        let key = (index, w, h);
        if let Some(fitted) = self.fitted.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
            return fitted.clone();
        }
        // fitted outside the lock: a run and its forks only wait for the cache, never for a fit
        let (name, file) = &self.files[index];
        let options = ImageOptions { size: Some((u32::from(w), u32::from(h))), fit: Fit::Contain, rotate: Rotate::R0, dither: Dither::Diffusion, background: [0; 3] };
        let fitted = decode_picture(file).and_then(|(width, height, rgba)| image(&rgba, width, height, &options))
            .map(|done| Payload::from(done.pixels.iter().flat_map(|p| p.to_be_bytes()).collect::<Vec<u8>>()))
            .map_err(|why| log!("electronics firmware: the picture {name} cannot be fitted to {w}x{h}: {why}"))
            .ok();
        self.fitted.lock().unwrap_or_else(|e| e.into_inner()).insert(key, fitted.clone());
        fitted
    }
}

fn text(line: String) -> (bool, Payload) { (false, line.into_bytes().into()) }

impl Service for Pictures {
    fn ready(&self) -> Vec<(bool, Payload)> {
        vec![text(format!("count {}", self.files.len()))]
    }

    fn answer(&self, binary: bool, bytes: &[u8]) -> Vec<(bool, Payload)> {
        let Some(line) = std::str::from_utf8(bytes).ok().filter(|_| !binary) else { return Vec::new() };
        let mut words = line.split(' ');
        let (Some("get"), Some(index)) = (words.next(), words.next().and_then(|w| w.parse::<u64>().ok())) else { return Vec::new() };
        let size: Vec<u16> = words.map_while(|w| w.parse().ok()).collect();
        let &[w, h, row] = &size[..] else { return Vec::new() };
        let none = || vec![text(format!("none {index}"))];
        if self.files.is_empty() || w == 0 || h == 0 { return none(); }
        let k = (index % self.files.len() as u64) as usize;
        let Some(rows) = self.fit(k, w, h) else { return none() };
        let (first, count) = (row.min(h), h - row.min(h));
        let mut message = Vec::with_capacity(16 + usize::from(count) * usize::from(w) * 2);
        message.extend_from_slice(&[b'P', 1, 1, 0]);
        for field in [k as u16, w, h, first, count, 0] { message.extend_from_slice(&field.to_le_bytes()); }
        message.extend_from_slice(&rows[usize::from(first) * usize::from(w) * 2..]);
        vec![(true, message.into())]
    }
}

/// The pictures table's checker, for the tests of either end: one per connection, fed every
/// message both ways in arrival order. Neither end keeps state across connections, so
/// [`Checker::ready`] starts each one afresh.
#[derive(Default)]
pub struct Checker {
    /// The last `count` down on this connection.
    count: Option<u64>,
}

impl Checker {
    /// The connection went ready.
    pub fn ready(&mut self) { self.count = None; }

    /// One message (`up`: from the device). Err names the rule of the table it breaks.
    pub fn check(&mut self, up: bool, binary: bool, bytes: &[u8]) -> Result<(), String> {
        if binary {
            if up { return Err("the device sends no binary message on pictures".into()); }
            if bytes.len() < 16 || bytes[..4] != [b'P', 1, 1, 0] {
                return Err(format!("a picture starts with 'P', version 1, format 1, 0 in a 16-byte header: {:?}", &bytes[..bytes.len().min(16)]));
            }
            let [index, w, h, first, rows, reserved] = [4, 6, 8, 10, 12, 14].map(|k| u16::from_le_bytes([bytes[k], bytes[k + 1]]));
            let carried = 16 + usize::from(rows) * usize::from(w) * 2;
            if reserved != 0 { return Err(format!("picture {index}: the header's last field is 0, not {reserved}")); }
            if bytes.len() != carried {
                return Err(format!("picture {index}: {rows} rows of {w} carry {carried} bytes with the header, not {}", bytes.len()));
            }
            if u32::from(first) + u32::from(rows) > u32::from(h) {
                return Err(format!("picture {index}: rows {first} to {} pass its height {h}", u32::from(first) + u32::from(rows)));
            }
            return match self.count {
                Some(n) if u64::from(index) < n => Ok(()),
                n => Err(format!("picture {index} is not below the last count ({n:?})")),
            };
        }
        let text = std::str::from_utf8(bytes).map_err(|_| format!("a text message is UTF-8: {bytes:?}"))?;
        let (verb, args) = text.split_once(' ').map_or((text, Vec::new()), |(verb, rest)| (verb, rest.split(' ').collect()));
        let want = match (up, verb) {
            (true, "get") => 4,
            (false, "count" | "none") => 1,
            _ => return Err(format!("{text:?}: {verb:?} is not in the table {}", if up { "up" } else { "down" })),
        };
        if args.len() != want || !args.iter().all(|a| !a.is_empty() && a.bytes().all(|b| b.is_ascii_digit())) {
            return Err(format!("{text:?}: {verb} takes {want} decimal argument{}", if want == 1 { "" } else { "s" }));
        }
        if verb == "count" {
            self.count = Some(args[0].parse().map_err(|_| format!("{text:?}: the count is too large"))?);
        }
        Ok(())
    }
}
