use super::{
    field, invalid, nonce,
    post::{Kind, Media},
    Scratch,
};
use makepad_town_core::hex;
use makepad_network::digest::Sha256;
use makepad_strict_json::{obj, parse, s, Value};
use std::{
    fs::{self, File},
    io::{self, BufRead, BufReader, Write},
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Author {
    Local,
    Peer,
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub id: String,
    pub author: Author,
    pub media: Media,
}

#[derive(Clone)]
pub struct Store {
    pub(super) home: PathBuf,
}

fn regular(path: &Path) -> io::Result<()> {
    if !fs::symlink_metadata(path)?.is_file() {
        return Err(invalid("expected a regular media store file"));
    }
    Ok(())
}

fn directory(path: &Path) -> io::Result<()> {
    if !fs::symlink_metadata(path)?.is_dir() {
        return Err(invalid("expected a local store directory"));
    }
    Ok(())
}

fn extension(path: &Path) -> io::Result<String> {
    path.extension().and_then(|v| v.to_str()).map(str::to_ascii_lowercase).ok_or_else(|| invalid("missing media extension"))
}

fn is_hex(id: &str, len: usize) -> bool {
    id.len() == len && id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl Store {
    pub fn new(home: &Path) -> Self {
        Self { home: home.into() }
    }
    fn media(&self) -> PathBuf {
        self.home.join("media")
    }
    fn index(&self) -> PathBuf {
        self.media().join("index")
    }

    /// Import on a worker, from a local file selection. An entry has its own provenance
    /// even when another selection already supplied exactly the same bytes.
    pub fn import(&self, source: &Path, author: Author) -> io::Result<Entry> {
        let original = source.file_name().and_then(|name| name.to_str()).ok_or_else(|| invalid("expected a UTF-8 filename"))?;
        let source = File::open(source)?;
        if !source.metadata()?.is_file() {
            return Err(invalid("select a regular file"));
        }
        self.import_reader(original, BufReader::new(source), author)
    }

    pub fn import_bytes(&self, name: &str, bytes: &[u8], author: Author) -> io::Result<Entry> {
        if Path::new(name).file_name().and_then(|n| n.to_str()) != Some(name) {
            return Err(invalid("expected a media filename"));
        }
        self.import_reader(name, io::Cursor::new(bytes), author)
    }

    fn import_reader(&self, original: &str, mut reader: impl BufRead, author: Author) -> io::Result<Entry> {
        let ext = extension(Path::new(original))?;
        let kind = Kind::for_extension(&ext)?;
        fs::create_dir_all(self.media())?;
        directory(&self.media())?;
        fs::create_dir_all(self.index())?;
        directory(&self.index())?;
        let work = Scratch::new(&self.media())?;
        let copy = work.0.join(format!("import.{ext}"));
        let mut output = File::create_new(&copy)?;
        let mut digest = Sha256::new();
        let mut bytes = 0u64;
        let mut header = Vec::new();
        loop {
            let block = reader.fill_buf()?;
            if block.is_empty() {
                break;
            }
            header.extend_from_slice(&block[..block.len().min(12 - header.len())]);
            digest.update(block);
            output.write_all(block)?;
            let len = block.len();
            bytes += len as u64;
            reader.consume(len);
        }
        output.sync_data()?;
        drop(output);
        let mut media =
            Media { file: original.into(), kind, bytes, sha256: hex(&digest.finalise()), width: None, height: None, duration_s: None };
        match kind {
            Kind::Image => {
                let data = fs::read(&copy)?;
                if let Ok((w, h)) = makepad_widgets::image_size_by_data(&data, &copy) {
                    media.width = w.try_into().ok();
                    media.height = h.try_into().ok();
                } else {
                    let brand = header.get(8..12).filter(|_| header.get(4..8) == Some(b"ftyp"));
                    let recognized = match ext.as_str() {
                        "tif" | "tiff" => header.starts_with(b"II\x2a\0") || header.starts_with(b"MM\0\x2a"),
                        "heic" | "heif" => matches!(brand, Some(b"heic" | b"heix" | b"mif1" | b"msf1")),
                        "avif" => matches!(brand, Some(b"avif" | b"avis")),
                        _ => false,
                    };
                    if !recognized {
                        return Err(invalid("unrecognized image"));
                    }
                }
            }
            Kind::Video => {
                let path = copy.to_str().ok_or_else(|| invalid("expected a UTF-8 store path"))?;
                if let Ok(reader) = makepad_widgets::makepad_platform::video_file::VideoFileDecoder::open(path) {
                    let info = reader.info();
                    media.width = (info.width > 0).then_some(info.width);
                    media.height = (info.height > 0).then_some(info.height);
                    media.duration_s = (info.duration_100ns > 0).then_some(info.duration_100ns as f64 / 10_000_000.0);
                } else {
                    let recognized = match ext.as_str() {
                        "mp4" | "mov" | "m4v" => matches!(header.get(4..8), Some(b"ftyp" | b"moov" | b"wide" | b"mdat")),
                        "mkv" | "webm" => header.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]),
                        "avi" => header.starts_with(b"RIFF") && header.get(8..12) == Some(b"AVI "),
                        _ => false,
                    };
                    if !recognized {
                        return Err(invalid("unrecognized video"));
                    }
                }
            }
        }
        let blob = self.media().join(format!("{}.{ext}", media.sha256));
        // A link publishes the complete copy without replacing a concurrent import.
        match fs::hard_link(&copy, &blob) {
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => regular(&blob)?,
            r => r?,
        }
        let id = nonce()?;
        let row = obj(vec![
            (
                "author",
                s(match author {
                    Author::Local => "local",
                    Author::Peer => "peer",
                }),
            ),
            ("media", media.json()),
        ]);
        super::write_private_bg(&self.index().join(format!("{id}.json")), row.to_json().as_bytes())?;
        Ok(Entry { id, author, media })
    }

    fn entry(&self, id: &str) -> io::Result<Entry> {
        if !is_hex(id, 32) {
            return Err(invalid("expected a media store entry id"));
        }
        directory(&self.media())?;
        directory(&self.index())?;
        let path = self.index().join(format!("{id}.json"));
        regular(&path)?;
        let row = parse(&fs::read(path)?).map_err(invalid)?;
        let author = match field(&row, "author")? {
            "local" => Author::Local,
            "peer" => Author::Peer,
            _ => return Err(invalid("unknown media author")),
        };
        let m = row.get("media").ok_or_else(|| invalid("missing media record"))?;
        let file = field(m, "file")?;
        if Path::new(file).file_name().and_then(|v| v.to_str()) != Some(file) {
            return Err(invalid("invalid stored filename"));
        }
        let kind = Kind::for_extension(&extension(Path::new(file))?)?;
        let hash = field(m, "sha256")?;
        if !is_hex(hash, 64) || field(m, "kind")? != kind.as_str() {
            return Err(invalid("invalid stored media identity"));
        }
        let dim = |key| {
            m.get(key).map(|v| v.as_u64().and_then(|n| u32::try_from(n).ok()).ok_or_else(|| invalid("invalid media dimension"))).transpose()
        };
        let media = Media {
            file: file.into(),
            kind,
            sha256: hash.into(),
            bytes: m.get("bytes").and_then(Value::as_u64).ok_or_else(|| invalid("missing media size"))?,
            width: dim("width")?,
            height: dim("height")?,
            duration_s: match m.get("duration_s") {
                None => None,
                Some(Value::F64(v)) if *v >= 0. => Some(*v),
                Some(Value::Int(v)) if *v >= 0 => Some(*v as f64),
                _ => return Err(invalid("invalid media duration")),
            },
        };
        Ok(Entry { id: id.into(), author, media })
    }

    pub fn offered(&self) -> io::Result<Vec<Entry>> {
        let rows = match fs::read_dir(self.index()) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            r => r?,
        };
        let mut local = Vec::new();
        for row in rows {
            let path = row?.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let id = path.file_stem().and_then(|s| s.to_str()).ok_or_else(|| invalid("invalid index filename"))?;
            let entry = self.entry(id)?;
            if entry.author == Author::Local {
                local.push(entry);
            }
        }
        local.sort_by(|a, b| a.media.file.cmp(&b.media.file).then(a.id.cmp(&b.id)));
        Ok(local)
    }

    pub(super) fn resolve(&self, id: &str) -> io::Result<(Media, PathBuf)> {
        let entry = self.entry(id)?;
        if entry.author != Author::Local {
            return Err(invalid("media entry belongs to the peer"));
        }
        let ext = extension(Path::new(&entry.media.file))?;
        let path = self.media().join(format!("{}.{ext}", entry.media.sha256));
        regular(&path)?;
        let path = fs::canonicalize(path)?;
        if path.metadata()?.len() != entry.media.bytes {
            return Err(invalid("stored media size changed"));
        }
        Ok((entry.media, path))
    }
}

