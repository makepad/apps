use makepad_asset_client::tree::*;
use makepad_asset_client::{PublishBundle, PublishExpectedHead, PublishRights, PublishThumbnail};
use makepad_asset_data::{AssetId, AssetKind, AssetRevisionId, AssetRevisionRef, ThumbnailMedia};
use std::collections::BTreeMap;

fn reference(n: u8) -> AssetRevisionRef {
    AssetRevisionRef { asset_id: AssetId::from_bytes([n; 16]), revision: AssetRevisionId::from_bytes([n; 32]) }
}

#[test]
fn manifest_is_sorted_stable_and_preserves_pinned_links() {
    let text = format!("tree \"A \\\"deck\\\"\"\ndeck deck.splash\nfile slides/opening.splash {}\nlink shared/look.splash stage/library/looks look.splash {}\nfile deck.splash {}\n", reference(2), reference(3), reference(1));
    let tree = Tree::parse(&text).unwrap();
    let serialized = tree.serialize().unwrap();
    assert_eq!(Tree::parse(&serialized).unwrap(), tree);
    assert_eq!(Tree::parse(&serialized).unwrap().serialize().unwrap(), serialized);
    assert_eq!(tree.dependencies(), vec![reference(1), reference(2), reference(3)]);
    assert_eq!(serialized.lines().skip(2).map(|l| l.split_whitespace().nth(1).unwrap()).collect::<Vec<_>>(), ["deck.splash", "shared/look.splash", "slides/opening.splash"]);
    assert!(Tree::parse(&format!("file deck.splash {}\nfile deck.splash {}", reference(1), reference(2))).is_err());
    assert!(Tree::parse("deck missing.splash").is_err());
    assert!(Tree::parse("file deck.splash not_a_revision").is_err());
}

#[test]
fn paths_are_relative_portable_and_unambiguous() {
    for bad in ["", "/deck.splash", "../deck.splash", "a//b.splash", "a\\b.splash", "Deck.splash", "my-slide.splash", "no_extension", "folder_/a.splash"] { assert!(validate_path(bad).is_err(), "{bad}"); }
    let tree = Tree { entries: BTreeMap::from([
        ("slides/opening.splash".into(), TreeEntry::File(reference(1))),
        ("shared/look.splash".into(), TreeEntry::File(reference(2))),
    ]), ..Default::default() };
    assert_eq!(tree.resolve("slides/opening.splash", "../shared/look").unwrap().0, "shared/look.splash");
    assert_eq!(tree.resolve("slides/opening.splash", "./opening").unwrap().0, "slides/opening.splash");
    assert!(tree.resolve("deck.splash", "../outside").is_err());
    let mut ambiguous = tree.clone();
    ambiguous.entries.insert("shared/look.png".into(), TreeEntry::File(reference(3)));
    assert!(ambiguous.resolve("deck.splash", "shared/look").is_err());
    assert_eq!(ambiguous.resolve("deck.splash", "shared/look.splash").unwrap().0, "shared/look.splash");
}

#[derive(Default)]
struct Store {
    files: BTreeMap<AssetRevisionRef, TreeContent>,
    head: Option<AssetRevisionRef>,
    publications: Vec<PublishBundle>,
}

impl TreeStore for Store {
    fn read(&mut self, reference: AssetRevisionRef) -> Result<TreeContent, String> {
        self.files.get(&reference).cloned().ok_or("missing content".into())
    }
    fn publish(&mut self, bundle: &PublishBundle) -> Result<AssetRevisionRef, String> {
        if bundle.kind == AssetKind::Tree {
            match bundle.expected_head {
                PublishExpectedHead::Absent if self.head.is_none() => {},
                PublishExpectedHead::Exact(head) if self.head == Some(head) => {},
                _ => return Err("stale head".into()),
            }
            let tree = Tree::parse(std::str::from_utf8(&bundle.files[0].bytes).unwrap()).unwrap();
            assert_eq!(tree.dependencies(), bundle.dependencies);
            assert!(bundle.dependencies.iter().all(|r| self.files.contains_key(r)));
        }
        let reference = AssetRevisionRef {
            asset_id: bundle.asset_id.unwrap_or_else(|| AssetId::from_bytes([(self.publications.len() + 1) as u8; 16])),
            revision: AssetRevisionId::hash_of(&bundle.files[0].bytes),
        };
        self.files.insert(reference, TreeContent { kind: bundle.kind, bytes: bundle.files[0].bytes.clone().into(), rights: bundle.rights.clone() });
        if bundle.kind == AssetKind::Tree { self.head = Some(reference); }
        self.publications.push(bundle.clone());
        Ok(reference)
    }
}

