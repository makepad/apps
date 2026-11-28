//! Build a shared app's palette and its scalar theme installation from the panel controls.
use super::combinations::{COMBINATIONS, PAIRS};
use super::lab::Readability;
use crate::tweak::{ThemeInstall, ThemeVal};
use makepad_widgets::{live_id, LiveId, WidgetRef, Rect, DVec2, dvec2};
use makepad_widgets::theme_tokens::{
    contrast, family_from_color, held_pairs, hsl_to_rgb, over, reads_on, rgb_to_hsl, roles_for, roles_from_colors,
    token_spec, ColorRoles, Harmony, RoleSource, Scheme, SeedColors, DERIVED_ROLES, KEPT_ERROR, KEPT_WARNING, LEGIBLE, READABLE,
};
use std::collections::BTreeMap;

/// Where the lightness slider's travel ends, as CIE lightness (`L*`, nought
/// for black to a hundred for white): the darkest page a dark theme is
/// offered and the two ends of a light one.
///
/// The LIGHTEST dark page is not here because it is not a choice: it is the
/// house dark page itself (`L*` 32.3), and [`dark_lightest`] reads it off the
/// file. The dark theme's inks are white and its brightest rung,
/// `color_surface_bright`, is the page carried a sixth of the way to white,
/// so pure white body text already stands only 4.54 off that rung on the
/// house page -- a hair over the 4.5 it is held to. One step lighter and no
/// ink the dark base theme can draw reads there.
///
/// The darkest LIGHT page is where the same thing happens from the other
/// side: pure black on the light theme's darkest rung, `color_surface_dim`,
/// which is the page carried a quarter of the way to black. A grey page
/// clears 4.5 from about `L*` 66.4, and a fully saturated page of the same
/// luminance needs up to about 67.6 depending on its hue, so the light end
/// starts at 68. Between 32.3 and 68 there is no page either base theme can
/// write readable text on, and the slider does not offer one: see
/// [`BuilderParams::lightness`].
///
/// The outer ends stop short of black and white because a page at either has
/// no room left in it for colour, and the saturation slider would do nothing
/// there at all.
const DARK_DARKEST: f64 = 8.0;
const LIGHT_DARKEST: f64 = 68.0;
const LIGHT_LIGHTEST: f64 = 98.0;

/// How much colour the rule gives the background of a palette it grows, as
/// the bounds on an HSL saturation taken from the favourite. The background
/// is the colour the saturation slider brings the page up to at its top, so
/// a rule that handed out a nearly grey one would leave the slider with
/// nothing to do; and a favourite that is already a highlighter pen does not
/// make its page one.
const RULE_GROUND_COLOUR: (f64, f64) = (0.45, 0.85);

/// What an ACCENT owes the page it is laid on: not that anything can be read
/// off it, only that it can be TOLD APART from the page.
///
/// An accent is a FILL. It is the ground of a button, the body of a value
/// bar, the disc of a mark -- and whatever words land on top of it answer to
/// their own bars, `LEGIBLE` and `READABLE`, which the WRITTEN table holds
/// them to ground by ground. Nothing is read off the accent itself. So the
/// one thing that would go wrong if the page came too near is that the shape
/// would disappear into it, and the bar for that is the bar for two colours
/// being two colours: about a half again in luminance, which is what the eye
/// needs to call an edge an edge. Under about 1.2 a fill on a page is the
/// page; 1.5 is clear of that with room to spare, and well under `LEGIBLE`.
///
/// Holding an accent to `LEGIBLE` instead is what made a BRIGHT pick
/// impossible to have a light theme with. The house colour sits at relative
/// luminance 0.292, and the lightest page there is only stands 3:1 off a
/// colour under about 0.28, so every page in the light run was refused and
/// the palette flipped to dark whatever the lightness slider said -- a
/// slider overruled for the sake of a bar that was never this one's to
/// answer. At 1.5 the same colour clears every light page from about `L*`
/// 74 up, and the slider decides again.
const TOLD_APART: f64 = 1.5;
/// The four globals the dimension sliders drive, with the field of
/// [`BuilderParams`] each one is.
const DIMENSIONS: [&str; 4] = ["space_factor", "corner_radius", "font_size_base", "font_size_contrast"];

/// How far a theme file mixes its page from one end toward the other, as
/// `(color_bg_app, color_fg_app)`: the dark file from black toward white, the
/// light file from white toward black. Each is the base of a `pow` whose
/// exponent is `color_contrast`. A second copy of four numbers the theme
/// files own, kept because a reading with no VM in it has to know where the
/// page lands; `the_grounds_are_mixed_the_way_the_theme_files_mix_them` holds
/// the copy to the files' text, and
/// `what_build_predicts_is_what_the_vm_resolves` holds the answer to the VM.
const DARK_GROUNDS: (f64, f64) = (0.3, 0.36);
const LIGHT_GROUNDS: (f64, f64) = (0.15, 0.175);

/// The two rungs of the opaque ladder the inverse page is made of, as the
/// amount `color_fg_app` is mixed toward white and toward black. Held to the
/// files by the same test.
const OPAQUE_U_6: f64 = 0.8;
const OPAQUE_D_5: f64 = 0.75;

const WHITE: u32 = 0xFFFFFFFF;
const BLACK: u32 = 0x000000FF;

/// What a panel's controls say, in the units the controls are in.
///
/// Nothing here is a token. The favourite colour is a colour; the two
/// background sliders run nought to one; the text contrast is a contrast
/// ratio; the four dimensions are in the units of the globals they drive,
/// because a person dragging "font size" wants to read 11 and not 0.37.
///
/// # Four colours, and which slider moves which
///
/// A palette is four colours: the primary, the secondary, the tertiary and
/// the BACKGROUND colour ([`BuilderParams::palette`]). The first three are
/// the accents, and no slider touches them. The fourth is what the page and
/// every ground stepped off it are coloured with, and the two background
/// sliders are about it and about nothing else:
///
/// * [`saturation`](BuilderParams::saturation) is how much of the
///   background colour's own saturation the grounds take -- all of it at the
///   top, which is the default, and none at nought, where the page is the
///   GREY of the same lightness. It never goes past the palette's own
///   colour, and it moves chroma alone.
/// * [`lightness`](BuilderParams::lightness) is how light or dark the page
///   is ASKED to be, and the page's lightness is the only thing that decides
///   whether the theme is a dark one or a light one at all. Where a colour
///   somebody picked cannot be told from the page there, the page walks to
///   the nearest lightness it can be -- across the middle if that is where
///   the room is, and the theme changes appearance with it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BuilderParams {
    /// The one colour somebody picked, `0xRRGGBBAA`, alpha ignored. Every
    /// suggestion is grown from it and a colour control shows it. It is the
    /// primary itself unless a mood suggestion has named a calmer primary in
    /// [`seeds`](BuilderParams::seeds) -- and choosing that suggestion does
    /// not move this, so the colour the person picked is never lost to a
    /// chip they were only trying out.
    pub favourite: u32,
    /// Where the secondary and tertiary sit round the hue circle from the
    /// favourite, for a palette the rule grows. A suggestion carries one; the
    /// panel no longer offers it as a control of its own, since the six
    /// harmonies are the first six suggestions.
    pub harmony: Harmony,
    /// The four colours a [`Suggestion`] named, or [`BuilderParams::with_palette`]
    /// named by hand, outright: the primary as it will be, the two
    /// companions, and the background colour.
    ///
    /// `None` is the default and means the palette is the plain one the
    /// harmony grows from the favourite. A seed named outright beats the
    /// harmony, so choosing a harmony by hand has to let these go or it
    /// would do nothing at all; [`BuilderParams::with_harmony`] is that move.
    pub seeds: Option<SuggestionSeeds>,
    /// How much of the background colour's own saturation the page and the
    /// controls' grounds carry, nought to one. One is the default: the first
    /// palette a person presses puts all four of its colours on the app at
    /// once, and this is how they take the colour back out of the grounds.
    /// Nought is a page with no colour in it at exactly the lightness it had.
    pub saturation: f64,
    /// How light the page is, nought (the darkest dark page) to one (the
    /// lightest light one), even to the eye along its whole travel.
    ///
    /// The lower half is the dark theme and the upper half the light one:
    /// nought to a half runs the page from `L*` 8 up to the house dark page,
    /// which is as light as a page carrying the dark theme's white text can
    /// be; just past a half it is `L*` 68, the darkest page the light
    /// theme's black text reads on, and one is `L*` 98. The band between
    /// (`L*` 32.3 to 68) is a page neither theme can write on, so the slider
    /// steps across it rather than offering a theme that does not read -- and
    /// the appearance asked for, [`BuilderParams::dark`], flips there and
    /// nowhere else. A half is the house dark page exactly; the house light
    /// page is where `BuilderParams::house(false)` says.
    ///
    /// It is what the page is ASKED for and not always what it comes to: a
    /// page that cannot be told from a colour somebody picked walks along
    /// this same axis until it can, and the axis runs the whole way, both
    /// halves of it. See [`BuilderParams::built_dark`] for the appearance the
    /// page lands in, which is the one the theme wears.
    pub lightness: f64,
    /// The contrast ratio the body text holds against the page it is written
    /// on, from `READABLE` (4.5) up to what pure white or pure black would
    /// give on that page. The house value is where the house theme's body
    /// text already sits, so leaving this alone leaves the text alone.
    ///
    /// It moves the text inks and nothing else, and the quieter voices --
    /// the placeholder, the meta line, the disabled label, the variant ink --
    /// keep their distance from the body ink rather than all landing on one
    /// number. It is not `font_contrast`, which is the step between type
    /// sizes, and it is not the theme's `color_contrast`, which moves every
    /// rung of the grey ladder, fills and bevels included.
    pub text_contrast: f64,
    /// `space_factor`: the unit every space rung and control height is a
    /// multiple of.
    pub spacing: f64,
    /// `corner_radius`.
    pub roundness: f64,
    /// `font_size_base`: the paragraph size.
    pub font_size: f64,
    /// `font_size_contrast`: the step between neighbouring sizes.
    pub font_contrast: f64,
    /// Which of the palette's four the words take their hue from, or `None`
    /// for text with no colour in it, which is the default and is every
    /// theme this builder made before there was a choice.
    ///
    /// Text is not a fifth colour, and it could not be one: whatever colour
    /// was picked for it, its lightness would have to be moved until it read
    /// on the page, so only its hue and its strength were ever free. Those
    /// are what this and [`text_tint`](BuilderParams::text_tint) are -- the
    /// hue comes off a colour the palette already has, and the strength is
    /// the slider's.
    ///
    /// It names a SLOT and not a colour, as the seed picker does: a colour
    /// dragged or edited into that slot retints the words, because the slot
    /// is what was chosen.
    pub text_color: Option<SeedSlot>,
    /// How much of that colour the words take, nought to one: nought is the
    /// text as it is without a colour, one is as coloured as the ink can be
    /// and still stand where the text contrast puts it.
    ///
    /// The strength is the slider's own and not the slot colour's, so a
    /// quiet surface colour tints its text as plainly as a loud primary --
    /// what the slot is asked for is a hue. A slot with no colour in it has
    /// no hue to give and leaves the words alone.
    pub text_tint: f64,
    /// How many of the four colours are chosen, one to four; four is the
    /// default and is the builder as it always was.
    ///
    /// The ones not chosen are derived from the primary, never asked for:
    /// at three the tertiary, at two the secondary and the tertiary, and at
    /// one the surface as well, which is then a grey (see
    /// [`BuilderParams::palette`]). They are derived rather than dropped,
    /// because every token the mapping pins still wants a family, and they
    /// are derived rather than stored, so the colours a person chose before
    /// stay in [`seeds`](BuilderParams::seeds) under them and come back the
    /// moment the count goes up again.
    pub color_count: usize,
    /// Which of the four the person last touched: the slot the carousel is
    /// grown from, and the one colour the page HAS to be able to be told
    /// from.
    ///
    /// It matters only where the palette pulls the page two ways at once --
    /// a near-black accent and a near-white one, which want opposite ends of
    /// the lightness axis. Somewhere has to give, and it gives at the colour
    /// the person is not working on: the seed is the one they have in hand.
    /// Everywhere else the page clears all of them and this changes nothing.
    ///
    /// The primary by default, which is the slot a palette is grown from
    /// until somebody says otherwise. A slot the count derives is nobody's
    /// pick and falls back to the primary.
    pub seed_slot: SeedSlot,
}

/// The counts a palette can have, fewest first.
pub const COLOR_COUNTS: [usize; 4] = [1, 2, 3, 4];

/// How far a colour derived from the primary stands from it, as the contrast
/// ratio between the two. Far enough that a tick on its ground or a caret by
/// a filled button is a second colour and not the first one again, and near
/// enough that it is still plainly the same colour a shade on.
const TONAL_CONTRAST: f64 = 1.6;

/// The lightnesses a tonal step may land on. Past these a colour is nearly
/// black or white and its hue is gone, which is the one thing a step of the
/// primary has to keep.
const TONAL_RANGE: (f64, f64) = (0.12, 0.88);

impl BuilderParams {
    /// The theme the library ships, in one appearance: the house colour in
    /// the house harmony with nothing named, the page at the house lightness
    /// and the text at the house contrast -- both read off the file -- and
    /// the four dimensions as the base theme's own file has them. Read, not
    /// remembered, so a change to a theme file moves the builder's idea of
    /// "untouched" with it.
    ///
    /// The saturation is at its top, where a palette puts all four of its
    /// colours on. The house palette's background colour has no colour in
    /// it, so at the house settings that colours nothing.
    pub fn house(dark: bool) -> Self {
        let scheme = if dark { Scheme::Dark } else { Scheme::Light };
        let of = |key: &str, otherwise: f64| file_number(scheme, key).unwrap_or(otherwise);
        Self {
            favourite: SeedColors::HOUSE.primary,
            harmony: Harmony::House,
            seeds: None,
            saturation: 1.0,
            lightness: house_lightness(dark),
            text_contrast: house_text_contrast(scheme),
            spacing: of("space_factor", 6.0),
            roundness: of("corner_radius", 2.5),
            font_size: of("font_size_base", 10.0),
            font_contrast: of("font_size_contrast", 2.5),
            text_color: None,
            text_tint: 0.0,
            color_count: 4,
            seed_slot: SeedSlot::Primary,
        }
    }

    /// Whether the page is ASKED for on the dark side: which half of the
    /// lightness slider it is in, and nothing else.
    ///
    /// This is the appearance the palette is grown for -- the companions a
    /// harmony works out, the steps a count derives, the chips the row shows
    /// -- so that pressing a chip puts on the colours the chip showed. What
    /// the built theme actually wears is [`BuilderParams::built_dark`], which
    /// is where the page ends up after it has moved clear of the accents, and
    /// the two differ exactly when a pick sends the page across the middle.
    pub fn dark(&self) -> bool {
        !(self.lightness > 0.5)
    }

    /// The appearance asked for, as a scheme. See [`BuilderParams::dark`].
    pub fn scheme(&self) -> Scheme {
        if self.dark() {
            Scheme::Dark
        } else {
            Scheme::Light
        }
    }


    /// The base theme a build derives from: the appearance of the page these
    /// settings come to. See [`BuilderParams::built_dark`].
    pub fn built_scheme(&self) -> Scheme {
        page_scheme_and_crowding(self).1
    }

    /// Every number brought inside what it can mean: the two background
    /// sliders into nought to one, the text contrast into what the page
    /// allows, and each dimension into the range its global is registered
    /// with in `THEME_TOKENS`, which is the range the token's own control
    /// offers. [`build`] does this for itself; it is public so that a panel
    /// can show the number that will actually be used.
    pub fn clamped(self) -> Self {
        let within = |key: &str, value: f64| match token_spec(key) {
            Some(spec) if spec.max > spec.min => value.clamp(spec.min, spec.max),
            _ => value,
        };
        let unit = |value: f64, lost: f64| if value.is_finite() { value.clamp(0.0, 1.0) } else { lost };
        let count = self.color_count.clamp(1, 4);
        let mut out = Self {
            saturation: unit(self.saturation, 1.0),
            lightness: unit(self.lightness, house_lightness(true)),
            spacing: within("space_factor", self.spacing),
            roundness: within("corner_radius", self.roundness),
            font_size: within("font_size_base", self.font_size),
            font_contrast: within("font_size_contrast", self.font_contrast),
            text_tint: unit(self.text_tint, 0.0),
            color_count: count,
            // A slot this count derives is nobody's pick, so it cannot be the
            // one the page has to clear: the primary is.
            seed_slot: if self.seed_slot.chosen_at(count) { self.seed_slot } else { SeedSlot::Primary },
            ..self
        };
        let (least, most) = out.text_contrast_range();
        out.text_contrast = if self.text_contrast.is_finite() {
            self.text_contrast.clamp(least, most)
        } else {
            house_text_contrast(out.built_scheme()).clamp(least, most)
        };
        out
    }

    /// The range the text contrast can be set in on the page these settings
    /// make: `READABLE` at the bottom, and at the top what the plain end of
    /// the appearance -- white on a dark page, black on a light one -- stands
    /// off it. A panel draws its track over this.
    ///
    /// The appearance is the one the page LANDS in, since that is the page
    /// the words will be written on.
    pub fn text_contrast_range(&self) -> (f64, f64) {
        let (page, scheme, _) = page_scheme_and_crowding(self);
        let most = reads_on(page, plain_ink(scheme));
        (READABLE, most.max(READABLE))
    }

    /// Whether naming `slot` as the [`text_color`](BuilderParams::text_color)
    /// would colour the words at all on these settings: whether the slot's
    /// colour in force clears [`HAS_HUE`], the bar the rest of the builder
    /// uses for a colour that has a hue. Below it the slot is a grey, and a
    /// grey has no hue to give -- the words stay plain whatever the tint.
    pub fn gives_the_words_a_hue(&self, slot: SeedSlot) -> bool {
        text_hue_of(self, slot).is_some()
    }



    /// The four colours in force: the primary as it will be, the secondary,
    /// the tertiary, and the background colour -- which is the fourth colour
    /// at its own saturation and at the appearance's house page lightness,
    /// the square a palette chip shows.
    ///
    /// For a suggestion's settings this is that suggestion's
    /// [`colors`](Suggestion::colors), exactly. The built theme's
    /// `color_primary`, `color_secondary` and `color_tertiary` are the first
    /// three to within what reading forced, and its page wears the fourth's
    /// hue.
    ///
    /// The untouched house settings are no exception. They are the house SEED
    /// -- the favourite `color_makepad`, the companions the house harmony
    /// turns out of it, and the house page as the fourth, because the house
    /// seed names no neutral and a seed with no neutral leaves the page the
    /// theme file's own grey. It used to hand back the ROLE BASES the theme
    /// files carry instead, and those are a different set of colours: the
    /// primary square drew the base the rule built while every chip in the
    /// row beside it drew the favourite, which is the one thing a palette
    /// square must never do. What the built theme wears is
    /// [`BuiltTheme::color`] on the theme this builds, and that is still the
    /// roles: untouched, nothing at all is installed and the files stand.
    ///
    /// Under four colours ([`BuilderParams::color_count`]) the ones not
    /// chosen are the ones [`derived_palette`] makes of the primary, and
    /// this is what the theme is built from; [`BuilderParams::chosen_palette`]
    /// is what stands under them.
    pub fn palette(&self) -> [u32; 4] {
        derived_palette(self.chosen_palette(), self.color_count, self.dark())
    }

    /// The four colours as they were chosen, whatever the count: at four
    /// this is [`BuilderParams::palette`], and under four the slots the count
    /// derives still hold what the person had in them. A panel edits and
    /// reorders these, so a colour hidden by a lower count is never
    /// overwritten by what was derived over it.
    ///
    /// With nothing named it is the harmony laid round the favourite, the
    /// untouched house settings included: they are a favourite and a harmony
    /// like any other, and the colour in the primary square is the colour the
    /// row of chips is grown from because they are the one colour. The house
    /// keeps only its page -- the house seed names no neutral, so the fourth
    /// square is the file's own grey rather than the page the rule would have
    /// invented, and an untouched builder still tints nothing.
    pub fn chosen_palette(&self) -> [u32; 4] {
        if let Some(seeds) = self.seeds {
            return [seeds.primary, seeds.secondary, seeds.tertiary, seeds.background];
        }
        let [primary, secondary, tertiary, background] = grown(self.favourite, self.harmony, None).colors;
        if !colors_named(self) {
            return [primary, secondary, tertiary, house_page(self.scheme())];
        }
        [primary, secondary, tertiary, background]
    }

    /// These settings with all four colours named outright, as a person
    /// editing the palette's squares by hand names them: the first becomes
    /// the favourite and the primary, plain and with no mood; the other three
    /// become the named seeds, the fourth as the background colour. Whatever
    /// a suggestion had named goes. The sliders and the dimensions stay.
    ///
    /// Literal colours are used exactly as they are, so
    /// `with_palette(p).chosen_palette() == p` -- and at four colours
    /// `palette()` too, and the built theme's own accents -- and a
    /// suggestion's settings and `with_palette(suggestion.colors)` build the
    /// same theme. Where a colour cannot be told from the page it is the
    /// page that moves: see `page_of`.
    pub fn with_palette(self, palette: [u32; 4]) -> Self {
        let [primary, secondary, tertiary, background] = palette.map(|color| color | 0xFF);
        Self {
            favourite: primary,
            harmony: Harmony::House,
            seeds: Some(SuggestionSeeds { primary, secondary, tertiary, background }),
            ..self
        }
    }

    fn dimension(&self, key: &str) -> f64 {
        match key {
            "space_factor" => self.spacing,
            "corner_radius" => self.roundness,
            "font_size_base" => self.font_size,
            _ => self.font_contrast,
        }
    }
}

impl Default for BuilderParams {
    /// The house theme on a dark page, which is the base theme an app gets
    /// when it never chooses one. [`ThemeBuilderState::enter`] starts from the
    /// appearance actually in force instead.
    fn default() -> Self {
        Self::house(true)
    }
}

// ---------------------------------------------------------------------------
// The page: a colour at a lightness
// ---------------------------------------------------------------------------

