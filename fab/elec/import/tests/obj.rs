//! OBJ geometry and MTL factors from small, in-memory files, including located refusals.

use fab_elec_import::mesh::srgb_to_linear;
use fab_elec_import::obj::{read_mtl, read_obj, DEFAULT_CREASE};
use fab_elec_import::{Issue, Material};

#[test]
fn obj_reads_faces_materials_and_normals() {
    let library = concat!(
        "# display colour material factors\r\n",
        "newmtl body\r\n",
        "Kd 0.2 0.4 0.6\r\n",
        "d 0.5\r\n",
        "Ke 0.1 0.2 0.3\r\n",
        "Ns 900\r\n",
        "newmtl metal\r\n",
        "Pr 0.2\r\n",
        "Pm 1\r\n",
        "Ns 10\r\n",
        "map_Kd colour.png\r\n",
        "map_detail detail.png\r\n",
    );
    let source = concat!(
        "# a unit cube and a concave L\r\n",
        "o housing\r\n",
        "g shell contacts\r\n",
        "mtllib parts.mtl\r\n",
        "v 0 0 0\r\n",
        "v 2 0 0 2\r\n",
        "v 0 1 0\r\n",
        "v 1 1 0\r\n",
        "v 0 0 1\r\n",
        "v 1 0 1\r\n",
        "v 0 1 1\r\n",
        "v 1 1 1\r\n",
        "vt 0\r\n",
        "vt 1 0\r\n",
        "vt 1 1 0\r\n",
        "vt 0 1\r\n",
        "vn 0 3 4\r\n",
        "s off\r\n",
        "usemtl body\r\n",
        "f 1 3 4 \\ \t\r\n",
        "  2 # the bottom face continues from the previous line\r\n",
        "s 1\r\n",
        "f 1/1 2/2 6/3 5/4\r\n",
        "f -6/-4 -2//-1 -1/-2/-1 -5\r\n",
        "usemtl metal\r\n",
        "f 5/1/1 6/2/1 8/3/1 7/4/1\r\n",
        "usemtl delayed paint\r\n",
        "f 1 5 7 3\r\n",
        "usemtl absent\r\n",
        "f 2 4 8 6\r\n",
        "newmtl delayed paint\r\n",
        "Kd 0.2\r\n",
        "Tr 0.25\r\n",
        "d 0.6\r\n",
        "Ns 1000\r\n",
        "Pr 0.3\r\n",
        "v 3 0 0\r\n",
        "v 5 0 0\r\n",
        "v 5 1 0\r\n",
        "v 4 1 0\r\n",
        "v 4 2 0\r\n",
        "v 3 2 0\r\n",
        "usemtl delayed paint\r\n",
        "f 9 10 11 12 13 14\r\n",
        "extra ignored\r\n",
        "l 1 2\r\n",
        "extra also ignored\r\n",
        "l 3 4\r\n",
    );
    let mut requested = Vec::new();
    let read = read_obj(source.as_bytes(), &mut |name| {
        requested.push(name.to_owned());
        (name == "parts.mtl").then(|| library.as_bytes().to_vec())
    }).expect("the cube, materials and L read");
    assert_eq!(requested, ["parts.mtl"], "the callback gets the name as written");
    assert_eq!(read.mesh.triangle_count(), 16, "six quads and a concave hexagon");
    assert_eq!(read.mesh.materials.len(), 4, "late definitions fill their existing material");
    let named = |name: &str| read.mesh.materials.iter().find(|m| m.name == name).expect("material by name");
    let body = named("body");
    assert_eq!(body.base_color, [srgb_to_linear(0.2), srgb_to_linear(0.4), srgb_to_linear(0.6), 0.5],
        "absent Ks preserves the exact decoded diffuse and alpha factors");
    assert_eq!(body.emissive, [0.1, 0.2, 0.3].map(srgb_to_linear), "emissive display colours decode to linear light");
    assert!((body.roughness - (1.0 - 0.9f32.sqrt())).abs() < 1e-6, "Ns converts to roughness");
    assert_eq!(body.metallic, 0.0, "absent Pm keeps zero metallic");
    assert_eq!(body.specular, None, "specular is None without a Ks statement");
    assert_eq!(named("metal").roughness, 0.2, "Pr wins over a later Ns");
    assert_eq!(named("metal").metallic, 1.0, "Pm sets metallic");
    assert_eq!(named("delayed paint").base_color, [srgb_to_linear(0.2), srgb_to_linear(0.2), srgb_to_linear(0.2), 0.6],
        "scalar Kd decodes to linear light and later d wins");
    assert_eq!(named("delayed paint").roughness, 0.3, "Pr wins over an earlier Ns");
    assert_eq!(named("absent"), &Material::named("absent"), "undefined materials stay grey");
    assert!((DEFAULT_CREASE - 30.0f32.to_radians()).abs() < 1e-6, "the default crease is thirty degrees");

    let top = read.mesh.prims.iter().find(|p| read.mesh.materials[p.material].name == "metal").expect("top face");
    assert_eq!(top.indices.len(), 6, "the given-normal face is a quad");
    assert!(top.normals.iter().all(|n| {
        n[0] == 0.0 && (n[1] - 0.6).abs() < 1e-6 && (n[2] - 0.8).abs() < 1e-6
    }), "given normals are normalised and kept instead of being regenerated");
    let body_prim = read.mesh.prims.iter().find(|p| read.mesh.materials[p.material].name == "body").expect("body");
    assert!(body_prim.normals.iter().all(|n| n.iter().filter(|v| v.abs() == 1.0).count() == 1),
        "partial corner normals fall back to crease normals; cube edges stay sharp");
    let paint = read.mesh.prims.iter().find(|p| read.mesh.materials[p.material].name == "delayed paint").expect("paint");
    let mut area = 0.0;
    for triangle in paint.indices.chunks_exact(3) {
        let [a, b, c] = [0, 1, 2].map(|i| paint.positions[triangle[i] as usize]);
        if a[0] >= 3.0 && b[0] >= 3.0 && c[0] >= 3.0 {
            let twice_area = (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]);
            assert!(twice_area > 0.0, "L triangles retain their front winding");
            area += twice_area / 2.0;
        }
    }
    assert!((area - 3.0).abs() < 1e-6, "the concave L is covered once");
    assert_eq!(read.warnings, vec![
        Issue::new(11, "map_Kd", "parts.mtl: textures are left out; the colours are kept"),
        Issue::new(29, "usemtl", "material absent is not defined; grey"),
        Issue::new(45, "extra", "unknown statement, left out (2 statements)"),
        Issue::new(46, "l", "2 lines are left out"),
    ], "warnings keep first-occurrence order, count repetitions and retain library locations");

    let (mats, warnings) = read_mtl(library.as_bytes()).expect("the standalone library reads");
    assert_eq!(mats, vec![body.clone(), named("metal").clone()], "MTL materials stay in file order");
    assert_eq!(warnings, vec![Issue::new(11, "map_Kd", "textures are left out; the colours are kept")]);

    // Late library definitions, repeated definitions across sources, and alpha precedence.
    let source = concat!(
        "v 0 0 0\nv 1 0 0\nv 0 1 0\nusemtl coating\nf 1 2 3\n",
        "mtllib first.mtl second.mtl\nnewmtl coating\nKd 0\nnewmtl coating\nKd 0.5\n",
    );
    let read = read_obj(source.as_bytes(), &mut |name| match name {
        "first.mtl" => Some(b"newmtl coating\nKd 2 -1 0.5\nd -halo 0.2\nTr 0.25\n".to_vec()),
        "second.mtl" => Some(b"newmtl coating\nKd 0\nnewmtl coating\nKd 1\n".to_vec()),
        _ => None,
    }).expect("the first definition stands across libraries and inline statements");
    assert_eq!(read.mesh.materials.len(), 1, "one builder material per name");
    assert_eq!(read.mesh.materials[0].base_color, [1.0, 0.0, srgb_to_linear(0.5), 0.75],
        "decoded colours clamp at the builder and later Tr wins");
    assert_eq!(read.warnings, vec![
        Issue::new(3, "d", "first.mtl: halo is left out"),
        Issue::new(1, "newmtl", "second.mtl: material coating is defined twice; the first stands"),
    ], "one duplicate warning per name and no unresolved warning after a late definition");

    let (mats, warnings) = read_mtl(concat!(
        "Kd ignored\nPm ignored\nnewmtl raw factors\nKd 2 -1 0.5\nPr 2\nPm -1\nKe 0.3\n",
        "Kd spectral sample\nKd xyz 1 1 1\nextra a\nextra b\n",
    ).as_bytes()).expect("ignored preamble and colour models leave usable factors");
    assert_eq!(mats[0].base_color, [2.0, -1.0, srgb_to_linear(0.5), 1.0], "standalone MTL decodes in-range colours without clamping");
    assert_eq!((mats[0].roughness, mats[0].metallic, mats[0].emissive), (2.0, -1.0, [srgb_to_linear(0.3); 3]),
        "scalar Ke decodes to linear light while standalone Pr and Pm stay unclamped");
    assert_eq!(warnings, vec![
        Issue::new(1, "Kd", "statements before newmtl are left out"),
        Issue::new(8, "Kd", "non-RGB colour is left out; the colour is kept"),
        Issue::new(9, "Kd", "non-RGB colour is left out; the colour is kept"),
        Issue::new(10, "extra", "unknown statement, left out (2 statements)"),
    ]);

    // An empty normal index is no normal: `1//` reads as `1`, shaded by creases; `1/2/` as `1/2`.
    let quad = "v 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nvt 0\nvt 1\n";
    let plain = read_obj(format!("{quad}f 1 2 3 4\n").as_bytes(), &mut |_| None).expect("plain corners");
    let empty = read_obj(format!("{quad}f 1// 2// 3// 4//\n").as_bytes(), &mut |_| None).expect("empty normal indices");
    let textured = read_obj(format!("{quad}f 1/1/ 2/2/ 3/1/ 4/2/\n").as_bytes(), &mut |_| None).expect("empty normal after vt");
    assert!(empty.warnings.is_empty() && textured.warnings.is_empty(), "{:?} {:?}", empty.warnings, textured.warnings);
    assert_eq!((&empty.mesh, &textured.mesh), (&plain.mesh, &plain.mesh), "an empty normal index changes nothing");

    let library = concat!(
        "newmtl dark\nKd 0.15 0.15 0.15\nKs 0.4 0.4 0.4\n",
        "newmtl explicit first\nPm 0.7\nKs 0.4\nKd 0.15\n",
        "newmtl explicit last\nKd 0.15\nKs 0.4\nPm 0.7\n",
        "newmtl reversed\nKs 0.4 0.4 0.4\nKd 0.15 0.15 0.15\n",
    );
    let (mats, warnings) = read_mtl(library.as_bytes()).expect("Phong material examples read");
    assert!(warnings.is_empty(), "Phong materials have no warnings: {warnings:?}");
    let named = |name: &str| mats.iter().find(|m| m.name == name).expect("Phong material by name");
    let dark = named("dark");
    assert_eq!(dark.metallic, 0.0, "dark plastic metallic stays zero with a moderate highlight");
    assert_eq!(dark.base_color, [srgb_to_linear(0.15), srgb_to_linear(0.15), srgb_to_linear(0.15), 1.0],
        "dark plastic base colour is exactly decoded Kd");
    assert_eq!(dark.specular, Some([srgb_to_linear(0.4); 3]), "dark plastic specular is exactly decoded Ks");
    let mut reversed = named("reversed").clone();
    reversed.name = dark.name.clone();
    assert_eq!(&reversed, dark, "all material properties are independent of Ks before or after Kd");
    for name in ["explicit first", "explicit last"] {
        assert_eq!(named(name).metallic, 0.7, "{name}: explicit Pm sets metallic regardless of statement order");
        assert_eq!(named(name).base_color, dark.base_color, "{name}: explicit Pm preserves exact decoded base colour");
        assert_eq!(named(name).specular, dark.specular, "{name}: explicit Pm preserves exact decoded specular");
    }
    let faces = mats.iter().map(|m| format!("usemtl {}\nf 1 2 3\n", m.name)).collect::<String>();
    let geometry = format!("v 0 0 0\nv 1 0 0\nv 0 1 0\n{faces}");
    for (source, kind) in [
        (format!("{library}{geometry}"), "inline"),
        (format!("mtllib phong.mtl\n{geometry}"), "library"),
    ] {
        let read = read_obj(source.as_bytes(), &mut |name| {
            (name == "phong.mtl").then(|| library.as_bytes().to_vec())
        }).expect("OBJ with Phong materials reads");
        assert_eq!(read.mesh.materials, mats, "{kind}: OBJ finalizes every Phong material exactly as standalone MTL");
        assert!(read.warnings.is_empty(), "{kind}: valid Phong OBJ has no warnings: {:?}", read.warnings);
    }
}

