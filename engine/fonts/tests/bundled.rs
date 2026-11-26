//! The bundled display faces are this crate's files, each with its licence.

#[test]
fn every_bundled_font_file_is_in_the_crate_with_its_licence() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/fonts");
    for family in makepad_doc_fonts::FAMILIES.iter().filter(|f| f.bundled) {
        for file in family.files {
            let makepad_doc_fonts::Place::Resource(resource) = file.place else { continue };
            if !resource.starts_with("makepad_doc_fonts:") {
                continue;
            }
            let path = makepad_doc_fonts::resource_path(resource);
            assert!(path.parent().unwrap().join("OFL.txt").is_file(), "{resource}: OFL.txt");
            let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{resource}: {e}"));
            // TrueType outlines (0x00010000) or OpenType with CFF outlines ("OTTO", the Math font).
            assert!(&bytes[..4] == &[0, 1, 0, 0] || &bytes[..4] == b"OTTO", "{resource} is an OpenType font");
        }
    }
    let total: u64 = std::fs::read_dir(&root).unwrap().flat_map(|d| std::fs::read_dir(d.unwrap().path()).unwrap()).map(|f| f.unwrap().metadata().unwrap().len()).sum();
    // The display fonts are ~2 MB; the Math layer's STIX Two Math face is another 0.84 MB.
    assert!(total < 3_100_000, "the bundled fonts stay around 3 MB: {total}");
}