#[cfg(test)]
pub(super) fn test_image(home: &Path) -> PathBuf {
    let path = home.join("tiny.gif");
    fs::write(&path, b"GIF89a\x01\0\x01\0\x80\0\0\0\0\0\xff\xff\xff\x2c\0\0\0\0\x01\0\x01\0\0\x02\x02\x44\x01\0\x3b").unwrap();
    path
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selection_requires_an_owned_store_entry() {
        let dir = Scratch::new(&std::env::temp_dir()).unwrap();
        let store = Store::new(&dir.0);
        let path = test_image(&dir.0);
        let peer = store.import(&path, Author::Peer).unwrap();
        assert!(store.offered().unwrap().is_empty());
        let local = store.import(&path, Author::Local).unwrap();
        fs::remove_file(&path).unwrap();
        assert_eq!(peer.media.sha256, local.media.sha256);
        assert_eq!(store.offered().unwrap()[0].id, local.id);
        assert_eq!((local.media.width, local.media.height), (Some(1), Some(1)));
        let (_, blob) = store.resolve(&local.id).unwrap();
        assert!(blob.exists());
        for id in [peer.id.as_str(), path.to_str().unwrap(), blob.to_str().unwrap(), "../local/key"] {
            assert!(store.resolve(id).is_err());
        }
        #[cfg(unix)]
        {
            fs::remove_file(&blob).unwrap();
            std::os::unix::fs::symlink(&dir.0, &blob).unwrap();
            assert!(store.resolve(&local.id).is_err());
        }
    }
}
