//! Typed indices. Every id is a dense index into the matching `Vec` on
//! [`crate::model::Scene`] / [`crate::ModelData`], so lookups are O(1) and the ids
//! double as GPU-side element indices (see [`crate::Vertex::element`]).

macro_rules! dense_id {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(pub u32);

        impl $name {
            /// Sentinel meaning "no such thing". Never a valid index.
            pub const NONE: $name = $name(u32::MAX);

            #[inline]
            pub fn index(self) -> usize {
                self.0 as usize
            }

            #[inline]
            pub fn is_none(self) -> bool {
                self.0 == u32::MAX
            }

            #[inline]
            pub fn from_index(i: usize) -> Self {
                $name(i as u32)
            }
        }

        impl Default for $name {
            fn default() -> Self {
                $name::NONE
            }
        }
    };
}

dense_id!(
    /// A scene element (a part, the bed, a helper object). Index into
    /// `Scene::elements`.
    ElementId
);
dense_id!(
    /// A material slot. Index into `Scene::materials`.
    MaterialId
);
dense_id!(
    /// A mesh as delivered by the parser. Index into `ModelData::meshes`.
    /// Meshes do not survive into `Scene`; they are merged into batches.
    MeshId
);
