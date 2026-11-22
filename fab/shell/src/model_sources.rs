//! 3D model sources: where Fab may get the 3D models of electronics parts (user, 11-19: "we need to
//! make the 3D model source of components configurable"). One switch per external source, kept per
//! machine like the view's preferences:
//!
//! - KiCad's 3D library from a KiCad installed on this computer (on by default; no network);
//! - KiCad's 3D library downloaded one model file at a time from KiCad's repository (on by default);
//! - EasyEDA's part service (off by default). It turns on only after the person accepts
//!   [`EASYEDA_WARNING`]; the acceptance is kept with its time. Turning it off needs no confirmation,
//!   and turning it on again asks again.
//!
//! Off means not used: Fab sends nothing to a source that is off, and its models already in the
//! person's part library are not drawn either (the part keeps its plain body). The library keeps
//! them, so turning the source on again needs no download.
//!
//! `~/.makepad/fab/model_sources.prefs` (or `$FAB_MODEL_SOURCES`), one `key=value` per line; times
//! are Unix seconds. No file: the defaults. A lane's test instance (`MAKEPAD_TEST_LANE`) and a unit
//! test read no file and keep none, so a test never changes the person's choice.
//!
//! Cost: a load reads one small file (at start); a change writes it (a person's click). Nothing per
//! frame.

use std::path::PathBuf;

/// An external source of part models.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ModelSource {
    KicadLocal,
    KicadDownload,
    EasyEda,
}

impl ModelSource {
    pub const ALL: [ModelSource; 3] = [ModelSource::KicadLocal, ModelSource::KicadDownload, ModelSource::EasyEda];

    /// The source's bit in [`ModelSources::bits`].
    pub const fn bit(self) -> u8 {
        match self {
            ModelSource::KicadLocal => 1,
            ModelSource::KicadDownload => 2,
            ModelSource::EasyEda => 4,
        }
    }

    /// Its name in the settings and the AI's answers.
    pub fn label(self) -> &'static str {
        match self {
            ModelSource::KicadLocal => "KiCad library, from a KiCad installed on this computer",
            ModelSource::KicadDownload => "KiCad library, downloaded from KiCad's repository",
            ModelSource::EasyEda => "EasyEDA part service",
        }
    }

    fn key(self) -> &'static str {
        match self {
            ModelSource::KicadLocal => "kicad_local",
            ModelSource::KicadDownload => "kicad_download",
            ModelSource::EasyEda => "easyeda",
        }
    }
}

/// The title of the warning shown before EasyEDA's models are turned on.
pub const EASYEDA_WARNING_TITLE: &str = "Use 3D models from EasyEDA?";

/// The warning shown before EasyEDA's models are turned on (approved wording, 11-19).
pub const EASYEDA_WARNING: &str = "EasyEDA's part service (easyeda.com) is run by another company. \
Its 3D models come under that service's own terms, not Fab's. Fab has not checked where these models \
come from or whether you may use them, and does not vouch for them.\n\
If you turn this on, you take responsibility for using these models in what you make.\n\
While it is on, Fab sends the LCSC part numbers of the parts on your open boards to easyeda.com to get \
their models.";

/// The person's choice of sources on this machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelSources {
    pub kicad_local: bool,
    pub kicad_download: bool,
    pub easyeda: bool,
    /// When the person last accepted EasyEDA's warning (Unix seconds); kept when it is turned off.
    pub easyeda_accepted: Option<i64>,
    /// A KiCad 3D library folder the person chose, looked in before the usual places.
    pub kicad_dir: Option<String>,
    /// When each source last gave a model (Unix seconds), by [`ModelSource::ALL`] order.
    pub last_used: [Option<i64>; 3],
}

impl Default for ModelSources {
    fn default() -> Self {
        ModelSources {
            kicad_local: true,
            kicad_download: true,
            easyeda: false,
            easyeda_accepted: None,
            kicad_dir: None,
            last_used: [None; 3],
        }
    }
}

