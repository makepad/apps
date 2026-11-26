//! A document's font values: the face a `font:` names (`Font{family weight
//! italic axes}`), checked against the document font table
//! (`makepad_doc_fonts`), whose families and files are not Motion's own.

use makepad_doc_fonts::{bundled, downloadable, family, FAMILIES};

/// Scene3D hears of every font root: a Text3D's lowered scene names its font
/// file by absolute path, so the folders are ones a document may read, and
/// the 3D type builder reads the widget library's fonts and the host's font
/// bytes. Once, before the first font is resolved.
pub(crate) fn font_roots() {
    #[cfg(feature = "scene3d")]
    {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            makepad_stage_motion3d::doc::trust_resource_root(std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources"));
            makepad_text_mesh::letters::set_font_reader(Box::new(makepad_doc_fonts::host_font));
            makepad_doc_fonts::observe_roots(|crate_name, dir| {
                makepad_stage_motion3d::doc::trust_resource_root(dir.join("resources"));
                if crate_name == "makepad_widgets" {
                    makepad_text_mesh::letters::set_fonts_dir(dir.join("resources"));
                }
            });
        });
    }
}

/// A face as a document names it: a `Font{family weight italic axes}` value.
#[derive(Clone, Debug, PartialEq)]
pub struct FontSpec {
    /// The family's canonical name (checked: a document font, installed).
    pub family: &'static str,
    pub weight: f64,
    pub italic: bool,
    pub axes: Vec<(String, f64)>,
}

/// The face a `font:` value names: a `Font{…}` (family Inter, weight 400 when
/// left out). A string is the old form, an error saying what to write.
pub fn font_spec(vm: &makepad_script::ScriptVm, value: makepad_script::ScriptValue) -> Result<FontSpec, String> {
    use makepad_script::*;
    let old = |name: &str| format!("`font` is a Font value: write `font: Font{{family: \"{name}\"}}` (with `weight: 700`, `italic: true`, `axes: {{wdth: 100}}` inside it), or a `let` of one");
    if let Some(name) = vm.bx.heap_ref().string_with(value, |_, s| s.to_string()) {
        return Err(old(&name));
    }
    let Some(obj) = value.as_object() else {
        return Err(old("Inter"));
    };
    // Outside a Motion document's VM (a 3D scene running the document's top-level code again) a Font is
    // a plain record of the same fields.
    let motion_vm = vm.host.get::<crate::value::DocHost>().is_some();
    if motion_vm && crate::layers::type_of(vm, obj) != Some("Font") {
        return Err("`font` is a Font value: `Font{family: \"Inter\" weight: 400}`".into());
    }
    let get = |key: LiveId| vm.bx.heap_ref().value(obj, key.into(), NoTrap);
    let name = vm.bx.heap_ref().string_with(get(id!(family)), |_, s| s.to_string()).unwrap_or_else(|| "Inter".to_string());
    let family = font_or_error(&name)?;
    let weight = get(id!(weight)).as_number().filter(|w| w.is_finite()).unwrap_or(400.0);
    let italic = get(id!(italic)).as_bool().unwrap_or(false);
    let axes = match get(id!(axes)).as_object() {
        Some(a) => vm.bx.heap_ref().map_entries(a).iter().filter_map(|e| Some((e.key.as_id()?.as_string(|s| s.map(str::to_string))?, e.value.as_number()?))).collect(),
        None => Vec::new(),
    };
    Ok(FontSpec { family, weight, italic, axes })
}

/// The document font a name means, or what is wrong: no such font (the
/// fonts and the nearest name). A downloadable family that is not installed
/// here is its own name still: its text lays out and draws in the default
/// face, said once a session (`makepad_doc_fonts::drawn_family`).
pub fn font_or_error(name: &str) -> Result<&'static str, String> {
    font_roots();
    let Some(f) = family(name) else {
        let names: Vec<&str> = FAMILIES.iter().map(|f| f.name).collect();
        return Err(format!(
            "font \"{name}\" is no document font.{} Bundled fonts: {}; installable fonts (Stage's Resources): {} (`makepad-motion fonts` lists them)",
            crate::load::did_you_mean(name, &names),
            bundled().collect::<Vec<_>>().join(", "),
            downloadable().map(|f| f.name).collect::<Vec<_>>().join(", ")
        ));
    };
    Ok(f.name)
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_alias_of_a_bundled_family_is_a_document_font() {
        assert!(font_or_error("cormorant").is_ok());
    }
}
