//! VRML faces and refusal locations, from small scenes written here.

use fab_elec_import::mesh::srgb_to_linear;
use fab_elec_import::wrl::read_wrl;
use fab_elec_import::{Material, Mesh};

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn close(a: [f32; 3], b: [f32; 3]) -> bool {
    (0..3).all(|i| (a[i] - b[i]).abs() < 1e-5)
}

fn material<'a>(mesh: &'a Mesh, name: &str) -> &'a Material {
    mesh.materials.iter().find(|m| m.name == name).expect("material by name")
}

#[test]
fn wrl_reads_shapes_transforms_and_materials() {
    let read = read_wrl(br#"#VRML V2.0 utf8
DEF shared Material {
    diffuseColor .2e0, .4, .6 shininess .25
    specularColor 1 1 1 ambientIntensity .3
}
# The cube has six outward quads, flat across its right-angle edges.
Transform {
    translation 10, 20, 30 rotation 0 0 1 1.5707963267948966 scale 2 3 4
    children [
        Shape {
            appearance Appearance { material USE shared }
            geometry DEF cube IndexedFaceSet {
                coord Coordinate { point [
                    0 0 0, 1 0 0, 0 1 0, 1 1 0,
                    0 0 1, 1 0 1, 0 1 1, 1 1 1
                ] }
                coordIndex [0 2 3 1 -1, 4 5 7 6 -1, 0 1 5 4 -1,
                    2 6 7 3 -1, 0 4 6 2 -1, 1 3 7 5 -1]
                creaseAngle .5 solid TRUE convex TRUE
            }
        }
    ]
}
Transform {
    translation -4 0 0 rotation 0 1 0 1.5707963267948966 scale 2 3 4
    children [Shape {
        appearance Appearance { material USE shared }
        geometry IndexedFaceSet {
            coord DEF triangle Coordinate { point [0 0 1, 1 0 1, 0 1 1] }
            coordIndex [0 2 1 -1] ccw FALSE
            normal DEF outward Normal { vector [0 0 -1, 1 0 1, 0 1 1, 0 0 1] }
            normalIndex [3 2 1 -1]
        }
    }]
}
Transform {
    translation 0 -4 0 scale -1 1 1
    children [Shape {
        appearance Appearance { material Material {
            diffuseColor .8 .2 .1 transparency .4 emissiveColor .1 .3 .5 shininess .9
        } }
        geometry IndexedFaceSet {
            coord USE triangle coordIndex [0 1 2] # The final separator is optional.
            normal USE outward normalPerVertex FALSE normalIndex [3]
        }
    }]
}
Switch {
    whichChoice 1 choice [
        Shape { geometry USE cube }
        Shape { geometry IndexedFaceSet {
            coord USE triangle coordIndex [0x0 +1 2 -1]
            normal USE outward normalPerVertex FALSE normalIndex [3]
        } }
    ]
}
ROUTE source.value TO target.value
"#).expect("the scene reads");
    assert!(read.warnings.is_empty(), "{:?}", read.warnings);
    let mesh = &read.mesh;
    assert_eq!((mesh.triangle_count(), mesh.materials.len()), (15, 3), "chosen shapes and shared material");
    let (lo, hi) = mesh.bounds().expect("nonempty bounds");
    assert!(close(lo, [-1.0, -4.0, -2.0]) && close(hi, [10.0, 22.0, 34.0]), "bounds: {lo:?}, {hi:?}");
    let shared = material(mesh, "shared");
    assert_eq!(shared.base_color, [srgb_to_linear(0.2), srgb_to_linear(0.4), srgb_to_linear(0.6), 1.0],
        "base colour preserves exact decoded diffuse and opaque alpha despite white specular");
    assert_eq!((shared.roughness, shared.emissive), (0.75, [srgb_to_linear(0.0); 3]),
        "roughness follows shininess and emissive keeps default emission");
    assert_eq!(shared.metallic, 0.0, "metallic stays zero despite white Phong specular");
    assert_eq!(shared.specular, Some([srgb_to_linear(1.0); 3]), "specular retains decoded white Phong colour");
    let second = material(mesh, "material 2");
    assert_eq!(second.base_color, [srgb_to_linear(0.8), srgb_to_linear(0.2), srgb_to_linear(0.1), 0.6],
        "absent specular preserves the exact decoded diffuse and transparency");
    assert_eq!(second.emissive, [0.1, 0.3, 0.5].map(srgb_to_linear), "emissive display colours decode to linear light");
    assert!((second.roughness - 0.1).abs() < 1e-6 && second.metallic == 0.0,
        "absent specular keeps zero metalness and shininess-derived roughness");
    assert_eq!(second.specular, None, "specular is None without a specularColor field");
    assert_eq!(material(mesh, "default"), &Material::named("default"));

    for prim in &mesh.prims {
        let name = mesh.materials[prim.material].name.as_str();
        if name == "shared" {
            assert_eq!(prim.indices.len() / 3, 13, "USE shares the same builder material");
            let normal_at = |p| {
                let at = prim.positions.iter().position(|&v| close(v, p)).expect("transformed corner");
                prim.normals[at]
            };
            assert!(close(normal_at([0.0, 0.0, 0.0]), [1.0, 0.0, 0.0]), "normal rotated to +X");
            assert!(close(normal_at([0.0, 3.0, 0.0]), [0.6, 0.8, 0.0]), "inverse transpose with nonuniform scale");
            let unit = 1.0 / 5.0f32.sqrt();
            assert!(close(normal_at([0.0, 0.0, -2.0]), [unit, 0.0, -2.0 * unit]), "normalIndex follows reversed corners");
        }
        for tri in prim.indices.chunks_exact(3) {
            let [a, b, c] = [0, 1, 2].map(|i| prim.positions[tri[i] as usize]);
            let centre = match name {
                "shared" if a[2] > 10.0 => [8.5, 21.0, 32.0],
                "shared" => [-4.0, 0.9, -0.6],
                "material 2" => [-0.3, -3.7, 0.0],
                _ => [0.3, 0.3, 0.0],
            };
            let face = cross(sub(b, a), sub(c, a));
            assert!(dot(face, sub(a, centre)) > 0.0, "{name}: triangle faces out, including ccw FALSE and mirror");
            for &corner in tri {
                assert!(dot(face, prim.normals[corner as usize]) > 0.0, "{name}: given normals still face out");
            }
        }
    }

    // Centre and scale orientation act before the parent, with a translated nested child.
    let centred = read_wrl(br#"#VRML V2.0 utf8
Transform {
    translation 1 2 3 center 1 0 0 rotation 0 0 1 1.5707963267948966
    scale 2 3 1 scaleOrientation 0 0 1 1.5707963267948966
    children Transform { translation 0 1 0 children Shape { geometry IndexedFaceSet {
        coord Coordinate { point [0 0 0, 1 0 0, 0 1 0] } coordIndex [0 1 2]
    } } }
}
"#).expect("centred transform");
    let (lo, hi) = centred.mesh.bounds().expect("centred bounds");
    assert!(close(lo, [-2.0, -1.0, 3.0]) && close(hi, [0.0, 2.0, 3.0]), "centre and scale orientation: {lo:?}, {hi:?}");

    let read = read_wrl(br#"#VRML V2.0 utf8
DEF face IndexedFaceSet {
    coord Coordinate { point [0 0 0, 1 0 0, 0 1 0] } coordIndex [0 1 2]
}
Shape {
    appearance Appearance { material DEF dark Material {
        diffuseColor 0.15 0.15 0.15 specularColor 0.4 0.4 0.4
    } }
    geometry USE face
}
"#).expect("Phong material examples read");
    assert!(read.warnings.is_empty(), "Phong materials have no warnings: {:?}", read.warnings);
    let dark = material(&read.mesh, "dark");
    assert_eq!(dark.metallic, 0.0, "dark plastic metallic stays zero with a moderate highlight");
    assert_eq!(dark.base_color, [srgb_to_linear(0.15), srgb_to_linear(0.15), srgb_to_linear(0.15), 1.0],
        "dark plastic base colour is exactly decoded diffuse");
    assert_eq!(dark.specular, Some([srgb_to_linear(0.4); 3]), "dark plastic specular is exactly decoded Phong colour");
}

#[test]
fn wrl_warns_and_refuses() {
    let read = read_wrl(br#"#VRML V2.0 utf8
Mystery { note "braces { [ and # inside a string; \"quote\" and \\ slash" }
Mystery {}
Shape { geometry Box {} }
Shape { geometry Box {} }
Shape { geometry IndexedLineSet {} }
Shape { geometry IndexedLineSet {} }
Shape { appearance Appearance { texture ImageTexture { url ["unused"] } } }
Shape { appearance Appearance { texture ImageTexture { url ["unused"] } } }
PROTO Hidden [field SFVec3f offset 0 0 0] { Transform { translation IS offset } }
Group { extra 1 children [] }
Group { extra 2 children [] }
"#).expect("unsupported content is left out");
    let warnings = &read.warnings;
    assert_eq!(warnings.len(), 7, "one warning per kind plus the empty file: {warnings:?}");
    for (i, (line, field, message)) in [
        (2, "Mystery", "Mystery node is left out (2 occurrences)"),
        (4, "Box", "Box geometry is left out (2 occurrences)"),
        (6, "IndexedLineSet", "IndexedLineSet geometry is left out (2 occurrences)"),
        (8, "Appearance.texture", "textures are left out; the colours are kept"),
        (10, "PROTO", "PROTO Hidden is left out"),
        (11, "Group.extra", "unknown field is left out (2 occurrences)"),
        (1, "VRML", "the file has no faces"),
    ].into_iter().enumerate() {
        assert_eq!((warnings[i].line, warnings[i].field.as_str(), warnings[i].message.as_str()), (line, field, message));
    }

    let partial = read_wrl(br#"#VRML V2.0 utf8
EXTERNPROTO External [] ["unused.wrl"]
Inline { url ["part \"one\"\\two" "unused"] }
LOD { range [10] level [
    Group { children Anchor { children Collision { children Billboard { children Shape {
        geometry IndexedFaceSet {
            coord Coordinate { point [0 0 0, 1 0 0, 0 1 0] }
            coordIndex [0 1 2 -1, 0 0 1 -1]
            color Color { color [1 0 0] } colorIndex [0]
            texCoord TextureCoordinate { point [0 0, 1 0, 0 1] }
        }
    } } } } }
    Shape { geometry IndexedFaceSet {
        coord Coordinate { point [20 0 0, 21 0 0, 20 1 0] } coordIndex [0 1 2]
    } }
] }
LOD { level [] }
"#).expect("partial geometry survives skipped content");
    assert_eq!(partial.mesh.triangle_count(), 1, "only the first LOD level and nondegenerate face survive");
    assert_eq!(partial.mesh.bounds(), Some(([0.0; 3], [1.0, 1.0, 0.0])));
    assert_eq!(partial.warnings.iter().map(|w| w.message.as_str()).collect::<Vec<_>>(), [
        "PROTO External is left out",
        "Inline part \"one\"\\two is left out",
        "LOD: only the first level is read",
        "vertex colours are left out",
        "1 faces with no area are left out",
    ]);

    // All 257 nested nodes are literal input; the refusal is on the innermost node.
    let deep = br#"#VRML V2.0 utf8
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group { children Group { children Group { children Group { children
Group {}
}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}
}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}
}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}
}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}}
"#;
    let error = read_wrl(deep).expect_err("nesting is bounded");
    assert_eq!((error.line, error.field.as_str(), error.message.as_str()),
        (66, "Group", "nesting deeper than 256 nodes"));

    for (source, line, field, message) in [
        ("Shape {}", 1, "#VRML", "not a VRML 2.0 file"),
        ("#VRML V1.0 ascii\n", 1, "#VRML", "VRML 1.0 is not read"),
        ("#VRML V2.0 utf8\nShape { geometry IndexedFaceSet {\n\
            coord Coordinate { point [0 0 0, 1 0 0, 0 1 0] }\ncoordIndex [0 1 9]\n} }",
            4, "IndexedFaceSet.coordIndex", "index 9 is outside the 3 points"),
        ("#VRML V2.0 utf8\nGroup { children [\nUSE absent\n] }", 3, "USE", "undefined USE absent"),
        ("#VRML V2.0 utf8\nGroup {\nchildren []\n", 4, "Group", "expected '}'"),
        ("#VRML V2.0 utf8\nInline { url \"unfinished", 2, "Inline.url", "unterminated string"),
        ("#VRML V2.0 utf8\nTransform\ntranslation 1 2 3", 3, "Transform", "expected '{'"),
        ("#VRML V2.0 utf8\nTransform { scale 1e999 1 1 }", 2, "Transform.scale", "number is not finite"),
        ("#VRML V2.0 utf8\nShape { geometry IndexedFaceSet {\ncoord Coordinate { point [0 0] } coordIndex [0]\n} }",
            3, "Coordinate.point", "expected a multiple of 3 numbers"),
        ("#VRML V2.0 utf8\nShape { geometry IndexedFaceSet {\n\
            coord Coordinate { point [0 0 0, 1 0 0, 0 1 0] } coordIndex [0 1 2]\n\
            normal Normal { vector [0 0 1] }\nnormalIndex [0 0]\n} }",
            5, "IndexedFaceSet.normalIndex", "normalIndex is shorter than needed"),
        ("#VRML V2.0 utf8\nShape { geometry IndexedFaceSet {\n\
            coord Coordinate { point [0 0 0, 1 0 0, 0 1 0] } coordIndex [0 1 2]\nnormal Normal { vector [0 0 1] }\n} }",
            4, "IndexedFaceSet.normal", "index 1 is outside the 1 vectors"),
    ] {
        let error = read_wrl(source.as_bytes()).expect_err("damaged geometry is refused");
        assert_eq!((error.line, error.field.as_str(), error.message.as_str()), (line, field, message), "{source}");
    }
}