#[test]
fn obj_refuses_malformed_files() {
    for (source, line, field, message) in [
        ("# header\nv bad 0 0\n", 2, "v", "'bad' is not a number"),
        ("v 0 0 0\nf 0 1 1\n", 2, "f", "vertex 0 is not defined (1 so far)"),
        ("v 0 0 0\nf 2 1 1\n", 2, "f", "vertex 2 is not defined (1 so far)"),
        ("v 0 0 0\nf -2 1 1\n", 2, "f", "vertex -2 is not defined (1 so far)"),
        ("v 0 0 0\nf 1.5 1 1\n", 2, "f", "vertex 1.5 is not defined (1 so far)"),
        ("\nv 1 2\n", 2, "v", "v takes 3, 4 or 6 numbers"),
        ("v 1 2 3 0\n", 1, "v", "vertex weight is zero"),
        ("v 3e38 0 0 1e-38\n", 1, "v", "homogeneous position is not finite"),
        ("vn 0 NaN 1\n", 1, "vn", "'NaN' is not a number"),
        ("vt inf\n", 1, "vt", "'inf' is not a number"),
        ("vn 0 1\n", 1, "vn", "vn takes 3 numbers"),
        ("vt\n", 1, "vt", "vt takes 1, 2 or 3 numbers"),
        ("v 0 0 0\nvt 0\nf 1/2 1/1 1/1\n", 3, "f", "texture coordinate 2 is not defined (1 so far)"),
        ("v 0 0 0\nvn 0 0 1\nf 1//-2 1//1 1//1\n", 3, "f", "normal -2 is not defined (1 so far)"),
        ("v 0 0 0\nf 1/ 1 1\n", 2, "f", "'1/' is not a face corner"),
        ("v 0 \\\n bad 0\n", 1, "v", "'bad' is not a number"),
        ("newmtl paint\nKd 0 nope 0\n", 2, "Kd", "'nope' is not a number"),
        ("newmtl paint\nNs NaN\n", 2, "Ns", "'NaN' is not a number"),
        (
            "v 0 0 0\nf -170141183460469231731687303715884105728 1 1\n", 2, "f",
            "vertex -170141183460469231731687303715884105728 is not defined (1 so far)",
        ),
        (
            "v 0 0 0\nf 999999999999999999999999999999999999999999999 1 1\n", 2, "f",
            "vertex 999999999999999999999999999999999999999999999 is not defined (1 so far)",
        ),
    ] {
        let error = read_obj(source.as_bytes(), &mut |_| None).expect_err("malformed input is refused");
        assert_eq!(error, Issue::new(line, field, message), "input: {source:?}");
    }
    let error = read_obj(b"mtllib bad.mtl\n", &mut |_| Some(b"newmtl paint\nNi nope\n".to_vec()))
        .expect_err("bad unused numeric material fields are still refused");
    assert_eq!(error, Issue::new(2, "Ni", "bad.mtl: 'nope' is not a number"), "MTL refusal keeps its own location");
    let error = read_mtl(b"newmtl paint\nPm nope\n").expect_err("the standalone MTL reader also refuses bad numbers");
    assert_eq!(error, Issue::new(2, "Pm", "'nope' is not a number"));

    let read = read_obj(b"mtllib missing.mtl\nv 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n", &mut |_| None)
        .expect("a missing library does not hide valid geometry");
    assert_eq!(read.mesh.triangle_count(), 1);
    assert_eq!(read.mesh.materials, vec![Material::named("default")], "faces without usemtl use default");
    assert_eq!(read.warnings, vec![Issue::new(1, "mtllib", "missing.mtl is not available; its materials are grey")]);

    let read = read_obj(b"# only a comment\n", &mut |_| None).expect("empty geometry is allowed");
    assert!(read.mesh.is_empty());
    assert_eq!(read.warnings, vec![Issue::new(0, "f", "the file has no faces")]);
}