/// Relative luminance, the quantity a contrast ratio is made of: each channel
/// taken out of its display curve and weighted by how bright the eye finds
/// it. `theme_tokens::contrast` is built on the same sum.
fn luminance(rgba: u32) -> f64 {
    let channel = |shift: u32| {
        let v = ((rgba >> shift) & 0xFF) as f64 / 255.0;
        if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * channel(24) + 0.7152 * channel(16) + 0.0722 * channel(8)
}

/// CIE lightness from relative luminance: the scale on which equal steps look
/// equal, which is what a slider's travel has to be spent on.
fn lstar_of(y: f64) -> f64 {
    if y > 216.0 / 24389.0 {
        116.0 * y.cbrt() - 16.0
    } else {
        y * 24389.0 / 27.0
    }
}

/// Relative luminance from CIE lightness.
fn luminance_at(lstar: f64) -> f64 {
    if lstar > 8.0 {
        ((lstar + 16.0) / 116.0).powi(3)
    } else {
        lstar * 27.0 / 24389.0
    }
}

/// The page the base theme's own file makes, with no tint: `color_bg_app` at
/// the house settings.
fn house_page(scheme: Scheme) -> u32 {
    grounds(scheme, WHITE, 0.0).0
}

/// How light the lightest dark page is: the house dark page's own, read off
/// the file. See [`DARK_DARKEST`] for why it is also the edge.
fn dark_lightest() -> f64 {
    lstar_of(luminance(house_page(Scheme::Dark)))
}

/// The page's CIE lightness at a place on the slider. Two even runs, one per
/// appearance, meeting at a half with the unreadable band between them left
/// out.
fn lightness_lstar(lightness: f64) -> f64 {
    let v = lightness.clamp(0.0, 1.0);
    if v <= 0.5 {
        DARK_DARKEST + (dark_lightest() - DARK_DARKEST) * (v / 0.5)
    } else {
        LIGHT_DARKEST + (LIGHT_LIGHTEST - LIGHT_DARKEST) * ((v - 0.5) / 0.5)
    }
}

/// Where on the slider the house page of an appearance is. A half for the
/// dark one, exactly, because the dark run ends on it.
fn house_lightness(dark: bool) -> f64 {
    if dark {
        return 0.5;
    }
    let lstar = lstar_of(luminance(house_page(Scheme::Light)));
    0.5 + 0.5 * (lstar - LIGHT_DARKEST) / (LIGHT_LIGHTEST - LIGHT_DARKEST)
}

/// The least lightness that is still a LIGHT page. The slider's two runs
/// meet at a half and the half itself belongs to the dark one
/// ([`BuilderParams::dark`] asks whether the lightness is past it), so the
/// light run opens at the next number a `f64` has. It stands for `L*` 68 to
/// fourteen decimal places, which is nearer than a colour's own lightness
/// can be told anyway.
const LIGHT_FLOOR: f64 = 0.5 + f64::EPSILON;

/// Where a page of this lightness stands on the slider: [`lightness_lstar`]
/// run backwards, inside one appearance's run.
///
/// The run is the one `dark` names, and the number is clamped into it. Two
/// things follow, and both are wanted. A `L*` past the run's ends -- black
/// past the dark run's bottom, white past the light run's top -- lands on
/// the end, because that is as near as a page gets. And a `L*` in the band
/// between the two runs, which is most of the middle of the colour space,
/// lands on the near end of the run it is being read into rather than
/// crossing over: no page can be written on in that band, and which
/// appearance the theme wears is the lightness slider's own question and not
/// a question a colour gets to answer. See [`surface_sliders`].
fn page_lightness_of(color: u32, dark: bool) -> f64 {
    let lstar = lstar_of(luminance(color | 0xFF));
    if dark {
        (0.5 * (lstar - DARK_DARKEST) / (dark_lightest() - DARK_DARKEST)).clamp(0.0, 0.5)
    } else {
        (0.5 + 0.5 * (lstar - LIGHT_DARKEST) / (LIGHT_LIGHTEST - LIGHT_DARKEST)).clamp(LIGHT_FLOOR, 1.0)
    }
}

/// Where the two background sliders have to stand for the page to BE this
/// colour: the saturation first, then the lightness, both in the 0..1 the
/// settings hold them in.
///
/// A colour becomes the two numbers like this, and it is worth writing down
/// because the two halves are not the same kind of arithmetic.
///
/// * The SATURATION slider is a share of the background colour's own
///   saturation, not a saturation of its own: the page carries `own * share`
///   of it ([`page_hue_and_sat`]). The share at which the page carries all
///   the colour there is in the colour is therefore one, whatever the colour
///   -- a pale grey-blue surface asks for the same whole share as a loud one,
///   and gets its own pale result. So this half is a constant, and the number
///   the row shows afterwards is a hundred.
/// * The LIGHTNESS slider names a `L*` on two even runs ([`lightness_lstar`]),
///   so this half is that map run backwards from the colour's own `L*`
///   ([`page_lightness_of`]), clamped into the run the appearance in force is
///   on.
///
/// And back again: [`asked_page`] takes the hue and the saturation off the
/// background colour, works out the luminance the lightness names, and asks
/// [`at_luminance`] for the colour of that hue and saturation at that
/// luminance. Put the two numbers this returns into the settings and the
/// answer is the colour itself, exactly, for every colour a page can be --
/// the hue and the saturation are the colour's own and never left it, and
/// the luminance is the one the colour's own `L*` came from. For a colour a
/// page CANNOT be -- one in the unwritable band, or past either end -- the
/// lightness lands on the nearest page there is and the round trip is as
/// near as the axis allows, which is what "trying to match" comes to.
///
/// What the page finally WEARS can still be a step off, and that is a
/// separate rule left untouched: a page that cannot be told from one of the
/// accents walks along this same axis until it can ([`page_scheme_and_crowding`]).
/// The sliders show what was ASKED for.
pub fn surface_sliders(color: u32, dark: bool) -> (f64, f64) {
    (1.0, page_lightness_of(color, dark))
}

/// The colour of `hue` at HSL saturation `sat` whose luminance is `y`, or the
/// nearest the eight bits allow.
///
/// The lightness is solved for rather than set, and that is the whole point.
/// HSL's own lightness is a lie about brightness: a pure blue and a pure
/// yellow are both "fifty per cent" and one is five times as bright as the
/// other, so a page that kept its HSL lightness while its colour came up or
/// went out would brighten and darken under the saturation slider -- which is
/// exactly what the slider must not do. Holding the luminance holds what the
/// eye calls lightness, and it holds every contrast ratio against the page
/// with it, since those are made of nothing else.
///
/// No saturation is the grey of that luminance. So the grey at the bottom of
/// the saturation slider is the colour at its top with the colour taken out,
/// and not black, white, or the appearance's stock page.
fn at_luminance(hue: f64, sat: f64, y: f64) -> u32 {
    if sat <= 0.0 {
        let grey = |g: u32| (g << 24) | (g << 16) | (g << 8) | 0xFF;
        return (0..=255u32)
            .map(grey)
            .min_by(|a, b| (luminance(*a) - y).abs().partial_cmp(&(luminance(*b) - y).abs()).unwrap())
            .unwrap_or(BLACK);
    }
    // Luminance only rises with HSL lightness at a fixed hue and saturation,
    // so halving the interval finds it.
    let (mut low, mut high) = (0.0f64, 1.0f64);
    for _ in 0..40 {
        let middle = (low + high) / 2.0;
        if luminance(hsl_to_rgb(hue, sat, middle)) < y {
            low = middle;
        } else {
            high = middle;
        }
    }
    let (under, over) = (hsl_to_rgb(hue, sat, low), hsl_to_rgb(hue, sat, high));
    if (luminance(under) - y).abs() <= (luminance(over) - y).abs() {
        under
    } else {
        over
    }
}

/// The colour a palette chip shows for a background colour: its own hue and
/// saturation at the house page lightness of the appearance. What the page
/// is with both sliders where they open.
fn ground_swatch(background: u32, dark: bool) -> u32 {
    let (hue, sat, _) = rgb_to_hsl(background | 0xFF);
    let scheme = if dark { Scheme::Dark } else { Scheme::Light };
    at_luminance(hue, sat, luminance(house_page(scheme)))
}

/// The plain end of an appearance: the ink its base theme writes in.
fn plain_ink(scheme: Scheme) -> u32 {
    match scheme {
        Scheme::Light => BLACK,
        _ => WHITE,
    }
}

/// `color_fg_app` for a page: the page scaled by the file's own ratio between
/// its two fractions, which is what the file's multiply makes of a tinted
/// page -- 1.2 of it in the dark theme, 0.97 in the light one. Scaling keeps
/// the hue and the saturation, so the panel ground is the page a step off
/// and not a second colour.
fn fg_of(scheme: Scheme, page: u32) -> u32 {
    let ratio = match scheme {
        Scheme::Light => (1.0 - LIGHT_GROUNDS.1) / (1.0 - LIGHT_GROUNDS.0),
        _ => DARK_GROUNDS.1 / DARK_GROUNDS.0,
    };
    let channel = |shift: u32| ((((page >> shift) & 0xFF) as f64 * ratio).round().min(255.0)) as u32;
    (channel(24) << 24) | (channel(16) << 16) | (channel(8) << 8) | 0xFF
}

/// Everything a page is the ground of, as the file derives it from
/// `color_bg_app` and `color_fg_app`: the two opaque rungs the inverse page
/// is made of, and the surface ladder.
fn page_colors(scheme: Scheme, bg: u32, fg: u32) -> BTreeMap<String, u32> {
    let mut colors: BTreeMap<String, u32> = BTreeMap::new();
    colors.insert("color_bg_app".to_string(), bg);
    colors.insert("color_fg_app".to_string(), fg);
    colors.insert("color_opaque_u_6".to_string(), vm_mix(fg, WHITE, OPAQUE_U_6));
    colors.insert("color_opaque_d_5".to_string(), vm_mix(fg, BLACK, OPAQUE_D_5));
    for (role, light, dark) in DERIVED_ROLES {
        let known = |key: &str| colors.get(key).copied();
        let value = match if scheme == Scheme::Dark { *dark } else { *light } {
            RoleSource::Token(t) => known(t),
            RoleSource::HalfMix(a, b) => known(a).zip(known(b)).map(|(a, b)| vm_mix(a, b, 0.5)),
            RoleSource::Mix(a, b, amount) => known(a).zip(known(b)).map(|(a, b)| vm_mix(a, b, amount)),
            RoleSource::MixTo(t, end, amount) => known(t).map(|c| vm_mix(c, end, amount)),
        };
        if let Some(rgba) = value {
            colors.insert(role.to_string(), rgba);
        }
    }
    colors
}

/// The page and the panel ground for these settings. The house page, exactly
/// and as the file derives it, wherever the settings come to it.
fn grounds_of(params: &BuilderParams) -> (u32, u32) {
    let (grounds, _, _) = grounds_and_crowding(params);
    grounds
}

/// [`grounds_of`] with the appearance the page landed in and the accent it
/// could not be moved clear of, for the one caller that wants all three at
/// once and would otherwise solve for the page twice.
/// See [`page_scheme_and_crowding`].
fn grounds_and_crowding(params: &BuilderParams) -> ((u32, u32), Scheme, Option<SeedSlot>) {
    let (page, scheme, crowding) = page_scheme_and_crowding(params);
    if page == house_page(scheme) {
        return (grounds(scheme, WHITE, 0.0), scheme, crowding);
    }
    ((page, fg_of(scheme, page)), scheme, crowding)
}


/// What colour the page takes and how much of it: the background colour's
/// own hue, and the share of that colour's own saturation the saturation
/// slider names. A background with no colour in it has none to share, so the
/// page is a grey wherever the slider stands.
fn page_hue_and_sat(params: &BuilderParams) -> (f64, f64) {
    let [_, _, _, background] = params.palette();
    let (hue, own, _) = rgb_to_hsl(background | 0xFF);
    (hue, if own < 0.01 { 0.0 } else { own * params.saturation.clamp(0.0, 1.0) })
}

/// The page the two background sliders ASK for: their colour at their
/// lightness, before anything moves it clear of the accents.
///
/// It is [`page_scheme_and_crowding`] with the one rule that can move a page left out, and it
/// is what [`surface_sliders`] is the inverse of: the two are a round trip,
/// and a Surf colour read into the sliders and built again here is that
/// colour.
#[cfg(test)]
pub fn asked_page(params: &BuilderParams) -> u32 {
    let (hue, sat) = page_hue_and_sat(params);
    at_luminance(hue, sat, luminance_at(lightness_lstar(params.lightness)))
}

/// The page these settings make, the appearance it lands in, and the accent
/// it could not be moved clear of, if any.
///
/// The page may walk the whole lightness axis and not only the half the
/// slider is in, which is the difference between a rule that works and one
/// that does not: a half is a finite run, and no dark page can be made darker
/// than a near-black colour, so a near-black pick on a dark page would have
/// nowhere to go and the theme could only apologise. Over the whole axis
/// there is always somewhere, because at [`TOLD_APART`] a colour clears the
/// lightest page the light run offers unless it is nearly white and the
/// darkest page the dark run offers unless it is nearly black.
///
/// It walks out of its half only when it must. The slider is asking for an
/// appearance, and the answer to "can this accent be told from this page" is
/// yes for every ordinary colour somewhere inside the asked half, so that is
/// where the page lands and the slider gets what it asked for. The two ends
/// of the colour space are the exception, and they are the reason the whole
/// axis is open at all.
///
/// A SET of accents can still be impossible -- a near-black one and a
/// near-white one want opposite ends -- and that is the only case left. The
/// page clears the seed ([`BuilderParams::seed_slot`]) whatever happens and
/// then clears as many of the rest as it can; the colours are still used
/// exactly, and what comes back beside the page is which accent stands too
/// close to it, for the panel to say under its controls. Moving the colour
/// instead would be the old fault with better manners.
fn page_scheme_and_crowding(params: &BuilderParams) -> (u32, Scheme, Option<SeedSlot>) {
    let (hue, sat) = page_hue_and_sat(params);
    let at = |lstar: f64| at_luminance(hue, sat, luminance_at(lstar));
    let asked = lightness_lstar(params.lightness);
    let (lstar, crowding) = clear_of(asked, &page_accents(params), params.seed_slot, &at);
    let scheme = scheme_at_lightness(lstar);
    let mut y = luminance_at(lstar);
    let mut page = at(lstar);
    for _ in 0..40 {
        if plain_ink_reads(scheme, page) {
            break;
        }
        y = match scheme {
            Scheme::Light => y + (1.0 - y) * 0.02,
            _ => y * 0.97,
        };
        page = at_luminance(hue, sat, y);
    }
    (page, scheme, crowding)
}

/// The two runs of the lightness axis, darkest first: the dark theme's pages
/// and the light theme's, with the band neither can write on left out. What
/// the page may be moved between, all of it.
fn lightness_runs() -> [(f64, f64); 2] {
    [(DARK_DARKEST, dark_lightest()), (LIGHT_DARKEST, LIGHT_LIGHTEST)]
}

/// Which appearance a page of this lightness is: the run it falls in, with
/// the unwritable band between them split down the middle so that a number a
/// hair outside either run still answers.
fn scheme_at_lightness(lstar: f64) -> Scheme {
    if lstar <= (dark_lightest() + LIGHT_DARKEST) / 2.0 {
        Scheme::Dark
    } else {
        Scheme::Light
    }
}

/// The colours the page has to be told apart from, each with the slot it
/// stands in: the accents the palette CHOOSES, as [`build`] uses them.
///
/// The chosen ones and not all three. A page moves because a colour somebody
/// picked may not, and a colour the count derives is nobody's pick: it is a
/// tonal step of the primary, made to stand off the page the primary stands
/// off, and it can be moved like anything else derived. Making the page dodge
/// those too would carry it away from where the lightness slider was put for
/// the sake of colours nobody chose.
///
/// Empty where the palette has not moved. Such a theme installs no accent at
/// all -- the roles are the ones the theme files carry -- and the page a
/// house palette makes is the file's own business, held by the file's own
/// numbers and by every sweep over them.
fn page_accents(params: &BuilderParams) -> Vec<(SeedSlot, u32)> {
    if !palette_moved(params) {
        return Vec::new();
    }
    let palette = params.palette();
    [SeedSlot::Primary, SeedSlot::Secondary, SeedSlot::Tertiary]
        .into_iter()
        .filter(|slot| slot.chosen_at(params.color_count))
        .map(|slot| (slot, palette[slot.index()] | 0xFF))
        .collect()
}

/// Where the page has to stand to be told from the accents: `asked` if it
/// already can be, and otherwise the NEAREST place on the lightness axis that
/// works -- no further, so a person who moved the lightness slider still gets
/// as near to where they put it as reading allows.
///
/// The axis is both runs, [`lightness_runs`], but the half the slider is
/// sitting in is asked first and only given up where it holds no answer at
/// all. The slider is asking for an appearance; a near-black pick has no
/// dark page it can be told from and a near-white one no light page, and
/// those two are the whole of what sends a theme across the middle. Every
/// other pick, the bright ones included, keeps the appearance that was asked
/// for and moves only within it.
///
/// One colour always has a place; only a set of them can want two places at
/// once, and then the order is fixed: the SEED's slot has to be cleared, the
/// asked half is kept if it can be, and among the places left the one that
/// clears the most of the others wins, nearest to `asked` breaking the tie.
/// The seed is the colour the person has in hand, so it is the last one a
/// rule may give up on.
///
/// The bar here is [`TOLD_APART`] and not `LEGIBLE`: an accent is a fill, and
/// what it owes the page is only that the two are two colours. See the
/// constant for why holding it to the text bar took the lightness slider's
/// job away from it.
///
/// Where a page may not stand is solved and not searched for. A contrast
/// ratio is made of luminance and nothing else, so an accent of luminance
/// `y` forbids the page every luminance between
/// `(y + 0.05) / TOLD_APART - 0.05` and
/// `TOLD_APART * (y + 0.05) - 0.05`: the bar read backwards. Those bounds,
/// on the lightness scale, are the only places worth trying, along with the
/// four ends of the two runs; each is then nudged outward until the page
/// really clears, because a page is eight bits a channel and a bound is not.
///
/// `None` back means it stands clear of all of them. `Some(slot)` names the
/// accent standing nearest the page of the ones it could not get clear of.
fn clear_of(
    asked: f64,
    accents: &[(SeedSlot, u32)],
    seed: SeedSlot,
    at: &impl Fn(f64) -> u32,
) -> (f64, Option<SeedSlot>) {
    if accents.is_empty() {
        return (asked, None);
    }
    // The one that has to be cleared: the seed's, or the primary's where the
    // seed is a slot this count derives and so nobody's pick.
    let must = accents.iter().find(|(slot, _)| *slot == seed).or_else(|| accents.first()).copied();
    let seed_slot = must.map(|(slot, _)| slot);
    // How the accents stand at a lightness: how many are too close to the
    // page, which of those stands nearest it -- the one a panel names -- and
    // whether the seed is among them. Solved for in one pass, because this is
    // the inner loop of the whole search.
    let crowded = |lstar: f64| {
        let page = at(lstar);
        let (mut count, mut nearest, mut seed_close) = (0usize, None, false);
        for (slot, accent) in accents {
            let ratio = contrast(page, *accent);
            if ratio >= TOLD_APART {
                continue;
            }
            count += 1;
            seed_close |= Some(*slot) == seed_slot;
            if nearest.is_none_or(|(_, worst)| ratio < worst) {
                nearest = Some((*slot, ratio));
            }
        }
        (count, nearest.map(|(slot, _)| slot), seed_close)
    };
    if crowded(asked).0 == 0 {
        return (asked, None);
    }
    // Its forbidden band in luminance, so that a run lying wholly inside it
    // can be passed over rather than walked step by step. This is the common
    // case and the expensive one: a near-black pick forbids the whole of the
    // dark run, and every place in it would have to be tried and thrown away.
    let shut = must.map(|(_, accent)| {
        let y = luminance(accent | 0xFF);
        (((y + 0.05) / TOLD_APART - 0.05).max(0.0), (TOLD_APART * (y + 0.05) - 0.05).min(1.0))
    });
    let mut tries: Vec<f64> = Vec::with_capacity(4 + accents.len() * 2);
    for (low, high) in lightness_runs() {
        tries.push(low);
        tries.push(high);
    }
    for (_, accent) in accents {
        let y = luminance(*accent | 0xFF);
        tries.push(lstar_of(((y + 0.05) / TOLD_APART - 0.05).max(0.0)));
        tries.push(lstar_of((TOLD_APART * (y + 0.05) - 0.05).min(1.0)));
    }
    // What a place is worth: whether it clears the seed first, then whether
    // it is in the half the slider is sitting in, then how many of the others
    // it clears, then how near it is to where the slider asked.
    //
    // The half comes second because the slider is asking for an appearance
    // and it is entitled to one whenever the seed can be told from any page
    // in it. Only a colour no page of the asked half can be told from -- a
    // near-black asked for on a dark page, a near-white on a light one --
    // sends the theme across the middle, and that is the case the whole-axis
    // search was put in for.
    let asked_half = scheme_at_lightness(asked);
    let mut best: Option<(bool, bool, usize, f64, f64)> = None;
    for (low, high) in lightness_runs() {
        if let Some((from, to)) = shut {
            if luminance_at(low) > from && luminance_at(high) < to {
                continue;
            }
        }
        for try_at in &tries {
            let start = try_at.clamp(low, high);
            // Away from where the page was asked to be, a twentieth of a step
            // of lightness at a time: a bound landed on is a bound the eight
            // bits may fall a hair short of. Two whole steps of `L*` of it,
            // because at the dark end of the axis one value of eight bits IS
            // about two steps of `L*`.
            let away = if start >= asked { 1.0 } else { -1.0 };
            let reach = (start + away * 2.0).clamp(low, high);
            // A walk that stays inside the seed's own forbidden band the
            // whole way cannot end anywhere this rule would take, and forty
            // pages would be solved for to find that out. The bounds say so
            // in two multiplications.
            if let Some((from, to)) = shut {
                if luminance_at(start.min(reach)) > from && luminance_at(start.max(reach)) < to {
                    continue;
                }
            }
            for step in 0..40 {
                let lstar = start + away * step as f64 * 0.05;
                if lstar < low || lstar > high {
                    break;
                }
                let (close, _, seed_close) = crowded(lstar);
                let cleared = accents.len() - close;
                let held = !seed_close;
                let own_half = scheme_at_lightness(lstar) == asked_half;
                let near = -(lstar - asked).abs();
                let worth = (held, own_half, cleared, near, lstar);
                if best.is_none_or(|kept| (worth.0, worth.1, worth.2, worth.3) > (kept.0, kept.1, kept.2, kept.3)) {
                    best = Some(worth);
                }
                if held {
                    break;
                }
            }
        }
    }
    let landed = match best {
        Some((_, _, _, _, lstar)) => lstar,
        None => asked,
    };
    (landed, crowded(landed).1)
}

/// Whether the appearance's plain ink reads on every rung this page makes, at
/// the bar the library holds each rung to.
fn plain_ink_reads(scheme: Scheme, page: u32) -> bool {
    let colors = page_colors(scheme, page, fg_of(scheme, page));
    let ink = plain_ink(scheme);
    held_pairs().iter().all(|(ground, held, need)| match (*held, colors.get(*ground)) {
        ("color_on_surface" | "color_on_surface_variant", Some(g)) => reads_on(*g | 0xFF, ink) >= *need,
        _ => true,
    })
}

/// What the house theme's body text stands off the house page: the text
/// contrast that leaves the text alone.
fn house_text_contrast(scheme: Scheme) -> f64 {
    let body = file_value(scheme, "color_text", &BTreeMap::new()).unwrap_or(plain_ink(scheme));
    reads_on(house_page(scheme), body)
}

/// A number a base theme's file gives one of its top-level keys, read off the
/// file's text: `space_factor: 6. // Increase for a less dense layout` is 6.
/// `None` for a key the file derives rather than states.
fn file_number(scheme: Scheme, key: &str) -> Option<f64> {
    file_lines(scheme, key).iter().find_map(|rest| rest.split("//").next()?.trim().parse::<f64>().ok())
}

/// The theme files' top-level lines indexed once by key: every line that is
/// `        key:` and then something, in file order, each as what follows the
/// colon. The build looks a handful of keys up hundreds of times, and
/// scanning a four-thousand-line file for each of them was most of its time.
fn file_lines(scheme: Scheme, key: &str) -> &'static [&'static str] {
    type Index = std::collections::HashMap<&'static str, Vec<&'static str>>;
    static INDEX: std::sync::OnceLock<[Index; 3]> = std::sync::OnceLock::new();
    let indexes = INDEX.get_or_init(|| {
        Scheme::ALL.map(|scheme| {
            let mut index = Index::new();
            for line in scheme.source().lines() {
                let Some(rest) = line.strip_prefix("        ") else { continue };
                let Some((name, after)) = rest.split_once(':') else { continue };
                index.entry(name).or_default().push(after);
            }
            index
        })
    });
    let at = Scheme::ALL.iter().position(|one| *one == scheme).unwrap_or(0);
    indexes[at].get(key).map(Vec::as_slice).unwrap_or(&[])
}
/// time a colour leaves an expression. `theme_tokens::mix_rgb` rounds, which
/// is the right answer and a different one, and a reading of a built theme
/// has to be a reading of the theme the VM will actually make.
fn vm_mix(a: u32, b: u32, t: f64) -> u32 {
    let t = t as f32;
    let channel = |shift: u32| {
        let x = ((a >> shift) & 0xFF) as f32 / 255.0;
        let y = ((b >> shift) & 0xFF) as f32 / 255.0;
        ((x * (1.0 - t) + y * t) * 255.0) as u8 as u32
    };
    (channel(24) << 24) | (channel(16) << 16) | (channel(8) << 8) | channel(0)
}

/// The script's `*` on two colours, channel by channel, cut the same way.
fn vm_mul(a: u32, b: u32) -> u32 {
    let channel = |shift: u32| {
        let x = ((a >> shift) & 0xFF) as f32 / 255.0;
        let y = ((b >> shift) & 0xFF) as f32 / 255.0;
        (x * y * 255.0) as u8 as u32
    };
    (channel(24) << 24) | (channel(16) << 16) | (channel(8) << 8) | channel(0)
}

/// `color_bg_app` and `color_fg_app` as the base theme's file will derive
/// them under a tint: both ends of the page multiplied by the tint mixed
/// `amount` of the way from white, then mixed by the file's own fraction.
fn grounds(scheme: Scheme, tint: u32, amount: f64) -> (u32, u32) {
    let contrast = file_number(scheme, "color_contrast").unwrap_or(1.0);
    let cast = vm_mix(WHITE, tint, amount);
    let (from, to, (bg, fg)) = match scheme {
        Scheme::Dark => (BLACK, WHITE, DARK_GROUNDS),
        _ => (WHITE, BLACK, LIGHT_GROUNDS),
    };
    let ground = |fraction: f64| vm_mix(vm_mul(from, cast), vm_mul(to, cast), fraction.powf(contrast));
    (ground(bg), ground(fg))
}

/// What a base theme's file says one of its colour keys is, worked out from
/// the file's own text.
///
/// The builder never moves these tokens -- until this mapping nothing in a
/// built theme touched one at all -- so they are not values that have to be
/// PREDICTED. They are values that have to be KNOWN, and for two reasons.
/// They are the grounds a check mark, a value fill or a caret lands on, and
/// an accent chosen without looking at the ground under it can come out
/// invisible. And they are the bar a new colour has to clear: a control that
/// reads WORSE than the grey it replaced is not an improvement, and the grey
/// it replaced is exactly what this function says.
///
/// Three forms cover the colour half of both files -- a literal, an alias to
/// another key, and `mix`, whose amount is either a plain number or the
/// `pow(fraction, theme.color_contrast)` the translucent ladder is built
/// with. That is enough for every name [`ACCENTED`] uses, and a gate test
/// says so in both files. Anything else -- `theme.color_d_3 * 0.8`, or the
/// two page colours, which run over several lines -- comes back as `None`,
/// and a row that needs a value and gets none simply leaves its token alone.
///
/// `known` is the build's own answers so far, and it is consulted before the
/// file is: the page and the roles have already been worked out here, under
/// a tint the file knows nothing about, and reading them off the file again
/// would answer with the untinted theme.
fn file_value(scheme: Scheme, key: &str, known: &BTreeMap<String, u32>) -> Option<u32> {
    file_key(scheme, key, known, 0)
}

/// [`file_value`] on a key, with the hop count that stops a file whose
/// aliases somehow come round in a circle from hanging the builder.
fn file_key(scheme: Scheme, key: &str, known: &BTreeMap<String, u32>, depth: u32) -> Option<u32> {
    if depth > 8 {
        return None;
    }
    if let Some(rgba) = known.get(key) {
        return Some(*rgba);
    }
    file_expr(scheme, &file_stated(scheme, key)?, known, depth + 1)
}

/// What a file writes after one of its top-level keys, comment and
/// surrounding space taken off: `color_inset: theme.color_d_1` is
/// `theme.color_d_1`.
fn file_stated(scheme: Scheme, key: &str) -> Option<String> {
    file_lines(scheme, key)
        .iter()
        .find_map(|after| after.strip_prefix(' '))
        .map(|rest| rest.split("//").next().unwrap_or(rest).trim().to_string())
}

fn is_key_char(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'
}

/// One of the three forms, worked out the way the VM will work it out --
/// `vm_mix` and not `mix_rgb`, because a reading of a built theme has to be
/// a reading of the theme the VM will actually make.
fn file_expr(scheme: Scheme, text: &str, known: &BTreeMap<String, u32>, depth: u32) -> Option<u32> {
    if depth > 8 {
        return None;
    }
    let text = text.trim();
    if let Some(alias) = text.strip_prefix("theme.") {
        return alias.chars().all(is_key_char).then(|| file_key(scheme, alias, known, depth))?;
    }
    if text.starts_with('#') {
        return hash_color(text);
    }
    let inside = text.strip_prefix("mix(")?.strip_suffix(')')?;
    let parts = commas(inside)?;
    let [a, b, t] = parts[..] else { return None };
    let a = file_expr(scheme, a, known, depth + 1)?;
    let b = file_expr(scheme, b, known, depth + 1)?;
    Some(vm_mix(a, b, file_amount(scheme, t)?))
}

/// A `mix`'s third argument: a number, or the power of the contrast global
/// that every rung of the translucent ladder is spaced by.
fn file_amount(scheme: Scheme, text: &str) -> Option<f64> {
    let text = text.trim();
    let Some(inside) = text.strip_prefix("pow(").and_then(|rest| rest.strip_suffix(')')) else {
        return text.parse::<f64>().ok();
    };
    let (base, exponent) = inside.split_once(',')?;
    if exponent.trim() != "theme.color_contrast" {
        return None;
    }
    let contrast = file_number(scheme, "color_contrast").unwrap_or(1.0);
    Some(base.trim().parse::<f64>().ok()?.powf(contrast))
}

/// An argument list split on its own commas, leaving any inside a nested
/// call where they are. `None` for anything that is not three arguments,
/// which is the only shape `mix` has.
fn commas(text: &str) -> Option<Vec<&str>> {
    let mut out = Vec::new();
    let (mut depth, mut from) = (0i32, 0usize);
    for (at, c) in text.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                out.push(&text[from..at]);
                from = at + 1;
            }
            _ => {}
        }
    }
    out.push(&text[from..]);
    (out.len() == 3).then_some(out)
}

/// A colour literal as the script's own parser reads one: `#x` or `#`, then
/// one, three, four, six or eight hex digits, the short forms doubling each
/// digit. `#F` is white and `#0` is black, which is how both files write the
/// ends of the opaque ladder.
fn hash_color(text: &str) -> Option<u32> {
    let digits = text.trim().strip_prefix('#')?.trim_start_matches('x');
    if !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let widened: String = match digits.len() {
        1 | 3 | 4 => digits.chars().flat_map(|c| [c, c]).collect(),
        6 | 8 => digits.to_string(),
        _ => return None,
    };
    let widened = match widened.len() {
        2 => format!("{widened}{widened}{widened}FF"),
        6 => format!("{widened}FF"),
        8 => widened,
        _ => return None,
    };
    u32::from_str_radix(&widened, 16).ok()
}

/// Which member of which family a row of [`ACCENTED`] draws on.
///
/// All three brand families are spent here, and that is the point of the
/// table. A library that gave every coloured thing the primary would answer
/// "pick a colour, get a theme" with a screen in one colour, which is what
/// the panel used to do and what a person looking at it said out loud: the
/// palette had three colours in it and the interface showed one.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Source {
    /// The accent itself, the colour a person picked.
    Primary,
    /// What the palette already decided reads on that container, and so the
    /// second thing to try where the accent itself is too close to a ground.
    OnPrimaryContainer,
    Secondary,
    SecondaryContainer,
    OnSecondaryContainer,
    Tertiary,
    OnTertiaryContainer,
    /// The palette's fourth colour, the one the page is made of, at its own
    /// saturation and at the lightness of the token it is leant into: pale
    /// where the token is a wash of white, deep where it is a wash of black.
    /// A control ground is a background, so it wears the background's hue
    /// and not an accent's.
    Background,
}

impl Source {
    /// The colour this member is, for a token whose own value is `token`.
    /// `None` for [`Source::Background`] where the fourth colour has no
    /// colour in it: a lean toward a grey would only move a wash of white
    /// toward a wash of grey, and the house palette's grounds are not the
    /// builder's to repaint.
    fn toward(self, roles: &ColorRoles, background: u32, token: u32) -> Option<u32> {
        if self != Source::Background {
            return Some(self.of(roles));
        }
        let (hue, sat, _) = rgb_to_hsl(background | 0xFF);
        if sat < 0.01 {
            return None;
        }
        let light = if luminance(token | 0xFF) > 0.5 { 0.8 } else { 0.2 };
        Some(hsl_to_rgb(hue, sat, light))
    }

    fn of(self, roles: &ColorRoles) -> u32 {
        match self {
            Source::Primary => roles.primary.base,
            Source::OnPrimaryContainer => roles.primary.on_container,
            Source::Secondary => roles.secondary.base,
            Source::SecondaryContainer => roles.secondary.container,
            Source::OnSecondaryContainer => roles.secondary.on_container,
            Source::Tertiary => roles.tertiary.base,
            Source::OnTertiaryContainer => roles.tertiary.on_container,
            // Never asked: the fourth colour is only ever leant toward, and
            // only through `toward`, which knows the token it leans.
            Source::Background => roles.secondary.container,
        }
    }
}

