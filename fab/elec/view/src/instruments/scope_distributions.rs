//! Distribution pickup and drawing, separate from the answer controls.

use super::*;
use crate::instruments::distribution_view::{same_bins, AnswerDistribution};

impl SignalLane {
    pub(super) fn distribution_pins_changed(&mut self, cx: &mut Cx) {
        self.distributions.retain(|(q, _)| self.pins.contains(q));
        self.distribution_ranges.retain(|(q, _)| self.pins.contains(q));
        self.distribution_plots.retain(|(q, _)| self.pins.contains(q));
        self.answers_dirty = true;
        self.answers_at = None;
        self.answers_layout_key = None;
        self.answers_popover_key = None;
        self.area.redraw(cx);
    }

    pub fn set_distributions(&mut self, cx: &mut Cx, distributions: &[(TrendQuantity, Distribution)]) {
        // Totals change on appended events; comparing the grid also catches a reset that
        // completed the same number of events before the UI picked up its next snapshot.
        if self.distributions == distributions { return; }
        let mut changed = self.distributions.len() != distributions.len();
        self.distribution_plots.retain(|(quantity, _)| distributions.iter().any(|(q, _)| q == quantity));
        for (quantity, distribution) in distributions {
            if self.distributions.iter().any(|(q, old)| q == quantity && same_bins(old, distribution)) { continue; }
            changed = true;
            if let Some((_, plot)) = self.distribution_plots.iter_mut().find(|(q, _)| q == quantity) { plot.set(cx, distribution); }
            else { self.distribution_plots.push((*quantity, Histogram::new(cx, distribution))); }
        }
        self.distributions = distributions.to_vec();
        if changed { self.answers_popover_key = None; self.area.redraw(cx); }
    }

    pub(in crate::instruments) fn set_distribution_ranges(&mut self, cx: &mut Cx, ranges: Vec<(TrendQuantity, (f32, f32))>) {
        if self.distribution_ranges == ranges { return; }
        self.distribution_ranges = ranges;
        self.answers_popover_key = None;
        self.area.redraw(cx);
    }

    pub(super) fn pinned_answer(&self, index: usize) -> Option<TrendQuantity> {
        self.answer_names.get(index).copied().flatten().and_then(TrendQuantity::from_name).filter(|q| self.pins.contains(q))
    }

    pub(super) fn distribution_space(&self, index: usize) -> f64 { if self.pinned_answer(index).is_some() { 32.0 } else { 0.0 } }

    pub(super) fn draw_answer_distribution(&mut self, cx: &mut Cx2d, index: usize, pos: DVec2) -> f64 {
        let Some(quantity) = self.pinned_answer(index) else { return 0.0 };
        let mut color = self.channel_color(0);
        color[3] *= self.answer_alpha(index);
        if let Some((_, plot)) = self.distribution_plots.iter_mut().find(|(q, _)| *q == quantity) {
            plot.draw(cx, &mut self.draw_shape, Rect { pos: pos + dvec2(4.0, 1.0), size: dvec2(28.0, 12.0) }, color);
        }
        32.0
    }

    fn distribution_unit(&self, quantity: TrendQuantity) -> &str {
        match quantity {
            TrendQuantity::Freq => "Hz", TrendQuantity::Duty => "%", TrendQuantity::Width => "s",
            TrendQuantity::Vpp | TrendQuantity::Mean => self.settings.channels.first().map_or("V", |channel| channel.unit.as_str()),
        }
    }

    pub(super) fn update_answer_distributions(&self, cx: &mut Cx, rows: &[WidgetRef]) {
        let look = &InstrumentLook::for_theme(self.dark).on_panel();
        for (i, row) in rows.iter().enumerate() {
            let widget = row.widget(cx, ids!(distribution));
            let quantity = self.pinned_answer(i);
            widget.set_visible(cx, quantity.is_some());
            let Some(quantity) = quantity else { continue };
            let distribution = self.distributions.iter().find(|(q, _)| *q == quantity).map(|(_, d)| d).cloned().unwrap_or_default();
            let unit = self.distribution_unit(quantity);
            let labels = self.distribution_ranges.iter().find(|(q, _)| *q == quantity)
                .map_or_else(|| (String::new(), String::new()), |(_, (lo, hi))| (si_label(f64::from(*lo), unit), si_label(f64::from(*hi), unit)));
            let current = self.measurements.iter().find(|m| m.lane == 0 && (m.name == quantity.name() || quantity == TrendQuantity::Vpp && m.name == "Ipp")).map(|m| m.value);
            if let Some(mut histogram) = widget.borrow_mut::<AnswerDistribution>() {
                histogram.set(cx, &distribution, labels, current, self.channel_color(0), look.text_tertiary);
            };
        }
    }

    pub(super) fn distribution_summary(&self, index: usize) -> Option<String> {
        let quantity = self.pinned_answer(index)?;
        let distribution = &self.distributions.iter().find(|(q, _)| *q == quantity)?.1;
        let digits = distribution.total.to_string();
        let mut text = String::from("n = ");
        for (i, c) in digits.chars().enumerate() { if i > 0 && (digits.len() - i) % 3 == 0 { text.push(' '); } text.push(c); }
        if let Some((_, (lo, hi))) = self.distribution_ranges.iter().find(|(q, _)| *q == quantity) {
            let unit = self.distribution_unit(quantity);
            text.push_str(&format!(", {} to {}", si_label(f64::from(*lo), unit), si_label(f64::from(*hi), unit)));
        }
        if distribution.nonfinite > 0 { text.push_str(&format!(", {} nonfinite", distribution.nonfinite)); }
        Some(text)
    }
}