/// Why a change was not made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// EasyEDA turns on only through [`ModelSources::accept_easyeda`].
    NeedsAcceptance,
}

impl ModelSources {
    pub fn on(&self, source: ModelSource) -> bool {
        match source {
            ModelSource::KicadLocal => self.kicad_local,
            ModelSource::KicadDownload => self.kicad_download,
            ModelSource::EasyEda => self.easyeda,
        }
    }

    /// The enabled sources as one word, for readers off the UI thread.
    pub fn bits(&self) -> u8 {
        ModelSource::ALL.iter().filter(|s| self.on(**s)).fold(0, |bits, s| bits | s.bit())
    }

    /// Turn a source on or off. EasyEDA turns on only by accepting its warning.
    pub fn set(&mut self, source: ModelSource, on: bool) -> Result<(), Refused> {
        match source {
            ModelSource::KicadLocal => self.kicad_local = on,
            ModelSource::KicadDownload => self.kicad_download = on,
            ModelSource::EasyEda if on && !self.easyeda => return Err(Refused::NeedsAcceptance),
            ModelSource::EasyEda => self.easyeda = on,
        }
        Ok(())
    }

    /// The person accepted [`EASYEDA_WARNING`] at `now` (Unix seconds): EasyEDA is on.
    pub fn accept_easyeda(&mut self, now: i64) {
        self.easyeda = true;
        self.easyeda_accepted = Some(now);
    }

    pub fn last_used(&self, source: ModelSource) -> Option<i64> {
        self.last_used[ModelSource::ALL.iter().position(|s| *s == source).unwrap_or(0)]
    }

    pub fn set_last_used(&mut self, source: ModelSource, at: i64) {
        self.last_used[ModelSource::ALL.iter().position(|s| *s == source).unwrap_or(0)] = Some(at);
    }

    fn text(&self) -> String {
        let flag = |on: bool| if on { "1" } else { "0" };
        let mut out = String::new();
        for source in ModelSource::ALL {
            out.push_str(&format!("{}={}\n", source.key(), flag(self.on(source))));
        }
        if let Some(at) = self.easyeda_accepted {
            out.push_str(&format!("easyeda_accepted={at}\n"));
        }
        if let Some(dir) = &self.kicad_dir {
            out.push_str(&format!("kicad_dir={}\n", dir.replace('\n', " ")));
        }
        for source in ModelSource::ALL {
            if let Some(at) = self.last_used(source) {
                out.push_str(&format!("{}_used={at}\n", source.key()));
            }
        }
        out
    }

    /// The choice in `text`; a missing or unreadable key keeps its default. EasyEDA reads as on only
    /// with an acceptance time beside it.
    fn parse(text: &str) -> ModelSources {
        let mut sources = ModelSources::default();
        let value = |key: &str| text.lines().rev().find_map(|l| l.strip_prefix(key)?.strip_prefix('=')).map(str::trim);
        let flag = |key: &str, default: bool| match value(key) {
            Some("1") => true,
            Some("0") => false,
            _ => default,
        };
        sources.kicad_local = flag("kicad_local", true);
        sources.kicad_download = flag("kicad_download", true);
        sources.easyeda_accepted = value("easyeda_accepted").and_then(|v| v.parse().ok());
        sources.easyeda = flag("easyeda", false) && sources.easyeda_accepted.is_some();
        sources.kicad_dir = value("kicad_dir").filter(|d| !d.is_empty()).map(str::to_string);
        for source in ModelSource::ALL {
            if let Some(at) = value(&format!("{}_used", source.key())).and_then(|v| v.parse().ok()) {
                sources.set_last_used(source, at);
            }
        }
        sources
    }
}

