//! Where a bound graphic's run is at one moment: the run's time, and how
//! the recording's pixels map into the graphic's own coordinates. The edit
//! decides both (the clip playing the run gives the time, its fit and any
//! Reframe crop give the space), so an overlay stays on the widget it
//! follows under a moving crop.

use super::run::Rect;

/// One moment of a binding.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BindFrame {
    /// Seconds of the recording shown at this moment.
    pub run_time: f64,
    /// The part of the recording that shows, in source pixels (a Reframe's
    /// crop, the part a `@fill` fit keeps, or the whole frame).
    pub crop: Rect,
    /// Where `crop` lands, in the graphic's coordinates.
    pub dst: Rect,
}

impl BindFrame {
    /// The recording shown whole and 1:1: a document the size of the run
    /// placed over the full frame (a motion document previewed on its own).
    pub fn identity(run_time: f64, size: (f64, f64)) -> BindFrame {
        let r = Rect::new(0.0, 0.0, size.0, size.1);
        BindFrame { run_time, crop: r, dst: r }
    }

    /// Between two frames (`u` 0..1), for sub-frame times.
    pub fn lerp(&self, to: &BindFrame, u: f64) -> BindFrame {
        BindFrame { run_time: self.run_time + (to.run_time - self.run_time) * u, crop: self.crop.lerp(&to.crop, u), dst: self.dst.lerp(&to.dst, u) }
    }

    /// Graphic units per source pixel.
    pub fn scale(&self) -> (f64, f64) {
        (self.dst.w / self.crop.w.max(1e-9), self.dst.h / self.crop.h.max(1e-9))
    }

    /// A point of the recording in the graphic's coordinates.
    pub fn point(&self, p: (f64, f64)) -> (f64, f64) {
        let (sx, sy) = self.scale();
        (self.dst.x + (p.0 - self.crop.x) * sx, self.dst.y + (p.1 - self.crop.y) * sy)
    }

    /// A rect of the recording in the graphic's coordinates.
    pub fn rect(&self, r: &Rect) -> Rect {
        let (sx, sy) = self.scale();
        let (x, y) = self.point((r.x, r.y));
        Rect::new(x, y, r.w * sx, r.h * sy)
    }

    /// How much of `r` (source pixels) the crop shows, 0..1 of its area
    /// (a zero-sized rect: 1 inside, 0 outside).
    pub fn shown(&self, r: &Rect) -> f64 {
        let c = &self.crop;
        let (x0, y0) = (r.x.max(c.x), r.y.max(c.y));
        let (x1, y1) = (r.right().min(c.right()), r.bottom().min(c.bottom()));
        if r.w <= 0.0 || r.h <= 0.0 {
            return if r.x >= c.x && r.x <= c.right() && r.y >= c.y && r.y <= c.bottom() { 1.0 } else { 0.0 };
        }
        ((x1 - x0).max(0.0) * (y1 - y0).max(0.0)) / (r.w * r.h)
    }

    /// This frame seen through an outer placement: the graphic's
    /// coordinates `from` land on `to` (e.g. a document drawn with a fit
    /// into the output; the inverse of that fit takes output pixels into
    /// the document).
    pub fn then(&self, from: &Rect, to: &Rect) -> BindFrame {
        let (sx, sy) = (to.w / from.w.max(1e-9), to.h / from.h.max(1e-9));
        let dst = Rect::new(to.x + (self.dst.x - from.x) * sx, to.y + (self.dst.y - from.y) * sy, self.dst.w * sx, self.dst.h * sy);
        BindFrame { dst, ..*self }
    }
}