/// How a row's source reaches its tokens.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Reaches {
    /// Straight, because nothing draws the token: it is one of the names the
    /// library keeps the accent itself under, for a sheet to be regrown from.
    Named,
    /// As something drawn ON the grounds named -- a mark, a ring, a caret, a
    /// value fill. The source where it stands clear of every one of them,
    /// `alt` where only that does, and failing both the plainer of black and
    /// white. `need` is the bar: a graphic answers to `LEGIBLE`, words to
    /// `READABLE`.
    Ink { alt: Source, on: &'static [&'static str], need: f64 },
    /// As a ground something else is drawn on. The source where `ink` still
    /// reads on it, `alt` where only that does, and the base theme's own
    /// value where neither does -- a selection a person cannot read the text
    /// in is worse than a selection with no colour in it.
    ///
    /// Where `ink` is itself a token this table pins, the question does not
    /// arise and the source is taken outright: the ink is chosen against
    /// this ground afterwards, in the second pass, so a ground that gave way
    /// to an ink that was about to move anyway would have given way for
    /// nothing.
    Ground { alt: Source, ink: &'static str, need: f64 },
    /// As the source at the token's own alpha: a wash over whatever is under
    /// it rather than a colour in front of it. What a drop target is -- it
    /// has to be seen through, so it cannot be a colour that covers.
    Veil,
    /// As a lean: the token's own value carried `most` of the way toward the
    /// source, at the token's OWN alpha, and given back as far as it has to
    /// be for `ink` to go on reading on the result, so a fill can never
    /// swallow its own label.
    ///
    /// `waits` is whether the lean is a matter of degree. A control's ground
    /// is: it waits on [`control_ground_lean`], the saturation slider, and is
    /// nothing at all at nought. A selection is not -- it lands at once, like every other
    /// accent -- and leans rather than taking the family's container outright
    /// only where the base theme's own ground is DARKER than the container,
    /// so that taking it would cost the words on it the contrast the base
    /// theme gave them.
    ///
    /// The alpha is what makes this safe, and it is not a detail. Nearly
    /// every control ground in both files is plain white or plain black at
    /// some percentage -- the ladder is one colour at nine weights -- so a
    /// mix that carried the alpha across would swap a fifteen-per-cent wash
    /// for a half-opaque fill and rewrite the whole depth of the interface
    /// on the way to tinting it. Kept, the wash stays a wash and only its
    /// colour moves.
    Lean { most: f64, ink: &'static str, need: f64, waits: bool },
}

/// One row of the mapping.
struct Accented {
    /// The older tokens this row pins. More than one where a family of
    /// states is the same thing drawn in the same place: a slider's value
    /// fill is `color_val` plain, hovered, focused and dragged, and a fill
    /// that took the accent only while the pointer was elsewhere would be a
    /// control that changes colour under the hand.
    ///
    /// A state is also listed where the FILE aliases it to one that moves.
    /// `color_inset_focus` is `color_inset_hover` is `color_inset`; a pin
    /// lands on the object after it has derived, so pinning the hover and
    /// leaving the focus would give one control two grounds.
    tokens: &'static [&'static str],
    from: Source,
    reaches: Reaches,
}

/// FROM the built roles TO the older tokens the classic controls actually
/// draw from. The other direction of `theme_tokens::derived_roles` with
/// `RoleGrowth::FromAccent`, which grows the roles from a style sheet's older
/// accent; here the roles exist and it is the older tokens that are still the
/// base theme's greys.
///
/// # Why a table and not a pin per control
///
/// Only 35 of the 109 widget files that read a theme colour read a ROLE at
/// all, and the classic ones read none. `check_box`, `radio_button`,
/// `text_input`, `tab`, `tab_bar`, `drop_down`, `scroll_bar` and
/// `link_label` between them read zero; `slider` reads one. They draw from
/// `theme_desktop_dark` / `theme_desktop_light`, where every one of these
/// tokens is a grey or the one fixed focus blue (`color_focus: #x7aa2f7`).
/// So a theme grown from an orange favourite had an orange-leaning page and
/// grey controls with a blue focus ring, and "pick a colour, get a theme"
/// was not true. It is fixed here rather than in those files because several
/// of them are the upstream library's, and which token a widget reads is not
/// this panel's to change.
///
/// # Which family a thing wears
///
/// Three voices, and a control kind always wears the same one, so that the
/// colour says what a thing IS and not merely that somebody chose a palette:
///
/// * The primary is the main action and the VALUE: value fills and the focus
///   ring, which is the app saying where you are.
/// * The secondary is SELECTION and the on-state: the ground under a ticked
///   box, a chosen radio, a selected row in a menu, a drop down or a file
///   tree, the label on the tab you are on.
/// * The tertiary POINTS THINGS OUT: the tick in a check box and the dot in
///   a radio, selected text, the caret, and the preview of where a drag would
///   land. The marks are here because without them the third colour showed
///   only when something was typed into or dragged; on them it is on every
///   screen that has a ticked box, drawn on the secondary's ground.
///
/// And the grounds of the controls at rest, under the pointer and pressed
/// wear none of the three. They are BACKGROUNDS -- a row of buttons is part
/// of the page it sits on -- so they step off the page in the palette's
/// fourth colour, carrying the same saturation the saturation slider gives
/// the page. A control ground in an accent was a second accent spread over
/// every button, and it put the colour of selection on things that were
/// not selected.
///
/// # What a style sheet colours, and what it leaves
///
/// Reading the twelve sheets under `widgets/themes` for the tokens they
/// assign their own accent to gives `color_focus`, `color_ctrl_selected`,
/// `color_ctrl_active`, the four numbered focus bevels, `color_outset_active`
/// and the two highlight grounds -- and that list is not a guess about what
/// CAN carry an accent, it is twelve independent answers to the question.
/// The marks, the value fills, the text selection and the caret are added to
/// it, because a sheet leaves those at the base theme's greys and the
/// complaint was exactly that the controls stayed grey.
///
/// What is deliberately NOT in it:
///
/// * Links. `link_label` draws from `color_label_inner`, the same token every
///   button's label draws from, so there is no colour to give a link that is
///   not also given to every label in the app. It needs a token of its own
///   before it can have a colour of its own, and that is a change to a widget
///   file and to both theme files, not a change to this table.
/// * The slider's handle. It sits ON the value fill; both in the primary
///   would be a thumb that disappears into the colour it is meant to mark
///   the end of.
/// * The two tracks, `color_inset_1` and `color_inset_2`. A track is the part
///   of a value that is not there yet; tinting it eats the fill that is.
/// * The rungs themselves -- `color_u_3`, `color_opaque_u_2` and the rest.
///   A rung is shared by the things that must STAY neutral, so the table pins
///   the TOKEN and never the rung it happens to alias.
/// * `color_bg_highlight`, although every sheet colours it. In this library
///   it is the ground behind a block quote, a code span and a table header in
///   `markdown`, `html`, `text_flow` and `code_block` -- three block grounds
///   sharing one name, none of them a highlight and none of them a control.
/// * The word beside a control. `color_label_outer_active` is the text next
///   to a ticked check box; that is body text that happens to sit by
///   something ticked, and colouring it would say the label is the accent.
/// * The disabled states, and the hover INKS. Nothing is selected, focused
///   or on in any of them.
/// * The categorical palettes, `color_map_*` and `color_syntax_*`. Those are
///   told apart BY their colours; a palette that leant on them would make two
///   directories or two token kinds the same thing.
const ACCENTED: &[Accented] = {
    use Reaches::{Ground, Ink, Lean, Named, Veil};
    use Source::{
        Background, OnPrimaryContainer, OnSecondaryContainer, OnTertiaryContainer, Primary, Secondary,
        SecondaryContainer, Tertiary,
    };
    /// Where a focus ring is seen: against the page, and against the panel
    /// ground a control more often sits on.
    const PAGES: &[&str] = &["color_bg_app", "color_fg_app"];
    /// The box behind a check mark or a radio dot, in the two states a mark
    /// is visible in.
    const BOXES: &[&str] = &["color_inset_active", "color_inset_focus"];
    /// The three grounds a value fill is drawn along.
    const TRACKS: &[&str] = &["color_inset", "color_inset_1", "color_inset_2"];
    /// Everything the label of a selected thing lands on: the four selected
    /// grounds below, and the page itself, because a tab draws its active
    /// label straight onto the page while a menu row draws it on the fill.
    const SELECTED: &[&str] = &[
        "color_outset_active",
        "color_outset_1_active",
        "color_outset_2_active",
        "color_highlight",
        "color_bg_app",
    ];
    &[
        // ------------------------------------------------ THE PRIMARY: the
        // main action, and the value.
        //
        // The accent under the two names the base themes keep it under.
        // Nothing draws these. `color_ctrl_selected` is `SHEET_ACCENT`, the
        // token a style sheet's roles are regrown from, so a sheet exported
        // from an orange theme that left this at the base theme's blue would
        // come back blue. (`color_ctrl_active` is the third name and is NOT
        // here: only the sheets declare it, and a token no base theme has is
        // a token this mapping cannot pin -- the gate test says so.)
        Accented { tokens: &["color_focus", "color_ctrl_selected"], from: Primary, reaches: Named },
        // The focus ring, on every control that draws one: the plain bevel
        // and the four numbered ones, which is what the sheets set and what
        // `button`, `check_box`, `radio_button`, `slider`, `text_input` and
        // `drop_down` read between them. A ring is a graphic.
        Accented {
            tokens: &[
                "color_bevel_focus",
                "color_bevel_inset_1_focus",
                "color_bevel_inset_2_focus",
                "color_bevel_outset_1_focus",
                "color_bevel_outset_2_focus",
            ],
            from: Primary,
            reaches: Ink { alt: OnPrimaryContainer, on: PAGES, need: LEGIBLE },
        },
        // The value fill: a slider's filled part, a progress bar, a wave.
        // Nothing is written on it (see `WRITTEN`), which is why it may be
        // chosen against its track alone.
        Accented {
            tokens: &[
                "color_val", "color_val_hover", "color_val_focus", "color_val_drag",
                "color_val_1", "color_val_1_hover", "color_val_1_focus", "color_val_1_drag",
                "color_val_2", "color_val_2_hover", "color_val_2_focus", "color_val_2_drag",
            ],
            from: Primary,
            reaches: Ink { alt: OnPrimaryContainer, on: TRACKS, need: LEGIBLE },
        },
        // ---------------------------------------------- THE SECONDARY: what
        // is selected, and what is on.
        //
        // The ground of a ticked box and a chosen radio. Its ink is the mark,
        // the tertiary's row below, which this table also chooses, so the
        // ground is taken outright and the mark follows it.
        Accented {
            tokens: &["color_inset_active"],
            from: SecondaryContainer,
            reaches: Ground { alt: Secondary, ink: "color_mark_active", need: LEGIBLE },
        },
        // The ground under a selected row: a drop down's chosen item, a menu
        // row, a combo box, a file tree's selected file, and the on-state of
        // anything built out of a radio, the wheel picker's band and the
        // time picker's chosen plate. `color_highlight` is the file tree's
        // own name for the same thing.
        Accented {
            tokens: &["color_outset_active", "color_outset_1_active", "color_outset_2_active", "color_highlight"],
            from: SecondaryContainer,
            reaches: Ground { alt: Secondary, ink: "color_label_inner_active", need: READABLE },
        },
        // The ink on those grounds, and the one thing a tab has to say it is
        // the one you are on -- `tab` draws this label straight onto the
        // page, so it is held to the page as well as to the fills.
        Accented {
            tokens: &["color_label_inner_active"],
            from: Secondary,
            reaches: Ink { alt: OnSecondaryContainer, on: SELECTED, need: READABLE },
        },
        // -------------------------------------------- THE BACKGROUND: the
        // grounds of the controls, which are the page's and not an accent's.
        //
        // The fills a pointer or a press puts up, and the controls at rest.
        // Each is a wash of white or of black over the page, so over a
        // coloured page it already carries the page's hue, thinned; the lean
        // puts back the colour the wash thinned out, in the background's hue
        // and in step with the saturation slider -- none of it at nought,
        // where the page is a grey and so is every ground on it.
        Accented {
            tokens: &[
                "color_outset_hover", "color_outset_down", "color_outset_drag",
                "color_outset_1_hover", "color_outset_1_down", "color_outset_1_drag",
                "color_outset_2_hover", "color_outset_2_down", "color_outset_2_drag",
            ],
            from: Background,
            reaches: Lean { most: 0.45, ink: "color_label_inner_hover", need: READABLE, waits: true },
        },
        Accented {
            tokens: &["color_inset_hover", "color_inset_down", "color_inset_drag"],
            from: Background,
            reaches: Lean { most: 0.45, ink: "color_text", need: READABLE, waits: true },
        },
        Accented {
            tokens: &[
                "color_outset", "color_outset_focus",
                "color_outset_1", "color_outset_1_focus",
                "color_outset_2", "color_outset_2_focus",
            ],
            from: Background,
            reaches: Lean { most: 0.30, ink: "color_label_inner", need: READABLE, waits: true },
        },
        // `color_icon_inactive` and `color_mark_empty` are here because both
        // files say they ARE the inset -- an icon with nothing behind it and
        // a mark with nothing in it are the field they sit in. A theme that
        // leant the inset and left those two would be a theme whose exported
        // FILE re-derived them off the lean while the pins over the base
        // object did not, which is one theme with two answers.
        Accented {
            tokens: &[
                "color_inset", "color_inset_focus", "color_inset_empty",
                "color_icon_inactive", "color_mark_empty",
            ],
            from: Background,
            reaches: Lean { most: 0.30, ink: "color_text", need: READABLE, waits: true },
        },
        // ----------------------------------------------- THE TERTIARY: what
        // is being pointed out.
        //
        // The ground behind selected TEXT: a text field's selection, and the
        // one inside a slider's editable value. The words on it are
        // `color_text`, one token for the whole app, so the ground is what
        // has to give way here -- and it gives way as far as the base
        // theme's own selection, never further.
        // The ground behind selected TEXT: a text field's selection, and the
        // one inside a slider's editable value.
        //
        // A wash and not a fill, which is the whole of why this is a lean.
        // A text input draws its selection OVER the glyphs, so the base
        // theme's white-at-a-quarter lets the words through; an opaque
        // colour in the same place is a coloured block with the text gone
        // under it. Every readability sweep passed that block -- the words
        // were still a bar clear of the ground they were no longer on -- and
        // the screen showed it in one glance, which is what the screen is
        // for. So the alpha is the base theme's and only the colour moves.
        //
        // The family's BASE and not its container, because the base is the
        // member that follows the page: light in a dark theme, dark in a
        // light one, which is the same way round as the wash it replaces.
        // And no waiting on the slider -- a selection is a selection as soon
        // as there is a palette.
        Accented {
            tokens: &[
                "color_selection_hover",
                "color_selection_focus",
                "color_selection_down",
                "color_bg_highlight_inline",
            ],
            from: Tertiary,
            reaches: Lean { most: 0.85, ink: "color_text", need: READABLE, waits: false },
        },
        // The caret, in a text field and in a slider's editable value.
        Accented {
            tokens: &["color_text_cursor"],
            from: Tertiary,
            reaches: Ink { alt: OnTertiaryContainer, on: &["color_inset", "color_bg_app"], need: LEGIBLE },
        },
        // The mark: a check box's tick, a radio's dot, a menu row's tick and
        // a switch's knob while it is on. A tick points out that a thing is
        // chosen, and it is the one piece of the third colour an ordinary
        // screen of controls shows at rest -- without it the tertiary lived
        // only in a selection and a caret. It is read on the ticked box, the
        // secondary's ground below, so a ticked box is two of the palette's
        // colours at once and the pair is measured like any other.
        Accented {
            tokens: &["color_mark_active", "color_mark_active_hover", "color_mark_focus", "color_mark_down"],
            from: Tertiary,
            reaches: Ink { alt: OnTertiaryContainer, on: BOXES, need: LEGIBLE },
        },
        // Where a drag would land, in a dock and on a board. Drawn over the
        // content it would replace, so it keeps the base theme's alpha and
        // takes only the hue.
        Accented { tokens: &["color_drag_target_preview"], from: Tertiary, reaches: Veil },
    ]
};

/// Words a widget writes on a ground: which widget files, the grounds -- one
/// per state the words are drawn in, the ones a pointer, a focus or a press
/// puts up as well as the one at rest -- the ink, and the bar the words
/// answer to.
struct Written {
    grounds: &'static [&'static str],
    ink: &'static str,
    need: f64,
}

/// The bar disabled words answer to. Lower than the one for words, because
/// a disabled control says it cannot be used partly by being quieter, and
/// the library holds none of its own themes to more; not nothing, because
/// the words still say what the control is.
const DISABLED_WORDS: f64 = LEGIBLE;

/// Every place a widget in this crate writes words on a ground the mapping
/// colours, state by state.
///
/// [`ACCENTED`] was written from the grounds' side: each row that makes a
/// ground names the ONE ink it gives way to. That is enough only where one
/// ink is ever written on it, and it was not. The WheelPicker wrote the body
/// ink on a slider's value fill, which the mapping pushes dark on a light
/// page to stand off its track and holds to nothing else, because nothing is
/// ever written on a fill -- and the digits in the band went dark on dark,
/// and darker again under the pointer. So this is the other side: what the
/// widgets actually draw, read off their templates, one row per ink with
/// every state's ground in it. A build measures every pair here, and every
/// ground the mapping leans or chooses gives way to every ink written on it
/// and not only to its row's own (see [`accent_pins`]).
///
/// The disabled grounds are not accented -- the mapping leaves them at the
/// base theme's washes -- but they are in the list all the same, because the
/// page under them is the builder's, and a disabled label that reads on the
/// house page need not read on a page the lightness slider moved.
const WRITTEN: &[Written] = {
    &[
        // ------------------------------------------ the faces of the controls
        Written {
            grounds: &["color_outset", "color_outset_1", "color_outset_2"],
            ink: "color_label_inner",
            need: READABLE,
        },
        Written {
            grounds: &["color_outset_focus", "color_outset_1_focus", "color_outset_2_focus"],
            ink: "color_label_inner_focus",
            need: READABLE,
        },
        Written {
            grounds: &["color_outset_hover", "color_outset_1_hover", "color_outset_2_hover"],
            ink: "color_label_inner_hover",
            need: READABLE,
        },
        // the recent-files list: a hovered row's name, in the page's text colour
        Written {
            grounds: &["color_outset_hover"],
            ink: "color_text",
            need: READABLE,
        },
        Written {
            grounds: &["color_outset_down", "color_outset_1_down", "color_outset_2_down"],
            ink: "color_label_inner_down",
            need: READABLE,
        },
        // A radio drawn as a tab writes its label on the field at rest.
        Written { grounds: &["color_inset"], ink: "color_label_inner", need: READABLE },
        // ----------------------------------------------------- a chosen row
        Written {
            grounds: &["color_outset_active", "color_outset_1_active", "color_outset_2_active", "color_highlight"],
            ink: "color_label_inner_active",
            need: READABLE,
        },
        // ------------------------------------------------------- the fields
        Written { grounds: &["color_inset"], ink: "color_text", need: READABLE },
        Written { grounds: &["color_inset_hover"], ink: "color_text_hover", need: READABLE },
        Written { grounds: &["color_inset_focus"], ink: "color_text_focus", need: READABLE },
        Written { grounds: &["color_inset_down"], ink: "color_text_down", need: READABLE },
        Written {
            grounds: &["color_inset_empty"],
            ink: "color_text_placeholder",
            need: LEGIBLE,
        },
        // The quieter line a drop zone, a tree select and a waveform write
        // under or beside their main words.
        Written {
            grounds: &["color_inset", "color_inset_hover", "color_inset_focus", "color_inset_drag"],
            ink: "color_text_meta",
            need: LEGIBLE,
        },
        // A slider's value, where the slider draws its field round it.
        Written { grounds: &["color_inset"], ink: "color_text_val", need: READABLE },
        Written { grounds: &["color_inset_hover"], ink: "color_text_hover", need: READABLE },
        Written { grounds: &["color_inset_focus"], ink: "color_text_focus", need: READABLE },
        Written { grounds: &["color_inset_drag"], ink: "color_text_down", need: READABLE },
        // The time picker's values that were not chosen, at rest and under
        // the pointer, and the wheel picker's rows away from its band.
        Written {
            grounds: &["color_inset", "color_inset_hover"],
            ink: "color_label_inner_inactive",
            need: READABLE,
        },
        Written {
            grounds: &["color_inset", "color_inset_hover", "color_inset_focus", "color_inset_drag"],
            ink: "color_label_outer_off",
            need: LEGIBLE,
        },
        // ------------------------------------------------- selected words
        Written {
            grounds: &["color_selection_hover", "color_selection_focus", "color_selection_down"],
            ink: "color_text",
            need: READABLE,
        },
        // A value input writes its typed text over the focused selection.
        Written {
            grounds: &["color_selection_focus"],
            ink: "color_text",
            need: READABLE,
        },
        Written {
            grounds: &["color_selection_focus"],
            ink: "color_label_inner",
            need: READABLE,
        },
        // A Label's own wash under selected text (draw_selection), in the
        // label's body ink.
        Written {
            grounds: &["color_selection_focus"],
            ink: "color_label_outer",
            need: READABLE,
        },
        // The selection inside a slider's value, which is only ever up while
        // the value has the focus.
        Written {
            grounds: &["color_bg_highlight_inline"],
            ink: "color_text_focus",
            need: READABLE,
        },
        // ------------------------------------------------- the tween widgets
        // The ease editor's handle readout, and the Motion inspector's lane
        // names, picker rows and time readout: words straight on the field.
        Written { grounds: &["color_inset"], ink: "color_text", need: READABLE },
        // The inspector's chips: Play, Restart, the speeds, Loop and Yoyo.
        Written { grounds: &["color_outset"], ink: "color_text", need: READABLE },
        // A Sequencer marker's label, beside its diamond on the track rows.
        Written { grounds: &["color_inset"], ink: "color_on_surface_variant", need: LEGIBLE },
        // A Sequencer track's name on a hovered or a selected row, and the
        // zoom buttons' labels under the pointer. The hover is the
        // selection's wash and not `color_highlight`: that is a chosen row's
        // ground, which gives way to `color_label_inner_active` alone, and
        // holding it to the body ink as well made it move with the text
        // contrast and the saturation, and still not read on every palette.
        Written {
            grounds: &["color_selection_hover", "color_selection_focus"],
            ink: "color_text",
            need: READABLE,
        },
        // ------------------------------------------------------ disabled
        Written {
            grounds: &["color_outset_disabled"],
            ink: "color_label_inner_disabled",
            need: DISABLED_WORDS,
        },
        Written {
            grounds: &["color_outset_disabled"],
            ink: "color_label_inner",
            need: DISABLED_WORDS,
        },
        Written {
            grounds: &["color_outset_disabled"],
            ink: "color_label_inner_inactive",
            need: DISABLED_WORDS,
        },
    ]
};



/// Every ground the mapping reads and every ink it protects, so that a build
/// can resolve them once and the gate test can hold every one of them to the
/// theme files.
fn accent_grounds() -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    let mut want = |key: &'static str| {
        if !out.contains(&key) {
            out.push(key);
        }
    };
    for row in ACCENTED {
        match row.reaches {
            Reaches::Ink { on, .. } => on.iter().for_each(|key| want(key)),
            Reaches::Ground { ink, .. } | Reaches::Lean { ink, .. } => want(ink),
            Reaches::Named | Reaches::Veil => {}
        }
    }
    for written in WRITTEN {
        written.grounds.iter().for_each(|key| want(key));
        want(written.ink);
    }
    out
}

/// A ground carried along its own lightness, keeping its hue, saturation
/// and alpha, just far enough for every ink given to read on it at its bar,
/// laid over the page: toward whichever end gets there in the shorter move.
/// `None` where neither end does.
fn stand_off(fill: u32, inks: &[(u32, f64)], page: u32) -> Option<u32> {
    let (h, s, l) = rgb_to_hsl(fill);
    let alpha = fill & 0xFF;
    let at = |l: f64| (hsl_to_rgb(h, s, l) & 0xFFFF_FF00) | alpha;
    let reads = |rgba: u32| inks.iter().all(|(ink, need)| reads_on(over(page, rgba), *ink) >= *need);
    (1..=100)
        .map(|step| step as f64 * 0.01)
        .flat_map(|by| [l - by, l + by])
        .filter(|l| (0.0..=1.0).contains(l))
        .map(at)
        .find(|rgba| reads(*rgba))
}

