//! The output window in another language. The slides are written in every
//! language they are shown in: each text a slide shows is
//! `tr({en: "What is open" zh: "什么是开源"})` (the talks' shared base), and a
//! graphic picks by Motion's `lang()`. The presenter's picture, the next
//! preview and the notes render the deck as written (`en`); the output
//! window renders the language chosen here ([`super::output_lane`]). A text
//! a slide does not give in that language shows as written. Nothing is
//! prepared or translated at show time: the source is the truth.

use makepad_widgets::*;
use std::sync::Arc;

/// A language an output can be drawn in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Zh,
}

impl Lang {
    pub const ALL: [Lang; 1] = [Lang::Zh];

    pub fn code(self) -> &'static str {
        match self {
            Lang::Zh => "zh",
        }
    }

    /// As the language's own speakers write its name.
    pub fn label(self) -> &'static str {
        match self {
            Lang::Zh => "中文",
        }
    }

    pub fn from_code(code: &str) -> Option<Lang> {
        Lang::ALL.into_iter().find(|lang| lang.code().eq_ignore_ascii_case(code))
    }
}

/// A language chosen at startup for the output (`STAGE_DECK_OUTPUT_LANG=zh`).
pub fn env_output() -> Option<Lang> {
    std::env::var("STAGE_DECK_OUTPUT_LANG").ok().and_then(|code| Lang::from_code(&code))
}

/// The output's language and the deck it was chosen for.
#[derive(Default)]
pub struct I18n {
    pub output: Option<Lang>,
    /// Which open deck the language belongs to (DeckPage's `opened_serial`):
    /// another deck takes the language it was last presented with.
    pub(super) deck: u64,
    /// The language the output lane was last told (None: not yet).
    pushed: Option<Option<Lang>>,
}


impl super::DeckPage {
    /// The output language chosen at startup or remembered for the open
    /// deck, into the output lane.
    pub(super) fn pump_i18n(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        if self.i18n.deck != self.opened_serial && self.session.is_some() {
            self.i18n.deck = self.opened_serial;
            let lang = env_output().or_else(|| self.deck_key().and_then(|key| remembered(&key)));
            if lang != self.i18n.output {
                self.set_output_lang(cx, ui, lang);
            }
        }
        if self.i18n.pushed != Some(self.i18n.output) {
            self.i18n.pushed = Some(self.i18n.output);
            super::output_lane::shared().borrow_mut().set_lang(self.i18n.output.map(|lang| Arc::<str>::from(lang.code())));
            ui.widget(cx, ids!(deck_view)).redraw(cx);
        }
    }

    fn set_output_lang(&mut self, cx: &mut Cx, ui: &WidgetRef, lang: Option<Lang>) {
        self.i18n.output = lang;
        ui.drop_down(cx, ids!(deck_output_lang)).set_selected_item(cx, lang.map_or(0, |lang| 1 + Lang::ALL.iter().position(|l| *l == lang).unwrap_or(0)));
        log!("deck i18n: output language {}", lang.map_or("as written", Lang::label));
        cx.redraw_all();
    }

    /// The open deck, for remembering its output language: its library
    /// alias, else its tree's name.
    fn deck_key(&self) -> Option<String> {
        let session = self.session.as_ref()?;
        self.tree_alias.as_ref().map(|alias| format!("{alias:?}")).or_else(|| session.tree.as_ref().map(|tree| tree.name.clone()))
    }

    /// The studio API's `set_deck_output_language`: the output window's
    /// language by code (`zh`; `en` or `as_written`: the deck as written).
    pub(crate) fn set_output_language(&mut self, cx: &mut Cx, ui: &WidgetRef, language: &str) -> Result<makepad_strict_json::Value, String> {
        if self.doc.is_none() { return Err("open a deck first".into()); }
        let wanted = language.trim().to_lowercase();
        let lang = match wanted.as_str() {
            "en" | "english" | "as_written" | "as written" | "none" | "" => None,
            code => Some(Lang::ALL.iter().copied().find(|lang| lang.code() == code || lang.label() == language.trim() || (code == "chinese" && *lang == Lang::Zh))
                .ok_or_else(|| format!("`{language}` is not an output language here: {}", Lang::ALL.iter().map(|lang| lang.code()).chain(["en"]).collect::<Vec<_>>().join(", ")))?),
        };
        self.set_output_lang(cx, ui, lang);
        if let Some(key) = self.deck_key() { remember(&key, lang); }
        Ok(self.output_language())
    }

    /// The output window's language, for the API.
    pub(crate) fn output_language(&self) -> makepad_strict_json::Value {
        use makepad_strict_json as json;
        json::obj(vec![
            ("language", json::s(self.i18n.output.map_or("en", Lang::code))),
            ("label", json::s(self.i18n.output.map_or("as written", Lang::label))),
            ("languages", json::Value::Arr(Lang::ALL.iter().map(|lang| json::s(lang.code())).chain([json::s("en")]).collect())),
        ])
    }

    pub(super) fn i18n_actions(&mut self, cx: &mut Cx, ui: &WidgetRef, actions: &Actions) {
        if let Some(index) = ui.drop_down(cx, ids!(deck_output_lang)).selected(actions) {
            let lang = index.checked_sub(1).and_then(|i| Lang::ALL.get(i).copied());
            self.set_output_lang(cx, ui, lang);
            if let Some(key) = self.deck_key() {
                remember(&key, lang);
            }
        }
    }
}

/// Where the output language last chosen per deck is kept (one
/// `deck<TAB>code` line each).
fn languages_file() -> std::path::PathBuf {
    makepad_app_shell::user_state::dir(None, "deck-output").join("languages.txt")
}

fn remembered(deck: &str) -> Option<Lang> {
    let text = std::fs::read_to_string(languages_file()).ok()?;
    text.lines().filter_map(|line| line.split_once('\t')).find(|(key, _)| *key == deck).and_then(|(_, code)| Lang::from_code(code))
}

fn remember(deck: &str, lang: Option<Lang>) {
    let path = languages_file();
    let deck = deck.replace(['\t', '\n'], " ");
    let mut lines: Vec<String> = std::fs::read_to_string(&path).unwrap_or_default().lines().filter(|line| line.split_once('\t').is_none_or(|(key, _)| key != deck)).map(str::to_string).collect();
    if let Some(lang) = lang {
        lines.push(format!("{deck}\t{}", lang.code()));
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(error) = std::fs::write(&path, lines.join("\n") + "\n") {
        log!("deck i18n: could not remember the output language: {error}");
    }
}