fn path() -> Option<PathBuf> {
    if cfg!(test) || cfg!(target_arch = "wasm32") {
        return None;
    }
    if let Some(p) = std::env::var_os("FAB_MODEL_SOURCES") {
        return Some(PathBuf::from(p));
    }
    if std::env::var_os("MAKEPAD_TEST_LANE").is_some() {
        return None;
    }
    Some(makepad_fab_settings::home::user_home().join(".makepad").join("fab").join("model_sources.prefs"))
}

/// This machine's choice, or the defaults.
pub fn load() -> ModelSources {
    path().and_then(|p| std::fs::read_to_string(p).ok()).map_or_else(ModelSources::default, |t| ModelSources::parse(&t))
}

/// Keep the choice for the next start. A failed write is logged once per run; the choice holds
/// for this run.
pub fn save(sources: &ModelSources) {
    let Some(p) = path() else { return };
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(e) = std::fs::write(&p, sources.text()) {
        static LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if !LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
            makepad_widgets_core::log!("fab: could not keep the 3D model sources in {}: {e}", p.display());
        }
    }
}

/// `2026-11-19`, UTC.
pub fn date(secs: i64) -> String {
    let t = makepad_civil_time::utc(secs);
    format!("{:04}-{:02}-{:02}", t.year, t.month, t.day)
}

/// One external resource on the licences page: its name and what to say about it, line by line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resource {
    pub title: String,
    pub lines: Vec<String>,
}

fn state_lines(sources: &ModelSources, source: ModelSource) -> Vec<String> {
    let mut lines = Vec::new();
    lines.push(match (sources.on(source), source, sources.easyeda_accepted) {
        (true, ModelSource::EasyEda, Some(at)) => format!("On: you accepted its terms on {}.", date(at)),
        (false, ModelSource::EasyEda, Some(at)) => format!("Off (you accepted its terms on {}; turning it on asks again).", date(at)),
        (false, ModelSource::EasyEda, None) => "Off. Turning it on asks you to accept its terms first.".into(),
        (true, ..) => "On.".into(),
        (false, ..) => "Off.".into(),
    });
    lines.push(match sources.last_used(source) {
        Some(at) => format!("Last used: {}.", date(at)),
        None => "Not used yet.".into(),
    });
    lines
}

/// Every external resource Fab uses, for the licences page: the part model sources with their
/// state, then the third-party data built into Fab (from `CREDITS.md`). O(credits).
pub fn resources(sources: &ModelSources) -> Vec<Resource> {
    use fab_elec_import::kicad3d;
    let licence = format!("Licence: {} ({}).", kicad3d::LICENCE, kicad3d::LICENCE_URL);
    let newest = kicad3d::newest_tag();
    let mut out = vec![
        Resource {
            title: "KiCad 3D library, from a KiCad installed on this computer".into(),
            lines: [
                vec![
                    "Fab reads the .wrl 3D models of the KiCad library on this computer and keeps a converted copy in your part library.".into(),
                    "From: the KiCad installation's 3D model folder (no network).".into(),
                    licence.clone(),
                ],
                state_lines(sources, ModelSource::KicadLocal),
            ].concat(),
        },
        Resource {
            title: "KiCad 3D library, downloaded".into(),
            lines: [
                vec![
                    "Fab downloads one .wrl model file per model a board uses, by its library path, and keeps it in your part library with where it came from, its library tag and the date. It never downloads the whole library.".into(),
                    format!("From: {} (kicad/libraries/kicad-packages3D, KiCad's own), else {} (KiCad/kicad-packages3D, its earlier home).", kicad3d::GITLAB_HOST, kicad3d::GITHUB_HOST),
                    format!("Library tag: the one of the KiCad version a board comes from ({}; a board from a newer KiCad takes {newest}).",
                        kicad3d::TAGS.iter().map(|(major, tag)| format!("KiCad {major}: {tag}")).collect::<Vec<_>>().join(", ")),
                    licence,
                ],
                state_lines(sources, ModelSource::KicadDownload),
            ].concat(),
        },
        Resource {
            title: "EasyEDA part service".into(),
            lines: [
                vec![
                    "Fab gets a part's footprint, symbol and 3D model (OBJ) by its LCSC part number, and keeps them in your part library.".into(),
                    "From: easyeda.com and modules.easyeda.com, run by another company.".into(),
                    "Licence: the service's own terms; it states no licence for its models. You take responsibility for using them.".into(),
                ],
                state_lines(sources, ModelSource::EasyEda),
            ].concat(),
        },
    ];
    out.push(radio_pack(&fab_elec_mcu_pack::espressif_s3()));
    out.push(Resource { title: "Built into Fab".into(), lines: built_in(include_str!("../../CREDITS.md")) });
    out
}