fn editor() -> TreeEditor {
    let mut bundle = PublishBundle::new("stage", AssetKind::Tree, "Tree", vec![], PublishThumbnail::plain(vec![], ThumbnailMedia::Png, 256, 256), PublishRights::generated_cc0());
    bundle.asset_id = Some(AssetId::from_bytes([240; 16]));
    bundle.alias = Some("stage/tests/tree".parse().unwrap());
    TreeEditor::new(Tree::default(), bundle, None).unwrap()
}

fn text(store: &Store, editor: &TreeEditor, path: &str) -> String {
    String::from_utf8(store.files[&editor.tree.entries[path].reference().unwrap()].bytes.to_vec()).unwrap()
}

#[test]
fn each_edit_publishes_one_tree_and_rename_updates_only_parsed_references() {
    let (mut store, mut editor) = (Store::default(), editor());
    editor.write(&mut store, "shared/look.splash", b"{color: 1}", AssetKind::Motion).unwrap();
    editor.write(&mut store, "slides/opening.splash", b"let look = import(\"../shared/look\")\n// graphic(\"shared/look\")\n{caption: \"shared/look\" color: look.color}", AssetKind::Motion).unwrap();
    let old = editor.tree.clone();
    editor.rename(&mut store, "shared/look.splash", "shared/deck_look.splash").unwrap();
    assert_eq!(text(&store, &editor, "slides/opening.splash"), "let look = import(\"shared/deck_look.splash\")\n// graphic(\"shared/look\")\n{caption: \"shared/look\" color: look.color}");
    assert!(old.entries.contains_key("shared/look.splash"));
    editor.copy(&mut store, "slides/opening.splash", "slides/closing.splash").unwrap();
    assert_eq!(editor.tree.entries["slides/opening.splash"], editor.tree.entries["slides/closing.splash"]);
    let before = store.publications.len();
    assert!(editor.delete(&mut store, "shared/deck_look.splash").is_err());
    assert_eq!(store.publications.len(), before, "refuse dangling paths before publication");
    editor.delete(&mut store, "slides/closing.splash").unwrap();
    assert_eq!(store.publications.iter().filter(|b| b.kind == AssetKind::Tree).count(), 5);
    assert_eq!(store.head, editor.head);
}

#[test]
fn moving_or_copying_a_document_preserves_its_relative_targets() {
    let (mut store, mut editor) = (Store::default(), editor());
    editor.write(&mut store, "slides/base.splash", b"{}", AssetKind::Motion).unwrap();
    editor.write(&mut store, "slides/opening.splash", b"let base = import(\"./base\")\n{}", AssetKind::Motion).unwrap();
    editor.copy(&mut store, "slides/opening.splash", "shared/copy.splash").unwrap();
    assert!(text(&store, &editor, "shared/copy.splash").contains("import(\"slides/base.splash\")"));
    editor.rename(&mut store, "slides/opening.splash", "shared/opening.splash").unwrap();
    assert!(text(&store, &editor, "shared/opening.splash").contains("import(\"slides/base.splash\")"));
}

#[test]
fn invalid_references_and_stale_heads_never_change_the_tree() {
    let (mut store, mut editor) = (Store::default(), editor());
    editor.write(&mut store, "deck.splash", b"{}", AssetKind::Deck).unwrap();
    let (head, tree) = (editor.head, editor.tree.clone());
    assert!(editor.write(&mut store, "deck.splash", b"let image = asset(\"id\", \"rev\")\n{}", AssetKind::Deck).is_err());
    assert!(editor.write(&mut store, "deck.splash", b"{src: graphic(\"missing\")}", AssetKind::Deck).is_err());
    assert!(editor.rename(&mut store, "deck.splash", "deck.splash").is_err());
    store.head = Some(reference(99));
    assert!(editor.write(&mut store, "deck.splash", b"{title: \"Changed\"}", AssetKind::Deck).is_err());
    assert_eq!(editor.head, head);
    assert_eq!(editor.tree, tree);
    assert_eq!(store.head, Some(reference(99)));
}

#[test]
fn editing_a_copy_keeps_the_other_files_exact_revision() {
    let (mut store, mut editor) = (Store::default(), editor());
    editor.write(&mut store, "slides/opening.splash", b"{title: 1}", AssetKind::Motion).unwrap();
    let original = editor.tree.entries["slides/opening.splash"].clone();
    editor.copy(&mut store, "slides/opening.splash", "slides/closing.splash").unwrap();
    editor.write(&mut store, "slides/closing.splash", b"{title: 2}", AssetKind::Motion).unwrap();
    assert_eq!(editor.tree.entries["slides/opening.splash"], original);
    assert_ne!(editor.tree.entries["slides/closing.splash"], original);
    assert_eq!(editor.tree.dependencies().len(), 2);
}