/// The inks [`WRITTEN`] says are written on a token, each with its bar.
fn written_on(token: &str) -> impl Iterator<Item = (&'static str, f64)> + Clone + '_ {
    WRITTEN.iter().filter(move |w| w.grounds.contains(&token)).map(|w| (w.ink, w.need))
}

/// Whether the settings name a palette that is not the house one: a
/// favourite, a harmony, or colours a suggestion or a hand named.
///
/// The whole of the mapping hangs off this. Somebody who opened the panel and
/// dragged the spacing did not ask for coloured controls and does not get
/// them; somebody who moved nothing at all still installs nothing at all,
/// which is what [`ThemeBuilderState::evaluate_apply`] promises and what the three house
/// tests hold it to. The two background sliders are not in it: they move
/// the grounds, and the grounds are not the accents.
///
/// A count under four moves it as well: its derived colours are the
/// primary's steps and not the house families, so there is nothing of the
/// house palette left to stand.
fn palette_moved(params: &BuilderParams) -> bool {
    colors_named(params) || params.color_count < 4
}

/// Whether the four colours themselves were moved off the house ones: a
/// favourite, a harmony, or colours named. [`palette_moved`] without the
/// count, which is what decides what stands UNDER a count.
fn colors_named(params: &BuilderParams) -> bool {
    let house = SeedColors::HOUSE.primary;
    (params.favourite | 0xFF) != (house | 0xFF) || params.harmony != Harmony::House || params.seeds.is_some()
}

/// The palette a count makes of four chosen colours: at four the colours
/// as they are, and under it the slots [`SeedSlot::chosen_at`] leaves out
/// derived from the primary.
///
/// * The tertiary, at three and below, is a tonal step of the primary, AWAY
///   from the page -- lighter on a dark one, darker on a light one -- where
///   the band below has room for it, so the colour of what is pointed out,
///   the tick among it, stands off the page at least as well as the primary
///   does and apart from the primary's fill.
/// * The secondary, at two and below, is a step the other way, so a ticked
///   box's ground and the tick on it are two lightnesses of the one hue and
///   never the same colour.
/// * The surface, at one, is the grey of the appearance's house page: no
///   hue, so the saturation slider has nothing to bring up, and the
///   lightness slider still carries the page from dark to light.
///
/// Same hue and saturation as the primary, HSL lightness alone moved: a
/// step is the primary a shade on, and a family grown from it is the
/// primary's family in another voice.
///
/// The steps are taken from the primary AS IT WILL READ, and inside the band
/// of lightness the rule leaves a colour of that hue alone in, because the
/// rule moves any accent that cannot be seen off the page -- and a step
/// toward the page from a primary already at the edge would be carried
/// straight back onto the primary, two families of one colour. A step is
/// measured in contrast, [`TONAL_CONTRAST`] of it, and not in lightness: a
/// yellow a fifth lighter is hardly a different yellow, a blue a fifth
/// lighter is another blue. Where a side of the band is too short for a
/// whole step it is shortened to fit, and where one side is short the two
/// may both go the other way, one step and two, whichever leaves the three
/// colours furthest apart. What comes out needs no moving, and reads as
/// every chosen colour does.
fn derived_palette(chosen: [u32; 4], count: usize, dark: bool) -> [u32; 4] {
    let count = count.clamp(1, 4);
    if count == 4 {
        return chosen;
    }
    let [primary, secondary, _, surface] = chosen;
    let primary = primary | 0xFF;
    let scheme = if dark { Scheme::Dark } else { Scheme::Light };
    let reference = house_page(scheme);
    let (hue, sat, _) = rgb_to_hsl(primary);
    let (low, high) = TONAL_RANGE;
    let lightness = |color: u32| rgb_to_hsl(color | 0xFF).2;
    // Where the primary will stand, and the page's end of the band: the
    // nearest the page a colour of this hue is let stand.
    let at = lightness(family_from_color(scheme, primary, reference).base);
    let away = if dark { 1.0 } else { -1.0 };
    let (edge_from, ink_end) = if dark { (low, high) } else { (high, low) };
    let page_end = lightness(family_from_color(scheme, hsl_to_rgb(hue, sat, edge_from), reference).base);
    // How far the band runs each way from the primary, in lightness, and a
    // colour `along` it: toward the ink where `along` is positive.
    let toward_ink = ((ink_end - at) * away).max(0.0);
    let toward_page = ((at - page_end) * away).max(0.0);
    let step = |along: f64| hsl_to_rgb(hue, sat, (at + away * along).clamp(low, high));
    // The place `want` of contrast on from `from`, going `dir` (+1 toward the
    // ink, -1 toward the page) no further than `end`. Contrast only grows
    // with the distance at one hue and saturation, so halving finds it.
    let reach = |from: f64, dir: f64, end: f64, want: f64| {
        let room = (end - from) * dir;
        if room <= 0.0 {
            return from;
        }
        if reads_on(step(from), step(end)) <= want {
            return end;
        }
        let (mut near, mut far) = (0.0f64, room);
        for _ in 0..30 {
            let middle = (near + far) / 2.0;
            if reads_on(step(from), step(from + dir * middle)) < want {
                near = middle;
            } else {
                far = middle;
            }
        }
        from + dir * far
    };
    let (ink_end, page_end) = (toward_ink, -toward_page);
    // What the whole of each side stands off the primary: two steps on one
    // side share it, each taking its square root.
    let (ink_room, page_room) = (reads_on(step(0.0), step(ink_end)), reads_on(step(0.0), step(page_end)));
    let one = |dir: f64| {
        let end = if dir > 0.0 { ink_end } else { page_end };
        reach(0.0, dir, end, TONAL_CONTRAST)
    };
    let two = |dir: f64| {
        let (end, room) = if dir > 0.0 { (ink_end, ink_room) } else { (page_end, page_room) };
        let each = TONAL_CONTRAST.min(room.sqrt());
        let first = reach(0.0, dir, end, each);
        (first, reach(first, dir, end, each))
    };
    // How far apart the colours of a choice stand: the least contrast
    // between any two of them, the primary included.
    let apart = |alongs: &[f64]| {
        let mut colors = vec![step(0.0)];
        colors.extend(alongs.iter().map(|along| step(*along)));
        let mut least = f64::INFINITY;
        for a in 0..colors.len() {
            for b in a + 1..colors.len() {
                least = least.min(reads_on(colors[a], colors[b]));
            }
        }
        least
    };
    let (secondary_along, tertiary_along) = if count == 3 {
        // Toward the ink wherever that stands off the primary as well as
        // toward the page would, since the tick has to stand off the page.
        let (ink, page) = (one(1.0), one(-1.0));
        if apart(&[ink]) + 1e-9 >= apart(&[page]) {
            (0.0, ink)
        } else {
            (0.0, page)
        }
    } else {
        // Either side of the primary -- the tick toward the ink, its ground
        // toward the page -- or both along one side, one step and two:
        // whichever leaves the three furthest apart, either side first.
        let (ink_first, ink_second) = two(1.0);
        let (page_first, page_second) = two(-1.0);
        let choices = [(one(-1.0), one(1.0)), (ink_first, ink_second), (page_second, page_first)];
        let mut best = choices[0];
        for choice in &choices[1..] {
            if apart(&[choice.0, choice.1]) > apart(&[best.0, best.1]) + 1e-9 {
                best = *choice;
            }
        }
        best
    };
    let tertiary = step(tertiary_along);
    let secondary = if count <= 2 { step(secondary_along) } else { secondary | 0xFF };
    let surface = if count == 1 { ground_swatch(BLACK, dark) } else { surface | 0xFF };
    [primary, secondary, tertiary, surface]
}

/// How far the GROUNDS of the controls lean toward the palette's background
/// colour: none of the way at nought, all of a row's `most` at one.
///
/// The accents themselves do not ask this. A mark, a value fill, a focus
/// ring, a selection, a caret -- those are the palette showing up at all,
/// and they land in full the moment the palette moves. What is a matter of
/// degree is the colour in the BACKGROUNDS, and the saturation slider is the
/// control of exactly that: the page takes this share of the background
/// colour's saturation, and so do the grounds stepped off it.
fn control_ground_lean(params: &BuilderParams) -> f64 {
    params.clamped().saturation
}

/// How an ink reads on the ground it does worst on. One token, several
/// grounds, and the worst of them decides -- the same shape as
/// [`settle_ink`] and for the same reason.
fn worst_reading(grounds: &[u32], ink: u32) -> f64 {
    grounds.iter().map(|g| reads_on(*g, ink)).fold(f64::INFINITY, f64::min)
}

/// The bar a colour this mapping chooses has to clear: the row's own, or
/// what the base theme's value for that token manages on the same grounds,
/// whichever is LOWER.
///
/// Without the second half the mapping would mostly refuse to do anything.
/// The base themes' own controls do not meet the library's bars: the dark
/// theme's check mark is white at 35% on a box that is black at 15% over the
/// page, which reads 2.7 against a graphic bar of 3, and its text selection
/// reads 1.9 against a text bar of 4.5. Held to the stated bar, every accent
/// would be rejected in favour of a fallback and the controls would stay
/// grey -- which is the defect. Held to this bar, a colour is taken whenever
/// it is no worse to read than leaving the token alone, and the stated bar
/// still applies wherever the base theme meets it.
///
/// The twentieth is there because "no worse" cannot be decided in the third
/// decimal. At the far end of the saturation slider the base theme's own
/// text selection reads 3.23 and the palette's reads 3.22, a difference no
/// eye has ever seen, and without the allowance the selection would lose its
/// colour at exactly that point on the slider and nowhere else.
fn accent_bar(need: f64, base_reading: Option<f64>) -> f64 {
    match base_reading {
        Some(reading) if reading * 0.95 < need => reading * 0.95,
        _ => need,
    }
}

/// What the mapping came to.
#[derive(Default)]
struct Accents {
    /// The tokens it pinned, as the script will write them.
    pins: Vec<(String, u32)>,
    /// Every ground a pin was chosen against that the theme does not already
    /// carry, as the VM will resolve it and not as it was composited --
    /// `color_inset_1` is a translucent rung, and it goes into the built
    /// theme's colours so that `what_build_predicts_is_what_the_vm_resolves`
    /// holds the file reading to the VM along with everything else.
    grounds: Vec<(String, u32)>,
    /// Every pair the mapping created, as `(ground, ink, bar)` in the units
    /// [`read_pairs`] measures in: the mark on the box it is drawn in, the
    /// value fill along its track, the focus ring against the page, the text
    /// selection under the words it holds, and each leaning fill under its
    /// own label. The bar is the one the choice was actually made against,
    /// [`accent_bar`] and not the row's stated `need`, or the reading would
    /// report as a failure the very case the bar was lowered for.
    ///
    /// Kept here and not folded into `theme_tokens::held_pairs`, which is the
    /// list the library holds EVERY theme and sheet to: these pairs only
    /// exist where a palette put an accent on a control, and a base theme
    /// whose check mark is grey on grey is not a theme that fails -- it is a
    /// theme with no accent in it, which is what a base theme is.
    pairs: Vec<(String, String, f64)>,
}

/// One page an accent is chosen on: the colours the build knows under it,
/// and the page itself for laying the translucent rungs over.
struct Under<'a> {
    colors: &'a BTreeMap<String, u32>,
    page: u32,
    /// The text inks at the quietest the text contrast can make them, for
    /// the rows that give way to the words on them. Chosen against these,
    /// a fill reads under every text contrast -- a louder ink only reads
    /// better on the same ground -- and so the text contrast, which is the
    /// words' own setting, never moves a fill.
    quiet: BTreeMap<String, u32>,
}

/// The pages [`accent_pins`] asks about, in the order it asks.
struct Pages<'a> {
    /// The page these settings make, with the colour taken out: the grey of
    /// the same lightness.
    grey: Under<'a>,
    /// The appearance's house page.
    house: Under<'a>,
    /// The page these settings make.
    real: Under<'a>,
}

/// The colours the mapping pins. Empty where the palette has not moved.
///
/// Two passes, because the grounds have to exist before what stands on them
/// can be chosen: the rows that make a ground go first, and the rows that put
/// an ink on one read the answers.
///
/// # Three pages
///
/// A choice is made on two pages that do not depend on the background
/// colour -- the page these settings make with its colour taken out, and the
/// appearance's house page -- and only then checked against the real page,
/// which is the one it is measured on and the one that has the last word.
///
/// That is what keeps the accents where they are while the background
/// sliders move. The palette's roles already stand off the house page and
/// not the real one (see [`build`]); if the choices made on top of them were
/// made on the real page alone, a check mark would change member of its
/// family somewhere along a slider, because a coloured page and the grey of
/// the same lightness make their translucent rungs a hair apart, and a
/// person dragging the saturation would watch an accent change under them --
/// the one thing that slider is promised not to do. Made this way, the
/// choice is the same at every saturation, and it is the house page's
/// choice wherever the page lets it read. It changes only where the real
/// page genuinely cannot hold it: the accent moves then because reading
/// forced it, and not otherwise. In practice that is the ends of the
/// lightness slider -- the darkest light page and the blackest dark one --
/// and never the saturation.
fn accent_pins<'a>(
    params: &BuilderParams,
    scheme: Scheme,
    roles: &ColorRoles,
    pages: &Pages<'a>,
) -> Accents {
    let mut out = Accents::default();
    if !palette_moved(params) {
        return out;
    }
    let lean = control_ground_lean(params);
    let [_, _, _, background] = params.palette();
    let chosen_on = [&pages.grey, &pages.house];
    let real = &pages.real;
    let mut pinned: BTreeMap<&'static str, u32> = BTreeMap::new();
    // The same pins less the grounds that lean with the saturation, which
    // is what the two steady pages are asked with: a box whose colour came
    // up with the slider is a box the steady pages have not got, and a mark
    // chosen on it would move with the slider without anything forcing it.
    let mut steady: BTreeMap<&'static str, u32> = BTreeMap::new();
    // What a token is worth right now: a pin if this build has made one yet,
    // otherwise the theme's own value under that page.
    let raw = |pinned: &BTreeMap<&'static str, u32>, under: &Under, key: &str| -> Option<u32> {
        pinned.get(key).copied().or_else(|| file_value(scheme, key, under.colors))
    };
    // The same, as a GROUND: laid over the page, because half of these are
    // translucent and a colour chosen against the black that `color_inset`
    // really is, rather than against the grey it makes on the page, is
    // chosen against something nobody ever sees. An ink is never composited
    // this way -- its alpha is what lets it read on whatever it lands on,
    // and `reads_on` spends it against the right ground.
    let ground = |pinned: &BTreeMap<&'static str, u32>, under: &Under, key: &str| {
        raw(pinned, under, key).map(|rgba| over(under.page, rgba))
    };
    // Whether the mapping also chooses what goes ON a token, in which case a
    // ground does not have to protect its ink: the ink is chosen against the
    // ground in the second pass.
    let repaired = |key: &str| ACCENTED.iter().any(|row| row.tokens.contains(&key));
    for inks_pass in [false, true] {
        for row in ACCENTED {
            if matches!(row.reaches, Reaches::Ink { .. }) != inks_pass {
                continue;
            }
            for token in row.tokens {
                let base = file_value(scheme, token, real.colors);
                let Some(source) = row.from.toward(roles, background, base.unwrap_or(WHITE)) else {
                    continue;
                };
                let value = match row.reaches {
                    Reaches::Named => source,
                    Reaches::Veil => {
                        let Some(base) = base else { continue };
                        (source & 0xFFFF_FF00) | (base & 0xFF)
                    }
                    Reaches::Ink { alt, on, need } => {
                        // The grounds under one page and the bar there.
                        let bar_on = |under: &Under| {
                            let pins = if std::ptr::eq(under, real) { &pinned } else { &steady };
                            let grounds: Vec<u32> = on.iter().filter_map(|key| ground(pins, under, key)).collect();
                            let base = file_value(scheme, token, under.colors);
                            // Words are held to the bar for words, however
                            // far short of it the base theme's own label
                            // falls. The allowance is for marks and rings,
                            // which a base theme draws grey on grey and a
                            // palette only has to draw no worse; a chosen
                            // row's label let down to the dark theme's three
                            // to one was a secondary ink at three to one on
                            // the secondary's own ground, which is the band
                            // of a WheelPicker nobody could read.
                            let bar = if need >= READABLE {
                                need
                            } else {
                                accent_bar(need, base.map(|base| worst_reading(&grounds, base)))
                            };
                            (grounds, bar)
                        };
                        let clears = |ink: u32, under: &Under| {
                            let (grounds, bar) = bar_on(under);
                            grounds.is_empty() || worst_reading(&grounds, ink) >= bar
                        };
                        let everywhere = |ink: u32| chosen_on.iter().all(|under| clears(ink, under));
                        let candidates = [source, alt.of(roles)];
                        let (real_grounds, real_bar) = bar_on(real);
                        let chosen = match candidates.into_iter().find(|ink| everywhere(*ink)) {
                            Some(ink) if clears(ink, real) => ink,
                            _ => match candidates.into_iter().find(|ink| everywhere(*ink) && clears(*ink, real)) {
                                Some(ink) => ink,
                                None if clears(source, real) => source,
                                None if clears(alt.of(roles), real) => alt.of(roles),
                                None if worst_reading(&real_grounds, WHITE) >= worst_reading(&real_grounds, BLACK) => WHITE,
                                None => BLACK,
                            },
                        };
                        for key in on {
                            if ground(&pinned, real, key).is_some() {
                                out.pairs.push((key.to_string(), token.to_string(), real_bar));
                            }
                        }
                        chosen
                    }
                    Reaches::Ground { alt, ink, need } => {
                        let bar_on = |under: &Under| {
                            let pins = if std::ptr::eq(under, real) { &pinned } else { &steady };
                            let ink_rgba = raw(pins, under, ink)?;
                            let base = file_value(scheme, token, under.colors);
                            Some((ink_rgba, accent_bar(need, base.map(|base| reads_on(over(under.page, base), ink_rgba)))))
                        };
                        let Some((_, real_bar)) = bar_on(real) else { continue };
                        let clears = |fill: u32, under: &Under| {
                            bar_on(under).is_none_or(|(ink_rgba, bar)| reads_on(over(under.page, fill), ink_rgba) >= bar)
                        };
                        let everywhere = |fill: u32| chosen_on.iter().all(|under| clears(fill, under)) && clears(fill, real);
                        let chosen = if repaired(ink) || everywhere(source) {
                            source
                        } else if everywhere(alt.of(roles)) {
                            alt.of(roles)
                        } else if clears(source, real) {
                            source
                        } else if clears(alt.of(roles), real) {
                            alt.of(roles)
                        } else {
                            continue;
                        };
                        if !repaired(ink) {
                            out.pairs.push((token.to_string(), ink.to_string(), real_bar));
                        }
                        chosen
                    }
                    Reaches::Lean { most, ink, need, waits } => {
                        let Some(base) = base else { continue };
                        let leant = |amount: f64| (vm_mix(base, source, amount) & 0xFFFF_FF00) | (base & 0xFF);
                        // The row's own ink first, then every other ink a
                        // widget writes on this token: a fill that gave way
                        // to its label and not to the hover label or the
                        // placeholder drawn on it too still swallowed words.
                        let inks = std::iter::once((ink, need)).chain(written_on(token));
                        let bar_on = |under: &Under, ink: &str, need: f64| {
                            let pins = if std::ptr::eq(under, real) { &pinned } else { &steady };
                            let ink_rgba = under.quiet.get(ink).copied().or_else(|| raw(pins, under, ink))?;
                            let bar = accent_bar(need, Some(reads_on(over(under.page, base), ink_rgba)));
                            Some((ink_rgba, bar))
                        };
                        let Some((_, real_bar)) = bar_on(real, ink, need) else { continue };
                        let reads = |amount: f64, under: &Under| {
                            inks.clone().all(|(ink, need)| {
                                bar_on(under, ink, need)
                                    .is_none_or(|(ink_rgba, bar)| reads_on(over(under.page, leant(amount)), ink_rgba) >= bar)
                            })
                        };
                        // As far as the lean asks, and then back off a
                        // twentieth at a time until the label on the fill
                        // reads again: first on the two pages that do not
                        // move with the colour, then, if it has to, on the
                        // real one. A lean that ended at nothing is not
                        // pinned at all: the token is already its base value.
                        let mut amount = if waits { most * lean } else { most };
                        while amount > 0.0 && !chosen_on.iter().all(|under| reads(amount, under)) {
                            amount -= 0.05;
                        }
                        while amount > 0.0 && !reads(amount, real) {
                            amount -= 0.05;
                        }
                        if amount <= 0.0 {
                            continue;
                        }
                        if !repaired(ink) {
                            out.pairs.push((token.to_string(), ink.to_string(), real_bar));
                        }
                        // And every other ink written on it, at the bar the
                        // lean gave way to, so the reading asks what the
                        // choice was made against.
                        for (written, need) in written_on(token) {
                            if written == ink || repaired(written) {
                                continue;
                            }
                            if let Some((_, bar)) = bar_on(real, written, need) {
                                out.pairs.push((token.to_string(), written.to_string(), bar));
                            }
                        }
                        leant(amount)
                    }
                };
                pinned.insert(token, value);
                if row.from != Source::Background {
                    steady.insert(token, value);
                }
            }
        }
    }
    // A chosen ground the words on it still cannot be read on. The ink row
    // above has already done what an ink can -- the palette's own member,
    // its other one, then the plainer of black and white -- and on some
    // palettes even that stands a quarter short on a container of middling
    // lightness. So the ground gives way instead: the same hue and
    // saturation, carried lighter or darker, whichever is the shorter way,
    // until the words on it read. A selection a person cannot read the
    // chosen row in is worse than a selection a shade off the palette.
    for row in ACCENTED {
        let Reaches::Ground { .. } = row.reaches else { continue };
        for token in row.tokens {
            let Some(fill) = pinned.get(token).copied() else { continue };
            let inks: Vec<(u32, f64)> = written_on(token)
                .filter_map(|(ink, need)| raw(&pinned, real, ink).map(|rgba| (rgba, need)))
                .collect();
            if inks.iter().all(|(ink, need)| reads_on(over(real.page, fill), *ink) >= *need) {
                continue;
            }
            if let Some(moved) = stand_off(fill, &inks, real.page) {
                pinned.insert(token, moved);
            }
        }
    }
    // Every pair a widget writes, measured whether or not a row above chose
    // either half of it: the reading is what says a built theme can be read,
    // and a pair it never looks at is a pair nothing stops from failing. The
    // bar is the row's own or what the ground the base theme gives it would
    // manage under the same ink, whichever is lower, as for every other pair
    // the mapping answers for.
    for written in WRITTEN {
        for key in written.grounds {
            if out.pairs.iter().any(|(g, i, _)| g == key && i == written.ink) {
                continue;
            }
            let (Some(_), Some(ink), Some(base)) = (
                ground(&pinned, real, key),
                real.quiet.get(written.ink).copied().or_else(|| raw(&pinned, real, written.ink)),
                file_value(scheme, key, real.colors),
            ) else {
                continue;
            };
            // Asked with the quietest ink the text contrast allows, as the
            // choices above are: a louder one only reads better on the same
            // ground, so what holds here holds under every text contrast.
            let chosen = ACCENTED
                .iter()
                .any(|row| matches!(row.reaches, Reaches::Ground { .. }) && row.tokens.contains(key));
            let bar = if chosen {
                written.need
            } else {
                accent_bar(written.need, Some(reads_on(over(real.page, base), ink)))
            };
            out.pairs.push((key.to_string(), written.ink.to_string(), bar));
        }
    }
    // The grounds and the inks, where the theme does not already carry one:
    // a token this mapping measured on is a token the VM will be held to.
    let wanted = accent_grounds().into_iter().chain(ACCENTED.iter().flat_map(|row| row.tokens.iter().copied()));
    for key in wanted {
        if real.colors.contains_key(key) || pinned.contains_key(key) || out.grounds.iter().any(|(k, _)| k == key) {
            continue;
        }
        if let Some(rgba) = file_value(scheme, key, real.colors) {
            out.grounds.push((key.to_string(), rgba));
        }
    }
    out.pins = pinned.into_iter().map(|(key, rgba)| (key.to_string(), rgba)).collect();
    out
}

#[derive(Clone, Debug, PartialEq)]
pub struct BuiltTheme {
    pub params: BuilderParams,
    pub scheme: Scheme,
    pub globals: Vec<(String, ThemeVal)>,
    pub overrides: Vec<(String, ThemeVal)>,
    pub readability: Readability,
    pub crowding: Option<SeedSlot>,
}

