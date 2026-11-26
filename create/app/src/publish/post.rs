use super::invalid;
use makepad_strict_json::{obj, s, Value};
use std::{
    collections::HashSet,
    io,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Image,
    Video,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Video => "video",
        }
    }

    pub(super) fn for_extension(ext: &str) -> io::Result<Self> {
        match ext {
            "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "qoi" | "ico" | "tif" | "tiff" | "heic" | "heif" | "avif" => Ok(Self::Image),
            "mp4" | "mov" | "m4v" | "webm" | "mkv" | "avi" => Ok(Self::Video),
            _ => Err(invalid("unsupported media format")),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Media {
    pub file: String,
    pub kind: Kind,
    pub bytes: u64,
    pub sha256: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_s: Option<f64>,
}
impl Media {
    pub(super) fn json(&self) -> Value {
        let mut fields = vec![
            ("file", s(&self.file)),
            ("kind", s(self.kind.as_str())),
            ("bytes", Value::Int(self.bytes as i64)),
            ("sha256", s(&self.sha256)),
        ];
        if let Some(w) = self.width {
            fields.push(("width", Value::Int(w.into())));
        }
        if let Some(h) = self.height {
            fields.push(("height", Value::Int(h.into())));
        }
        if let Some(d) = self.duration_s {
            fields.push(("duration_s", Value::F64(d)));
        }
        obj(fields)
    }
}

pub(super) struct Post {
    pub title: String,
    pub text: String,
    pub date: String,
    pub media: Vec<Media>,
}

impl Post {
    pub fn manifest(&self) -> Vec<u8> {
        obj(vec![
            ("format", Value::Int(1)),
            ("title", s(&self.title)),
            ("text", s(&self.text)),
            ("date", s(&self.date)),
            ("media", Value::Arr(self.media.iter().map(Media::json).collect())),
        ])
        .to_json()
        .into_bytes()
    }
    pub fn base(&self) -> String {
        format!("{}-{}", &self.date[..10], slug(&self.title))
    }
}

fn slug(title: &str) -> String {
    let words: Vec<_> = title.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()).collect();
    if words.is_empty() {
        "post".into()
    } else {
        words.join("-").to_ascii_lowercase()
    }
}

pub(super) fn available_folder(base: &str, taken: &HashSet<String>) -> String {
    if !taken.contains(base) {
        return base.into();
    }
    (2u64..).map(|n| format!("{base}-{n}")).find(|name| !taken.contains(name)).unwrap()
}

pub(super) fn part_folder(folder: &str, nonce: &str) -> String {
    format!(".{folder}.part-{nonce}")
}

pub(super) fn utc(time: SystemTime) -> io::Result<String> {
    let secs = time.duration_since(UNIX_EPOCH).map_err(|_| invalid("clock before epoch"))?.as_secs();
    let mut days = secs / 86400;
    let mut year = 1970;
    let leap = |y| y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    loop {
        let length = if leap(year) { 366 } else { 365 };
        if days < length {
            break;
        }
        days -= length;
        year += 1;
        if year > 9999 {
            return Err(invalid("clock outside four-digit years"));
        }
    }
    let months = [31, if leap(year) { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut month = 1;
    for length in months {
        if days < length {
            break;
        }
        days -= length;
        month += 1;
    }
    Ok(format!("{year:04}-{month:02}-{:02}T{:02}:{:02}:{:02}Z", days + 1, secs / 3600 % 24, secs / 60 % 60, secs % 60))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn manifest_bytes_and_folder_names() {
        let mut media = Media {
            file: "clip.mp4".into(),
            kind: Kind::Video,
            bytes: 7,
            sha256: "ab".repeat(32),
            width: Some(16),
            height: Some(9),
            duration_s: Some(1.25),
        };
        let mut post = Post {
            title: "Demo! \"A\"".into(),
            text: "one\ntwo".into(),
            date: utc(UNIX_EPOCH + std::time::Duration::from_secs(951827696)).unwrap(),
            media: vec![media.clone()],
        };
        let expected = format!(
            r#"{{"format":1,"title":"Demo! \"A\"","text":"one\ntwo","date":"2000-02-29T12:34:56Z","media":[{{"file":"clip.mp4","kind":"video","bytes":7,"sha256":"{}","width":16,"height":9,"duration_s":1.25}}]}}"#,
            "ab".repeat(32)
        );
        assert_eq!(post.manifest(), expected.as_bytes());
        media.kind = Kind::Image;
        (media.width, media.height, media.duration_s) = (None, None, None);
        post.media = vec![media];
        let manifest = String::from_utf8(post.manifest()).unwrap();
        assert!(!["width", "height", "duration_s"].iter().any(|key| manifest.contains(key)));
        let base = post.base();
        assert_eq!(base, "2000-02-29-demo-a");
        assert_eq!(slug(" -- __☃ "), "post");
        assert_eq!(available_folder(&base, &HashSet::new()), base);
        let used = HashSet::from([base.clone(), format!("{base}-2")]);
        assert_eq!(available_folder(&base, &used), format!("{base}-3"));
        assert_eq!(part_folder(&base, "0123"), ".2000-02-29-demo-a.part-0123");
    }
}
