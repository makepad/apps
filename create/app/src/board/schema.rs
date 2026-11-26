//! The data-page vocabulary used for printing and reading constructor and property names.
use makepad_work_core::{entity::{field, kind}, event::Key};

#[derive(Clone, Copy)]
pub enum Vocabulary { Board }

pub struct Schema {
    pub root: Constructor,
    pub kinds: &'static [Constructor],
    pub item_fields: &'static [Field],
}

pub struct Constructor {
    /// Slice 1 stores these constructors under numeric entity kinds. Later kinds can be named only.
    pub entity_kind: Option<u16>,
    pub name: &'static str,
    pub fields: &'static [Field],
}

pub struct Field {
    pub name: &'static str,
    pub form: Literal,
    pub source: Source,
}

#[derive(Clone, Copy)]
pub enum Literal { Reference, Units, Color, Text, Enum, Bool, Anchor, Children }

#[derive(Clone)]
pub enum Source { Register(Key), Identity, RootTitle, Body, Children }

const fn register(name: &'static str, form: Literal, key: Key) -> Field {
    Field { name, form, source: Source::Register(key) }
}

const fn constructor(id: u16, name: &'static str, fields: &'static [Field]) -> Constructor {
    Constructor { entity_kind: Some(id), name, fields }
}

const ITEMS: Field = Field { name: "items", form: Literal::Children, source: Source::Children };

const BOARD: Schema = Schema {
    root: constructor(kind::BOARD, "Board", &[
        Field { name: "title", form: Literal::Text, source: Source::RootTitle }, ITEMS,
    ]),
    kinds: &[
        constructor(kind::STICKY, "Sticky", &[]),
        constructor(kind::TEXT_BLOCK, "TextBlock", &[]),
        constructor(kind::IMAGE, "Image", &[]),
        constructor(kind::FILE, "File", &[]),
        constructor(kind::WEB_LINK, "WebLink", &[]),
        constructor(kind::SHAPE, "Shape", &[]),
        constructor(kind::CONNECTOR, "Connector", &[]),
        constructor(kind::SECTION, "Section", &[ITEMS]),
        constructor(kind::STROKE, "Stroke", &[]),
        constructor(kind::INLINE_APP, "InlineApp", &[]),
    ],
    // Slice 1 allows style fields on any item. Absent optional registers have no literal.
    item_fields: &[
        Field { name: "id", form: Literal::Reference, source: Source::Identity },
        register("x", Literal::Units, field::X), register("y", Literal::Units, field::Y),
        register("w", Literal::Units, field::W), register("h", Literal::Units, field::H),
        register("color", Literal::Color, field::COLOUR), register("title", Literal::Text, field::TITLE),
        Field { name: "text", form: Literal::Text, source: Source::Body },
        register("locked", Literal::Bool, field::LOCKED), register("shape", Literal::Enum, field::SHAPE),
        register("blob", Literal::Reference, field::BLOB), register("url", Literal::Text, field::URL),
        register("from", Literal::Reference, field::FROM), register("to", Literal::Reference, field::TO),
        register("anchor", Literal::Anchor, field::ANCHOR), register("label", Literal::Text, field::LABEL),
    ],
};

impl Vocabulary {
    pub fn schema(self) -> &'static Schema {
        match self { Self::Board => &BOARD }
    }
}

impl Schema {
    pub fn by_name(&self, name: &str) -> Option<&Constructor> {
        std::iter::once(&self.root).chain(self.kinds).find(|c| c.name == name)
    }

    /// Resolve a constructor for a local write. Foreign events keep the key their author signed.
    pub fn kind_for_write(&self, key: Key) -> Key {
        if let Key::Name(name) = &key {
            if let Some(id) = self.by_name(name).and_then(|c| c.entity_kind) {
                return Key::Number(id)
            }
        }
        key
    }

    pub(crate) fn item(&self, kind: u16) -> Option<&Constructor> {
        self.kinds.iter().find(|c| c.entity_kind == Some(kind))
    }
}