/// The radio libraries the Firmware pane's Get downloads, from the pack's own manifest (its
/// repositories, pinned commits, files and licence), so the page and the download agree.
fn radio_pack(pack: &fab_elec_mcu_pack::Manifest) -> Resource {
    let host = |url: &str| url.strip_prefix("https://").and_then(|rest| rest.split('/').next()).unwrap_or(url).to_string();
    let mut hosts: Vec<String> = pack.repositories.iter().map(|r| host(&r.url_base)).collect();
    hosts.dedup();
    let libraries = pack.files.iter().filter(|f| f.path.ends_with(".a")).count();
    let from = pack.repositories.iter().map(|r| format!("{} at {}", r.name, &r.commit[..r.commit.len().min(7)])).collect::<Vec<_>>().join(" and ");
    Resource {
        title: format!("Espressif {} Wi-Fi libraries", pack.chip),
        lines: vec![
            format!("Fab downloads {libraries} binary libraries in object form ({:.1} MB with their licence files), for the radio of {} designs, only when you press Get on the Firmware pane's Radio row.",
                pack.total_bytes() as f64 / 1e6, pack.chip),
            format!("From: {from}, on {}; each file is checked against its pinned SHA-256.", hosts.join(", ")),
            format!("Licence: {} (the licence file comes with them).", pack.license),
        ],
    }
}