impl BuiltTheme {
    pub fn to_install(&self) -> ThemeInstall {
        ThemeInstall {
            label: "built".into(), base: self.scheme.theme_name().into(), sheet: None,
            globals: self.globals.clone(), overrides: self.overrides.clone(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) enum BuildPalette {
    #[default]
    House,
    Explicit { colors: Vec<u32> },
    Grown { favourite: u32, harmony: Harmony, mood: Option<Mood> },
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct BuildSpec {
    pub dark: Option<bool>,
    pub color_count: Option<usize>,
    pub palette: BuildPalette,
    pub roll: bool,
    pub seed: Option<u64>,
    pub seed_slot: Option<SeedSlot>,
    pub follow_surface: Option<bool>,
    pub saturation: Option<f64>,
    pub lightness: Option<f64>,
    pub font_size: Option<f64>,
    pub font_contrast: Option<f64>,
    pub text_contrast: Option<f64>,
    pub text_color: Option<Option<SeedSlot>>,
    pub text_tint: Option<f64>,
    pub spacing: Option<f64>,
    pub roundness: Option<f64>,
}

#[derive(Clone, Debug)]
pub(crate) struct ThemeBuildResult {
    pub built: BuiltTheme,
    pub install: ThemeInstall,
}

pub(crate) fn parse_hex_rgb(text: &str) -> Option<u32> { super::hex(text).map(|c| c | 0xff) }

pub(crate) fn harmony_name(h: Harmony) -> &'static str {
    match h { Harmony::House => "house", Harmony::Single => "single", Harmony::Analogous => "analogous",
        Harmony::Complementary => "complementary", Harmony::Split => "split", Harmony::Triadic => "triadic" }
}
pub(crate) fn parse_harmony(name: &str) -> Option<Harmony> { Harmony::ALL.into_iter().find(|h| harmony_name(*h) == name) }
pub(crate) fn parse_mood(name: &str) -> Option<Option<Mood>> {
    if name == "plain" { Some(None) } else { Mood::ALL.into_iter().find(|m| m.label() == name).map(Some) }
}
pub(crate) fn seed_slot_name(slot: SeedSlot) -> &'static str {
    match slot { SeedSlot::Primary => "primary", SeedSlot::Secondary => "secondary",
        SeedSlot::Tertiary => "tertiary", SeedSlot::Surface => "surface" }
}
pub(crate) fn parse_seed_slot(name: &str) -> Option<SeedSlot> { SeedSlot::ALL.into_iter().find(|s| seed_slot_name(*s) == name) }

pub(crate) fn build_from_spec(spec: &BuildSpec, next_seed: u64) -> Result<ThemeBuildResult, String> {
    if !spec.roll && spec.seed.is_some() { return Err("seed requires roll: true".into()) }
    if let (Some(dark), Some(l)) = (spec.dark, spec.lightness) {
        if dark != (l <= 50.) { return Err("dark and lightness disagree: lightness <= 50 is dark, > 50 is light".into()) }
    }
    let sliders = [(BuildRow::Saturation, spec.saturation, 0., 100.), (BuildRow::Lightness, spec.lightness, 0., 100.),
        (BuildRow::FontSize, spec.font_size, 6., 30.), (BuildRow::FontContrast, spec.font_contrast, 0., 8.),
        (BuildRow::TextContrast, spec.text_contrast, 4.5, 21.), (BuildRow::Spacing, spec.spacing, 2., 20.),
        (BuildRow::Roundness, spec.roundness, 0., 20.), (BuildRow::TextTint, spec.text_tint, 0., 100.)];
    for (row, value, min, max) in sliders {
        if value.is_some_and(|v| !(min..=max).contains(&v)) { return Err(format!("{row:?} must be in {min}..={max}")) }
    }
    let dark = spec.dark.unwrap_or_else(|| spec.lightness.is_none_or(|l| l <= 50.));
    let mut p = BuilderParams::house(dark);
    if spec.roll { p = rolled_from(&p, spec.seed.unwrap_or(next_seed)).0; }
    let count = spec.color_count.unwrap_or_else(|| match &spec.palette {
        BuildPalette::Explicit { colors } => colors.len(), _ => p.color_count,
    });
    if !(1..=4).contains(&count) { return Err("color_count must be 1 through 4".into()) }
    let slot = spec.seed_slot.unwrap_or(SeedSlot::Primary);
    for s in [Some(slot), spec.text_color.flatten()].into_iter().flatten() {
        if !s.chosen_at(count) { return Err(format!("{} is unavailable at color_count {count}", seed_slot_name(s))) }
    }
    p.color_count = count;
    match &spec.palette {
        BuildPalette::House => (),
        BuildPalette::Explicit { colors } => {
            if colors.len() != count && colors.len() != 4 { return Err("palette length must equal color_count or 4".into()) }
            let mut palette = p.chosen_palette();
            for (slot, &color) in SeedSlot::chosen(colors.len()).into_iter().zip(colors) { palette[slot.index()] = color; }
            p = p.with_palette(palette);
        }
        BuildPalette::Grown { favourite, harmony, mood } => {
            let suggestion = if count == 1 {
                single(in_mood(*favourite, *mood), String::new(), *mood, Some(*harmony), p.dark())
            } else {
                grown_for_count(grown_at(slot, *favourite, *harmony, *mood), slot, count, p.dark())
            };
            p = suggestion.params(p);
        }
    }
    let picked = !matches!(spec.palette, BuildPalette::House);
    if count >= 2 && spec.follow_surface.unwrap_or(picked) && (!spec.roll || picked) {
        let (s, l) = surface_sliders(p.palette()[3], p.dark());
        p.saturation = s;
        p.lightness = l;
    }
    for (row, value, _, _) in sliders { if let Some(v) = value { p = row.moved(p, v); } }
    p.text_color = spec.text_color.unwrap_or(p.text_color.filter(|s| s.chosen_at(count)));
    if spec.text_tint.is_none() && p.text_color.is_some() && p.text_tint == 0. { p.text_tint = TB_FIRST_TINT; }
    p.seed_slot = slot;
    let built = build(&p);
    Ok(ThemeBuildResult { install: built.to_install(), built })
}

/// Build a theme from a panel's settings. Pure: the same settings are the
/// same scalar installation and readability measurements.
///
/// Four things, in the order they depend on one another:
///
/// * The accents. With the palette untouched these are the house roles,
///   exactly. Otherwise the three brand families are the palette's first
///   three colours through `roles_from_colors` -- the colours THEMSELVES,
///   byte for byte, nothing moving them at all.
/// * The page, from the background colour and the two background sliders,
///   and moved clear of those accents where it has to be ([`page_scheme_and_crowding`]). It
///   goes in as `color_bg_app` and `color_fg_app` written into a copy of the
///   base theme's source, so the surface ladder, the opaque ladder and every
///   wash laid over them re-derive from it. The accents are still chosen
///   against the appearance's HOUSE page rather than this one, so that no
///   background slider can move an accent.
/// * The text, put to the page at the text contrast asked for
///   ([`text_inks`]), which is also what keeps words readable on a page the
///   lightness slider has moved.
/// * The older tokens the classic controls draw from ([`ACCENTED`]).
///
/// The reading that comes back is over the theme after all of that, and
/// `every_built_theme_reads` holds it to the bar for every hue, both ends of
/// both background sliders and both appearances.
pub fn build(params: &BuilderParams) -> BuiltTheme {
    let params = params.clamped();
    // The page first, because the appearance follows it: a pick the slider's
    // own half cannot be told from sends the page across the middle, and the
    // base theme this whole build derives from goes with it.
    let ((bg, fg), scheme, crowding) = grounds_and_crowding(&params);
    let reference = house_page(scheme);
    let roles = if palette_moved(&params) {
        let [primary, secondary, tertiary, _] = params.palette();
        roles_from_colors([primary, secondary, tertiary], scheme, reference)
    } else {
        roles_for(scheme)
    };

    // The globals, in file order, and only the ones that moved: a build with
    // none derives from the base object, keeps its fonts for free, and costs
    // a slider drag nothing but forty literals.
    let mut globals: Vec<(String, ThemeVal)> = Vec::new();
    for key in DIMENSIONS {
        let value = params.dimension(key);
        if file_number(scheme, key).is_none_or(|house| (house - value).abs() > 1e-9) {
            globals.push((key.to_string(), ThemeVal::Num(value)));
        }
    }
    let page_moved = bg != reference;
    if page_moved {
        globals.push(("color_bg_app".to_string(), ThemeVal::Color(bg)));
        globals.push(("color_fg_app".to_string(), ThemeVal::Color(fg)));
    }

    // Everything measured, on the real page and on the house one: the second
    // is what the accents are chosen against as well, see `accent_pins`.
    let tint = text_tint_of(&params);
    let text_moved = page_moved || params.text_contrast != house_text_contrast(scheme) || tint.is_some();
    let known = |bg: u32, fg: u32| {
        let mut colors = page_colors(scheme, bg, fg);
        for (key, rgba) in roles.entries() {
            colors.insert(key.to_string(), rgba);
        }
        colors.insert("color_error".to_string(), KEPT_ERROR);
        colors.insert("color_warning".to_string(), KEPT_WARNING);
        let inks = text_inks(scheme, &colors, params.text_contrast, text_moved, tint);
        for (key, rgba, _) in &inks {
            colors.insert(key.to_string(), *rgba);
        }
        (colors, inks)
    };
    let (mut colors, inks) = known(bg, fg);
    let (house_bg, house_fg) = grounds(scheme, WHITE, 0.0);
    let (house_colors, _) = known(house_bg, house_fg);
    let (grey_bg, grey_fg) = grounds_of(&BuilderParams { saturation: 0.0, ..params });
    let (grey_colors, _) = known(grey_bg, grey_fg);

    let mut overrides: Vec<(String, ThemeVal)> = roles
        .entries()
        .into_iter()
        .filter(|(key, _)| *key != "color_error" && *key != "color_warning")
        .map(|(key, rgba)| (key.to_string(), ThemeVal::Color(rgba)))
        .collect();

    // And the older tokens the classic controls actually draw from, where the
    // palette moved at all. See `ACCENTED`: without this a theme grown from
    // an orange favourite had an orange page and grey controls.
    let under = |colors: &BTreeMap<String, u32>| -> BTreeMap<String, u32> {
        let quiet = text_inks(scheme, colors, READABLE, true, tint);
        quiet.into_iter().map(|(key, rgba, _)| (key.to_string(), rgba)).collect()
    };
    let pages = Pages {
        grey: Under { colors: &grey_colors, page: grey_bg, quiet: under(&grey_colors) },
        house: Under { colors: &house_colors, page: house_bg, quiet: under(&house_colors) },
        real: Under { colors: &colors, page: bg, quiet: under(&colors) },
    };
    let accents = accent_pins(&params, scheme, &roles, &pages);
    drop(pages);
    for (key, rgba) in accents.grounds.iter().chain(accents.pins.iter()) {
        colors.insert(key.clone(), *rgba);
    }
    // The text inks, less any the mapping has its own answer for: the label
    // of a selected thing is the secondary's, and a token pinned twice is a
    // token whose value depends on which line of the script came last.
    for (key, rgba, _) in inks.iter().filter(|(_, _, pin)| *pin) {
        if !accents.pins.iter().any(|(pinned, _)| pinned == key) {
            overrides.push((key.to_string(), ThemeVal::Color(*rgba)));
        }
    }
    for (key, rgba) in &accents.pins {
        overrides.push((key.clone(), ThemeVal::Color(*rgba)));
    }

    // A pin lands after the theme has derived, so the tokens made from a
    // pinned ink or accent must be rebuilt against the colours it will
    // actually wear, and so must the ones made from those in turn.
    let mut rebuilt: Vec<&str> = Vec::new();
    for key in REBUILT_TOKENS {
        let pinned = |key: &str| {
            inks.iter().any(|(ink, _, pin)| *pin && *ink == key) || accents.pins.iter().any(|(pin, _)| pin == key) || rebuilt.contains(&key)
        };
        let value = if *key == "prop_ink_dim" {
            if !pinned("color_text_meta") && !pinned("color_text") {
                continue;
            }
            makepad_widgets::theme_tokens::property_dim_ink(&mut |key| colors.get(key).copied())
        } else {
            let Some((_, light, dark)) = DERIVED_ROLES.iter().find(|(role, _, _)| role == key) else {
                continue;
            };
            let known = |key: &str| colors.get(key).copied();
            match if scheme == Scheme::Dark { *dark } else { *light } {
                RoleSource::Token(t) if pinned(t) => known(t),
                RoleSource::HalfMix(a, b) if pinned(a) || pinned(b) => {
                    known(a).zip(known(b)).map(|(a, b)| vm_mix(a, b, 0.5))
                }
                RoleSource::Mix(a, b, amount) if pinned(a) || pinned(b) => {
                    known(a).zip(known(b)).map(|(a, b)| vm_mix(a, b, amount))
                }
                RoleSource::MixTo(t, end, amount) if pinned(t) => known(t).map(|c| vm_mix(c, end, amount)),
                _ => None,
            }
        };
        if let Some(rgba) = value {
            colors.insert(key.to_string(), rgba);
            overrides.push((key.to_string(), ThemeVal::Color(rgba)));
            rebuilt.push(key);
        }
    }

    let readability = read_pairs(&colors, &accents.pairs, bg);
    BuiltTheme { params, scheme, globals, overrides, readability, crowding }
}

/// Every token that is words: the body ink and the tokens the files alias to
/// it, the quieter voices -- the value, the placeholder, the meta line, the
/// disabled text -- the labels inside and beside the controls in all their
/// states, and the two surface inks the newer widgets read.
///
/// All the states are named, the ones that are only an alias of another as
/// well: a pin lands on the object after it has derived, so a theme that
/// pinned `color_text` and left `color_text_hover` would be a theme whose
/// text changes contrast under the pointer. `color_text_on_accent` is not
/// here -- it is written on an accent, not on the page.
const TEXT_INKS: &[&str] = &[
    "color_text",
    "color_text_hl",
    "color_text_hover",
    "color_text_active",
    "color_text_focus",
    "color_text_down",
    "color_text_val",
    "color_text_disabled",
    "color_text_placeholder",
    "color_text_placeholder_hover",
    "color_text_meta",
    "color_label_inner",
    "color_label_inner_down",
    "color_label_inner_drag",
    "color_label_inner_hover",
    "color_label_inner_focus",
    "color_label_inner_active",
    "color_label_inner_inactive",
    "color_label_inner_disabled",
    "color_label_outer",
    "color_label_outer_off",
    "color_label_outer_down",
    "color_label_outer_drag",
    "color_label_outer_hover",
    "color_label_outer_focus",
    "color_label_outer_active",
    "color_label_outer_active_focus",
    "color_label_outer_disabled",
    "color_on_surface",
    "color_on_surface_variant",
];

/// The tokens made from text inks and accents, pinned after those so their
/// fields, chosen faces, value fills and quiet labels do not keep the values
/// derived before the pins landed. In order: each is made from those before it.
const REBUILT_TOKENS: &[&str] = &[
    "prop_field",
    "prop_field_hover",
    "prop_field_active",
    "prop_choice_on",
    "prop_fill",
    "prop_ink_dim",
];

/// The inks in [`TEXT_INKS`] that are NOT tinted, because they are not
/// written on the page: the label of a chosen row sits on the secondary's
/// own ground, and an ink belongs to the ground it is drawn on. Where the
/// palette has moved, [`ACCENTED`] chooses this one outright and the tint
/// would never reach it anyway; where it has not, this is what keeps it off.
const UNTINTED_INKS: &[&str] = &["color_label_inner_active"];

/// The hue the words take and how much of it, worked out from the slot
/// [`BuilderParams::text_color`] names. See [`tinted_ink`] for why a hue and
/// a saturation are the whole of what a text colour can be.
#[derive(Clone, Copy, Debug)]
struct TextTint {
    hue: f64,
    sat: f64,
}

/// What the text colour comes to for these settings, or `None` where the
/// words are left with no colour in them: no slot named, the tint at nought,
/// or a slot holding a colour with no hue to give -- a grey names none, and
/// what rounding left in one is not a colour anybody chose. All three are the
/// theme byte for byte as it was before there was a text colour at all.
fn text_tint_of(params: &BuilderParams) -> Option<TextTint> {
    let slot = params.text_color?;
    let sat = params.text_tint.clamp(0.0, 1.0);
    if sat <= 0.0 {
        return None;
    }
    let hue = text_hue_of(params, slot)?;
    Some(TextTint { hue, sat })
}

/// The hue a slot gives the words on these settings, or `None` where it
/// has none to give: the slot's colour in force, as [`BuilderParams::palette`]
/// has it, holding less colour than [`HAS_HUE`]. The one test for it, which
/// the build and the die both ask, so that a slot the die can roll is always
/// a slot the build will tint from.
fn text_hue_of(params: &BuilderParams, slot: SeedSlot) -> Option<f64> {
    let (hue, own, _) = rgb_to_hsl(params.palette()[slot.index()] | 0xFF);
    (own >= HAS_HUE).then_some(hue)
}

/// The END the tinted inks are drawn from: the text's hue and saturation at
/// the lightness that stands `want` off the page, which is the loudest thing
/// the words have to say. It takes the place of the appearance's plain end,
/// and every ink is then that colour at an alpha, exactly as every ink in
/// both theme files is white or black at one.
///
/// The hue and the saturation are the setting's and the LIGHTNESS is solved
/// for, which is the only way round it can be: a contrast ratio against a
/// page is a statement about luminance and nothing else, so the one thing a
/// coloured ink cannot also be asked for is how light it is. Solved from the
/// ratio, the contrast a person set holds exactly, and the colour they chose
/// is as much of itself as that leaves room for -- which at the top of the
/// text contrast, on either appearance, is the plain end and no colour at
/// all. There is nowhere else for it to be.
///
/// The lightness is found by halving the interval, and the LEAST one that
/// reads is the one taken -- exactly what `alpha_for` does with an alpha, and
/// for the same reason: the ratio asked for is a floor, and an ink that
/// cleared it by a third of a step would be a theme louder than the one the
/// slider says. Eight bits of channel make the reading a staircase rather
/// than a curve, so the answer is read off the quantised colour and not off
/// the luminance that was solved for.
fn tinted_ink(scheme: Scheme, page: u32, tint: TextTint, want: f64) -> u32 {
    let at = |lightness: f64| hsl_to_rgb(tint.hue, tint.sat, lightness);
    let reads = |lightness: f64| reads_on(page | 0xFF, at(lightness)) >= want;
    // The plain end is as far as an ink of this hue can go, and at it there
    // is no hue left: a lightness of one is white whatever it is asked for.
    let plain = if scheme == Scheme::Light { 0.0 } else { 1.0 };
    if !reads(plain) {
        return plain_ink(scheme);
    }
    let (mut near, mut far) = (1.0 - plain, plain);
    for _ in 0..24 {
        let middle = (near + far) / 2.0;
        if reads(middle) {
            far = middle;
        } else {
            near = middle;
        }
    }
    at(far)
}

/// The text inks put to the page at the contrast asked for, each with
/// whether it has to be pinned: all of them where the page or the contrast
/// moved, and otherwise only a surface ink the file's own page somehow
/// failed -- which the house page never does -- with the two surface inks
/// handed back regardless, because the reading is taken over them.
///
/// Every ink in both files is the appearance's plain end -- white in the dark
/// theme, black in the light one -- at some alpha, so an ink's contrast is
/// one number, its alpha, and that is all this moves: the body ink gets the
/// least alpha that stands `target` off the page, which is the ratio the
/// setting names.
///
/// The other voices keep the distance they had from the body ink on the
/// house page, measured as a ratio of contrasts: a placeholder that stood at
/// four fifths of the body's contrast stands at four fifths of the new one.
/// So a quieter theme is quieter throughout and a louder one louder, and the
/// voices do not all collapse onto one number at either end. And the two
/// surface inks the library HOLDS to a bar, on every rung of the surface
/// ladder, are then raised as far as it takes to clear it there, and failing
/// that to the plain end, which is `settle_ink`'s rule.
///
/// A `tint` is the text colour, and it changes ONE thing: which colour the
/// alphas are spent on. The ratio each voice wants is worked out first and in
/// the same words, and the end those ratios are reached from is the tint's
/// hue at the lightness the loudest of them needs ([`tinted_ink`]) instead of
/// white or black. So the text contrast goes on saying exactly what it said,
/// the quieter voices keep their distance from the body in a coloured theme
/// as in a grey one, and every ink is still translucent -- which is what lets
/// a word written on a control's ground rather than on the page follow that
/// ground instead of standing on it like a sticker. An opaque ink of the
/// right lightness for the page is the wrong lightness for the ground beside
/// it, and that is a disabled label at 1.3 to 1 on the grey it sits on.
fn text_inks(
    scheme: Scheme,
    colors: &BTreeMap<String, u32>,
    target: f64,
    moved: bool,
    tint: Option<TextTint>,
) -> Vec<(&'static str, u32, bool)> {
    let page = colors.get("color_bg_app").copied().unwrap_or(BLACK);
    let house = house_page(scheme);
    let nothing = BTreeMap::new();
    let Some(body) = file_value(scheme, "color_text", &nothing) else {
        return Vec::new();
    };
    let body_house = reads_on(house, body);
    let want_of = |base: u32| target * reads_on(house, base) / body_house;
    let surface = |key: &str| key == "color_on_surface" || key == "color_on_surface_variant";
    // The colour the tinted inks are drawn from, made to carry the loudest
    // voice it has to: an end quieter than that would leave an ink at the
    // whole of its alpha and still short of the ratio it was promised, and no
    // voice may be made quieter by a colour.
    //
    // The two surface inks are not counted, and that is what leaves a dark
    // theme any colour at all: the dark file writes `color_on_surface` as
    // pure white, which is the most its page can give, so an end that had to
    // carry it would be white itself and there would be nothing left to tint
    // with. They are served below instead -- by the tinted end where it
    // reaches their bar, and by the plain end where it does not.
    let end = tint.filter(|_| moved).map(|tint| {
        let loudest = TEXT_INKS
            .iter()
            .filter(|key| !UNTINTED_INKS.contains(*key) && !surface(key))
            .filter_map(|key| file_value(scheme, key, &nothing))
            .map(want_of)
            .fold(target, f64::max)
            .min(reads_on(page, plain_ink(scheme)));
        tinted_ink(scheme, page, tint, loudest)
    });
    let mut out = Vec::new();
    for key in TEXT_INKS {
        let Some(base) = file_value(scheme, key, &nothing) else {
            continue;
        };
        if !moved {
            // Nothing to do but put the file's own surface inks to the
            // file's own page, which they already read on.
            if *key == "color_on_surface" || *key == "color_on_surface_variant" {
                let settled = settle_ink(colors, key, base);
                out.push((*key, settled, settled != base));
            }
            continue;
        }
        let want = want_of(base);
        let from = match end {
            Some(end) if !UNTINTED_INKS.contains(key) => end,
            _ => base,
        };
        // The least alpha that stands the ratio off the page, and then as
        // much more of it as the ladder this ink is held to asks for.
        let raised = |from: u32| {
            let mut ink = alpha_for(page, from, want);
            while ink & 0xFF < 0xFF && !holds_its_bar(colors, key, ink) {
                ink += 1;
            }
            ink
        };
        let mut ink = alpha_for(page, from, want);
        if surface(key) {
            ink = raised(from);
            // A surface ink the tint cannot carry is drawn in the plain end
            // after all, as an untinted theme has it: it is held to a bar on
            // every rung of the ladder, and a rung a person cannot read is
            // worse than a word that is not the colour they chose.
            if end.is_some() && !holds_its_bar(colors, key, ink) {
                ink = raised(base);
            }
            ink = settle_ink(colors, key, ink);
        }
        out.push((*key, ink, true));
    }
    out
}

/// The least alpha at which an ink of this colour stands `want` off `page`,
/// or the whole of it where nothing less does.
fn alpha_for(page: u32, ink: u32, want: f64) -> u32 {
    let rgb = ink & 0xFFFF_FF00;
    if reads_on(page, rgb | 0xFF) < want {
        return rgb | 0xFF;
    }
    let (mut low, mut high) = (0u32, 0xFFu32);
    while low < high {
        let middle = (low + high) / 2;
        if reads_on(page, rgb | middle) >= want {
            high = middle;
        } else {
            low = middle + 1;
        }
    }
    rgb | low
}

/// Whether an ink clears the bar on every ground the library holds it to.
fn holds_its_bar(colors: &BTreeMap<String, u32>, ink_key: &str, ink: u32) -> bool {
    held_pairs()
        .iter()
        .filter(|(_, held_ink, _)| *held_ink == ink_key)
        .all(|(ground, _, need)| colors.get(*ground).is_none_or(|g| reads_on(*g | 0xFF, ink) >= *need))
}

/// The ink to draw on every ground the library holds this ink to: the one
/// given where that reaches its bar on all of them, and otherwise
/// whichever of white and black stands furthest clear of the ground it does
/// worst on. One ink for the whole ladder, because that is what the token is.
fn settle_ink(colors: &BTreeMap<String, u32>, ink_key: &str, chosen: u32) -> u32 {
    let worst = |ink: u32| {
        held_pairs()
            .iter()
            .filter(|(_, held_ink, _)| *held_ink == ink_key)
            .filter_map(|(ground, _, need)| colors.get(*ground).map(|g| reads_on(*g | 0xFF, ink) - need))
            .fold(f64::INFINITY, f64::min)
    };
    if worst(chosen) >= 0.0 {
        chosen
    } else if worst(WHITE) >= worst(BLACK) {
        WHITE
    } else {
        BLACK
    }
}

/// How a set of colours reads over every pair the library holds a theme to
/// AND every pair the accent mapping created, reported the way
/// `ThemeLab::readability` reports a mix: each ink laid over its ground
/// before it is measured, failures worst first, and the tightest pair named
/// whether or not it fails.
///
/// The two kinds of pair differ in one thing. A held pair names roles, which
/// are opaque; an accent pair can name a rung of the translucent ladder --
/// the box a check mark sits in is black at fifteen per cent -- so its ground
/// is laid over the page before anything is measured on it. A mark measured
/// against that black rather than against the grey it actually makes would be
/// held to a ground nobody ever sees.
fn read_pairs(colors: &BTreeMap<String, u32>, accents: &[(String, String, f64)], page: u32) -> Readability {
    let mut out = Readability::default();
    let mut margin: Option<f64> = None;
    let mut failures: Vec<(f64, String)> = Vec::new();
    let held = held_pairs().iter().map(|(g, i, need)| (g.to_string(), i.to_string(), *need, false));
    let accents = accents.iter().map(|(g, i, need)| (g.clone(), i.clone(), *need, true));
    for (ground, ink, need, composite) in held.chain(accents) {
        let (Some(g), Some(i)) = (colors.get(&ground), colors.get(&ink)) else {
            continue;
        };
        let g = if composite { over(page, *g) } else { *g | 0xFF };
        let stands = reads_on(g, *i);
        let line = format!("{ink} on {ground} = {stands:.2}, wants {need:.1}");
        out.measured += 1;
        if margin.is_none_or(|best| stands - need < best) {
            margin = Some(stands - need);
            out.tightest = line.clone();
        }
        if stands < need {
            failures.push((stands - need, line));
        }
    }
    failures.sort_by(|(a, _), (b, _)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    out.failures = failures.into_iter().map(|(_, line)| line).collect();
    out.margin = margin.unwrap_or(0.0);
    out
}

/// SplitMix64, as the equalizer's randomiser uses it and for its reason: the
/// same seed is the same sequence on every machine and every run, and the
/// builder owes nothing to a crate or the clock for it.
fn next_unit(state: &mut u64) -> f64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    ((z ^ (z >> 31)) >> 11) as f64 / (1u64 << 53) as f64
}


/// The random settings in two halves: the theme with the words left
/// plain, and the draw the words are then made of. The lightness is drawn
/// inside one appearance's half of the slider where one is named: a surprise
/// button does not turn a dark room white.
///
/// Two halves because the words are the one part of a roll that has to be
/// worked out on the settings the roll LANDS on and not the ones it drew: a
/// panel that holds the palette under a lock puts its own count and colours
/// back, and the words have to be chosen among what THAT palette offers.
fn random_draw(seed: u64, dark: Option<bool>) -> (BuilderParams, RolledWords) {
    let mut state = seed;
    let mut draw = |low: f64, high: f64| low + (high - low) * next_unit(&mut state);
    let hue = draw(0.0, 360.0);
    let favourite = hsl_to_rgb(hue, draw(0.55, 1.0), draw(0.42, 0.62));
    let harmony = Harmony::ALL[(draw(0.0, Harmony::ALL.len() as f64) as usize).min(Harmony::ALL.len() - 1)];
    let saturation = draw(0.0, 1.0);
    let lightness = match dark {
        Some(true) => draw(0.0, 0.5),
        Some(false) => draw(0.5, 1.0).max(0.5 + 1e-6),
        None => draw(0.0, 1.0),
    };
    let text_share = draw(0.0, 1.0);
    // Whole and half steps, which is what the sliders themselves land on.
    let mut stepped = |low: f64, high: f64| (draw(low, high) * 2.0).round() / 2.0;
    let mut params = BuilderParams {
        favourite,
        harmony,
        // A surprise is a palette the rule grew, not one off a list.
        seeds: None,
        saturation,
        lightness,
        text_contrast: READABLE,
        spacing: stepped(4.0, 9.0),
        roundness: stepped(0.0, 8.0),
        font_size: stepped(9.0, 12.0),
        font_contrast: stepped(1.5, 3.5),
        // Plain here, and drawn at the very end: see `RolledWords`.
        text_color: None,
        text_tint: 0.0,
        // Drawn below, once the closures above are done with the state.
        color_count: 4,
        // A roll names all four at once, so the primary is the one in hand.
        seed_slot: SeedSlot::Primary,
    };
    // HOW MANY COLOURS, drawn evenly across the four. The Colours row offers
    // four equal choices and a die that never rolled one of them would be
    // saying less than the row does -- a palette of two is not a rarer thing
    // than a palette of four, only a quieter one. There is nothing to weight
    // them BY: the counts are not degrees of anything, they are four
    // different palettes, and the cheapest way for a person to see all four
    // is for the die to offer all four equally often.
    //
    // Drawn after every colour and dimension, so that a seed that drew a
    // theme before the count was rolled still draws that theme's colours and
    // dimensions; only the count was new. It is set before the text contrast
    // is worked out below, because the count moves the page -- the colours
    // the count derives are the page's as much as the primary's -- and the
    // contrast is drawn across what THAT page allows.
    params.color_count = COLOR_COUNTS[(next_unit(&mut state) * COLOR_COUNTS.len() as f64) as usize % COLOR_COUNTS.len()];
    let (least, most) = params.text_contrast_range();
    params.text_contrast = least + (most - least) * text_share;
    // THE WORDS, drawn last of all and for the count's reason: every seed
    // still draws the colours, count and dimensions it drew before there was
    // a text colour to roll, and only the words are new. Both numbers are
    // always drawn, whatever the first comes to, so that nothing drawn after
    // them one day could shift with what the words landed on either.
    let words = RolledWords { pick: next_unit(&mut state), tint: next_unit(&mut state) };
    (params.clamped(), words)
}

/// What the builder rolls from `was`, and the words it drew:
/// the page's half of the lightness kept, and the words rolled onto the
/// result with no lock on either row. Pure, so a test can roll six thousand
/// times without building six thousand themes.
pub(super) fn rolled_from(was: &BuilderParams, seed: u64) -> (BuilderParams, RolledWords) {
    let (params, words) = random_draw(seed, Some(was.dark()));
    (words.on(params, was, false, false), words)
}

/// How often a roll leaves the words with no colour in them: two rolls in
/// three. The operator's ruling, and the reason under it is what plain words
/// are -- the theme as a person reads it every day. Coloured text is a
/// surprise worth having now and then, and a die that tinted the words on
/// most presses would make tinted words the ordinary thing and plain ones
/// the rarity.
pub const PLAIN_WORDS_SHARE: f64 = 2.0 / 3.0;

/// The tint a roll gives the words when it names a slot, nought to one, on
/// whole parts of a hundred as the Text tint row reads them.
///
/// The FLOOR is the panel's own first tint, the least that plainly shows:
/// the tint is a strength, and a slot named at a strength nobody can see is
/// a roll that changed the picker and nothing on the screen. The TOP stops
/// short of the whole of it, because a page of words at full colour is the
/// shout the panel's first tint was chosen to stay clear of.
///
/// Neither end is about reading, and neither has to be: the lightness of
/// every tinted ink is solved from the text contrast, and an ink that cannot
/// carry the colour to its bar is drawn in the plain end (see
/// [`tinted_ink`]), so at whatever tint a roll lands on the words hold the
/// contrast and the written pairs exactly as they did plain.
pub const ROLLED_TINT: (f64, f64) = (0.35, 0.80);

/// The words' half of a roll: two numbers off the roll's own seed, drawn
/// after everything else the roll draws. What they come to is worked out on
/// the settings the roll lands on, with [`RolledWords::on`], because which
/// slots can colour the words depends on the count and the four colours in
/// force, and a panel can hold both under a lock.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RolledWords {
    /// Below [`PLAIN_WORDS_SHARE`] the words stay plain; above it, where in
    /// the rest it falls names the slot, evenly across the ones offered.
    pick: f64,
    /// Where in [`ROLLED_TINT`] the tint lands, if a slot is named.
    tint: f64,
}

impl RolledWords {
    /// The slot these words are tinted from on `params`, or `None`.
    ///
    /// Two rolls in three, `None` outright. The third is one of the slots
    /// the Text colour picker offers at `params`' count -- the chosen ones,
    /// [`SeedSlot::chosen`], which is the list the picker is written from --
    /// evenly across those that can colour the words at all, which is what
    /// [`BuilderParams::gives_the_words_a_hue`] decides. A slot with no hue to
    /// give is left out because a roll that named it would change the picker
    /// and nothing on the screen; where the count offers no slot with a hue,
    /// the third roll is `None` as well.
    ///
    /// Only the chosen slots, never a derived one: the panel lets a text
    /// colour follow the count down to None where the count stops choosing
    /// its slot, and a roll that picked a slot its own count derives would be
    /// undone by that rule on the same press.
    pub fn color_on(&self, params: &BuilderParams) -> Option<SeedSlot> {
        if self.pick < PLAIN_WORDS_SHARE {
            return None;
        }
        let offered: Vec<SeedSlot> = SeedSlot::chosen(params.color_count)
            .into_iter()
            .filter(|slot| params.gives_the_words_a_hue(*slot))
            .collect();
        if offered.is_empty() {
            return None;
        }
        let share = (self.pick - PLAIN_WORDS_SHARE) / (1.0 - PLAIN_WORDS_SHARE);
        Some(offered[((share * offered.len() as f64) as usize).min(offered.len() - 1)])
    }

    /// The tint these words take if a slot is named: inside [`ROLLED_TINT`],
    /// on a whole part of a hundred.
    pub fn tint(&self) -> f64 {
        let (least, most) = ROLLED_TINT;
        ((least + (most - least) * self.tint) * 100.0).round() / 100.0
    }

    /// `params` with the words rolled onto it, `was` being the settings the
    /// roll was pressed from. `hold_color` and `hold_tint` are the locks a
    /// panel puts on the Text colour and Text tint rows; the engine has no
    /// locks of its own, and the builder holds neither.
    ///
    /// The tint only ever rolls together with a slot. Under None the tint
    /// row is off, and the number on it is one the person set and comes back
    /// to when they choose a slot again, so a roll that leaves the words
    /// plain leaves the tint exactly where `was` had it. A roll that names a
    /// slot draws a tint in [`ROLLED_TINT`] with it, because the tint is a
    /// strength and a slot named at nought shows nothing.
    ///
    /// The four cases of the two locks:
    ///
    /// - Neither held: the colour rolls, and the tint rolls with it when the
    ///   colour lands on a slot and stays where it was when it lands on None.
    /// - Colour held, tint free: the colour stays; the tint rolls if that
    ///   colour is a slot and stays if it is None, since a strength of no
    ///   colour is nothing on the screen.
    /// - Colour free, tint held: the colour rolls and the tint stays. With
    ///   the tint held at nought the colour rolls to None, because a slot at
    ///   a strength of nought is exactly the invisible roll the draw keeps
    ///   out.
    /// - Both held: neither moves.
    ///
    /// A held colour is still a slot the count offers, or nothing: where the
    /// count a roll lands on derives the held slot, it goes to None, as it
    /// does under a press on the Colours row. Holding the count is the
    /// palette lock's work.
    pub fn on(self, params: BuilderParams, was: &BuilderParams, hold_color: bool, hold_tint: bool) -> BuilderParams {
        let text_color = if hold_color {
            was.text_color.filter(|slot| slot.chosen_at(params.color_count))
        } else if hold_tint && was.text_tint <= 0.0 {
            None
        } else {
            self.color_on(&params)
        };
        let text_tint = if hold_tint || text_color.is_none() { was.text_tint } else { self.tint() };
        BuilderParams { text_color, text_tint, ..params }
    }
}

// ---------------------------------------------------------------------------
// Suggestions: several palettes from the one colour
// ---------------------------------------------------------------------------

/// A look for a whole palette, from the top of it to the bottom.
///
/// A harmony says WHERE the other hues sit and nothing else, so the six of
/// them from one colour are six palettes of the same strength and the same
/// brightness. The mood is the other axis, and it is the one somebody points
/// at when they say they like a palette better: the same hues drawn quietly,
/// or pale, or deep.
///
/// It moves ALL FOUR colours, the primary included. A person who does not
/// like a colour that loud is not asked to go and pick a quieter one: they
/// press "muted", and the palette -- the primary, both companions and the
/// background colour -- is calm from top to bottom, while the colour they
/// picked stays picked ([`BuilderParams::favourite`]) for the next chip. The
/// background colour takes the mood's saturation only; its lightness is the
/// lightness slider's, whatever the mood.
///
/// There is no "vivid". The plain palette of a harmony IS the vivid one --
/// the companions at the primary's own saturation and lightness, and the
/// primary exactly the colour picked -- and the six plain palettes are
/// already the first six on the strip, so a vivid mood would offer each of
/// them twice.
///
/// A mood does not touch the background sliders. Those are the person's, and
/// a chip that moved them would undo a choice made on purpose every time a
/// palette was tried.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mood {
    /// Half the colour, at the same lightness.
    Muted,
    /// Lighter and softer.
    Pastel,
    /// Darker and nearly as strong.
    Deep,
}

/// What a mood is, in numbers. Private because these are the kind of numbers
/// that are argued with by looking at them, not by reading them.
struct MoodRule {
    /// The saturation every colour keeps, as a share of its own.
    colour: f64,
    /// What is added to every accent's lightness.
    lift: f64,
}

impl Mood {
    pub const ALL: [Mood; 3] = [Mood::Muted, Mood::Pastel, Mood::Deep];

