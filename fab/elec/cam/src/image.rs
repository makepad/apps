//! What the readers return and the writers take: a Gerber layer and a drill file as plain data.

use fab_elec_core::{Contour, Nm, PolySet, Shape, P};

/// Gerber layer polarity of an object: dark adds copper, clear removes what is under it.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Polarity {
    Dark,
    Clear,
}

/// Object attributes (`TO.*`) and the aperture function (`TA.AperFunction`) of one object.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct ObjAttrs {
    /// `TA.AperFunction` fields, e.g. `["SMDPad", "CuDef"]`. Regions carry none.
    pub aper_function: Vec<String>,
    /// `TO.N`: the net name.
    pub net: Option<String>,
    /// `TO.P`: reference designator and pin.
    pub pin: Option<(String, String)>,
    /// `TO.C`: the component reference.
    pub component: Option<String>,
}

/// The geometry of one object.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Geom {
    /// A flashed pad (circle, rectangle, oval, rounded rectangle), a drawn track (segment or
    /// arc) or polygons (written as regions; holes as clear regions after).
    Shape(Shape),
    /// One region: a single contour of lines and true arcs. A polygon with holes is written as
    /// one contour that cuts in to each hole and back (the cut is a zero-width slit), so a region
    /// never erases what lies in its holes and the write order does not matter.
    Region(Contour),
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct GItem {
    pub polarity: Polarity,
    pub geom: Geom,
    pub attrs: ObjAttrs,
}

impl GItem {
    pub fn dark(shape: Shape) -> GItem {
        GItem { polarity: Polarity::Dark, geom: Geom::Shape(shape), attrs: ObjAttrs::default() }
    }
    pub fn with_attrs(mut self, attrs: ObjAttrs) -> GItem {
        self.attrs = attrs;
        self
    }
}

/// A file attribute: `TF.<key>` with its fields.
pub type FileAttr = (String, Vec<String>);

/// A Gerber layer as read back (or as it will be written).
#[derive(Clone, PartialEq, Debug, Default)]
pub struct GerberImage {
    /// `TF.*` attributes in file order, key without the `TF.` prefix.
    pub file_attrs: Vec<FileAttr>,
    /// Objects in drawing order.
    pub items: Vec<GItem>,
    /// Fraction digits of the coordinate format the file used.
    pub decimals: u8,
    /// True when the file's unit is inches.
    pub inch: bool,
}

impl GerberImage {
    pub fn file_attr(&self, key: &str) -> Option<&[String]> {
        self.file_attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_slice())
    }

    /// The copper (or mask, silk) the layer draws: objects in order, dark adds, clear removes.
    /// Curves are chords at most `max_err` off.
    pub fn to_polyset(&self, max_err: Nm) -> PolySet {
        items_polyset(&self.items, max_err)
    }
}

/// The region a list of objects draws, in drawing order.
pub fn items_polyset(items: &[GItem], max_err: Nm) -> PolySet {
    let mut acc = PolySet::new();
    let mut pending: Vec<PolySet> = Vec::new();
    let mut pending_dark = true;
    let flush = |acc: &mut PolySet, pending: &mut Vec<PolySet>, dark: bool| {
        if pending.is_empty() {
            return;
        }
        let u = PolySet::union_all(pending.iter());
        *acc = if dark { acc.union(&u) } else { acc.difference(&u) };
        pending.clear();
    };
    for it in items {
        let dark = it.polarity == Polarity::Dark;
        if dark != pending_dark {
            flush(&mut acc, &mut pending, pending_dark);
            pending_dark = dark;
        }
        pending.push(geom_polyset(&it.geom, max_err));
    }
    flush(&mut acc, &mut pending, pending_dark);
    acc
}

/// One object's region.
pub fn geom_polyset(g: &Geom, max_err: Nm) -> PolySet {
    match g {
        Geom::Shape(s) => s.to_polyset(max_err),
        Geom::Region(c) => PolySet::from_ring_nonzero(c.to_ring(max_err)),
    }
}

/// A drilled hole: a round hole, or a slot from `at` to `to`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DrillHole {
    pub at: P,
    pub dia: Nm,
    /// The far end of a slot (`dia` wide), `None` for a round hole.
    pub to: Option<P>,
}


/// A drill file as read back.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct DrillImage {
    pub file_attrs: Vec<FileAttr>,
    pub holes: Vec<DrillHole>,
    /// Tool numbers and diameters, in file order.
    pub tools: Vec<(u32, Nm)>,
    pub inch: bool,
}

impl DrillImage {
    pub fn file_attr(&self, key: &str) -> Option<&[String]> {
        self.file_attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_slice())
    }
}