/// One line per top-level entry of `CREDITS.md`'s first section (the data built into Fab): its
/// name and the licence it names, or that it gives figures only.
fn built_in(credits: &str) -> Vec<String> {
    let section = credits.split("\n## ").next().unwrap_or("");
    let mut entries: Vec<String> = Vec::new();
    for line in section.lines() {
        if let Some(entry) = line.strip_prefix("- ") {
            entries.push(entry.to_string());
        } else if let Some(last) = entries.last_mut() {
            last.push(' ');
            last.push_str(line.trim());
        }
    }
    const LICENCES: &[&str] = &["Apache License 2.0", "MIT License", "BSD 3-Clause License", "CERN-OHL-W-2.0"];
    let mut lines: Vec<String> = entries.iter().map(|entry| {
        let name = entry.split(" (`").next().unwrap_or(entry).split(':').next().unwrap_or(entry).trim();
        let named: Vec<&str> = LICENCES.iter().copied().filter(|l| entry.contains(l)).collect();
        let licence = if !named.is_empty() { named.join(", ") } else if entry.contains("figures only") { "figures only".to_string() } else { "see CREDITS.md".to_string() };
        format!("{name}: {licence}.")
    }).collect();
    lines.push("Built in; nothing is fetched for these. In full: CREDITS.md.".into());
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_acceptance_and_the_kept_file() {
        let fresh = ModelSources::default();
        assert!(fresh.kicad_local && fresh.kicad_download && !fresh.easyeda, "both KiCad sources on, EasyEDA off by default");
        assert_eq!(fresh.bits(), ModelSource::KicadLocal.bit() | ModelSource::KicadDownload.bit(), "the default word");

        let mut sources = fresh.clone();
        assert_eq!(sources.set(ModelSource::EasyEda, true), Err(Refused::NeedsAcceptance), "EasyEDA does not turn on without its warning");
        assert!(!sources.easyeda, "a refused switch changes nothing");
        sources.accept_easyeda(1_795_000_000);
        assert!(sources.easyeda && sources.easyeda_accepted == Some(1_795_000_000), "accepting turns it on and keeps the time");
        assert_eq!(sources.set(ModelSource::EasyEda, false), Ok(()), "off needs no confirmation");
        assert!(!sources.easyeda && sources.easyeda_accepted.is_some(), "off keeps when it was accepted");
        assert_eq!(sources.set(ModelSource::EasyEda, true), Err(Refused::NeedsAcceptance), "on again asks again");
        sources.set(ModelSource::KicadDownload, false).expect("KiCad's switches need no acceptance");
        sources.kicad_dir = Some("/opt/kicad/3dmodels".into());
        sources.set_last_used(ModelSource::KicadLocal, 1_795_000_100);

        let read = ModelSources::parse(&sources.text());
        assert_eq!(read, sources, "the kept file reads back as written");
        assert_eq!(ModelSources::parse(""), fresh, "no file is the defaults");
        assert!(!ModelSources::parse("easyeda=1\n").easyeda, "a file saying on without an acceptance reads as off");
        assert!(ModelSources::parse("easyeda=1\neasyeda_accepted=5\n").easyeda, "on with an acceptance reads as on");
        assert_eq!(date(1_795_000_000), "2026-11-18", "the acceptance shows as a UTC date");
        assert!(load() == fresh, "a unit test reads no machine file");
    }

    #[test]
    fn the_licences_page_names_every_source_and_every_credit() {
        let mut sources = ModelSources::default();
        sources.set_last_used(ModelSource::KicadDownload, 1_795_000_000);
        let page = resources(&sources);
        assert_eq!(page.len(), 5, "three model sources, the radio libraries, then the built-in data");
        let text = |i: usize| page[i].lines.join("\n");
        for i in 0..2 {
            assert!(text(i).contains("CC-BY-SA 4.0") && text(i).contains("https://www.kicad.org/libraries/license/"),
                "KiCad's licence and its link: {}", text(i));
        }
        assert!(text(1).contains("gitlab.com") && text(1).contains("raw.githubusercontent.com") && text(1).contains("9.0.0"),
            "the download names both hosts and its tags: {}", text(1));
        assert!(text(1).contains("Last used: 2026-11-18") && text(0).contains("Not used yet"), "when each was last used");
        assert!(text(2).contains("easyeda.com") && text(2).contains("Off. Turning it on asks"), "EasyEDA starts off: {}", text(2));
        sources.accept_easyeda(1_795_000_000);
        assert!(resources(&sources)[2].lines.join(" ").contains("accepted its terms on 2026-11-18"), "the acceptance date shows");
        let pack = fab_elec_mcu_pack::espressif_s3();
        assert!(page[3].title.contains(&pack.chip) && text(3).contains("press Get on the Firmware pane's Radio row"), "{}", text(3));
        for repo in &pack.repositories {
            assert!(text(3).contains(&format!("{} at {}", repo.name, &repo.commit[..7])), "the pinned commits come from the pack: {}", text(3));
        }
        assert!(text(3).contains(&format!("Licence: {}", pack.license)) && text(3).contains("raw.githubusercontent.com"), "{}", text(3));
        let credits = include_str!("../../CREDITS.md");
        let entries = credits.split("\n## ").next().unwrap().lines().filter(|l| l.starts_with("- ")).count();
        assert_eq!(page[4].lines.len(), entries + 1, "one line per built-in credit, then where they are in full");
        assert!(page[4].lines.iter().any(|l| l.starts_with("Organic routing: MIT License")), "a credit names its licence: {:?}", page[4].lines);
        assert!(page[4].lines.iter().any(|l| l.starts_with("Standard colour codes: figures only")), "figures-only data says so");
    }
}