    /// The second half of a suggestion's label, in the case it is read in:
    /// "Triadic, muted".
    pub fn label(self) -> &'static str {
        match self {
            Mood::Muted => "muted",
            Mood::Pastel => "pastel",
            Mood::Deep => "deep",
        }
    }

    fn rule(self) -> MoodRule {
        match self {
            Mood::Muted => MoodRule { colour: 0.5, lift: 0.0 },
            Mood::Pastel => MoodRule { colour: 0.65, lift: 0.18 },
            Mood::Deep => MoodRule { colour: 0.9, lift: -0.18 },
        }
    }
}

/// A colour in a mood: its hue exactly, its saturation shared down and its
/// lightness lifted or dropped. No mood is the colour, byte for byte.
fn in_mood(color: u32, mood: Option<Mood>) -> u32 {
    let Some(mood) = mood else {
        return color | 0xFF;
    };
    let rule = mood.rule();
    let (hue, sat, light) = rgb_to_hsl(color | 0xFF);
    hsl_to_rgb(hue, (sat * rule.colour).clamp(0.0, 1.0), (light + rule.lift).clamp(0.08, 0.92))
}

/// The shares of the primary's saturation a companion keeps where the
/// harmony puts it on the primary's OWN hue -- the secondary of a single-hue
/// or a complementary palette, the tertiary of a single-hue one. The rule's
/// own shares, and for its reason: a companion in the same hue at the same
/// saturation and lightness is the primary a second time, and a palette of
/// three colours that shows one has lost two of them.
const SECONDARY_SHARE: f64 = 0.55;
const TERTIARY_SHARE: f64 = 0.80;

/// How near two colours have to be before a swatch is saying the same thing
/// twice: the largest difference on any channel. Measured in bytes and not in
/// hue, because a hue is a lie about a colour with no colour in it -- two
/// greys 120 degrees apart are one grey, and a favourite with no colour in it
/// grows the same palette in every harmony there is.
const SAME_COLOR: i32 = 8;

/// How much colour a colour needs before it has a hue worth sorting by, and
/// before a background colour carries any. Below this it is a grey said in a
/// roundabout way, and its hue is whatever rounding left behind.
const HAS_HUE: f64 = 0.08;

/// What a suggestion grown from a person's own list is called.
pub const OWN_LABEL: &str = "From your palettes";

/// What a suggestion taken from the built-in table is called, before its
/// number: see [`super::combinations`] for what the numbers are.
pub const COMBINATION_LABEL: &str = "Combination";

/// The number the first row of the built-in table carries.
pub const FIRST_COMBINATION: usize = 121;

/// How many schemes off ONE list a colour is offered at most -- the cap of
/// the reach, and what a list that answers a pick in its hundreds is cut back
/// to. The strip holds eight to a page, so this is six pages' worth, the
/// nearest first and the rest dropped; a cap that did not land on a page's
/// edge would leave a last page with two chips on it.
///
/// It was three pages, and the reason given was that nobody turns nine pages
/// to see what a colour can do. That reason was about a strip whose first
/// three pages were chips the rule grew: the palettes worth turning to came
/// last and were the first thing a cut took away. They come first now, so the
/// pages a person turns are the curated ones and the cut is what decides how
/// many of those there are -- and three pages of them was throwing away
/// matches worth showing.
pub const MOST_MATCHES: usize = 48;

/// How many schemes off one list a colour is offered at least -- the floor of
/// the reach, and what a list that answers a pick with two or three is made
/// up to by taking the nearest of the rest.
///
/// Ten because a row a person can choose from is a row with a choice in it,
/// and two chips are not a choice. It is a page and a bit of the strip, which
/// is on purpose: a pick the list hardly answers should still show that there
/// is more of the list past the first page, and it should not pretend to six
/// pages of matches it has not got.
pub const FEWEST_MATCHES: usize = 10;

/// What the row at `at` in the built-in table is called.
pub fn combination_label(at: usize) -> String {
    format!("{COMBINATION_LABEL} {}", FIRST_COMBINATION + at)
}

/// How far a colour in a scheme off a list may stand from the favourite and
/// still count as the one the scheme was found by -- the book's tables and a
/// person's own file alike. Sixty of the sum of hue in degrees and saturation
/// and lightness in percent, which is near enough that the scheme is
/// recognisably about that colour and loose enough that a colour picked by
/// eye off a screen finds it.
///
/// It is the PREFERRED cut and not a gate: see [`offer_reach`], where a pick
/// the list hardly answers reaches past it to fill [`FEWEST_MATCHES`] and a
/// pick it answers in its hundreds is cut back to [`MOST_MATCHES`], which is
/// a tighter tolerance by another name. What this number sets is where the
/// reach would stand if the list answered the pick about as often as a list
/// usually does.
///
/// It was thirty, and thirty is tight: a scheme built round a red and a pick
/// a shade off that red missed each other, and the row fell back to what the
/// rule grows. A scheme somebody chose reads better than one arithmetic grew
/// even when it is not an exact answer to the pick, since
/// [`adjust_scheme`] lays it round the pick anyway, so the loose measure is
/// the right one here.
pub const OWN_TOLERANCE: f64 = 60.0;

/// The four colours a palette names outright, in the form [`BuilderParams`]
/// carries them. Each is used exactly as it is: nothing in the builder moves
/// a colour somebody named.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SuggestionSeeds {
    /// The primary as it will be: the favourite exactly for a plain palette,
    /// the favourite in the mood for a mood one.
    pub primary: u32,
    pub secondary: u32,
    pub tertiary: u32,
    /// The background colour, the palette's fourth. Its hue is the page's
    /// hue and its saturation is the most the page takes, at the top of the
    /// saturation slider; its lightness is only what a chip shows it at,
    /// since how light the page is belongs to the lightness slider. A grey
    /// here is a palette whose page stays grey wherever the slider is.
    pub background: u32,
}

/// One palette offered for a favourite colour: what to call it, what it looks
/// like, and what the builder needs to grow it.
#[derive(Clone, Debug, PartialEq)]
pub struct Suggestion {
    /// Whole words, the way a person would say it: "Triadic" for a plain
    /// harmony, "Triadic, muted" for a mood, a combination's number, or
    /// [`OWN_LABEL`] for one of their own.
    pub label: String,
    /// The harmony it was grown in, or `None` for one off a list, which is in
    /// no harmony the library names.
    pub harmony: Option<Harmony>,
    /// The mood it was grown in: `None` for a plain harmony and for one off a
    /// list.
    pub mood: Option<Mood>,
    /// The four colours a swatch shows, which are the four the theme is
    /// built from: the primary as it will be, the secondary, the tertiary,
    /// and the background colour at the appearance's house page lightness.
    /// None of them depends on where the background sliders are, so a strip
    /// of these does not have to be grown again when a slider moves.
    pub colors: [u32; 4],
    pub seeds: SuggestionSeeds,
    /// Which of the four the row it stands in was grown from: the colour in
    /// that slot is the seed itself, and the chip varies the other three.
    pub grown_from: SeedSlot,
    /// How many colours it was grown for, which is how many squares its chip
    /// shows. Under four, the slots the count derives hold the derived
    /// colours in [`colors`](Suggestion::colors) and in the seeds, so the
    /// chip is exactly the palette it builds.
    pub color_count: usize,
}

impl Suggestion {
    /// This suggestion over some settings: its four colours and its harmony
    /// go on, and everything else stays as it was -- the favourite the
    /// person picked, both background sliders, the text contrast and the four
    /// dimensions. A chip is a palette, and trying one on does not undo the
    /// settings a person made on purpose.
    ///
    /// One exception, and it follows from what the favourite is for: a chip's
    /// own primary becomes the favourite. For a row grown from the primary
    /// that changes nothing, since no chip moves the pick any more and the
    /// two are the same colour. It is for the chips whose primary is not the
    /// pick -- a row grown from another slot, where the pick stays in its own
    /// slot, and a scheme shown as it was written, which is not about the
    /// pick at all. Left alone, the favourite would be a primary nobody can
    /// see any more, and a row grown from the primary afterwards would be
    /// grown from it.
    ///
    /// A chip grown for fewer than four colours names only the ones it
    /// shows. The slots its count derives keep what the settings had in them,
    /// so a person who tries a two-colour chip and goes back to four finds
    /// their own secondary and tertiary again, not the steps the chip was
    /// derived with.
    pub fn params(&self, base: BuilderParams) -> BuilderParams {
        let favourite = self.colors[0] | 0xFF;
        let mut seeds = self.seeds;
        if self.color_count < 4 {
            let kept = base.chosen_palette();
            let slots = [&mut seeds.primary, &mut seeds.secondary, &mut seeds.tertiary, &mut seeds.background];
            for (slot, value) in SeedSlot::ALL.into_iter().zip(slots) {
                if !slot.chosen_at(self.color_count) {
                    *value = kept[slot.index()] | 0xFF;
                }
            }
        }
        BuilderParams {
            favourite,
            harmony: self.harmony.unwrap_or_default(),
            seeds: Some(seeds),
            color_count: self.color_count,
            // The row this chip stands in was grown from one slot, and that
            // slot holds the colour the person is working on: it goes on
            // being the one the page has to clear.
            seed_slot: if self.grown_from.chosen_at(self.color_count) { self.grown_from } else { SeedSlot::Primary },
            ..base
        }
    }

    /// Whether these settings are wearing this palette: its count, and the
    /// colours it shows named in the slots that show them. At four that is
    /// every seed, exactly; under four the derived slots are left out, since
    /// they hold whatever the person chose before and no chip names them.
    pub fn worn_by(&self, params: &BuilderParams) -> bool {
        let Some(seeds) = params.seeds else {
            return false;
        };
        if self.color_count == 4 {
            return params.color_count == 4 && seeds == self.seeds;
        }
        let worn = [seeds.primary, seeds.secondary, seeds.tertiary, seeds.background];
        let own = [self.seeds.primary, self.seeds.secondary, self.seeds.tertiary, self.seeds.background];
        params.color_count == self.color_count
            && SeedSlot::ALL
                .into_iter()
                .filter(|slot| slot.chosen_at(self.color_count))
                .all(|slot| worn[slot.index()] == own[slot.index()])
    }

    /// The colours its chip shows, in slot order: all four at four, and
    /// under it only the chosen ones.
    pub fn shown(&self) -> Vec<u32> {
        SeedSlot::chosen(self.color_count).into_iter().map(|slot| self.colors[slot.index()]).collect()
    }
}

/// Which of the four colours in force a row of suggestions is grown from.
///
/// A palette is four colours, and a person who has found the one they are
/// sure of has not always found the primary: it can be the quiet accent, the
/// one pointed out, or the page. Whichever slot it is, the row keeps that
/// colour exactly where it is in every chip and offers what the other three
/// could be.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SeedSlot {
    #[default]
    Primary,
    Secondary,
    Tertiary,
    /// The background colour, the palette's fourth.
    Surface,
}

impl SeedSlot {
    pub const ALL: [SeedSlot; 4] = [SeedSlot::Primary, SeedSlot::Secondary, SeedSlot::Tertiary, SeedSlot::Surface];

    /// Whether this slot is chosen at a count, rather than derived from the
    /// primary. The order they go in is the order of how hard they are to
    /// find: the tertiary first, a third accent being what a person least
    /// often has in mind, then the secondary, and the surface last of all,
    /// because a page is something everyone has an opinion about. The
    /// primary is always chosen: everything else is grown from it.
    pub fn chosen_at(self, count: usize) -> bool {
        match self {
            SeedSlot::Primary => true,
            SeedSlot::Secondary => count >= 3,
            SeedSlot::Tertiary => count >= 4,
            SeedSlot::Surface => count >= 2,
        }
    }

    /// The slots chosen at a count, in slot order.
    pub fn chosen(count: usize) -> Vec<SeedSlot> {
        SeedSlot::ALL.into_iter().filter(|slot| slot.chosen_at(count)).collect()
    }

    /// Where this slot stands in [`BuilderParams::palette`] and in
    /// [`Suggestion::colors`].
    pub fn index(self) -> usize {
        match self {
            SeedSlot::Primary => 0,
            SeedSlot::Secondary => 1,
            SeedSlot::Tertiary => 2,
            SeedSlot::Surface => 3,
        }
    }

    /// The colour a row for this slot is grown from, off the settings in
    /// force: the favourite for the primary, which is what the row has always
    /// been grown from -- a mood chip names a calmer primary and leaves the
    /// colour picked where it was -- and the colour in that slot for the
    /// other three.
    ///
    /// A slot the count derives is not a seed anybody chose, and its row is
    /// the primary's: see [`SeedSlot::chosen_at`].
    pub fn seed_of(self, params: &BuilderParams) -> u32 {
        if !self.chosen_at(params.color_count) {
            return SeedSlot::Primary.seed_of(params);
        }
        match self {
            SeedSlot::Primary => params.favourite | 0xFF,
            slot => params.palette()[slot.index()] | 0xFF,
        }
    }
}

/// Where a chip off a LIST gets its colours: the book's combinations and
/// pairs, and the palettes a person has saved. The panel's "Adjust" switch,
/// and nothing else in the builder reads it.
///
/// It says nothing about the chips the rule GROWS -- the harmonies and their
/// moods. Those are arithmetic done on the pick and have no written form to
/// show instead, so they are the same chips either way and the row goes on
/// offering them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Schemes {
    /// Re-laid so that the pick is the scheme's own base and every other
    /// colour moves by the offsets it was written with ([`adjust_scheme`]).
    /// The seed keeps its slot exactly, as it does in every grown chip. The
    /// switch is on by default and this is what it means.
    #[default]
    RoundThePick,
    /// As the scheme was written, in its own colours -- offered because ONE
    /// of them stands near the pick, not because it was made for it. Pressing
    /// such a chip puts its four colours in the four slots and the pick goes
    /// with them, which is the whole of what the switch turns off.
    AsWritten,
}


/// [`suggestions`] grown from the colour in any one of the four slots: the
/// same six plain harmonies first and the same round robin of moods after,
/// with `seed` standing in `slot` in every one of them, exactly, and the
/// other three what the harmony and the mood make of it. What each slot's
/// seed does is [`grown_at`]. The primary's row is [`suggestions`], byte for
/// byte, because it is the same code.
pub fn suggestions_from(slot: SeedSlot, seed: u32) -> Vec<Suggestion> {
    let mut out: Vec<Suggestion> = Vec::with_capacity(Harmony::ALL.len() * (Mood::ALL.len() + 1));
    for harmony in Harmony::ALL {
        offer(&mut out, Some(grown_at(slot, seed, harmony, None)));
    }
    for round in 0..Mood::ALL.len() {
        for (step, harmony) in Harmony::ALL.into_iter().enumerate() {
            let mood = Mood::ALL[(round + step) % Mood::ALL.len()];
            offer(&mut out, Some(grown_at(slot, seed, harmony, Some(mood))));
        }
    }
    out
}


/// [`all_suggestions`] grown from the colour in any one of the four slots.
///
/// The lists are searched for the SEED, whichever slot it is in: a scheme is
/// found by the colour of its that stands nearest the seed, laid out again
/// from the seed by [`adjust_scheme`], and handed its roles by
/// [`scheme_at`], which puts the seed in its slot and deals the rest out by
/// the role rule with that slot already taken.
/// `schemes` is the panel's "Adjust" switch and reaches only those two lists:
/// off, each one is dressed as it was written instead ([`Schemes`]).
pub fn all_suggestions_from(slot: SeedSlot, seed: u32, own: &[Vec<u32>], schemes: Schemes) -> Vec<Suggestion> {
    let mut out: Vec<Suggestion> = Vec::new();
    let mine = matched(seed, own.iter().map(|scheme| scheme.as_slice()));
    offer_reach(&mut out, ranked_schemes(mine), OWN_TOLERANCE, |(_, scheme)| {
        list_chip(slot, seed, &scheme, OWN_LABEL.to_string(), schemes)
    });
    let book = matched(seed, COMBINATIONS.iter().copied());
    offer_reach(&mut out, ranked_schemes(book), OWN_TOLERANCE, |(at, scheme)| {
        list_chip(slot, seed, &scheme, combination_label(at), schemes)
    });
    for grown in suggestions_from(slot, seed) {
        offer(&mut out, Some(grown));
    }
    out
}

/// A [`matched`] ranking in the shape [`offer_reach`] walks: the distance
/// first and the scheme with its place beside it.
fn ranked_schemes(found: Vec<(f64, usize, Vec<u32>)>) -> impl Iterator<Item = (f64, (usize, Vec<u32>))> {
    found.into_iter().map(|(distance, place, scheme)| (distance, (place, scheme)))
}

/// One scheme off a list, dressed as a chip for a palette of `count`
/// colours -- the whole of what the "Adjust" switch decides.
///
/// On, it is laid round the pick: the seed's slot holds the pick exactly and
/// the scheme's other colours move with it. Off, the scheme is laid round its
/// OWN front colour, which is [`adjust_scheme`] doing nothing at all, and no
/// slot is held for a pick this chip is not about. One path either way, so a
/// chip that is not re-anchored is still dealt its roles by the same rule.
///
/// The front colour is the one that stood nearest the pick -- that is how the
/// scheme was found -- so a chip shown as written leads with the colour that
/// drew it into the row, in the value the list holds for it.
fn list_chip(slot: SeedSlot, seed: u32, scheme: &[u32], label: String, schemes: Schemes) -> Option<Suggestion> {
    let (slot, seed) = laid_round(slot, seed, scheme, schemes);
    scheme_at(slot, seed, scheme, label)
}

/// [`list_chip`] for a palette of fewer than four colours, where the roles
/// are dealt short and the slots the count leaves out are derived: see
/// [`scheme_for`]. The appearance reaches the DERIVED slots and nothing a
/// chip draws.
fn list_chip_for(count: usize, slot: SeedSlot, seed: u32, dark: bool, scheme: &[u32], label: String, schemes: Schemes) -> Option<Suggestion> {
    let (slot, seed) = laid_round(slot, seed, scheme, schemes);
    scheme_for(count, slot, seed, dark, scheme, label)
}

/// Which slot a chip off a list holds and which colour it holds there: the
/// seed in its own slot with the switch on, and the scheme's own front
/// colour in the primary with it off.
fn laid_round(slot: SeedSlot, seed: u32, scheme: &[u32], schemes: Schemes) -> (SeedSlot, u32) {
    match schemes {
        Schemes::RoundThePick => (slot, seed | 0xFF),
        Schemes::AsWritten => (SeedSlot::Primary, scheme.first().copied().unwrap_or(seed) | 0xFF),
    }
}

/// What a one-colour chip of the colour itself is called, and what one off
/// the book's own colours is. The moods are called by the mood.
pub const PLAIN_LABEL: &str = "Plain";
/// The words before the colour in [`book_color_label`].
pub const BOOK_COLOR_LABEL: &str = "From the book";

/// What a chip of one of the book's own colours is called: the words and
/// the colour, since the colours carry no number of their own and it is the
/// name a chip is found again by when the page flips.
pub fn book_color_label(color: u32) -> String {
    format!("{BOOK_COLOR_LABEL}, #{:06X}", color >> 8)
}

/// What the book's pair at `at` in [`PAIRS`] is called: the book numbers
/// its pairs from one, so the label is the number it carries there.
pub fn pair_label(at: usize) -> String {
    format!("{COMBINATION_LABEL} {}", at + 1)
}

/// [`suggestions_from`] for a palette of `count` colours: the rule's own
/// chips, shaped to the count, each one the palette it builds.
///
/// * Four: [`suggestions_from`], exactly.
/// * Three: each of those with its FARTHER companion let go, which is the
///   one a third accent would have been -- the nearer stays as the
///   secondary, beside the primary as it always was -- and the tertiary
///   derived. A row grown from the secondary keeps its seed where it is.
/// * Two: the primary and the surface of each, the rest derived. The
///   moods and each harmony's own background are what still set the chips
///   apart, and two that came out the same are one chip.
/// * One: the colour itself and its three moods, each alone on a grey. This
///   row is the exception to the rule that a chip holds the pick exactly,
///   and for the reason the panel hides it: a one-colour palette chooses the
///   primary and nothing else, so a chip that held the pick would be the
///   palette already in force and a row of them would be one chip repeated.
///   What it offers instead is other colours to PICK -- the moods of this
///   one, then the book's own colours ([`all_suggestions_for`]) -- which is
///   why the panel draws no carousel at one colour at all.
///
/// A slot the count does not choose cannot be a seed, and a row asked for
/// one is the primary's.
pub fn suggestions_for(count: usize, slot: SeedSlot, seed: u32, dark: bool) -> Vec<Suggestion> {
    let count = count.clamp(1, 4);
    let slot = if slot.chosen_at(count) { slot } else { SeedSlot::Primary };
    if count == 4 {
        return suggestions_from(slot, seed);
    }
    let mut out = Vec::new();
    if count == 1 {
        offer(&mut out, Some(single(seed, PLAIN_LABEL.to_string(), None, Some(Harmony::House), dark)));
        for mood in Mood::ALL {
            offer(&mut out, Some(single(in_mood(seed, Some(mood)), capitalised(mood.label()), Some(mood), Some(Harmony::House), dark)));
        }
        return out;
    }
    for grown in suggestions_from(slot, seed) {
        offer(&mut out, Some(grown_for_count(grown, slot, count, dark)));
    }
    out
}

fn grown_for_count(grown: Suggestion, slot: SeedSlot, count: usize, dark: bool) -> Suggestion {
    if count == 4 { return grown }
    let [primary, secondary, tertiary, background] = grown.colors;
    let secondary = match slot {
        SeedSlot::Secondary => secondary,
        _ if hue_gap(primary, tertiary) < hue_gap(primary, secondary) - HUE_TIE => tertiary,
        _ => secondary,
    };
    shaped(grown, [primary, secondary, tertiary, background], count, dark)
}

