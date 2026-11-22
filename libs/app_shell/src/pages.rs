//! A product window's pages and its header: which pages the window shows,
//! which are built, the page flip that shows one, and the header's tabs.
//!
//! The window's template has a `PageFlip` named `product_pages` with one
//! template per page ([`ProductPage::id`]) and a header button per page
//! ([`ProductPage::tab`]). Only the first page is built before the first
//! frame; the others are built one per pump after it ([`Pages::next_to_build`]),
//! and a page opened before its turn is built when it is opened.
//!
//! Main-loop cost: a page build is the page's template, once; the rest is a
//! scan of a handful of pages.

use makepad_look::palette::Palette;
use makepad_widgets::*;

/// A page of a product window.
pub trait ProductPage: Copy + PartialEq + std::fmt::Debug + 'static {
    /// The page's template in the window's `product_pages`.
    fn id(self) -> LiveId;
    /// The page's tab in the window's header.
    fn tab(self) -> LiveId;
}

/// The pages a window shows, in the order they are built, and which are
/// built.
pub struct Pages<P: ProductPage> {
    order: Vec<P>,
    built: Vec<P>,
}

impl<P: ProductPage> Default for Pages<P> {
    fn default() -> Self {
        Self { order: Vec::new(), built: Vec::new() }
    }
}

impl<P: ProductPage> Pages<P> {
    /// The window's pages, in build order.
    pub fn new(order: Vec<P>) -> Self {
        Self { order, built: Vec::new() }
    }

    /// Whether the window shows `page`.
    pub fn has(&self, page: P) -> bool {
        self.order.contains(&page)
    }

    /// The window's pages, in build order.
    pub fn order(&self) -> &[P] {
        &self.order
    }

    pub fn is_built(&self, page: P) -> bool {
        self.built.contains(&page)
    }

    /// Every page of the window is built.
    pub fn all_built(&self) -> bool {
        self.order.iter().all(|page| self.is_built(*page))
    }

    /// The page the background build makes next.
    pub fn next_to_build(&self) -> Option<P> {
        self.order.iter().copied().find(|page| !self.is_built(*page))
    }

    /// Build `page`'s widgets, once. True when its template made them now
    /// (a page with no template stays unbuilt in the widgets but is not
    /// retried: nothing waits on widgets that cannot exist).
    pub fn ensure(&mut self, cx: &mut Cx, window: &WidgetRef, page: P) -> bool {
        if self.is_built(page) {
            return false;
        }
        let built = window.page_flip(cx, ids!(product_pages)).ensure_page(cx, page.id());
        self.built.push(page);
        built
    }

    /// Show `page` (built already).
    pub fn show(&self, cx: &mut Cx, window: &WidgetRef, page: P) {
        window.page_flip(cx, ids!(product_pages)).set_active_page(cx, page.id());
    }

    /// Hide the tabs of `all` that are not this window's pages.
    pub fn hide_other_tabs(&self, cx: &mut Cx, window: &WidgetRef, all: &[P]) {
        for page in all.iter().copied().filter(|page| !self.has(*page)) {
            window.button(cx, &[page.tab()]).set_visible(cx, false);
        }
    }
}

/// The header's look: the page tabs (`tabs`, the shown one in the accent)
/// and its toggle buttons (`toggles`, on ones in the accent).
pub fn paint_header(cx: &mut Cx, window: &WidgetRef, palette: &Palette, tabs: &[(LiveId, bool)], toggles: &[(LiveId, bool)]) {
    for (mode, list) in [(true, tabs), (false, toggles)] {
        for &(id, selected) in list {
            let face = Vec4f::from_u32(if selected { palette.accent } else if mode { palette.raised } else { palette.control });
            let hover = Vec4f::from_u32(if selected { palette.accent_hover } else if mode { palette.control_top } else { palette.control_hover });
            let down = Vec4f::from_u32(if selected { palette.accent_down } else if mode { palette.row } else { palette.control_down });
            let ink = Vec4f::from_u32(if selected { palette.on_accent } else if mode { palette.text_soft } else { palette.label });
            let ink_hover = Vec4f::from_u32(if selected { palette.on_accent } else if mode { palette.accent } else { palette.text_hi });
            let mut button = window.button(cx, &[id]);
            script_apply_eval!(cx, button, {
                draw_bg +: {
                    color: #(face) color_focus: #(face)
                    color_hover: #(hover) color_down: #(down)
                }
                draw_text +: {
                    color: #(ink) color_focus: #(ink)
                    color_hover: #(ink_hover) color_down: #(ink)
                }
            });
            if !mode {
                script_apply_eval!(cx, button, {
                    draw_icon +: {
                        color: #(ink) color_focus: #(ink)
                        color_hover: #(ink_hover) color_down: #(ink)
                    }
                });
            }
            button.redraw(cx);
        }
    }
}
