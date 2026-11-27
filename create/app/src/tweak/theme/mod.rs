//! The shared app's resolved theme and the colour names offered in its property editors.
use super::ThemeVal;
use makepad_strict_json::Value;
use makepad_widgets::{makepad_script::colorhex::hex_bytes_to_u32, BaseTheme};
use std::{collections::HashMap, rc::Rc};

#[derive(Clone, Debug, PartialEq)]
pub(in crate::tweak) struct SheetInfo {
    pub id: String,
    pub label: String,
    pub base: String,
    pub dark: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(in crate::tweak) struct ThemeToken {
    pub name: String,
    pub value: String,
    pub color: Option<u32>,
    pub site: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(in crate::tweak) struct ThemeSnapshot {
    pub base: String,
    pub sheet: Option<String>,
    pub has_mix: bool,
    pub has_pins: bool,
    pub edits: Vec<(String, String)>,
    pub sheets: Vec<SheetInfo>,
    pub tokens: Vec<ThemeToken>,
    token_index: HashMap<String, usize>,
    edit_index: HashMap<String, usize>,
}

#[derive(Clone, Default)]
pub(in crate::tweak) struct ThemePalette {
    by_rgba: HashMap<u32, Rc<str>>,
    entries: Vec<(String, [f32; 4])>,
}

fn hex(text: &str) -> Option<u32> {
    let digits = text.strip_prefix('#')?;
    if !matches!(digits.len(), 3 | 6 | 8) { return None }
    hex_bytes_to_u32(digits.as_bytes()).ok()
}

pub(super) fn scalar(text: &str) -> Option<ThemeVal> {
    hex(text).map(ThemeVal::Color).or_else(|| text.parse::<f64>().ok().filter(|n| n.is_finite()).map(ThemeVal::Num))
}

impl ThemeSnapshot {
    pub fn parse(json: &str) -> Option<Self> { Self::from_value(&super::parse_json(json).ok()?) }

    pub(super) fn from_value(v: &Value) -> Option<Self> {
        let base = BaseTheme::parse(v.get("base")?.as_str()?)?;
        let sheet = match v.get("sheet")? {
            Value::Null => None,
            id => Some(id.as_str()?.to_owned()),
        };
        let mut snap = Self { base: base.id().into(), sheet,
            has_mix: v.get("has_mix")?.as_bool()?, has_pins: v.get("has_pins")?.as_bool()?, ..Self::default() };
        for edit in v.get("edits")?.as_arr()? {
            let [key, literal] = edit.as_arr()? else { return None };
            let name = key.as_str()?;
            snap.edit_index.insert(name.to_owned(), snap.edits.len());
            snap.edits.push((name.to_owned(), literal.as_str()?.into()));
        }
        for sheet in v.get("sheets")?.as_arr()? {
            let base = BaseTheme::parse(sheet.get("base")?.as_str()?)?;
            let dark = match sheet.get("appearance")?.as_str()? {
                "dark" => true, "light" => false, _ => return None,
            };
            snap.sheets.push(SheetInfo { id: sheet.get("id")?.as_str()?.into(), label: sheet.get("label")?.as_str().filter(|s| !s.is_empty())?.into(),
                base: base.id().into(), dark });
        }
        for token in v.get("values")?.as_arr()? {
            let name = token.get("name")?.as_str()?;
            if !makepad_town_core::literal::is_identifier(name) || snap.token_index.insert(name.into(), snap.tokens.len()).is_some() {
                return None
            }
            let value = token.get("value")?.as_str()?;
            let color = match scalar(value)? { ThemeVal::Color(c) => Some(c), ThemeVal::Num(_) => None };
            snap.tokens.push(ThemeToken { name: name.into(), value: value.into(), color, site: token.get("site")?.as_str()?.into() });
        }
        Some(snap)
    }

    pub fn label(&self) -> String {
        self.sheet.as_ref().and_then(|id| self.sheets.iter().find(|s| &s.id == id)).map(|s| s.label.clone())
            .unwrap_or_else(|| BaseTheme::parse(&self.base).map_or("", BaseTheme::label).into())
    }

    pub fn patch_token(&mut self, name: &str, new_value: &str) -> bool {
        let Some(&index) = self.token_index.get(name) else { return false };
        let token = &mut self.tokens[index];
        if token.value == new_value { return false }
        let literal = match scalar(new_value) {
            Some(ThemeVal::Color(c)) if token.color.is_some() => {
                token.color = Some(c);
                format!("#x{c:08X}")
            }
            Some(ThemeVal::Num(n)) if token.color.is_none() => format!("{n:?}"),
            _ => return false,
        };
        token.value = new_value.into();
        let index = self.edit_index.entry(name.into()).or_insert(self.edits.len());
        if *index == self.edits.len() { self.edits.push((name.into(), literal)); }
        else { self.edits[*index].1 = literal; }
        true
    }

    pub fn palette(&self) -> ThemePalette {
        let mut palette = ThemePalette::default();
        for token in &self.tokens {
            if let Some(c) = token.color.filter(|_| token.name.starts_with("color")) {
                palette.by_rgba.entry(c).or_insert_with(|| Rc::from(token.name.as_str()));
                palette.entries.push((token.name.clone(), c.to_be_bytes().map(|n| f32::from(n) / 255.0)));
            }
        }
        palette
    }
}

impl ThemePalette {
    pub fn match_hex(&self, text: &str) -> Option<Rc<str>> {
        if self.is_empty() { return None }
        self.by_rgba.get(&hex(text.trim())?).cloned()
    }
    pub fn entries(&self) -> Vec<(String, [f32; 4])> { self.entries.clone() }
    pub fn is_empty(&self) -> bool { self.entries.is_empty() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_indexes_resolved_colours_and_keeps_the_first_matching_token() {
        let json = r##"{"base":"light","sheet":"paper","has_mix":false,"has_pins":true,"edits":[["size","2.0"]],
            "sheets":[{"id":"paper","label":"Paper","base":"light","appearance":"light"}],"values":[
            {"name":"color_one","value":"#abc","site":"sheet:1"},{"name":"color_two","value":"#aabbccff","site":""},
            {"name":"shadow","value":"#123456ff","site":""},{"name":"size","value":"2","site":"base:3"}]}"##;
        let mut snap = ThemeSnapshot::parse(json).expect("snapshot accepts the app's catalogue and resolved values");
        assert!(snap.label() == "Paper" && !snap.sheets[0].dark && snap.has_pins && !snap.has_mix, "sheet metadata is preserved");
        let names = snap.palette();
        assert_eq!(names.match_hex("#ABC").as_deref(), Some("color_one"), "the first matching token wins across hex spellings");
        assert!(names.entries().len() == 2 && names.match_hex("#123456").is_none(), "the strip lists only named theme colours");
        assert!(!snap.patch_token("size", "#abc") && !snap.patch_token("size", "NaN"), "invalid deltas cannot corrupt numeric tokens");
        assert!(snap.patch_token("color_one", "#00000080") && snap.patch_token("size", "3"), "edits update indexed tokens");
        assert_eq!(snap.palette().match_hex("#aabbcc").as_deref(), Some("color_two"), "matching follows a changed token");
        assert_eq!(snap.edits, [("size".into(), "3.0".into()), ("color_one".into(), "#x00000080".into())], "the snapshot retains typed edits");
        assert!(ThemeSnapshot::parse(&json.replace("aabbccff", "gg")).is_none(), "malformed snapshots fail as a whole");
    }
}