/// [`all_suggestions_from`] for a palette of `count` colours, in the same
/// order it uses: the person's own first, then the book's, then the rule's
/// chips from [`suggestions_for`]. See [`all_suggestions_from`] for why the
/// curated ones lead.
///
/// The book offers what it has for the count. Four colours are the
/// four-colour combinations as they always were; three are its threes, an
/// accent pair and the page; two are its PAIRS ([`PAIRS`]), an accent and a
/// page, which is the one shape a pair is the whole of. One colour is the
/// book's own colours, every colour that appears in its combinations, the
/// nearest to the pick first by [`color_distance`], each alone as the
/// primary: at one colour there is nothing to combine, and a dictionary of
/// colours somebody chose by eye is what is left to offer.
///
/// A person's own scheme serves every count: a longer one is cut down by
/// the role rule and a shorter one is filled by derivation, see
/// [`scheme_for`].
pub fn all_suggestions_for(count: usize, slot: SeedSlot, seed: u32, dark: bool, own: &[Vec<u32>], schemes: Schemes) -> Vec<Suggestion> {
    let count = count.clamp(1, 4);
    let slot = if slot.chosen_at(count) { slot } else { SeedSlot::Primary };
    if count == 4 {
        return all_suggestions_from(slot, seed, own, schemes);
    }
    let mut out: Vec<Suggestion> = Vec::new();
    let mine = matched(seed, own.iter().map(|scheme| scheme.as_slice()));
    offer_reach(&mut out, ranked_schemes(mine), OWN_TOLERANCE, |(_, scheme)| {
        list_chip_for(count, slot, seed, dark, &scheme, OWN_LABEL.to_string(), schemes)
    });
    if count == 1 {
        let mut colors: Vec<(f64, u32)> =
            book_colors().into_iter().map(|color| (color_distance(seed, color), color)).collect();
        // A stable sort, so two colours equally near stay in the book's order.
        colors.sort_by(|(a, _), (b, _)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        offer_reach(&mut out, colors, OWN_TOLERANCE, |color| {
            Some(single(color, book_color_label(color), None, None, dark))
        });
    } else {
        let book: Vec<(f64, (String, Vec<u32>))> = if count == 3 {
            // The threes are the table's first rows, so a place among them
            // is a place in the table.
            matched(seed, COMBINATIONS.iter().copied().take_while(|row| row.len() == 3))
                .into_iter()
                .map(|(distance, at, scheme)| (distance, (combination_label(at), scheme)))
                .collect()
        } else {
            matched(seed, PAIRS.iter().copied())
                .into_iter()
                .map(|(distance, at, scheme)| (distance, (pair_label(at), scheme)))
                .collect()
        };
        offer_reach(&mut out, book, OWN_TOLERANCE, |(label, scheme)| {
            list_chip_for(count, slot, seed, dark, &scheme, label, schemes)
        });
    }
    for grown in suggestions_for(count, slot, seed, dark) {
        offer(&mut out, Some(grown));
    }
    out
}

/// Every colour the book's combinations are made of, once each, in the order
/// the book first uses it: its pairs first, then the table of threes and
/// fours. The book's own dictionary of colours, in other words.
pub fn book_colors() -> Vec<u32> {
    let mut out: Vec<u32> = Vec::new();
    for row in PAIRS.iter().chain(COMBINATIONS.iter()) {
        for color in row.iter() {
            if !out.contains(color) {
                out.push(*color);
            }
        }
    }
    out
}

/// A mood's word as a chip is called by it on its own, "Muted", where the
/// rule's chips carry it after a harmony, "Triadic, muted".
fn capitalised(word: &str) -> String {
    let mut chars = word.chars();
    chars.next().map_or_else(String::new, |first| first.to_uppercase().chain(chars).collect())
}

/// A chip for a palette of one colour: that colour as the primary and the
/// rest derived from it.
///
/// The pick's own chips are in the house harmony, so a flip of the page
/// finds them again by their mood, as it finds the rule's; a colour off the
/// book is in none, and is found again by its name, as a list's is.
fn single(primary: u32, label: String, mood: Option<Mood>, harmony: Option<Harmony>, dark: bool) -> Suggestion {
    let primary = primary | 0xFF;
    let colors = derived_palette([primary, primary, primary, primary], 1, dark);
    let [primary, secondary, tertiary, background] = colors;
    Suggestion {
        label,
        harmony,
        mood,
        colors,
        seeds: SuggestionSeeds { primary, secondary, tertiary, background },
        grown_from: SeedSlot::Primary,
        color_count: 1,
    }
}

/// A four-colour chip cut to a count: the colours it keeps chosen, the rest
/// derived, under its own name and harmony.
fn shaped(grown: Suggestion, chosen: [u32; 4], count: usize, dark: bool) -> Suggestion {
    let colors = derived_palette(chosen, count, dark);
    let [primary, secondary, tertiary, background] = colors;
    Suggestion {
        colors,
        seeds: SuggestionSeeds { primary, secondary, tertiary, background },
        color_count: count,
        ..grown
    }
}

/// One scheme off a list, matched and dressed as a chip for a palette of
/// `count` colours, under three: [`scheme_at`] is the four-colour case.
///
/// The roles are dealt by the same rule, cut short. With the seed in the
/// primary or the secondary, the surface is the scheme's page end -- its
/// dark end in a dark theme, its light end in a light one -- where there is
/// a colour to spare for it, and the accent is the colour left nearest the
/// seed round the circle, or for a secondary seed the primary beside it.
/// With the seed on the surface, the primary is the colour that stands off
/// it the most, as it is at four. What a short scheme lacks is filled the
/// way the count fills it: an accent by a tonal step of the primary, a
/// surface by the rule's background.
fn scheme_for(count: usize, slot: SeedSlot, seed: u32, dark: bool, scheme: &[u32], label: String) -> Option<Suggestion> {
    let adjusted: Vec<u32> = adjust_scheme(seed, scheme).into_iter().map(|c| c | 0xFF).collect();
    if adjusted.is_empty() {
        return None;
    }
    let seed = seed | 0xFF;
    let mut rest: Vec<u32> = adjusted[1..].to_vec();
    // What a scheme too short to fill a slot borrows: the plain house
    // harmony's colour for it, which is the palette's own and the same in
    // either appearance, as every other square of a chip now is.
    let step_of = |primary: u32| grown(primary, Harmony::House, None).colors[1];
    let chosen = match slot {
        SeedSlot::Surface => {
            let primary = if rest.is_empty() {
                surface_anchor(Harmony::House, seed)
            } else {
                let mut at = 0;
                for next in 1..rest.len() {
                    if reads_on(seed, rest[next]) > reads_on(seed, rest[at]) + 1e-9 {
                        at = next;
                    }
                }
                rest.remove(at)
            };
            // A stable sort, as in `in_role_order`.
            rest.sort_by(|a, b| {
                hue_gap(primary, *a).partial_cmp(&hue_gap(primary, *b)).unwrap_or(std::cmp::Ordering::Equal)
            });
            let secondary = rest.first().copied().unwrap_or_else(|| step_of(primary));
            [primary, secondary, primary, seed]
        }
        _ => {
            // An accent is wanted beside the seed at three and not at two,
            // so the page is only taken where that leaves one.
            let spare = if count >= 3 { rest.len() >= 2 } else { !rest.is_empty() };
            let ground = spare.then(|| rest.remove(page_end(&rest)));
            let (primary, secondary) = match slot {
                SeedSlot::Secondary if !rest.is_empty() => (rest.remove(partner_of(slot, seed, &rest)), seed),
                SeedSlot::Secondary => (grown(seed, Harmony::House, None).colors[1], seed),
                _ => {
                    let anchor = adjusted[0];
                    rest.sort_by(|a, b| {
                        hue_gap(anchor, *a).partial_cmp(&hue_gap(anchor, *b)).unwrap_or(std::cmp::Ordering::Equal)
                    });
                    (anchor, rest.first().copied().unwrap_or_else(|| step_of(anchor)))
                }
            };
            let background = match ground {
                Some(ground) => ground,
                None => rule_background(Harmony::House, primary, None, [primary, secondary, step_of(primary)]),
            };
            [primary, secondary, primary, background]
        }
    };
    let colors = derived_palette(chosen, count, dark);
    let [primary, secondary, tertiary, background] = colors;
    Some(Suggestion {
        label,
        harmony: None,
        mood: None,
        colors,
        seeds: SuggestionSeeds { primary, secondary, tertiary, background },
        grown_from: slot,
        color_count: count,
    })
}

/// One more palette on the strip, unless it is one that is already on it.
fn offer(out: &mut Vec<Suggestion>, made: Option<Suggestion>) {
    let Some(made) = made else {
        return;
    };
    if !out.iter().any(|kept| the_same_palette(kept, &made)) {
        out.push(made);
    }
}

/// How far one colour stands from another for the purpose of finding a scheme
/// that already holds it: the three parts of a colour added up, the hue in
/// degrees and the saturation and lightness in percent.
///
/// The hue is taken the short way round the circle. A scheme holding a red at
/// 355 degrees is a scheme holding a red at 5 degrees, and a measure that
/// called those 350 apart would never find it.
///
/// And the hue counts only so far as the two colours HAVE one. A white, a
/// black or a grey is nowhere on the hue circle: the number `rgb_to_hsl`
/// hands back for it is whatever fell out of the arithmetic, and holding a
/// pick to it threw nearly every scheme out -- somebody picking white was
/// shown an empty row where a coloured pick fills one. So the hue term is
/// weighted by how much colour the PAIR has, the lesser of the two
/// saturations, ramped from nothing at a flat grey to its full weight at
/// [`HAS_HUE`], which is the bar the rest of this file already uses to decide
/// whether a colour has a hue at all. A ramp and not a switch, so that
/// nothing jumps as a colour crosses the bar; and full weight from the bar up,
/// so that two colours which both have a hue are measured exactly as they
/// always were. The saturation and the lightness go on counting in full for
/// everybody: they are what a grey has to be matched by.
pub fn color_distance(a: u32, b: u32) -> f64 {
    let (a_hue, a_colour, a_light) = rgb_to_hsl(a | 0xFF);
    let (b_hue, b_colour, b_light) = rgb_to_hsl(b | 0xFF);
    let turn = (a_hue - b_hue).rem_euclid(360.0);
    let has_hue = (a_colour.min(b_colour) / HAS_HUE).min(1.0);
    turn.min(360.0 - turn) * has_hue + (a_colour - b_colour).abs() * 100.0 + (a_light - b_light).abs() * 100.0
}


/// The WHOLE of a list ranked against the favourite: every scheme, how far it
/// stands, and where in the list it came from -- which is how a built-in
/// combination learns the number it is called by -- nearest first.
///
/// No tolerance here on purpose. A gate at this depth can only answer "in or
/// out", and what the row wants is an ORDER it can take as much or as little
/// of as the pick deserves: [`offer_reach`] chooses from the ranked list. Ranking the whole list costs one distance a scheme either way.
///
/// Over an iterator rather than a slice because the two lists are not the same
/// shape: a person's own is a `Vec` of `Vec`s read off a file, and the
/// built-in table is rows of a `const`.
fn matched<'a>(favourite: u32, schemes: impl Iterator<Item = &'a [u32]>) -> Vec<(f64, usize, Vec<u32>)> {
    let mut found: Vec<(f64, usize, Vec<u32>)> = Vec::new();
    for (place, scheme) in schemes.enumerate() {
        let nearest = scheme
            .iter()
            .enumerate()
            .map(|(at, colour)| (at, color_distance(favourite, *colour)))
            .min_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let Some((at, distance)) = nearest else {
            continue;
        };
        let mut ordered = Vec::with_capacity(scheme.len());
        ordered.push(scheme[at]);
        ordered.extend(scheme.iter().enumerate().filter(|(i, _)| *i != at).map(|(_, c)| *c));
        found.push((distance, place, ordered));
    }
    // A stable sort, so two schemes that stand equally near the favourite
    // stay in the order the person wrote them in.
    found.sort_by(|(a, _, _), (b, _, _)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    found
}

/// Take from one ranked list what this pick reaches, and offer each of them.
///
/// Everything inside `tolerance` is offered; then, if that came to fewer
/// than [`FEWEST_MATCHES`], the nearest of the rest are offered on until the
/// floor is met; and never more than [`MOST_MATCHES`] of one list, however
/// many it held inside the tolerance. So the tolerance is a preference and not a
/// gate: a colour the list hardly answers reaches past it rather than showing
/// two chips and a shrug, and a colour it answers three hundred times is cut
/// back to the nearest six pages, which is what a tighter tolerance would
/// have done for that colour and only for that colour. The effective reach
/// varies with the pick, which is the point.
///
/// Per list, each on its own: a person's own file and the book are two
/// different promises and neither should be able to starve the other of its
/// floor or spend the other's cap.
///
/// Counted in chips that SURVIVE: a scheme that came out as a palette already
/// offered is dropped by [`offer`] and is not one of the ten. A floor counted
/// before the dropping would leave a list that holds the same palette six
/// times showing four chips and calling it ten.
///
/// A list shorter than the floor is offered whole, which needs no saying: it
/// simply runs out.
fn offer_reach<T>(
    out: &mut Vec<Suggestion>,
    ranked: impl IntoIterator<Item = (f64, T)>,
    tolerance: f64,
    mut chip: impl FnMut(T) -> Option<Suggestion>,
) {
    let mut taken = 0usize;
    for (distance, item) in ranked {
        if taken >= MOST_MATCHES || (distance > tolerance && taken >= FEWEST_MATCHES) {
            break;
        }
        let before = out.len();
        offer(out, chip(item));
        taken += out.len() - before;
    }
}

/// A scheme's colours in the order the theme wants them: the anchor first,
/// then the secondary accent, the tertiary accent, and the background colour.
///
/// A list of colours has no roles in it. Somebody typed them in some order, or
/// a book printed them in one, and that order says which was written first and
/// nothing else -- so letting it decide which colour becomes the quiet accent
/// and which the ground hands the shape of a theme to an accident of typing.
///
/// What the colours themselves say decides it instead:
///
/// * The background is the palette's own END ([`page_end`]): of the colours
///   that are not the anchor, the one standing furthest from the middle of
///   the lightness axis. A page is what a palette is laid on, and the colour
///   a palette would be laid on is the one least usable as an accent, which
///   is the one nearest an end. Least saturated, which is what this used to
///   say, picked a grey for nineteen of the book's hundred and eight
///   four-colour rows, and a page that could never carry any colour. Ties go
///   to the less saturated, and then to written order. Only where there is
///   one to spare: three colours are the three accent families exactly, and
///   taking one of them for the page would leave the theme a family short.
///   The appearance does not come into it: a scheme is a scheme, and the row
///   of chips has to stand still while the lightness slider moves.
/// * Of the two accents left, the one NEARER the anchor round the circle is
///   the secondary and the farther one the tertiary -- the quiet accent beside
///   the favourite, the contrast accent across from it. That is what every
///   built-in harmony reads like, and a scheme from a list should read like
///   one rather than like whatever order it was typed in.
///
/// A colour with no colour in it has no hue, so it cannot be near anything:
/// it sorts as far from the anchor as a colour can get, which puts it in the
/// contrast place where it does the least harm -- a grey secondary beside a
/// strong favourite is a family that has quietly stopped being one. Where the
/// ANCHOR is the grey there is nothing at all to be near or far from, and then
/// written order decides, as it does for every other tie. Anything past the
/// fourth colour is left where it fell; nothing reads it.
fn in_role_order(scheme: &[u32]) -> Vec<u32> {
    if scheme.len() < 3 {
        return scheme.to_vec();
    }
    let anchor = scheme[0];
    let mut rest: Vec<u32> = scheme[1..].to_vec();
    let ground = (rest.len() >= 3).then(|| rest.remove(page_end(&rest)));
    // A stable sort, so colours standing equally far round the circle keep the
    // order they were written in.
    rest.sort_by(|a, b| {
        hue_gap(anchor, *a).partial_cmp(&hue_gap(anchor, *b)).unwrap_or(std::cmp::Ordering::Equal)
    });
    let accents = rest.len().min(2);
    let mut out = Vec::with_capacity(scheme.len());
    out.push(anchor);
    out.extend(rest.drain(..accents));
    out.extend(ground);
    out.extend(rest);
    out
}

/// Which of these colours is the palette's END: the one standing FURTHEST
/// from the middle of the lightness axis, either way. The less saturated of
/// two that stand equally far, and then the first written.
///
/// Furthest from the middle and not "the darkest" or "the lightest", because
/// a chip is the palette and not the page in force: asking which end by the
/// appearance made the same scheme hand its page role to one colour in the
/// dark and to another in the light, and the row of chips shuffled itself
/// under the hand every time the lightness slider crossed over. The question
/// the roles actually turn on is which colour is LEAST usable as an accent,
/// and that is the one nearest an end of the axis, whichever end it is.
///
/// The middle is the one [`scheme_at_lightness`] divides the two runs at, so
/// this and the appearance a colour's own lightness names never disagree.
/// How far from it is measured in `L*` and not in luminance, because
/// luminance is nearly all the light end -- a mid orange sits further from
/// the middle in luminance than a near-black green does, and a rule read off
/// that number would call the orange the page. `L*` is the axis the rest of
/// this file works in and the one the eye reads lightness on.
fn page_end(colors: &[u32]) -> usize {
    let middle = (dark_lightest() + LIGHT_DARKEST) / 2.0;
    let key = |packed: u32| {
        let at = lstar_of(luminance(packed | 0xFF));
        (-(at - middle).abs(), rgb_to_hsl(packed | 0xFF).1)
    };
    let mut best = 0;
    for at in 1..colors.len() {
        let (y, s) = key(colors[at]);
        let (best_y, best_s) = key(colors[best]);
        if y < best_y - 1e-12 || ((y - best_y).abs() <= 1e-12 && s < best_s) {
            best = at;
        }
    }
    best
}

/// How far round the circle a colour stands from the anchor, the short way.
///
/// Half a turn is the farthest anything can be, so that is what a colour with
/// no hue is given. An anchor with no hue makes the question meaningless
/// rather than hard, and every answer is nought: nothing is near a grey.
fn hue_gap(anchor: u32, other: u32) -> f64 {
    let (anchor_hue, anchor_colour, _) = rgb_to_hsl(anchor | 0xFF);
    if anchor_colour < HAS_HUE {
        return 0.0;
    }
    let (other_hue, other_colour, _) = rgb_to_hsl(other | 0xFF);
    if other_colour < HAS_HUE {
        return 180.0;
    }
    let turn = (anchor_hue - other_hue).rem_euclid(360.0);
    turn.min(360.0 - turn)
}

/// A scheme re-anchored on the favourite colour: the same scheme, said as
/// offsets from its first colour and then read back out from the favourite.
///
/// What a scheme knows is the SHAPE of a palette -- this much further round
/// the circle, this much paler, this much darker -- and that shape is what
/// survives being moved onto somebody else's colour. So every colour is
/// rewritten as its distance from the scheme's front colour (which
/// [`matched`] has already put there) and laid out again from the
/// favourite.
///
/// Three things happen at the edges, and they are not the same thing:
///
/// * A hue wraps, because a circle has no edges. A hue that comes out at -30
///   is 330, and that is the only answer: mirroring it to 30 -- which is what
///   subtracting from 360 does -- would turn a scheme the other way round and
///   quietly hand back a palette nobody wrote.
/// * A saturation clamps. There is nothing past nought or past full, and a
///   scheme that asked for more of either is asking for what it already has.
/// * A lightness cannot simply clamp, because two colours clamped to the same
///   end become one colour and the scheme loses a member. So when a lightness
///   leaves the range, the OTHER colours are drawn back in around the front
///   one, which does not move: each side of the anchor is compressed by the
///   one factor its own furthest member needs, so the palette keeps its
///   order and as much of its spacing as the range has room for, and `out[0]`
///   is the favourite EXACTLY, always. Where the anchor is hard against an
///   end of the axis and a side has no room to be squeezed into, that side
///   is turned round rather than lost: its members keep their distances from
///   the anchor and take them the only way there is.
///
/// That last point used to read the other way -- the front colour was
/// rescaled with the rest, on the grounds that anything else lost the
/// scheme's shape -- and it was wrong. The colour a person picked is the one
/// thing on the screen that is theirs; a chip that hands it back a shade off
/// is a chip offering somebody else's colour. The shape is what adapts, and
/// compressed toward a fixed anchor it keeps its order and its direction,
/// which is what a scheme is mostly made of.
pub fn adjust_scheme(favourite: u32, scheme: &[u32]) -> Vec<u32> {
    if scheme.is_empty() {
        return Vec::new();
    }
    // A scheme laid on its own front colour is itself, and is handed back
    // untouched rather than round-tripped through HSL, where a byte can be
    // lost. That is not an optimisation: it is what lets a chip show a
    // combination in the colours the book actually wrote down.
    if scheme[0] | 0xFF == favourite | 0xFF {
        return scheme.iter().map(|member| member | 0xFF).collect();
    }
    let (anchor_hue, anchor_colour, anchor_light) = rgb_to_hsl(scheme[0] | 0xFF);
    let (hue, colour, light) = rgb_to_hsl(favourite | 0xFF);
    let moved: Vec<(f64, f64, f64)> = scheme
        .iter()
        .map(|member| {
            let (member_hue, member_colour, member_light) = rgb_to_hsl(*member | 0xFF);
            (
                hue + (member_hue - anchor_hue),
                (colour + (member_colour - anchor_colour)).clamp(0.0, 1.0),
                light + (member_light - anchor_light),
            )
        })
        .collect();
    // A side with less room than this cannot hold a run of colours apart at
    // all: squeezed into it, two or three colours somebody wrote down come
    // back as one. Such a side is turned round instead -- see below.
    const LEAST_SIDE: f64 = 0.05;
    // Which way each member really goes. Ordinarily its own way; but where
    // the anchor is hard against an end of the axis, the members that wanted
    // to go past it go the other way instead, each as far from the anchor as
    // it asked to be. The anchor cannot move, and a run has to go somewhere:
    // turned round it keeps its spacing and stays three colours, where
    // squeezed against the end it would be one colour three times.
    let turned = |up: bool| if up { 1.0 - light < LEAST_SIDE } else { light < LEAST_SIDE };
    let reach: Vec<f64> = moved
        .iter()
        .map(|(_, _, l)| {
            let d = l - light;
            if turned(d >= 0.0) { -d } else { d }
        })
        .collect();
    // How much of each side's reach the range leaves. One side running over
    // never touches the other, so a scheme that only went dark keeps its
    // light end where it was written.
    let share = |room: f64, far: f64| if far > room { (room / far).max(0.0) } else { 1.0 };
    let above = reach.iter().fold(0.0f64, |far, d| far.max(*d));
    let below = reach.iter().fold(0.0f64, |far, d| far.max(-*d));
    let (up, down) = (share(1.0 - light, above), share(light, below));
    // `hsl_to_rgb` takes the hue round the circle for itself.
    moved
        .into_iter()
        .zip(reach)
        .map(|((h, s, _), d)| hsl_to_rgb(h, s, (light + d * if d >= 0.0 { up } else { down }).clamp(0.0, 1.0)))
        .collect()
}

/// One palette in one harmony and, if any, one mood, swatch and seeds
/// together so the two cannot say different things.
///
/// The primary is the favourite, in the mood. The companions take their hue
/// from the harmony and their saturation and lightness from the primary, so
/// a plain harmony's three colours are as strong and as light as the one
/// picked; where the harmony puts a companion on the primary's own hue it
/// keeps the rule's share of the saturation instead, and the single-hue
/// tertiary steps a fifth along the lightness axis toward the middle, or the
/// three would be one colour three times. The background is
/// [`rule_background`].
fn grown(favourite: u32, harmony: Harmony, mood: Option<Mood>) -> Suggestion {
    let primary = in_mood(favourite, mood);
    let (hue, sat, light) = rgb_to_hsl(primary);
    let (second, third) = harmony.offsets();
    let companion = |turn: f64, share: f64, step: f64| {
        if turn.rem_euclid(360.0).abs() < 0.5 {
            hsl_to_rgb(hue, sat * share, (light + step).clamp(0.0, 1.0))
        } else {
            hsl_to_rgb(hue + turn, sat, light)
        }
    };
    let secondary = companion(second, SECONDARY_SHARE, 0.0);
    let toward_middle = if light < 0.5 { 0.2 } else { -0.2 };
    let tertiary = companion(third, TERTIARY_SHARE, if harmony == Harmony::Single { toward_middle } else { 0.0 });
    let background = rule_background(harmony, favourite, mood, [primary, secondary, tertiary]);
    let seeds = SuggestionSeeds { primary, secondary, tertiary, background };
    Suggestion {
        label: match mood {
            None => harmony.label().to_string(),
            Some(mood) => format!("{}, {}", harmony.label(), mood.label()),
        },
        harmony: Some(harmony),
        mood,
        colors: [primary, secondary, tertiary, background],
        seeds,
        grown_from: SeedSlot::Primary,
        color_count: 4,
    }
}

/// Two hue gaps closer than this are one gap. Every hue here has been through
/// eight bits a channel, so the two companions of a triad stand 119.8 and
/// 120.1 degrees off the seed, and which of those is "nearer" is rounding.
const HUE_TIE: f64 = 1.0;

/// One palette in one harmony and mood, grown from the colour in `slot`.
///
/// * The primary: [`grown`], unchanged.
/// * The secondary and the tertiary: the seed is an accent, so the harmony is
///   laid round it exactly as it is laid round a primary -- [`grown`] from
///   the seed -- and the three accents are then dealt out so the seed keeps
///   its slot. Of the two companions, the primary is the one NEAREST the
///   seed round the circle when the seed is the secondary, and the one
///   FARTHEST from it when the seed is the tertiary, and the other takes the
///   slot left. That is the role rule of [`in_role_order`] read from the
///   other end: the secondary is the quiet accent beside the primary and the
///   tertiary the contrast accent across from it. Gaps within [`HUE_TIE`]
///   are a tie and keep the harmony's order, its secondary first. The
///   background is the one the rule gave the harmony round the seed.
/// * The surface: the seed is the background colour, and the accents are
///   chosen to sit on it -- see [`surface_anchor`] for where the primary
///   goes. The harmony is laid round that primary as [`grown`] lays it, and
///   its background is replaced by the seed.
///
/// A mood moves the three that are not the seed and NEVER the seed, in
/// every slot the seed can stand in, the primary included. A mood is a way
/// of saying what the colours round a colour should be like -- muted, pastel,
/// deep -- and a person who picked a green and was shown a dozen chips with
/// a dozen different greens on top of them has been offered somebody else's
/// colour a dozen times. So the mood is still laid on the whole palette, and
/// the seed's own square is then put back exactly: the companions and the
/// background are the mood's, the pick is the pick. Somebody who wants a
/// calmer version of their own colour has the colour picker and the die for
/// it, and choosing it is their move to make.
///
/// Where that leaves a mood chip showing the same palette as the plain one --
/// at one colour, where the primary is the only colour chosen and everything
/// else is derived from it -- the chip is not offered twice: `offer` drops
/// it and the row is shorter by one.
fn grown_at(slot: SeedSlot, seed: u32, harmony: Harmony, mood: Option<Mood>) -> Suggestion {
    let laid = match slot {
        SeedSlot::Primary => return with_primary(grown(seed, harmony, mood), seed | 0xFF),
        SeedSlot::Surface => grown(surface_anchor(harmony, seed), harmony, mood),
        _ => grown(seed, harmony, mood),
    };
    let seed = seed | 0xFF;
    let [first, second, third, background] = laid.colors;
    let colors = match slot {
        SeedSlot::Surface => [first, second, third, seed],
        _ => {
            let companions = [second, third];
            let at = partner_of(slot, seed, &companions);
            let (primary, other) = (companions[at], companions[1 - at]);
            match slot {
                SeedSlot::Secondary => [primary, seed, other, background],
                _ => [primary, other, seed, background],
            }
        }
    };
    let [primary, secondary, tertiary, background] = colors;
    Suggestion {
        colors,
        seeds: SuggestionSeeds { primary, secondary, tertiary, background },
        grown_from: slot,
        ..laid
    }
}

/// A suggestion with its primary named outright, swatch and seeds together:
/// what puts the pick back in its square after a mood has been laid over the
/// palette it grew.
fn with_primary(offer: Suggestion, primary: u32) -> Suggestion {
    let [_, secondary, tertiary, background] = offer.colors;
    Suggestion {
        colors: [primary, secondary, tertiary, background],
        seeds: SuggestionSeeds { primary, ..offer.seeds },
        ..offer
    }
}

/// Which of `candidates` becomes the primary beside a seed in the secondary
/// or the tertiary slot: the nearest round the circle for the secondary, the
/// farthest for the tertiary, the first written where gaps tie.
fn partner_of(slot: SeedSlot, seed: u32, candidates: &[u32]) -> usize {
    let mut best = 0;
    for at in 1..candidates.len() {
        let (gap, best_gap) = (hue_gap(seed, candidates[at]), hue_gap(seed, candidates[best]));
        let better = match slot {
            SeedSlot::Tertiary => gap > best_gap + HUE_TIE,
            _ => gap < best_gap - HUE_TIE,
        };
        if better {
            best = at;
        }
    }
    best
}

/// The primary a harmony is laid round when the seed is the background.
///
/// The rule that gives a palette its background ([`rule_background`]) puts
/// the page at a hue that depends only on the harmony and the primary's hue:
/// the primary's own for a single hue, sixty degrees on for analogous, the
/// middle of the widest empty stretch the three accents leave for the rest.
/// So it runs backwards: the primary goes where that rule would have put the
/// page on the seed's hue. The offset is read off the rule itself, from a
/// primary at hue nought, so the two cannot drift apart. Where the accents
/// leave two stretches equally wide -- complementary leaves two halves, a
/// triad three thirds -- the page is the middle of one of them, as the rule's
/// is; which one the rule itself would name turns only on where the hues
/// happen to fall round nought.
///
/// The seed's saturation says nothing about an accent, so the primary takes
/// a house primary's saturation, and its luminance too, which holds the
/// contrast that house primary stands off its own house page at: every hue
/// then reads on the page as the house colour does. A seed with no hue has
/// no hue to lay a harmony from, and the accents are laid from the house
/// primary's instead.
///
/// WHICH house primary is decided by the seed and not by the appearance in
/// force. The seed IS this palette's page, so the appearance to ask is the
/// one that page would be: a dark seed borrows the dark theme's primary, a
/// light seed the light theme's. That keeps a chip the palette's own four
/// colours -- the same in the dark and in the light, and the same wherever
/// the lightness slider is standing -- and it is the better question anyway,
/// since the accents here have to sit on THIS page and not on the house one.
fn surface_anchor(harmony: Harmony, surface: u32) -> u32 {
    let scheme = scheme_at_lightness(lstar_of(luminance(surface | 0xFF)));
    let house = roles_for(scheme).primary.base | 0xFF;
    let (house_hue, house_sat, _) = rgb_to_hsl(house);
    let y = luminance(house);
    let (hue, sat, _) = rgb_to_hsl(surface | 0xFF);
    if sat < HAS_HUE {
        return at_luminance(house_hue, house_sat, y);
    }
    let probe = grown(at_luminance(0.0, house_sat, y), harmony, None);
    let probe_hue = rgb_to_hsl(probe.colors[0]).0;
    let offset = ground_hue(harmony, probe_hue, &probe.colors[..3]) - probe_hue;
    at_luminance(hue - offset, house_sat, y)
}

/// The background colour the rule gives a palette that did not come with
/// one: a hue a designer would put behind those three accents, properly
/// saturated so that the top of the saturation slider is unmistakably a
/// colour, AT ITS OWN NATURAL LIGHTNESS.
///
/// Its own natural lightness is one half, the middle of the HSL axis, where
/// a hue is as much of itself as it can be and neither appearance has a
/// claim on it. It is the rule's own choice and it is the same in the dark
/// and in the light, which is the whole point of it: this colour is the
/// PALETTE's fourth, and a chip showing it is showing what the palette is.
/// What the built theme does with it is a separate matter and a later one --
/// the page takes its hue, the saturation slider says how much of its colour
/// the grounds carry, the lightness slider says how light the page is, and
/// the page then moves out of the accents' way. So the chip shows the
/// palette and the theme shows the setting, and dragging a slider no longer
/// makes the row of chips restless under the hand.
///
/// The hue, harmony by harmony:
///
/// * Single hue: the primary's own. A single-hue palette is one colour, and
///   its page is that colour too.
/// * Analogous: the next neighbour along, sixty degrees on from the primary,
///   so the run of neighbours carries on into the page.
/// * The others -- the house harmony, complementary, split and triadic --
///   the middle of the widest stretch of the circle the three accents leave
///   empty, the first such stretch where two are equally wide. A page in a
///   hue none of the accents is in lets every one of them stand out on it,
///   and it is the one hue the palette has room for.
///
/// The same rule for a scheme off a list that brought only three colours or
/// two ([`from_scheme`]), which is the widest-gap case on its own hues.
///
/// The saturation is the favourite's, kept between the two bounds of
/// [`RULE_GROUND_COLOUR`], in the mood. A favourite that is a grey has no
/// colour for a page to take, and its background is a grey.
fn rule_background(harmony: Harmony, favourite: u32, mood: Option<Mood>, accents: [u32; 3]) -> u32 {
    let (hue, own, _) = rgb_to_hsl(favourite | 0xFF);
    let ground_hue = ground_hue(harmony, hue, &accents);
    let share = mood.map_or(1.0, |mood| mood.rule().colour);
    let sat = if own < HAS_HUE { 0.0 } else { own.clamp(RULE_GROUND_COLOUR.0, RULE_GROUND_COLOUR.1) * share };
    hsl_to_rgb(ground_hue, sat, 0.5)
}

/// The hue [`rule_background`] gives the page, for a primary at `hue` and
/// these accents: the hue half of that rule, on its own so that
/// [`surface_anchor`] can run it backwards.
fn ground_hue(harmony: Harmony, hue: f64, accents: &[u32]) -> f64 {
    match harmony {
        Harmony::Single => hue,
        Harmony::Analogous => hue + 60.0,
        _ => widest_gap(accents).unwrap_or(hue),
    }
}

/// The middle of the widest stretch of the hue circle none of these colours
/// is in, or `None` where none of them has a hue. Colours with no colour in
/// them are passed over: a grey is not anywhere on the circle.
fn widest_gap(colors: &[u32]) -> Option<f64> {
    let mut hues: Vec<f64> = colors
        .iter()
        .map(|c| rgb_to_hsl(*c | 0xFF))
        .filter(|(_, sat, _)| *sat >= HAS_HUE)
        .map(|(hue, _, _)| hue.rem_euclid(360.0))
        .collect();
    if hues.is_empty() {
        return None;
    }
    hues.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mut best: Option<(f64, f64)> = None;
    for (at, from) in hues.iter().enumerate() {
        let to = if at + 1 < hues.len() { hues[at + 1] } else { hues[0] + 360.0 };
        let wide = to - from;
        if best.is_none_or(|(widest, _)| wide > widest + 1e-9) {
            best = Some((wide, from + wide / 2.0));
        }
    }
    best.map(|(_, middle)| middle.rem_euclid(360.0))
}

/// One scheme off a list -- a person's own or a built-in combination --
/// already matched and re-anchored, dressed as a suggestion under the name it
/// is offered by.
///
/// The roles are handed out by [`in_role_order`] and not by where a colour
/// stood in the line, and they are handed out after the re-anchoring, because
/// it is the colours the theme will actually wear that have to carry them.
/// The colours are the scheme's own: nothing is shared down or lifted, and
/// the primary is the anchor, which is the favourite wherever the scheme did
/// not have to be brought back inside the lightness range.
///
/// A four-colour scheme brings its own background colour, and it is taken as
/// it is: its hue for the page's, and its own saturation for the most the
/// page carries, with no floor -- a combination whose fourth colour is a pale
/// beige tops out at that beige. A scheme of three colours or two has none,
/// and gets one by [`rule_background`]; a scheme of two borrows its tertiary
/// from the plain house harmony first.
fn from_scheme(favourite: u32, scheme: &[u32], label: String) -> Option<Suggestion> {
    let adjusted = in_role_order(&adjust_scheme(favourite, scheme));
    if adjusted.len() < 2 {
        return None;
    }
    let primary = adjusted[0] | 0xFF;
    let secondary = adjusted[1] | 0xFF;
    let tertiary = adjusted.get(2).map_or_else(|| grown(favourite, Harmony::House, None).seeds.tertiary, |c| c | 0xFF);
    let background = match adjusted.get(3) {
        Some(ground) => *ground | 0xFF,
        None => rule_background(Harmony::House, favourite, None, [primary, secondary, tertiary]),
    };
    let seeds = SuggestionSeeds { primary, secondary, tertiary, background };
    Some(Suggestion {
        label,
        harmony: None,
        mood: None,
        colors: [primary, secondary, tertiary, background],
        seeds,
        grown_from: SeedSlot::Primary,
        color_count: 4,
    })
}

/// [`from_scheme`] for a seed in any slot: the scheme re-anchored on the
/// seed, the seed put in its slot exactly, and the rest dealt out by the
/// role rule with that slot already taken.
///
/// * The primary: [`from_scheme`], unchanged.
/// * The secondary or the tertiary: of the colours after the seed, the
///   background is taken first, where there is one to spare, exactly as
///   [`in_role_order`] takes it -- the palette's dark end in a dark theme,
///   its light end in a light one. Of what is left, the primary is chosen by
///   [`partner_of`] -- nearest the seed for a secondary, farthest for a
///   tertiary -- and the first of the rest takes the accent slot still open.
///   A scheme too short to fill it borrows it from the plain house harmony
///   round that primary, and one with no background colour gets the rule's.
/// * The surface: the seed is the background, so every other colour is an
///   accent. The primary is the one that stands off the seed the most, by
///   contrast -- it is the colour the page is covered with most -- and the
///   other two follow it in [`in_role_order`]'s way, the nearer round the
///   circle the secondary. What a short scheme lacks it borrows from the
///   plain house harmony round that primary.
fn scheme_at(slot: SeedSlot, seed: u32, scheme: &[u32], label: String) -> Option<Suggestion> {
    if slot == SeedSlot::Primary {
        return from_scheme(seed, scheme, label);
    }
    let adjusted = adjust_scheme(seed, scheme);
    if adjusted.len() < 2 {
        return None;
    }
    let seed = seed | 0xFF;
    let mut rest: Vec<u32> = adjusted[1..].iter().map(|c| c | 0xFF).collect();
    let colors = match slot {
        SeedSlot::Surface => {
            let mut at = 0;
            for next in 1..rest.len() {
                if reads_on(seed, rest[next]) > reads_on(seed, rest[at]) + 1e-9 {
                    at = next;
                }
            }
            let primary = rest.remove(at);
            // A stable sort, as in `in_role_order`.
            rest.sort_by(|a, b| {
                hue_gap(primary, *a).partial_cmp(&hue_gap(primary, *b)).unwrap_or(std::cmp::Ordering::Equal)
            });
            let plain = grown(primary, Harmony::House, None).colors;
            [primary, rest.first().copied().unwrap_or(plain[1]), rest.get(1).copied().unwrap_or(plain[2]), seed]
        }
        _ => {
            let ground = (rest.len() >= 3).then(|| rest.remove(page_end(&rest)));
            let primary = rest.remove(partner_of(slot, seed, &rest));
            let plain = grown(primary, Harmony::House, None).colors;
            let (secondary, tertiary) = match slot {
                SeedSlot::Secondary => (seed, rest.first().copied().unwrap_or(plain[2])),
                _ => (rest.first().copied().unwrap_or(plain[1]), seed),
            };
            let background = match ground {
                Some(ground) => ground,
                None => rule_background(Harmony::House, seed, None, [primary, secondary, tertiary]),
            };
            [primary, secondary, tertiary, background]
        }
    };
    let [primary, secondary, tertiary, background] = colors;
    Some(Suggestion {
        label,
        harmony: None,
        mood: None,
        colors,
        seeds: SuggestionSeeds { primary, secondary, tertiary, background },
        grown_from: slot,
        color_count: 4,
    })
}

/// Do two suggestions show the same four colours? Whole swatch or nothing: a
/// palette that shares three colours with another and differs in the fourth
/// is a different palette, and it is the fourth a person is choosing by.
fn the_same_palette(a: &Suggestion, b: &Suggestion) -> bool {
    a.colors.iter().zip(b.colors.iter()).all(|(x, y)| near(*x, *y))
}

fn near(a: u32, b: u32) -> bool {
    [24, 16, 8].into_iter().all(|shift| {
        let channel = |rgba: u32| ((rgba >> shift) & 0xFF) as i32;
        (channel(a) - channel(b)).abs() <= SAME_COLOR
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BuildRow {
    Saturation,
    Lightness,
    FontSize,
    FontContrast,
    TextContrast,
    TextTint,
    Spacing,
    Roundness,
}

impl BuildRow {
    /// Down the panel in this order, under the three headings the splash
    /// puts over them: the background's two first, then the text's four --
    /// the tint last, under the picker that turns it on -- then the shape's
    /// two. The text colour itself is not here: it is a picker and not a
    /// slider, and a row that is a different control is a different row.
    pub(in crate::tweak) const ALL: [BuildRow; 8] = [
        BuildRow::Saturation,
        BuildRow::Lightness,
        BuildRow::FontSize,
        BuildRow::FontContrast,
        BuildRow::TextContrast,
        BuildRow::TextTint,
        BuildRow::Spacing,
        BuildRow::Roundness,
    ];

    /// The slot it is drawn in. Declared in the splash and never made: see
    /// `EQ_ROW_IDS` for why every row this panel has is a fixed slot.
    pub(in crate::tweak) fn slot(self) -> LiveId {
        match self {
            BuildRow::Saturation => live_id!(tb_saturation),
            BuildRow::Lightness => live_id!(tb_lightness),
            BuildRow::FontSize => live_id!(tb_font_size),
            BuildRow::FontContrast => live_id!(tb_font_contrast),
            BuildRow::TextContrast => live_id!(tb_text_contrast),
            BuildRow::TextTint => live_id!(tb_text_tint),
            BuildRow::Spacing => live_id!(tb_spacing),
            BuildRow::Roundness => live_id!(tb_roundness),
        }
    }

    /// The line the slider is drawn on, which holds the row's lock in front
    /// of it. Declared in the splash beside the slider, for
    /// [`EQ_ROW_IDS`]' reason: every row this panel has is a fixed slot.
    pub(in crate::tweak) fn line(self) -> LiveId {
        match self {
            BuildRow::Saturation => live_id!(tb_saturation_row),
            BuildRow::Lightness => live_id!(tb_lightness_row),
            BuildRow::FontSize => live_id!(tb_font_size_row),
            BuildRow::FontContrast => live_id!(tb_font_contrast_row),
            BuildRow::TextContrast => live_id!(tb_text_contrast_row),
            BuildRow::TextTint => live_id!(tb_text_tint_row),
            BuildRow::Spacing => live_id!(tb_spacing_row),
            BuildRow::Roundness => live_id!(tb_roundness_row),
        }
    }


    /// Where the track stands for these settings. The two shares are held as
    /// 0..1 and read as parts of a hundred; the contrast is the ratio itself,
    /// and the four dimensions are in the unit the theme file writes them in.
    pub(crate) fn shown(self, params: &BuilderParams) -> f64 {
        match self {
            BuildRow::Saturation => params.saturation * 100.0,
            BuildRow::Lightness => params.lightness * 100.0,
            BuildRow::FontSize => params.font_size,
            BuildRow::FontContrast => params.font_contrast,
            BuildRow::TextContrast => params.clamped().text_contrast,
            BuildRow::TextTint => params.text_tint * 100.0,
            BuildRow::Spacing => params.spacing,
            BuildRow::Roundness => params.roundness,
        }
    }

    /// Whether the row answers at all. The saturation does not at one
    /// colour: the page is a grey there, and every share of nothing is
    /// nothing. The text tint does not with no text colour chosen: there is
    /// no hue for it to be a share of. Both keep the number they hold while
    /// they are off, so what was set comes back with what turned it off.
    pub(in crate::tweak) fn enabled(self, params: &BuilderParams) -> bool {
        match self {
            BuildRow::Saturation => params.color_count != 1,
            BuildRow::TextTint => params.text_color.is_some(),
            _ => true,
        }
    }

    /// The two stops of the track, where they are not the splash's: the text
    /// contrast runs from what reads to what the page allows, and the page
    /// moves with the background rows, so its stops are worked out on every
    /// draw rather than declared once.
    pub(in crate::tweak) fn range(self, params: &BuilderParams) -> Option<(f64, f64)> {
        match self {
            BuildRow::TextContrast => Some(params.text_contrast_range()),
            _ => None,
        }
    }

    /// The settings with this row moved to where the track now stands. The
    /// engine clamps on the way in, so a track that offers a number outside
    /// the token's own range cannot make a theme that is outside it.
    pub(in crate::tweak) fn moved(self, params: BuilderParams, shown: f64) -> BuilderParams {
        match self {
            BuildRow::Saturation => BuilderParams { saturation: shown / 100.0, ..params },
            BuildRow::Lightness => BuilderParams { lightness: shown / 100.0, ..params },
            BuildRow::FontSize => BuilderParams { font_size: shown, ..params },
            BuildRow::FontContrast => BuilderParams { font_contrast: shown, ..params },
            BuildRow::TextContrast => BuilderParams { text_contrast: shown, ..params },
            BuildRow::TextTint => BuilderParams { text_tint: shown / 100.0, ..params },
            BuildRow::Spacing => BuilderParams { spacing: shown, ..params },
            BuildRow::Roundness => BuilderParams { roundness: shown, ..params },
        }
    }

    /// The settings with this one row back where the house theme has it and
    /// every other row left where it is. The gesture is a click on the name,
    /// which is the slider's own reset -- and it resets THIS setting rather
    /// than the whole theme, because the name that was clicked names one row.
    ///
    /// The house of the appearance in force, so that the lightness goes back
    /// to its own half's page and never turns a dark theme light; the
    /// saturation's house is its top, the palette's own background colour.
    pub(in crate::tweak) fn house(self, params: BuilderParams) -> BuilderParams {
        let house = BuilderParams::house(params.dark());
        self.moved(params, self.shown(&house))
    }
}

/// One row of the builder that can be held where it is: the eight sliders,
/// the text colour picker -- which is a row like the rest even though the
/// control on it is not a slider -- and the row of colour squares itself.
///
/// The operator's ruling is "every slider will have a lock icon in front of
/// it ... when locked the slider won't move by clicking the dice or changing
/// the palette", and his mockup puts one in front of the picker too. A later
/// mockup puts a tenth in front of the four squares: "a lock in front of the
/// color swatches will lock the (amount of) colors when pressing the dice",
/// and then, in his own correction, "the lock in front of the swatches
/// doesn't only lock the amount, it also locks the colors themselves".
///
/// So the list is [`BuildRow::ALL`], then the picker, then the palette row
/// that stands above both of them, and the index into it is the index into
/// [`Tweaker::tb_locks`]. It is not screen order and never was: it is the
/// order the locks were added in, and nothing reads it as an order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::tweak) enum BuildLock {
    /// A slider row, by the setting it holds.
    Row(BuildRow),
    /// The row that says which of the palette's colours the words take.
    TextColor,
    /// The four colour squares: how many of them there are, and which
    /// colours stand on them.
    Palette,
}

impl BuildLock {
    pub(in crate::tweak) const ALL: [BuildLock; 10] = [
        BuildLock::Row(BuildRow::Saturation),
        BuildLock::Row(BuildRow::Lightness),
        BuildLock::Row(BuildRow::FontSize),
        BuildLock::Row(BuildRow::FontContrast),
        BuildLock::Row(BuildRow::TextContrast),
        BuildLock::Row(BuildRow::TextTint),
        BuildLock::Row(BuildRow::Spacing),
        BuildLock::Row(BuildRow::Roundness),
        BuildLock::TextColor,
        BuildLock::Palette,
    ];

    /// The line it stands at the head of.
    pub(in crate::tweak) fn line(self) -> LiveId {
        match self {
            BuildLock::Row(row) => row.line(),
            BuildLock::TextColor => live_id!(tb_text_color_row),
            BuildLock::Palette => live_id!(tb_seed_row),
        }
    }

    /// The lock itself, given the builder's body.
    ///
    /// Nine of the lines are inside `tb_rows` and the tenth is not: the
    /// colour squares stand above that group, between the Colours row and
    /// the carousel. One place that knows it, so that the draw, the tip and
    /// the tests all find the same ten locks.
    pub(in crate::tweak) fn lock_in(self, body: &WidgetRef) -> WidgetRef {
        self.line_in(body).child(TB_LOCK_ID)
    }

    /// The whole line the lock stands at the head of, given the body.
    pub(in crate::tweak) fn line_in(self, body: &WidgetRef) -> WidgetRef {
        match self {
            BuildLock::Palette => body.child(live_id!(tb_seed_row)),
            other => body.child(live_id!(tb_rows)).child(other.line()),
        }
    }

}
pub(in crate::tweak) const TB_LOCK_FACES: [LiveId; 2] = [live_id!(lock_open), live_id!(lock_shut)];

/// The id the lock is declared under inside every settings row.
const TB_LOCK_ID: LiveId = live_id!(lock);
pub(in crate::tweak) const TB_COLOR_IDS: [LiveId; 4] = [
    live_id!(tb_color_0),
    live_id!(tb_color_1),
    live_id!(tb_color_2),
    live_id!(tb_color_3),
];
const TB_SEED_LABELS: [&str; 5] = ["Prim", "Sec", "Tert", "Surf", "None"];
pub(in crate::tweak) const TB_FIRST_TINT: f64 = 0.35;

/// The text colour picker's entry for a slot, and back. "None" is the FIRST
/// entry here and the last one in the seed picker, and both are right: no
/// colour is where the words open and it is the answer a person comes back
/// to, where a carousel grown from nothing is the odd case at the end of a
/// list of slots.
///
/// A slot the count does not choose has no entry and reads as None, which is
/// where the panel moves it: see `tb_count_chosen`.
pub(in crate::tweak) fn tb_text_entry_at(slot: Option<SeedSlot>, count: usize) -> usize {
    match slot {
        None => 0,
        Some(slot) => SeedSlot::chosen(count).iter().position(|at| *at == slot).map_or(0, |at| at + 1),
    }
}

pub(in crate::tweak) fn tb_text_of_entry_at(entry: usize, count: usize) -> Option<SeedSlot> {
    entry.checked_sub(1).and_then(|at| SeedSlot::chosen(count).get(at).copied())
}

/// The text colour picker's words for a palette of `count` colours: None and
/// then the slots the count chooses, in the seed picker's own words, because
/// they are the same four squares named over the row.
pub(in crate::tweak) fn tb_text_labels(count: usize) -> Vec<String> {
    std::iter::once(TB_SEED_LABELS[SeedSlot::ALL.len()].to_string())
        .chain(SeedSlot::chosen(count).into_iter().map(|slot| TB_SEED_LABELS[slot.index()].to_string()))
        .collect()
}


/// The rungs of the colour count, in `COLOR_COUNTS` order, and the word
/// over them.
pub(in crate::tweak) const TB_COUNT_IDS: [LiveId; 4] = [
    live_id!(tb_count_1),
    live_id!(tb_count_2),
    live_id!(tb_count_3),
    live_id!(tb_count_4),
];
pub(in crate::tweak) fn tb_shown_slots(count: usize) -> Vec<usize> {
    SeedSlot::chosen(count).into_iter().map(SeedSlot::index).collect()
}

/// What a carried builder colour does if let go where it is.
///
/// The four names are the roles and never move; the colours under them are
/// an ordered list whose position is the role. So there are two drops: onto
/// another colour, which trades the two, and into a gap, which moves the
/// carried colour there and closes the list up behind it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::tweak) enum TbDrop {
    /// Onto the middle of this square: the two trade roles.
    Swap(usize),
    /// Into this insertion point, 0 in front of the first square to 4 after
    /// the last: the carried colour goes there and every colour between its
    /// old place and the new one shifts one role.
    ///
    /// Both count among the squares SHOWN. A palette of fewer colours hides
    /// the ones it derives, and those are not places in the list: a colour
    /// carried at two moves between the primary and the surface and nowhere
    /// else, and the insertion points run 0 to the number shown.
    Insert(usize),
}

/// How much of a square, from each of its sides, belongs to the gap beside
/// it rather than to the square. The row's own gap is three points, far too
/// thin to hit on purpose, so a fifth of each neighbour goes with it; the
/// middle three fifths stay the square's, wide enough that a trade is not a
/// near miss either.
const TB_INSERT_BAND: f64 = 0.2;

/// The zone a carried colour from square `from` is over, off the four
/// squares' rects, the top of the row (the names over the squares) and the
/// left edge of what follows them, the seed picker.
///
/// `None` when the drop would change nothing: off the row, on the seed
/// picker or the die (they are controls, not places in the list, so a drop
/// there is a cancel, and the zone after the last square stops at the
/// picker's edge), on the carried
/// colour's own square, or at either insertion point beside it -- slotting
/// a colour in next to where it already is leaves the order as it was.
pub(in crate::tweak) fn tb_drop_zone(squares: &[Rect], top: f64, beyond_x: f64, from: usize, at: DVec2) -> Option<TbDrop> {
    if squares.is_empty() || squares.iter().any(|square| square.size.x <= 0.0) {
        return None;
    }
    let last = squares.len() - 1;
    let bottom = squares.iter().map(|square| square.pos.y + square.size.y).fold(f64::MIN, f64::max);
    // The row's own gap above and below, so a hand a point off the squares'
    // line has not left the row.
    let slack = 2.0;
    if at.y < top - slack || at.y >= bottom + slack {
        return None;
    }
    let right = |which: usize| squares[which].pos.x + squares[which].size.x;
    let band = |which: usize| squares[which].size.x * TB_INSERT_BAND;
    let zone = if at.x >= beyond_x.max(right(last)) {
        return None;
    } else if at.x < squares[0].pos.x + band(0) {
        // In front of the first: as far out as a band's width, the same
        // reach the gaps between squares have.
        if at.x < squares[0].pos.x - band(0) {
            return None;
        }
        TbDrop::Insert(0)
    } else if at.x >= right(last) - band(last) {
        TbDrop::Insert(last + 1)
    } else {
        // Walked left to right: a square's middle, then the gap after it
        // with the bands on either side, up to the last square's middle,
        // whose far band the branch above has already taken.
        (0..=last)
            .find_map(|which| {
                if at.x < right(which) - band(which) {
                    Some(TbDrop::Swap(which))
                } else if which < last && at.x < squares[which + 1].pos.x + band(which + 1) {
                    Some(TbDrop::Insert(which + 1))
                } else {
                    None
                }
            })
            .unwrap_or(TbDrop::Insert(last + 1))
    };
    match zone {
        TbDrop::Swap(to) if to == from => None,
        TbDrop::Insert(point) if point == from || point == from + 1 => None,
        zone => Some(zone),
    }
}

/// Where the insertion bar for `point` stands: centred in its gap -- in
/// front of the first square and after the last as far out as the gaps
/// between them are wide -- two points wide, and tall enough to stand out
/// above the carried copy, which rides a little over the row.
pub(in crate::tweak) fn tb_insert_bar(squares: &[Rect], point: usize) -> Rect {
    let right = |which: usize| squares[which].pos.x + squares[which].size.x;
    let last = squares.len().saturating_sub(1);
    // A lone square has no gap beside it to measure; the row's own three
    // points is what one would be.
    let gap = if last == 0 { 3.0 } else { (squares[1].pos.x - right(0)).max(0.0) };
    let x = match point {
        0 => squares[0].pos.x - gap * 0.5,
        _ if point > last => right(last) + gap * 0.5,
        _ => (right(point - 1) + squares[point].pos.x) * 0.5,
    };
    let width = 2.0;
    let square = squares[point.min(last)];
    Rect {
        pos: dvec2(x - width * 0.5, square.pos.y - 8.0),
        size: dvec2(width, square.size.y + 10.0),
    }
}

/// The four colours after a drop: traded, or the carried one moved to the
/// insertion point and the rest closed up behind it -- among the squares
/// shown. `shown` are the slots on the row in order, `from` and the drop
/// count along them, and a slot not shown keeps its colour whatever moves
/// round it.
pub(in crate::tweak) fn tb_dropped_among(palette: [u32; 4], shown: &[usize], from: usize, drop: TbDrop) -> [u32; 4] {
    let list: Vec<u32> = shown.iter().map(|slot| palette[*slot]).collect();
    let moved = tb_dropped_list(&list, from, drop);
    let mut out = palette;
    for (slot, color) in shown.iter().zip(moved) {
        out[*slot] = color;
    }
    out
}

/// Where the carried colour ends up, among the squares SHOWN: the square it
/// traded with, or the insertion point once its own place has been taken out
/// from under the count. What `tb_dropped_list` does to it, said on its own,
/// because the seed follows the colour and not the slot it left.
pub(in crate::tweak) fn tb_landed_at(shown: usize, from: usize, drop: TbDrop) -> usize {
    match drop {
        TbDrop::Swap(to) => to,
        TbDrop::Insert(point) => (if point > from { point - 1 } else { point }).min(shown.saturating_sub(1)),
    }
}

fn tb_dropped_list(palette: &[u32], from: usize, drop: TbDrop) -> Vec<u32> {
    let mut out = palette.to_vec();
    match drop {
        TbDrop::Swap(to) => out.swap(from, to),
        TbDrop::Insert(point) => {
            let carried = out.remove(from);
            // Taken out first, so a point past its old place is one earlier.
            let to = if point > from { point - 1 } else { point };
            out.insert(to, carried);
        }
    }
    out
}


pub const TB_QUIET: f64 = 0.06;

pub enum BuildOutcome {
    Nothing,
    Theme(ThemeInstall),
    RestoreEntry,
}

pub struct ThemeBuilderState {
    entry_params: Option<BuilderParams>,
    params: BuilderParams,
    built: BuiltTheme,
    applied: Option<BuilderParams>,
    installed: bool,
    last_install: Option<ThemeInstall>,
}

impl ThemeBuilderState {
    pub fn enter(dark: bool) -> Self {
        let params = BuilderParams::house(dark);
        let built = build(&params);
        // Touching a different seed may leave all derived values identical.
        let last_install = Some(built.to_install());
        Self { entry_params: Some(params), params, built, applied: Some(params), installed: false, last_install }
    }

    pub fn params(&self) -> BuilderParams { self.params }
    pub fn set_moving(&mut self, params: BuilderParams) { self.params = params.clamped(); }
    pub fn set(&mut self, params: BuilderParams) {
        self.set_moving(params);
        self.derive();
    }

    fn derive(&mut self) {
        if self.built.params != self.params { self.built = build(&self.params); }
    }

    pub fn readability(&self) -> &Readability { &self.built.readability }
    pub fn crowding(&self) -> Option<SeedSlot> { self.built.crowding }

    pub fn evaluate_apply(&mut self) -> BuildOutcome {
        if self.applied == Some(self.params) { return BuildOutcome::Nothing }
        self.derive();
        self.applied = Some(self.params);
        if self.entry_params == Some(self.params) {
            self.last_install = Some(self.built.to_install());
            return if std::mem::take(&mut self.installed) { BuildOutcome::RestoreEntry } else { BuildOutcome::Nothing }
        }
        let install = self.built.to_install();
        if self.last_install.as_ref() == Some(&install) { return BuildOutcome::Nothing }
        self.last_install = Some(install.clone());
        self.installed = true;
        BuildOutcome::Theme(install)
    }

    pub fn mark_stood_down(&mut self, dark: bool) { *self = Self::enter(dark); }
    pub fn promote_standing(&mut self) { self.installed = false; self.entry_params = None; }
    pub fn install_failed(&mut self, stands: bool) {
        self.installed = stands;
        self.applied = None;
        self.last_install = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_widgets::theme_tokens::{ALLOWED_INSTALL_GLOBALS, is_token_key};

    #[test]
    fn builder_palette_readability_crowding_suggestions_and_to_install() {
        for (dark, hue) in [(true, 24.), (false, 147.), (true, 282.)] {
            let surface = at_luminance(hue, 0.55, luminance_at(if dark { 22. } else { 83. }));
            let (saturation, lightness) = surface_sliders(surface, dark);
            let params = BuilderParams { saturation, lightness, spacing: 12.0,
                ..BuilderParams::house(dark).with_palette([0x3366CCFF, 0xCC6633FF, 0x33CC66FF, surface]) };
            assert!(near(asked_page(&params), surface), "Surf round-trips through the surface sliders");
            let built = build(&params);
            assert!(built.readability.measured > 0 && built.readability.failures.is_empty(),
                "the built accent and text pairs read on the {dark:?} page at hue {hue}: {}", built.readability);
            let install = built.to_install();
            assert!(install.globals.contains(&("space_factor".into(), ThemeVal::Num(12.0))), "moved spacing re-derives dependent sizes");
            assert!(install.globals.iter().all(|(key, _)| ALLOWED_INSTALL_GLOBALS.contains(&key.as_str())), "only installer globals are re-derived");
            assert!(install.globals.iter().chain(&install.overrides).all(|(k, v)| is_token_key(k) && match v {
                ThemeVal::Color(_) => true, ThemeVal::Num(n) => n.is_finite(),
            }), "every install entry is a named scalar literal");
        }
        let crowded = BuilderParams::house(true).with_palette([0x353535FF, 0xC4C4C4FF, 0xE7E7E7FF, 0x404040FF]);
        assert!(build(&crowded).crowding.is_some_and(|s| s != crowded.seed_slot), "crowding names an accent while protecting the active seed");
        let seed = 0xFF5C39FF;
        for mode in [Schemes::RoundThePick, Schemes::AsWritten] {
            let chips = all_suggestions_for(4, SeedSlot::Primary, seed, true, &[vec![seed, 0x42A880FF, 0xBBC7DDFF]], mode);
            assert_eq!(chips[0].label, OWN_LABEL, "the member's schemes lead in either Adjust mode");
            let book = chips.iter().position(|c| c.label.starts_with(COMBINATION_LABEL)).expect("book schemes are present");
            let rule = chips.iter().position(|c| c.harmony.is_some()).expect("harmony chips are present");
            assert!(0 < book && book < rule, "book schemes precede the harmony and mood chips");
        }
        assert_eq!(tb_dropped_among([1,2,3,4], &[0,1,3], 0, TbDrop::Insert(3)), [2,4,3,1], "insertion reorders only visible slots");
        assert_eq!(tb_dropped_among([1,2,3,4], &[0,1,3], 0, TbDrop::Swap(2)), [4,2,3,1], "a centre drop swaps only its two colours");
    }
}


#[test]
fn build_from_spec_covers_palette_grown_roll_surface_follow_and_validation() {
    let palette = BuildPalette::Explicit { colors: vec![parse_hex_rgb("#ff5c3900").unwrap(), parse_hex_rgb("#282c30").unwrap()] };
    let built = build_from_spec(&BuildSpec { palette: palette.clone(), ..Default::default() }, 0).unwrap().built;
    assert_eq!((built.params.color_count, built.params.palette()[3]), (2, 0x282c30ff), "two colours name primary and surface");
    assert_eq!((built.params.saturation, built.params.lightness), surface_sliders(0x282c30ff, true), "the sliders follow the surface");
    assert!(built.readability.measured > 0 && built.readability.failures.is_empty(), "the installed palette reads");
    let grown = build_from_spec(&BuildSpec { palette: BuildPalette::Grown { favourite: 0xff5c39ff,
        harmony: parse_harmony("triadic").unwrap(), mood: parse_mood("muted").unwrap() }, spacing: Some(12.), text_color: Some(parse_seed_slot("primary")),
        ..Default::default() }, 0).unwrap();
    assert_eq!(grown.built.params.text_tint, 0.35, "choosing a text colour supplies the first tint");
    assert!(grown.install.globals.contains(&("space_factor".into(), ThemeVal::Num(12.))), "explicit spacing reaches the install");
    let roll = BuildSpec { roll: true, seed: Some(7), palette, lightness: Some(20.), ..Default::default() };
    let rolled = build_from_spec(&roll, 0).unwrap();
    assert_eq!(rolled.install, build_from_spec(&roll, 42).unwrap().install, "seeded rolls repeat");
    assert_eq!((rolled.built.params.chosen_palette()[0], rolled.built.params.palette()[3], rolled.built.params.lightness),
        (0xff5c39ff, 0x282c30ff, 0.2), "explicit palette and lightness stay locked during a roll");
    assert!(parse_hex_rgb("red").is_none(), "palette entries require hexadecimal colours");
    for bad in [BuildSpec { seed: Some(7), ..Default::default() }, BuildSpec { dark: Some(true), lightness: Some(80.), ..Default::default() },
        BuildSpec { color_count: Some(2), text_color: Some(Some(SeedSlot::Secondary)), ..Default::default() }] {
        assert!(build_from_spec(&bad, 0).is_err(), "invalid fields refused: {bad:?}");
    }
}
