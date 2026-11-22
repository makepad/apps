# Fab glTF loader

The in-tree loader builds an editable Fab `Document` from glTF or GLB: one
object per node (world transforms resolved), its triangle primitives as meshes,
the PBR materials (base colour, metallic/roughness, emission, the
`KHR_materials_transmission` and `KHR_materials_ior` factors) and the base
colour textures.
