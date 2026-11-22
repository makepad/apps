//! Laser cut lines and back-print layers in millimetres, seen from the front.

use super::{layout, SegInk, WindowParams};
use crate::csg::V2;
use std::fmt::Write;

fn svg_path(out: &mut String, poly: &[V2], cx: f64, cy: f64) {
    for (i, q) in poly.iter().enumerate() {
        let (x, y) = (q[0] + cx, q[1] + cy);
        write!(out, "{}{x:.4} {:.4} ", if i == 0 { 'M' } else { 'L' }, -y).unwrap();
    }
    out.push_str("Z ");
}

/// SVG cut, print, diffuser and adhesive layers, with millimetre coordinates.
pub fn laser_svg(p: &WindowParams) -> String {
    let l = layout(p);
    let (mut x0, mut x1, mut y0, mut y1) = (1e9_f64, -1e9_f64, 1e9_f64, -1e9_f64);
    for q in &l.outline {
        x0 = x0.min(q[0]);
        x1 = x1.max(q[0]);
        y0 = y0.min(q[1]);
        y1 = y1.max(q[1]);
    }
    let m = 2.0;
    let (w, h) = (x1 - x0 + 2.0 * m, y1 - y0 + 2.0 * m);
    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    writeln!(out, "<!-- display window \"{}\": seen from the FRONT (mirror the print layer if your printer works on the back face).", p.name).unwrap();
    out.push_str("     Layers: cut (red hairline: outline and holes), print (black even-odd mask), ink (diffuser), adhesive (blue hairline). mm. -->\n");
    writeln!(out, "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w:.3}mm\" height=\"{h:.3}mm\" viewBox=\"{:.3} {:.3} {w:.3} {h:.3}\">", x0 - m, -y1 - m).unwrap();
    if p.print {
        out.push_str("  <g id=\"print\">\n    <path fill=\"#000000\" fill-rule=\"evenodd\" d=\"");
        svg_path(&mut out, &l.print_outer, 0.0, 0.0);
        for q in l.openings.iter().chain(&l.holes) {
            svg_path(&mut out, &q.poly, q.cx, q.cy);
        }
        out.push_str("\"/>\n");
        if !l.dots.is_empty() {
            out.push_str("    <path fill=\"#000000\" d=\"");
            for q in &l.dots {
                svg_path(&mut out, &q.poly, q.cx, q.cy);
            }
            out.push_str("\"/>\n");
        }
        out.push_str("  </g>\n");
        if p.seg_ink() == SegInk::Diffuser && !l.segs.is_empty() {
            out.push_str("  <g id=\"ink\">\n    <path fill=\"#f4f4f0\" stroke=\"#999999\" stroke-width=\"0.02\" d=\"");
            for q in &l.segs {
                svg_path(&mut out, &q.poly, q.cx, q.cy);
            }
            out.push_str("\"/>\n  </g>\n");
        }
    }
    if p.adhesive {
        out.push_str("  <g id=\"adhesive\">\n    <path fill=\"none\" stroke=\"#0060ff\" stroke-width=\"0.02\" d=\"");
        svg_path(&mut out, &l.adhesive_outer, 0.0, 0.0);
        svg_path(&mut out, &l.adhesive_inner, 0.0, 0.0);
        for q in &l.holes {
            svg_path(&mut out, &q.poly, q.cx, q.cy);
        }
        out.push_str("\"/>\n  </g>\n");
    }
    out.push_str("  <g id=\"cut\">\n    <path fill=\"none\" stroke=\"#ff0000\" stroke-width=\"0.01\" d=\"");
    svg_path(&mut out, &l.outline, 0.0, 0.0);
    for q in &l.holes {
        svg_path(&mut out, &q.poly, q.cx, q.cy);
    }
    out.push_str("\"/>\n  </g>\n</svg>\n");
    out
}

fn dxf_poly(out: &mut String, poly: &[V2], cx: f64, cy: f64, layer: &str) {
    write!(out, "0\nPOLYLINE\n8\n{layer}\n66\n1\n70\n1\n10\n0.0\n20\n0.0\n30\n0.0\n").unwrap();
    for q in poly {
        let (x, y) = (q[0] + cx, q[1] + cy);
        write!(out, "0\nVERTEX\n8\n{layer}\n10\n{x:.4}\n20\n{y:.4}\n30\n0.0\n").unwrap();
    }
    write!(out, "0\nSEQEND\n8\n{layer}\n").unwrap();
}

/// DXF R12 closed polylines on CUT, PRINT, INK and ADHESIVE layers.
pub fn laser_dxf(p: &WindowParams) -> String {
    let l = layout(p);
    let mut out = String::new();
    writeln!(out, "999\ndisplay window {}, seen from the front, mm", p.name).unwrap();
    out.push_str("0\nSECTION\n2\nHEADER\n9\n$ACADVER\n1\nAC1009\n9\n$INSUNITS\n70\n4\n0\nENDSEC\n");
    out.push_str("0\nSECTION\n2\nTABLES\n0\nTABLE\n2\nLAYER\n70\n4\n");
    for (layer, colour) in [("CUT", 1), ("PRINT", 7), ("INK", 8), ("ADHESIVE", 5)] {
        write!(out, "0\nLAYER\n2\n{layer}\n70\n0\n62\n{colour}\n6\nCONTINUOUS\n").unwrap();
    }
    out.push_str("0\nENDTAB\n0\nENDSEC\n0\nSECTION\n2\nENTITIES\n");
    dxf_poly(&mut out, &l.outline, 0.0, 0.0, "CUT");
    for q in &l.holes {
        dxf_poly(&mut out, &q.poly, q.cx, q.cy, "CUT");
    }
    if p.print {
        dxf_poly(&mut out, &l.print_outer, 0.0, 0.0, "PRINT");
        for q in l.openings.iter().chain(&l.dots) {
            dxf_poly(&mut out, &q.poly, q.cx, q.cy, "PRINT");
        }
        if p.seg_ink() == SegInk::Diffuser {
            for q in &l.segs {
                dxf_poly(&mut out, &q.poly, q.cx, q.cy, "INK");
            }
        }
    }
    if p.adhesive {
        dxf_poly(&mut out, &l.adhesive_outer, 0.0, 0.0, "ADHESIVE");
        dxf_poly(&mut out, &l.adhesive_inner, 0.0, 0.0, "ADHESIVE");
    }
    out.push_str("0\nENDSEC\n0\nEOF\n");
    out
}
