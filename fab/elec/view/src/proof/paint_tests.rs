use fab_elec_core::{LayerId, PolySet};
use super::{layer_insts, role_ink};
use crate::canvas::looks::{Look, DARK, LIGHT};
use crate::proof::inst::Inst;
use crate::proof::scene::{Item, Layer, Prim, Role};
use fab_elec_core::Side;
use std::f32::consts::TAU;

#[test]
fn layer_paint_preserves_geometry_identity_and_role_style() {
    for (theme, base_tokens) in [("dark", DARK), ("light", LIGHT)] {
        let roles = [
            Role::Copper { side: Some(Side::Top), inner: 0 },
            Role::Copper { side: Some(Side::Bottom), inner: 0 },
            Role::Copper { side: None, inner: 2 },
            Role::Mask(Side::Top),
            Role::Paste(Side::Top),
            Role::Silk(Side::Top),
            Role::Edge,
            Role::Other,
        ];
        let mut colours = Vec::new();
        for role in roles {
            let colour = role_ink(role, &base_tokens);
            if !colours.contains(&colour) {
                colours.push(colour);
            }
        }
        // The specified mapping shares ink_dim between Paste and Other.
        assert_eq!(colours.len(), 7, "role palette: seven distinct colours for {theme}");
        assert_eq!(
            role_ink(Role::Paste(Side::Top), &base_tokens),
            role_ink(Role::Other, &base_tokens),
            "role palette: Paste and Other share ink_dim for {theme}"
        );

        for token_alpha in [1.0, 0.5] {
            let mut tokens = base_tokens;
            for colour in [
                &mut tokens.wire_power,
                &mut tokens.hover,
                &mut tokens.wire_ground,
                &mut tokens.ok,
                &mut tokens.ink_dim,
                &mut tokens.ink,
                &mut tokens.warning,
            ] {
                colour[3] = token_alpha;
            }
            let alpha = 0.4;
            let ink_alpha = token_alpha * alpha;
            let bottom_alpha = ink_alpha * 0.85;
            let cases = [
                (roles[0], tokens.wire_power, ink_alpha, ink_alpha, ink_alpha, [0.0, 0.0]),
                (roles[1], tokens.hover, bottom_alpha, bottom_alpha, bottom_alpha, [0.0, 0.0]),
                (roles[2], tokens.wire_ground, ink_alpha, ink_alpha, ink_alpha, [0.0, 0.0]),
                (roles[3], tokens.ok, ink_alpha, ink_alpha * 0.25, ink_alpha * 0.25, [1.0, 0.0]),
                (roles[4], tokens.ink_dim, ink_alpha, ink_alpha, ink_alpha * 0.55, [0.0, 0.0]),
                (roles[5], tokens.ink, ink_alpha, 1.0, 1.0, [0.0, 0.0]),
                (roles[6], tokens.warning, ink_alpha, 0.0, 0.0, [1.0, 0.0]),
                (roles[7], tokens.ink_dim, ink_alpha, ink_alpha, ink_alpha, [0.0, 0.0]),
                (Role::Mask(Side::Bottom), tokens.ok, ink_alpha, ink_alpha * 0.25, ink_alpha * 0.25, [1.0, 0.0]),
                (Role::Paste(Side::Bottom), tokens.ink_dim, ink_alpha, ink_alpha, ink_alpha * 0.55, [0.0, 0.0]),
                (Role::Silk(Side::Bottom), tokens.ink, ink_alpha, 1.0, 1.0, [0.0, 0.0]),
            ];
            for (role, colour, expected_alpha, disc_alpha, box_alpha, stroke) in cases {
                assert_eq!(role_ink(role, &tokens), colour, "role ink token: {theme} {role:?}");
                let layer = Layer {
                    file: String::new(),
                    label: String::new(),
                    role,
                    items: vec![
                        Item {
                            id: 41,
                            prim: Prim::Line { a: [-1.0, 2.0], b: [3.0, -4.0], w: 0.3 },
                            attrs: None,
                        },
                        Item {
                            id: 7,
                            prim: Prim::Arc { c: [5.0, -6.0], r: 2.5, start: 0.7, sweep: -1.2, w: 0.4 },
                            attrs: None,
                        },
                        Item { id: 99, prim: Prim::Disc { c: [-7.0, 8.0], r: 1.5 }, attrs: None },
                        Item {
                            id: 3,
                            prim: Prim::Box { c: [9.0, -10.0], half: [1.0, 2.0], rot: 0.3, corner: 0.2 },
                            attrs: None,
                        },
                    ],
                    attrs: Vec::new(),
                    id: LayerId::F_CU,
                    fills: PolySet::new(),
                };
                for look in [Look::Committed, Look::Preview, Look::Ghost] {
                    let insts = layer_insts(&layer, look, alpha, &tokens);
                    assert_eq!(insts.len(), layer.items.len(), "one instance per item: {theme} {role:?}");
                    let [Inst::Line(line), Inst::Arc(arc), Inst::Arc(disc), Inst::Box(rect)] = insts.as_slice() else {
                        panic!("instance kinds and item order: {theme} {role:?}");
                    };
                    for (item, inst) in layer.items.iter().zip(&insts) {
                        let common = inst.common();
                        assert_eq!(common.item, item.id as f32, "item identity and order: {theme} {role:?}");
                        assert_eq!(common.look, look.code(), "look code: {theme} {role:?}");
                        assert_eq!(&common.ink[..3], &colour[..3], "straight RGB ink: {theme} {role:?}");
                        assert_eq!(common.ink[3], expected_alpha, "token and layer alpha, bottom copper 0.85: {theme} {role:?}");
                        let pose = if item.id == 3 { [9.0, -10.0, 0.3, 1.0] } else { [0.0, 0.0, 0.0, 1.0] };
                        assert_eq!(common.pose_a, pose, "start pose, box centre and rotation: {theme} {role:?}");
                        assert_eq!(common.pose_b, pose, "end pose, box centre and rotation: {theme} {role:?}");
                        assert_eq!(common.span, [0.0; 2], "resting pose: {theme} {role:?}");
                        assert_eq!(common.live_ix, [-1.0, -1.0, -1.0, 0.0], "no live overlay: {theme} {role:?}");
                    }
                    assert_eq!(line.seg, [-1.0, 2.0, 3.0, -4.0], "line world millimetres, Y up: {theme} {role:?}");
                    assert_eq!(line.width, [1.0, 0.3], "line screen and world width: {theme} {role:?}");
                    assert_eq!(line.cap, 0.0, "round line caps: {theme} {role:?}");
                    assert_eq!(line.dash, [0.0; 2], "solid line: {theme} {role:?}");
                    assert_eq!(line.along0, 0.0, "line default path origin: {theme} {role:?}");
                    assert_eq!(arc.circle, [5.0, -6.0, 2.5, 0.0], "arc world centre and radius: {theme} {role:?}");
                    assert_eq!(arc.sweep, [0.7, -1.2], "arc start and signed sweep: {theme} {role:?}");
                    assert_eq!(arc.stroke, [1.0, 0.4], "arc screen and world stroke: {theme} {role:?}");
                    assert_eq!(arc.fill, [0.0; 4], "arc stroke only: {theme} {role:?}");
                    assert_eq!(arc.along0, 0.0, "arc default path origin: {theme} {role:?}");
                    assert_eq!(disc.circle, [-7.0, 8.0, 1.5, 0.0], "disc world centre and radius: {theme} {role:?}");
                    assert_eq!(disc.sweep, [0.0, TAU], "disc full circle: {theme} {role:?}");
                    assert_eq!(disc.along0, 0.0, "disc default path origin: {theme} {role:?}");
                    assert_eq!(disc.stroke, stroke, "disc mask or edge outline: {theme} {role:?}");
                    assert_eq!(rect.rect, [0.0, 0.0, 1.0, 2.0], "box local centre and half sizes: {theme} {role:?}");
                    assert_eq!(rect.corner, 0.2, "box corner radius: {theme} {role:?}");
                    assert_eq!(rect.stroke, stroke, "box mask or edge outline: {theme} {role:?}");
                    let mut disc_fill = colour;
                    let mut box_fill = colour;
                    disc_fill[3] = disc_alpha;
                    box_fill[3] = box_alpha;
                    if role == Role::Edge {
                        disc_fill = [0.0; 4];
                        box_fill = [0.0; 4];
                    }
                    assert_eq!(disc.fill, disc_fill, "disc fill: mask 0.25, silk 1, edge clear: {theme} {role:?}");
                    assert_eq!(rect.fill, box_fill, "box fill: mask 0.25, paste 0.55, silk 1, edge clear: {theme} {role:?}");
                }
                let empty = Layer { items: Vec::new(), ..layer };
                assert!(
                    layer_insts(&empty, Look::Committed, alpha, &tokens).is_empty(),
                    "empty layer has no instances: {theme} {role:?}"
                );
            }
        }
    }
}
