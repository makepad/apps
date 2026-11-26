//! The DJ surface: the original console's central presentation on the
//! flow runtime.
//!
//! Layout and control shapes follow the preserved native DJ page,
//! `donors/stage/src/music_view.rs::MusicDeckPage` (its `MusicButton`,
//! `MusicKnob`, `MusicFader`, `DeckWell` templates and the row order of the
//! page), and the capture `design-reference/old-dj.png`:
//!
//! 1. paired deck heads (letter, title, key/BPM slot, pitch readout,
//!    elapsed/duration) around the QUANT cluster;
//! 2. two whole-track overview bars;
//! 3. the console: a mirrored outer strip per deck (mode row, TEMPO and
//!    VOL faders, the EQ/FILTER knobs, stem mix and karaoke wells) flanking
//!    the two stacked wave lanes;
//! 4. the transport rows with the fade band between them.
//!
//! What draws here is backed by the flow decks: play, cue, loop, seek,
//! tempo (`speed`), deck trim/mute and the crossfader. Tempo, volume and
//! mute are declarative bindings; the host supplies measured deck peaks. The
//! overviews and scrolling center lanes share the donor GPU renderer. Measured
//! frequency bands tint the envelope until genuine stem sidecars supply its
//! four coloured layers. Stem gain, mute and solo bind directly to the live
//! mixer when a complete set of stems is available. Saved timed lyrics follow
//! the accepted deck position. Each deck's HIGH/MID/LOW (with a kill and a
//! solo per band) bind its isolator and FILTER its one-knob filter. SYNC,
//! the nudges, KEY and ±, SLIP, QUANT and the ½ grid flip report actions the
//! host runs. Colours follow the original theme-aware `stage.*` palette.
//!
//! The surface only draws and reports. A Rust app binds
//! [`MusicSurfaceAction`] to its flow decks.

use crate::deck_strip::{DeckStripAction, DeckStripState};
use crate::deck_tabs::{DeckTabs, TabFollow};
use crate::music_wave::{MusicWaveRef, MusicWaveTextures, MusicWaveWidgetRefExt, PlacedWave, PreparedWaveHandle, WaveCoverage};
use makepad_stage_audio_wave::WaveData;
use std::{rc::Rc, sync::Arc};
use makepad_widgets::tip::{TipWidgetExt, TipWidgetRefExt};
use makepad_widgets::*;

/// Timed text from the acknowledged source revision. Parsing belongs to the
/// asset worker; this is only the presentation data handed to the surface.
#[derive(Clone, Debug, PartialEq)]
pub struct LyricsData {
    pub duration_secs: f64,
    pub language: String,
    pub lines: Vec<LyricsLine>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LyricsLine {
    pub start_secs: f64,
    pub end_secs: f64,
    pub text: String,
    /// Source-second starts, one per whitespace-separated word.
    pub words: Vec<f64>,
    /// False means only the line interval is authoritative.
    pub confident: bool,
}

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*
    let stage = mod.stage

    // Exact donor material templates; this surface supplies current actions.
    let SurfaceValue = StageMusicValue{}
    let SurfaceCaption = StageMusicLabel{width: Fit height: Fit padding: 0}
    let SurfaceButton = StageMusicButton{
        height: 22 margin: 0
        padding: Inset{left: 8 right: 8 top: 0 bottom: 0}
        align: Align{x: 0.5 y: 0.5}
    }
    let SurfaceTransport = StageMusicTransportButton{width: 22 margin: 0}
    let SurfaceTransportIcon = StageMusicTransportIconButton{margin: 0 padding: 0 align: Align{x: 0.5 y: 0.5}}
    let SurfaceKnob = StageMusicKnob{margin: 0 label_walk: Walk{width: 0 height: 0 margin: 0}}
    let SurfaceFader = StageMusicFader{margin: 0 label_walk: Walk{width: 0 height: 0 margin: 0}}
    let DeckMute = CheckBoxCustom{
        width: Fill height: 14 margin: 0 padding: 0 text: "M"
        align: Align{x: 0.5 y: 0.5} label_align: Align{x: 0.5 y: 0.5}
        icon_walk: Walk{width: 0 height: 0}
        label_walk: Walk{width: Fill height: Fit margin: 0}
        draw_bg +: {
            color: stage.control color_hover: stage.control_hover color_down: stage.control_down
            color_active: stage.accent color_disabled: stage.ghost_face
            active_hover: uniform(stage.accent_hover) active_down: uniform(stage.accent_down)
            border_color: stage.border_hi border_color_disabled: stage.line_faint
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                let off = self.color.mix(self.color_hover, self.hover).mix(self.color_down, self.down)
                let on = self.color_active.mix(self.active_hover, self.hover).mix(self.active_down, self.down)
                sdf.box(0.5, 0.5, self.rect_size.x - 1., self.rect_size.y - 1., 6.)
                sdf.fill(off.mix(on, self.active).mix(self.color_disabled, self.disabled))
                sdf.stroke(self.border_color.mix(self.border_color_disabled, self.disabled), 1.)
                return sdf.result
            }
        }
        draw_text +: {
            color: stage.label color_hover: stage.text_hi color_active: stage.on_accent
            color_disabled: stage.dim_lo text_style: theme.font_bold{font_size: 9}
        }
    }
    let SurfaceWell = StageMusicDeckWell{}
    let SurfaceLane = StageMusicWave{
        width: Fill height: Fill overview: true
    }
    // A mark pad: lit while it holds a mark (the host sets it).
    let MarkPad = DeckMute{width: 22 height: 22 text: ""}
    // A band's kill (M) and solo (S), bound per deck and band.
    let EqToggle = DeckMute{height: 13 draw_bg +: {border_radius: 3.0} draw_text +: {text_style: theme.font_bold{font_size: 7}}}
    // One isolator band: its gain (0 gone, 1 as recorded, 2 doubled), kill
    // and solo; the deck binds each to its own Isolator node.
    let KnobColumn = StageMusicKnobStack{
        caption := StageMusicKnobLabel{text: "HIGH"}
        knob_tip := Tip{text: "This band's level: fully left takes it away, the middle is as recorded; double click for the middle" eq_knob := SurfaceKnob{min: 0 max: 2 default: 1}}
        ms := StageMusicMSRow{
            kill_tip := Tip{text: "Kill this band" kill := EqToggle{text: "M"}}
            solo_tip := Tip{text: "Hear only this band (with any other soloed)" solo := EqToggle{text: "S"}}
        }
    }
    let StemToggle = DeckMute{
        height: 13
        animator +: {disabled +: {default: @on}}
        draw_bg +: {border_radius: 3.0}
        draw_text +: {text_style: theme.font_bold{font_size: 7}}
    }
    let StemColumn = StageMusicStemStack{
        caption := Label{
            width: Fill height: Fit padding: 0 text: "DRUMS"
            align: Align{x: 0.5 y: 0.0}
            draw_text +: {color: stage.dim text_style: theme.font_bold{font_size: 7}}
        }
        gain_tip := Tip{text: "Load a track with four playable stems"
            gain := StageMusicStemKnob{
                width: 40 height: 40 min: 0 max: 1.5 default: 1.0
                margin: 0 label_walk: Walk{width: 0 height: 0 margin: 0}
                animator +: {disabled +: {default: @on}}
            }
        }
        ms := StageMusicMSRow{
            mute_tip := Tip{text: "Load a track with four playable stems" mute := StemToggle{text: "M"}}
            solo_tip := Tip{text: "Load a track with four playable stems" solo := StemToggle{text: "S"}}
        }
    }
    let FilterColumn = StageMusicKnobStack{
        StageMusicKnobLabel{text: "FILTER"}
        knob_tip := Tip{text: "Deck filter: left sweeps a low pass down, right a high pass up, the middle passes everything; double click for the middle" eq_knob := SurfaceKnob{min: 0 max: 1 default: 0.5 taper: Linear}}
    }

    mod.widgets.StageMusicWrapStripBase = #(MusicWrapStrip::register_widget(vm))
    mod.widgets.StageMusicWrapStrip = set_type_default() do mod.widgets.StageMusicWrapStripBase{
        width: Fill height: Fit
    }
    mod.widgets.StageMusicLyricsBase = #(MusicLyrics::register_widget(vm))
    mod.widgets.StageMusicLyrics = set_type_default() do mod.widgets.StageMusicLyricsBase{
        width: Fill height: Fill
        draw_text +: {
            color: stage.glass_label
            text_style: theme.font_bold{font_size: 10}
        }
    }
    mod.widgets.MusicSurfaceBase = #(MusicSurface::register_widget(vm))
    mod.widgets.MusicSurface = set_type_default() do mod.widgets.MusicSurfaceBase{
        width: Fill
        height: Fill
        flow: Down
        spacing: 6

        // ---- 1. deck heads around the QUANT cluster ----
        View{
            width: Fill height: Fit flow: Right spacing: 10 align: Align{x: 0.0, y: 0.5}
            new_batch: true
            View{
                width: Fill height: Fit flow: Right spacing: 8 align: Align{x: 0.0, y: 0.5}
                deck_a_eject := SurfaceButton{width: 22 height: 22 padding: 0 text: "×"}
                Label{text: "A" draw_text.color: stage.accent draw_text.text_style: theme.font_bold{font_size: 13}}  // stage.accent
                deck_a_title := SurfaceValue{width: Fill text: "empty"}
                View{
                    width: Fit height: Fit flow: Down align: Align{x: 1.0, y: 0.5}
                    deck_a_bpm := Label{width: Fit text: "—" draw_text.color: stage.accent draw_text.text_style: theme.font_bold{font_size: 17}}
                    deck_a_pitch := SurfaceCaption{text: "+0.0%"}
                }
                deck_a_key := Label{width: 34 text: "—" align: Align{x: 0.5 y: 0.5} draw_text.color: stage.key draw_text.text_style: theme.font_bold{font_size: 11}}
                deck_a_time := StageMusicLabel{width: 78 text: "0:00 / 0:00"}
            }
            View{
                width: Fit height: Fit flow: Down spacing: 2 align: Align{x: 0.5, y: 0.5}
                SurfaceCaption{text: "QUANT"}
                View{
                    width: Fit height: Fit flow: Right spacing: 4
                    Tip{text: "Deck A's QUANT: a jump on its waveform moves in whole units of this many beats, keeping the phase (— off; off also keeps SYNC from moving the beat)" quant_a := SurfaceButton{width: 30 text: "1"}}
                    Tip{text: "Deck B's QUANT: a jump on its waveform moves in whole units of this many beats, keeping the phase (— off; off also keeps SYNC from moving the beat)" quant_b := SurfaceButton{width: 30 text: "1"}}
                }
            }
            View{
                width: Fill height: Fit flow: Right spacing: 8 align: Align{x: 0.0, y: 0.5}
                deck_b_time := StageMusicLabel{width: 78 text: "0:00 / 0:00"}
                deck_b_key := Label{width: 34 text: "—" align: Align{x: 0.5 y: 0.5} draw_text.color: stage.key draw_text.text_style: theme.font_bold{font_size: 11}}
                View{
                    width: Fit height: Fit flow: Down align: Align{x: 0.0, y: 0.5}
                    deck_b_bpm := Label{width: Fit text: "—" draw_text.color: stage.link draw_text.text_style: theme.font_bold{font_size: 17}}   // stage.link
                    deck_b_pitch := SurfaceCaption{text: "+0.0%"}
                }
                deck_b_title := SurfaceValue{width: Fill text: "empty"}
                Label{text: "B" draw_text.color: stage.link draw_text.text_style: theme.font_bold{font_size: 13}}
                deck_b_eject := SurfaceButton{width: 22 height: 22 padding: 0 text: "×"}
            }
        }

        // ---- 2. whole-track overview strips ----
        View{
            width: Fill height: 46 flow: Right spacing: 10
            new_batch: true
            SurfaceWell{deck_a_overview := SurfaceLane{}}
            SurfaceWell{deck_b_overview := SurfaceLane{}}
        }

        // ---- the deck tabs: in a window too narrow for both deck panels,
        // one panel shows at a time; what moves it is the operator's. ----
        deck_tabs_row := View{
            visible: false width: Fill height: 26 flow: Right spacing: 6 align: Align{y: 0.5}
            Tip{text: "Show deck A's panel" tab_deck_a := MarkPad{width: 64 text: "DECK A"}}
            Tip{text: "Show deck B's panel" tab_deck_b := MarkPad{width: 64 text: "DECK B"}}
            Tip{text: "What moves the deck on screen: only a tab press (MANUAL), or the deck the room hears (AUDIBLE; it holds while both or neither are heard)" tab_follow := DropDown{width: 96 height: 22 labels: ["MANUAL" "AUDIBLE"]}}
        }

        // ---- 3. the console: outer strips around the stacked lanes ----
        View{
            width: Fill height: Fill flow: Right spacing: 8
            new_batch: true

            deck_a_panel := View{
                width: 316 height: Fill flow: Down spacing: 5

                // Keep the complete donor stack scrollable in a short console.
                // Its transport remains pinned beneath the scrolling controls.
                // Content dragging is off so the faders retain their drag gestures.
                deck_a_strip := ScrollYView{
                    width: Fill height: Fill flow: Down
                    scroll_bars.scroll_bar_y.drag_scrolling: false
                    View{
                        width: Fill height: Fill{min: 280} flow: Down spacing: 6
                        View{
                            width: Fill height: Fit flow: Right spacing: 4 align: Align{x: 0.0, y: 0.5}
                            Tip{text: "Match deck A's tempo to the other deck as the room hears it, and its beat onto the other's while both play (the pitch follows unless KEY locks it)" deck_a_sync := SurfaceButton{width: Fill text: "SYNC"}}
                            Tip{text: "Slip: while deck A loops or is under the hand, the record runs on unheard; letting go (or ending the loop) plays from where it would be" deck_a_slip := SurfaceButton{width: 40 text: "SLIP"}}
                            Tip{text: "Flip deck A's beat grid half a beat when its count sits on the off pulse (lit while flipped)" deck_a_phase := SurfaceButton{width: 26 padding: 0 text: "½"}}
                            Tip{text: "Hear deck A in the headphones before the crossfader (the cue: channels 3-4 of the device, or the left ear with SPLIT)" deck_a_hp := StageMusicIconButton{width: 30 height: 22 margin: 0 padding: 0 align: Align{x: 0.5 y: 0.5} draw_icon +: {svg: crate_resource("self:resources/icons/headphones.svg")}}}
                            Tip{text: "Key lock on deck A: the tempo moves with the speed, the key does not (time-stretched). Shows the key shift" deck_a_keylock := SurfaceButton{width: 44 text: "KEY"}}
                            Tip{text: "Deck A a semitone down, at the same tempo" deck_a_key_down := SurfaceButton{width: 22 padding: 0 text: "-"}}
                            Tip{text: "Deck A a semitone up, at the same tempo" deck_a_key_up := SurfaceButton{width: 22 padding: 0 text: "+"}}
                            Tip{text: "Cycle tempo range: ±8%, ±16%, ±50%, or 0.25–4×; expands to show the current speed" deck_a_range := SurfaceButton{width: 44 text: "±8%"}}
                        }
                        View{
                            width: Fill height: Fill flow: Right spacing: 8
                            View{width: Fit height: Fill flow: Right spacing: 6
                            View{
                                width: 44 height: Fill flow: Down spacing: 2 align: Align{x: 0.5, y: 0.0}
                                SurfaceCaption{text: "TEMPO"}
                                deck_a_tempo := SurfaceFader{bind := "deck_a.speed" min: 0.92 max: 1.08 default: 1.0 arc_from_origin: true}
                                deck_a_tempo_reset := SurfaceButton{width: Fill height: 14 padding: 0 text: "0"}
                            }
                            View{
                                width: 44 height: Fill flow: Down spacing: 2 align: Align{x: 0.5, y: 0.0}
                                SurfaceCaption{text: "VOL"}
                                deck_a_vol := SurfaceFader{bind := "deck_a_gain.gain" min: 0.0 max: 1.5 default: 1.0}
                                Tip{text: "Mute this deck without changing its volume" deck_a_mute := DeckMute{bind := "deck_a_gain.mute"}}
                            }
                            View{width: 10 height: Fill flow: Down spacing: 2 align: Align{x: 0.5 y: 0}
                                SurfaceCaption{text: ""}
                                deck_a_vu_tip := Tip{text: "Deck peak is waiting for audio" deck_a_vu := StageMusicDeckMeter{}}
                            }
                            }
                            View{
                                width: Fill height: Fill flow: Down spacing: 6
                                View{
                                    width: Fill height: Fit flow: Right spacing: 3
                                    KnobColumn{caption +: {text: "HIGH"} knob_tip +: {eq_knob +: {bind := "deck_a_eq.high"}} ms +: {kill_tip +: {kill +: {bind := "deck_a_eq.kill_high"}} solo_tip +: {solo +: {bind := "deck_a_eq.solo_high"}}}}
                                    KnobColumn{caption +: {text: "MID"} knob_tip +: {eq_knob +: {bind := "deck_a_eq.mid"}} ms +: {kill_tip +: {kill +: {bind := "deck_a_eq.kill_mid"}} solo_tip +: {solo +: {bind := "deck_a_eq.solo_mid"}}}}
                                    KnobColumn{caption +: {text: "LOW"} knob_tip +: {eq_knob +: {bind := "deck_a_eq.low"}} ms +: {kill_tip +: {kill +: {bind := "deck_a_eq.kill_low"}} solo_tip +: {solo +: {bind := "deck_a_eq.solo_low"}}}}
                                    FilterColumn{knob_tip +: {eq_knob +: {bind := "deck_a_filter.position"}}}
                                }
                                SurfaceCaption{text: "STEM MIX" draw_text.color: stage.live}   // stage.live
                                View{
                                    width: Fill height: Fit flow: Right spacing: 4
                                    deck_a_stem_drums := StemColumn{
                                        caption +: {text: "DRUMS"}
                                        gain_tip +: {gain +: {bind := "deck_a_stems.gain_drums"}}
                                        ms +: {
                                            mute_tip +: {mute +: {bind := "deck_a_stems.mute_drums"}}
                                            solo_tip +: {solo +: {bind := "deck_a_stems.solo_drums"}}
                                        }
                                    }
                                    deck_a_stem_bass := StemColumn{
                                        caption +: {text: "BASS"}
                                        gain_tip +: {gain +: {bind := "deck_a_stems.gain_bass"}}
                                        ms +: {
                                            mute_tip +: {mute +: {bind := "deck_a_stems.mute_bass"}}
                                            solo_tip +: {solo +: {bind := "deck_a_stems.solo_bass"}}
                                        }
                                    }
                                    deck_a_stem_vocals := StemColumn{
                                        caption +: {text: "VOCALS"}
                                        gain_tip +: {gain +: {bind := "deck_a_stems.gain_vocals"}}
                                        ms +: {
                                            mute_tip +: {mute +: {bind := "deck_a_stems.mute_vocals"}}
                                            solo_tip +: {solo +: {bind := "deck_a_stems.solo_vocals"}}
                                        }
                                    }
                                    deck_a_stem_other := StemColumn{
                                        caption +: {text: "OTHER"}
                                        gain_tip +: {gain +: {bind := "deck_a_stems.gain_other"}}
                                        ms +: {
                                            mute_tip +: {mute +: {bind := "deck_a_stems.mute_other"}}
                                            solo_tip +: {solo +: {bind := "deck_a_stems.solo_other"}}
                                        }
                                    }
                                }
                                SurfaceCaption{text: "KARAOKE" draw_text.color: stage.live}
                                // The deck's own picture, under its lyrics.
                                SurfaceWell{height: Fill{min: 36} flow: Overlay
                                    deck_a_video := mod.widgets.StageMusicVideo{width: Fill height: Fill visible: false}
                                    deck_a_karaoke := mod.widgets.StageMusicLyrics{width: Fill height: Fill}
                                }
                            }
                        }
                    }

                }

                View{
                    width: Fill height: Fit flow: Flow.Right{wrap: true row_align: RowAlign.Center} spacing: 3 wrap_spacing: 3 align: Align{x: 0.0, y: 0.5}
                    deck_a_play := SurfaceTransportIcon{width: 26 icon_walk: Walk{width: 13 height: Fit} draw_icon +: {svg: crate_resource("self:resources/icons/play.svg")}}
                    deck_a_cue := SurfaceTransportIcon{width: 38 icon_walk: Walk{width: 14 height: Fit} draw_icon +: {svg: crate_resource("self:resources/icons/cue.svg")}}
                    Tip{text: "Hold to slow deck A 4 %" deck_a_nudge_back := SurfaceTransport{text: "<"}}
                    Tip{text: "Hold to push deck A 4 % faster" deck_a_nudge_fwd := SurfaceTransport{text: ">"}}
                    deck_a_loop := SurfaceTransportIcon{width: 26 icon_walk: Walk{width: 13 height: Fit} draw_icon +: {svg: crate_resource("self:resources/icons/loop_one.svg")}}
                    Tip{text: "Halve the beat loop's length" deck_a_loop_shorter := SurfaceTransport{width: 22 text: "-"}}
                    Tip{text: "A loop this many beats long from the beat at the playhead, on the track's beat grid" deck_a_loop_len := SurfaceTransport{width: 30 text: "4"}}
                    Tip{text: "Double the beat loop's length" deck_a_loop_longer := SurfaceTransport{width: 22 text: "+"}}
                    deck_a_loop_in := SurfaceTransport{width: 22 text: "["}
                    deck_a_loop_out := SurfaceTransport{width: 22 text: "]"}
                    Tip{text: "Bake this track on the AI hub: its stems, lyrics and BPM; the deck plays on with its stems when they are in (again: stop)" deck_a_bake := SurfaceTransportIcon{width: 26 icon_walk: Walk{width: 13 height: Fit} draw_icon +: {svg: crate_resource("self:resources/icons/sparkle.svg")}}}
                }
                // THE MARKS: eight hot cues and four loop slots, kept per
                // track; DEL makes the next press clear one.
                View{
                    width: Fill height: Fit flow: Flow.Right{wrap: true row_align: RowAlign.Center} spacing: 2 wrap_spacing: 3 align: Align{x: 0.0, y: 0.5}
        Tip{text: "Hot cue 1: an empty pad keeps the playhead, a set pad sends the record there (with DEL lit, clears it)" deck_a_hot_0 := MarkPad{width: 20 text: "1"}}
        Tip{text: "Hot cue 2: an empty pad keeps the playhead, a set pad sends the record there (with DEL lit, clears it)" deck_a_hot_1 := MarkPad{width: 20 text: "2"}}
        Tip{text: "Hot cue 3: an empty pad keeps the playhead, a set pad sends the record there (with DEL lit, clears it)" deck_a_hot_2 := MarkPad{width: 20 text: "3"}}
        Tip{text: "Hot cue 4: an empty pad keeps the playhead, a set pad sends the record there (with DEL lit, clears it)" deck_a_hot_3 := MarkPad{width: 20 text: "4"}}
        Tip{text: "Hot cue 5: an empty pad keeps the playhead, a set pad sends the record there (with DEL lit, clears it)" deck_a_hot_4 := MarkPad{width: 20 text: "5"}}
        Tip{text: "Hot cue 6: an empty pad keeps the playhead, a set pad sends the record there (with DEL lit, clears it)" deck_a_hot_5 := MarkPad{width: 20 text: "6"}}
        Tip{text: "Hot cue 7: an empty pad keeps the playhead, a set pad sends the record there (with DEL lit, clears it)" deck_a_hot_6 := MarkPad{width: 20 text: "7"}}
        Tip{text: "Hot cue 8: an empty pad keeps the playhead, a set pad sends the record there (with DEL lit, clears it)" deck_a_hot_7 := MarkPad{width: 20 text: "8"}}
        Tip{text: "Clear: the next hot cue or loop slot pressed is emptied" deck_a_mark_del := MarkPad{width: 30 text: "DEL"}}
        Tip{text: "Loop slot 1: an empty slot keeps the loop that is set, a full one puts its loop back on and plays it (with DEL lit, clears it)" deck_a_lslot_0 := MarkPad{width: 24 text: "L1"}}
        Tip{text: "Loop slot 2: an empty slot keeps the loop that is set, a full one puts its loop back on and plays it (with DEL lit, clears it)" deck_a_lslot_1 := MarkPad{width: 24 text: "L2"}}
        Tip{text: "Loop slot 3: an empty slot keeps the loop that is set, a full one puts its loop back on and plays it (with DEL lit, clears it)" deck_a_lslot_2 := MarkPad{width: 24 text: "L3"}}
        Tip{text: "Loop slot 4: an empty slot keeps the loop that is set, a full one puts its loop back on and plays it (with DEL lit, clears it)" deck_a_lslot_3 := MarkPad{width: 24 text: "L4"}}
                }
            }

            deck_lanes := View{
                width: Fill height: Fill flow: Down spacing: 6

                // the stacked wave lanes: A over B, one column
                SurfaceWell{
                    width: Fill height: Fill flow: Down spacing: 2
                    View{
                        width: Fill height: Fill flow: Overlay
                        deck_a_lane := StageMusicWave{}
                        deck_a_lane_note := Label{
                            width: Fill height: Fit padding: Inset{left: 12 right: 0 top: 8 bottom: 0}
                            text: "deck A — load a track from the list below"
                            draw_text.color: stage.text draw_text.text_style.font_size: 10
                        }
                    }
                    View{
                        width: Fill height: Fill flow: Overlay
                        deck_b_lane := StageMusicWave{}
                        deck_b_lane_note := Label{
                            width: Fill height: Fit padding: Inset{left: 12 right: 0 top: 8 bottom: 0}
                            text: "deck B — load a track from the list below"
                            draw_text.color: stage.text draw_text.text_style.font_size: 10
                        }
                    }
                }

                xfade_strip := mod.widgets.StageMusicWrapStrip{
                    width: Fill height: Fit flow: Flow.Right{wrap: true row_align: RowAlign.Center} spacing: 6 wrap_spacing: 6 align: Align{x: 0.5, y: 0.5}
                    strip_shaping := View{
                        width: Fit height: Fit flow: Flow.Right{wrap: true row_align: RowAlign.Center} spacing: 6 wrap_spacing: 6 align: Align{x: 0.5, y: 0.5}
                        Tip{text: "How long FADE, the A and B buttons and the Auto DJ take to cross, in seconds" xfade_secs := Slider{width: 118 margin: 0 text: "duration" min: 0.05 max: 20.0 default: 4.0}}
                        Tip{text: "Fade to the other deck over the duration" xfade_now := SurfaceButton{width: 46 text: "FADE"}}
                        Tip{text: "Cut to the other deck at once" xfade_switch := SurfaceButton{width: 40 text: "CUT"}}
                        Tip{text: "Timed fades swap the basslines: the incoming low band comes in as the outgoing one goes, halfway" xfade_eq := MarkPad{width: 34 text: "EQ"}}
                        Tip{text: "Turn the crossfader round: B on the left" xfade_rev := MarkPad{width: 40 text: "REV"}}
                    }
                    fader_cue_a := View{
                        width: Fit height: Fit flow: Right spacing: 6 align: Align{x: 0.0, y: 0.5}
                        Tip{text: "Fade to this deck over the duration and start it" xfade_to_a := SurfaceButton{width: 40 text: "◀ A"}}
                        xfade_label_a := Label{width: 12 text: "A" draw_text.color: stage.label draw_text.text_style.font_size: 9}
                    }
                    fader_sweep := View{
                        width: Fit height: Fit flow: Right align: Align{x: 0.5, y: 0.5}
                        crossfader := StageMusicCrossFader{width: Fill margin: 0 label_walk: Walk{width: 0 height: 0 margin: 0}}
                    }
                    fader_cue_b := View{
                        width: Fit height: Fit flow: Right spacing: 6 align: Align{x: 0.0, y: 0.5}
                        xfade_label_b := Label{width: 12 text: "B" draw_text.color: stage.label draw_text.text_style.font_size: 9}
                        Tip{text: "Fade to this deck over the duration and start it" xfade_to_b := SurfaceButton{width: 40 text: "B ▶"}}
                    }
                    strip_automation := View{
                        width: Fit height: Fit flow: Flow.Right{wrap: true row_align: RowAlign.Center} spacing: 6 wrap_spacing: 6 align: Align{x: 0.5, y: 0.5}
                        Tip{text: "Swap what decks A and B play, each keeping its place" xfade_swap := SurfaceButton{width: 46 text: "SWAP"}}
                        // The headphones: what the cue blends with the room,
                        // a stereo device split, and the cue's level.
                        Tip{text: "Headphones: fully left only the cued decks, fully right only the room (the program)" cue_blend := SurfaceKnob{width: 30 height: 30 min: 0 max: 1 default: 0}}
                        Tip{text: "A stereo device split for the headphones: the cue in the left ear, the program in the right" cue_split := MarkPad{width: 44 text: "SPLIT"}}
                        Tip{text: "The headphone cue's level" cue_level := Label{width: 76 text: "CUE —" draw_text.color: stage.label draw_text.text_style.font_size: 9}}
                        Tip{text: "A fade that starts the other deck (FADE, A and B, the Auto DJ) syncs it first" xfade_auto_sync := MarkPad{width: 76 text: "AUTO SYNC"}}
                        Tip{text: "Play the set list through: the other deck takes the next track and the room fades over to it as the live record ends" xfade_auto_dj := MarkPad{width: 66 text: "AUTO DJ"}}
                    }
                }
            }

            deck_b_panel := View{
                width: 316 height: Fill flow: Down spacing: 5

                // deck B's strip, mirrored
                deck_b_strip := ScrollYView{
                    width: Fill height: Fill flow: Down
                    scroll_bars.scroll_bar_y.drag_scrolling: false
                    View{
                        width: Fill height: Fill{min: 280} flow: Down spacing: 6
                        View{
                            width: Fill height: Fit flow: Right spacing: 4 align: Align{x: 1.0, y: 0.5}
                            Tip{text: "Cycle tempo range: ±8%, ±16%, ±50%, or 0.25–4×; expands to show the current speed" deck_b_range := SurfaceButton{width: 44 text: "±8%"}}
                            Tip{text: "Deck B a semitone up, at the same tempo" deck_b_key_up := SurfaceButton{width: 22 padding: 0 text: "+"}}
                            Tip{text: "Deck B a semitone down, at the same tempo" deck_b_key_down := SurfaceButton{width: 22 padding: 0 text: "-"}}
                            Tip{text: "Key lock on deck B: the tempo moves with the speed, the key does not (time-stretched). Shows the key shift" deck_b_keylock := SurfaceButton{width: 44 text: "KEY"}}
                            Tip{text: "Hear deck B in the headphones before the crossfader (the cue: channels 3-4 of the device, or the left ear with SPLIT)" deck_b_hp := StageMusicIconButton{width: 30 height: 22 margin: 0 padding: 0 align: Align{x: 0.5 y: 0.5} draw_icon +: {svg: crate_resource("self:resources/icons/headphones.svg")}}}
                            Tip{text: "Flip deck B's beat grid half a beat when its count sits on the off pulse (lit while flipped)" deck_b_phase := SurfaceButton{width: 26 padding: 0 text: "½"}}
                            Tip{text: "Slip: while deck B loops or is under the hand, the record runs on unheard; letting go (or ending the loop) plays from where it would be" deck_b_slip := SurfaceButton{width: 40 text: "SLIP"}}
                            Tip{text: "Match deck B's tempo to the other deck as the room hears it, and its beat onto the other's while both play (the pitch follows unless KEY locks it)" deck_b_sync := SurfaceButton{width: Fill text: "SYNC"}}
                        }
                        View{
                            width: Fill height: Fill flow: Right spacing: 8
                            View{
                                width: Fill height: Fill flow: Down spacing: 6
                                View{
                                    width: Fill height: Fit flow: Right spacing: 3 align: Align{x: 1.0, y: 0.0}
                                    FilterColumn{knob_tip +: {eq_knob +: {bind := "deck_b_filter.position"}}}
                                    KnobColumn{caption +: {text: "LOW"} knob_tip +: {eq_knob +: {bind := "deck_b_eq.low"}} ms +: {kill_tip +: {kill +: {bind := "deck_b_eq.kill_low"}} solo_tip +: {solo +: {bind := "deck_b_eq.solo_low"}}}}
                                    KnobColumn{caption +: {text: "MID"} knob_tip +: {eq_knob +: {bind := "deck_b_eq.mid"}} ms +: {kill_tip +: {kill +: {bind := "deck_b_eq.kill_mid"}} solo_tip +: {solo +: {bind := "deck_b_eq.solo_mid"}}}}
                                    KnobColumn{caption +: {text: "HIGH"} knob_tip +: {eq_knob +: {bind := "deck_b_eq.high"}} ms +: {kill_tip +: {kill +: {bind := "deck_b_eq.kill_high"}} solo_tip +: {solo +: {bind := "deck_b_eq.solo_high"}}}}
                                }
                                SurfaceCaption{text: "STEM MIX" draw_text.color: stage.live}
                                View{
                                    width: Fill height: Fit flow: Right spacing: 4 align: Align{x: 1.0, y: 0.0}
                                    deck_b_stem_drums := StemColumn{
                                        caption +: {text: "DRUMS"}
                                        gain_tip +: {gain +: {bind := "deck_b_stems.gain_drums"}}
                                        ms +: {
                                            mute_tip +: {mute +: {bind := "deck_b_stems.mute_drums"}}
                                            solo_tip +: {solo +: {bind := "deck_b_stems.solo_drums"}}
                                        }
                                    }
                                    deck_b_stem_bass := StemColumn{
                                        caption +: {text: "BASS"}
                                        gain_tip +: {gain +: {bind := "deck_b_stems.gain_bass"}}
                                        ms +: {
                                            mute_tip +: {mute +: {bind := "deck_b_stems.mute_bass"}}
                                            solo_tip +: {solo +: {bind := "deck_b_stems.solo_bass"}}
                                        }
                                    }
                                    deck_b_stem_vocals := StemColumn{
                                        caption +: {text: "VOCALS"}
                                        gain_tip +: {gain +: {bind := "deck_b_stems.gain_vocals"}}
                                        ms +: {
                                            mute_tip +: {mute +: {bind := "deck_b_stems.mute_vocals"}}
                                            solo_tip +: {solo +: {bind := "deck_b_stems.solo_vocals"}}
                                        }
                                    }
                                    deck_b_stem_other := StemColumn{
                                        caption +: {text: "OTHER"}
                                        gain_tip +: {gain +: {bind := "deck_b_stems.gain_other"}}
                                        ms +: {
                                            mute_tip +: {mute +: {bind := "deck_b_stems.mute_other"}}
                                            solo_tip +: {solo +: {bind := "deck_b_stems.solo_other"}}
                                        }
                                    }
                                }
                                SurfaceCaption{text: "KARAOKE" draw_text.color: stage.live}
                                // The deck's own picture, under its lyrics.
                                SurfaceWell{height: Fill{min: 36} flow: Overlay
                                    deck_b_video := mod.widgets.StageMusicVideo{width: Fill height: Fill visible: false}
                                    deck_b_karaoke := mod.widgets.StageMusicLyrics{width: Fill height: Fill}
                                }
                            }
                            View{width: Fit height: Fill flow: Right spacing: 6
                            View{width: 10 height: Fill flow: Down spacing: 2 align: Align{x: 0.5 y: 0}
                                SurfaceCaption{text: ""}
                                deck_b_vu_tip := Tip{text: "Deck peak is waiting for audio" deck_b_vu := StageMusicDeckMeter{}}
                            }
                            View{
                                width: 44 height: Fill flow: Down spacing: 2 align: Align{x: 0.5, y: 0.0}
                                SurfaceCaption{text: "VOL"}
                                deck_b_vol := SurfaceFader{bind := "deck_b_gain.gain" min: 0.0 max: 1.5 default: 1.0}
                                Tip{text: "Mute this deck without changing its volume" deck_b_mute := DeckMute{bind := "deck_b_gain.mute"}}
                            }
                            View{
                                width: 44 height: Fill flow: Down spacing: 2 align: Align{x: 0.5, y: 0.0}
                                SurfaceCaption{text: "TEMPO"}
                                deck_b_tempo := SurfaceFader{bind := "deck_b.speed" min: 0.92 max: 1.08 default: 1.0 arc_from_origin: true}
                                deck_b_tempo_reset := SurfaceButton{width: Fill height: 14 padding: 0 text: "0"}
                            }
                            }
                        }
                    }

                }

                View{
                    width: Fill height: Fit flow: Flow.Right{wrap: true row_align: RowAlign.Center} spacing: 3 wrap_spacing: 3 align: Align{x: 0.0, y: 0.5}
                    Tip{text: "Bake this track on the AI hub: its stems, lyrics and BPM; the deck plays on with its stems when they are in (again: stop)" deck_b_bake := SurfaceTransportIcon{width: 26 icon_walk: Walk{width: 13 height: Fit} draw_icon +: {svg: crate_resource("self:resources/icons/sparkle.svg")}}}
                    deck_b_loop_out := SurfaceTransport{width: 22 text: "]"}
                    deck_b_loop_in := SurfaceTransport{width: 22 text: "["}
                    Tip{text: "Double the beat loop's length" deck_b_loop_longer := SurfaceTransport{width: 22 text: "+"}}
                    Tip{text: "A loop this many beats long from the beat at the playhead, on the track's beat grid" deck_b_loop_len := SurfaceTransport{width: 30 text: "4"}}
                    Tip{text: "Halve the beat loop's length" deck_b_loop_shorter := SurfaceTransport{width: 22 text: "-"}}
                    deck_b_loop := SurfaceTransportIcon{width: 26 icon_walk: Walk{width: 13 height: Fit} draw_icon +: {svg: crate_resource("self:resources/icons/loop_one.svg")}}
                    Tip{text: "Hold to slow deck B 4 %" deck_b_nudge_back := SurfaceTransport{text: "<"}}
                    Tip{text: "Hold to push deck B 4 % faster" deck_b_nudge_fwd := SurfaceTransport{text: ">"}}
                    deck_b_cue := SurfaceTransportIcon{width: 38 icon_walk: Walk{width: 14 height: Fit} draw_icon +: {svg: crate_resource("self:resources/icons/cue.svg")}}
                    deck_b_play := SurfaceTransportIcon{width: 26 icon_walk: Walk{width: 13 height: Fit} draw_icon +: {svg: crate_resource("self:resources/icons/play.svg")}}
                }
                // THE MARKS: eight hot cues and four loop slots, kept per
                // track; DEL makes the next press clear one.
                View{
                    width: Fill height: Fit flow: Flow.Right{wrap: true row_align: RowAlign.Center} spacing: 2 wrap_spacing: 3 align: Align{x: 1.0, y: 0.5}
        Tip{text: "Hot cue 1: an empty pad keeps the playhead, a set pad sends the record there (with DEL lit, clears it)" deck_b_hot_0 := MarkPad{width: 20 text: "1"}}
        Tip{text: "Hot cue 2: an empty pad keeps the playhead, a set pad sends the record there (with DEL lit, clears it)" deck_b_hot_1 := MarkPad{width: 20 text: "2"}}
        Tip{text: "Hot cue 3: an empty pad keeps the playhead, a set pad sends the record there (with DEL lit, clears it)" deck_b_hot_2 := MarkPad{width: 20 text: "3"}}
        Tip{text: "Hot cue 4: an empty pad keeps the playhead, a set pad sends the record there (with DEL lit, clears it)" deck_b_hot_3 := MarkPad{width: 20 text: "4"}}
        Tip{text: "Hot cue 5: an empty pad keeps the playhead, a set pad sends the record there (with DEL lit, clears it)" deck_b_hot_4 := MarkPad{width: 20 text: "5"}}
        Tip{text: "Hot cue 6: an empty pad keeps the playhead, a set pad sends the record there (with DEL lit, clears it)" deck_b_hot_5 := MarkPad{width: 20 text: "6"}}
        Tip{text: "Hot cue 7: an empty pad keeps the playhead, a set pad sends the record there (with DEL lit, clears it)" deck_b_hot_6 := MarkPad{width: 20 text: "7"}}
        Tip{text: "Hot cue 8: an empty pad keeps the playhead, a set pad sends the record there (with DEL lit, clears it)" deck_b_hot_7 := MarkPad{width: 20 text: "8"}}
        Tip{text: "Clear: the next hot cue or loop slot pressed is emptied" deck_b_mark_del := MarkPad{width: 30 text: "DEL"}}
        Tip{text: "Loop slot 1: an empty slot keeps the loop that is set, a full one puts its loop back on and plays it (with DEL lit, clears it)" deck_b_lslot_0 := MarkPad{width: 24 text: "L1"}}
        Tip{text: "Loop slot 2: an empty slot keeps the loop that is set, a full one puts its loop back on and plays it (with DEL lit, clears it)" deck_b_lslot_1 := MarkPad{width: 24 text: "L2"}}
        Tip{text: "Loop slot 3: an empty slot keeps the loop that is set, a full one puts its loop back on and plays it (with DEL lit, clears it)" deck_b_lslot_2 := MarkPad{width: 24 text: "L3"}}
        Tip{text: "Loop slot 4: an empty slot keeps the loop that is set, a full one puts its loop back on and plays it (with DEL lit, clears it)" deck_b_lslot_3 := MarkPad{width: 24 text: "L4"}}
                }
            }

        }
    }
}

/// Edits from the surface. Applying state does not emit these.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum MusicSurfaceAction {
    /// Deck 0 (A) or 1 (B): the same edits a deck strip reports.
    Deck(usize, DeckStripAction),
    /// A deck's marks: a hot cue or loop slot pressed, or a beat loop.
    Mark(usize, MarkAsk),
    /// A deck's headphone button pressed.
    Headphones(usize),
    /// The headphones' blend of cue and room, 0 cue .. 1 room.
    CueBlend(f64),
    /// A stereo device split for the headphones, or not.
    CueSplit(bool),
    /// The crossfader, 0 = A, 1 = B.
    Crossfade(f64),
    /// The timed fades' duration, seconds.
    FadeSecs(f64),
    /// Fade to the other deck over the duration.
    FadeNow,
    /// Cut to the other deck at once.
    Cut,
    /// Fade to this deck over the duration and start it.
    FadeTo(usize),
    /// Timed fades swap the basslines, or not.
    EqFade(bool),
    /// The crossfader turned round (B on the left), or not.
    Reverse(bool),
    AutoDj(bool),
    AutoSync(bool),
    /// SYNC on deck 0 (A) or 1 (B).
    Sync(usize),
    /// A nudge on a deck: -1 slower or 1 faster, pressed (true) or let go.
    Nudge(usize, i8, bool),
    /// KEY on a deck: the key lock flips.
    KeyLock(usize),
    /// A deck's key a semitone down (-1) or up (1).
    Key(usize, i8),
    /// SLIP on a deck flips.
    Slip(usize),
    /// A deck's QUANT steps to its next unit.
    Quant(usize),
    /// ½ on a deck: its beat grid flips half a beat.
    PhaseFlip(usize),
    /// The bake icon on a deck: its track's stems, lyrics and BPM on the AI
    /// hub, or stop the bake while it runs.
    Bake(usize),
    #[default]
    None,
}

/// A deck's key lock, key shift (semitones), slip, QUANT and grid flip, as
/// the surface shows them.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DeckKeyView {
    pub keylock: bool,
    pub key: i32,
    pub slip: bool,
    /// QUANT's unit in beats, 0 off.
    pub quant: u32,
    pub phase_flipped: bool,
}

/// What a press on a deck's marks asks for. The host keeps the marks and
/// decides: an empty pad is set, a full one used.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MarkAsk {
    HotCue(usize),
    ClearHotCue(usize),
    LoopSlot(usize),
    ClearLoopSlot(usize),
    /// A loop this many beats long from the beat at the playhead.
    BeatLoop(f64),
}

/// A deck's marks as the pads show them.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DeckMarksView {
    pub hot: [Option<f64>; HOT_CUES],
    pub loops: [Option<(f64, f64)>; LOOP_SLOTS],
}

pub const HOT_CUES: usize = 8;
pub const LOOP_SLOTS: usize = 4;
/// The beat loop lengths -/+ step through.
pub const LOOP_BEATS: [f64; 10] = [0.125, 0.25, 0.5, 1.0, 2.0, 4.0, 8.0, 16.0, 32.0, 64.0];

/// A beat count as the length button reads it: "4", "1/2".
pub fn beats_label(beats: f64) -> String {
    if beats >= 1.0 { format!("{beats:.0}") } else { format!("1/{:.0}", 1.0 / beats) }
}

fn mark_ids(deck: usize) -> ([LiveId; HOT_CUES], [LiveId; LOOP_SLOTS], LiveId, LiveId, LiveId, LiveId) {
    if deck == 0 {
        (
            [live_id!(deck_a_hot_0), live_id!(deck_a_hot_1), live_id!(deck_a_hot_2), live_id!(deck_a_hot_3), live_id!(deck_a_hot_4), live_id!(deck_a_hot_5), live_id!(deck_a_hot_6), live_id!(deck_a_hot_7)],
            [live_id!(deck_a_lslot_0), live_id!(deck_a_lslot_1), live_id!(deck_a_lslot_2), live_id!(deck_a_lslot_3)],
            live_id!(deck_a_mark_del), live_id!(deck_a_loop_shorter), live_id!(deck_a_loop_len), live_id!(deck_a_loop_longer),
        )
    } else {
        (
            [live_id!(deck_b_hot_0), live_id!(deck_b_hot_1), live_id!(deck_b_hot_2), live_id!(deck_b_hot_3), live_id!(deck_b_hot_4), live_id!(deck_b_hot_5), live_id!(deck_b_hot_6), live_id!(deck_b_hot_7)],
            [live_id!(deck_b_lslot_0), live_id!(deck_b_lslot_1), live_id!(deck_b_lslot_2), live_id!(deck_b_lslot_3)],
            live_id!(deck_b_mark_del), live_id!(deck_b_loop_shorter), live_id!(deck_b_loop_len), live_id!(deck_b_loop_longer),
        )
    }
}

/// The controls that act on the loaded track and so hold disabled while a
/// deck is empty: the mark pads, DEL, the beat loop steppers, SYNC, the
/// nudges and the grid flip.
fn track_controls(deck: usize) -> Vec<LiveId> {
    let (hot, loops, del, shorter, len, longer) = mark_ids(deck);
    let own = if deck == 0 {
        [live_id!(deck_a_sync), live_id!(deck_a_nudge_back), live_id!(deck_a_nudge_fwd), live_id!(deck_a_phase)]
    } else {
        [live_id!(deck_b_sync), live_id!(deck_b_nudge_back), live_id!(deck_b_nudge_fwd), live_id!(deck_b_phase)]
    };
    hot.into_iter().chain(loops).chain([del, shorter, len, longer]).chain(own).collect()
}

/// The ids of one deck's controls, so both decks run the same code.
struct DeckIds {
    eject: LiveId,
    title: LiveId,
    pitch: LiveId,
    time: LiveId,
    overview: LiveId,
    lane: LiveId,
    lane_note: LiveId,
    karaoke: LiveId,
    video: LiveId,
    tempo: LiveId,
    tempo_reset: LiveId,
    play: LiveId,
    cue: LiveId,
    loop_toggle: LiveId,
    loop_in: LiveId,
    loop_out: LiveId,
}

fn deck_ids(deck: usize) -> DeckIds {
    if deck == 0 {
        DeckIds {
            eject: live_id!(deck_a_eject), title: live_id!(deck_a_title), pitch: live_id!(deck_a_pitch), time: live_id!(deck_a_time),
            overview: live_id!(deck_a_overview), lane: live_id!(deck_a_lane), lane_note: live_id!(deck_a_lane_note), karaoke: live_id!(deck_a_karaoke), video: live_id!(deck_a_video),
            tempo: live_id!(deck_a_tempo), tempo_reset: live_id!(deck_a_tempo_reset),
            play: live_id!(deck_a_play), cue: live_id!(deck_a_cue), loop_toggle: live_id!(deck_a_loop),
            loop_in: live_id!(deck_a_loop_in), loop_out: live_id!(deck_a_loop_out),
        }
    } else {
        DeckIds {
            eject: live_id!(deck_b_eject), title: live_id!(deck_b_title), pitch: live_id!(deck_b_pitch), time: live_id!(deck_b_time),
            overview: live_id!(deck_b_overview), lane: live_id!(deck_b_lane), lane_note: live_id!(deck_b_lane_note), karaoke: live_id!(deck_b_karaoke), video: live_id!(deck_b_video),
            tempo: live_id!(deck_b_tempo), tempo_reset: live_id!(deck_b_tempo_reset),
            play: live_id!(deck_b_play), cue: live_id!(deck_b_cue), loop_toggle: live_id!(deck_b_loop),
            loop_in: live_id!(deck_b_loop_in), loop_out: live_id!(deck_b_loop_out),
        }
    }
}

/// The timed fades' settings as the strip shows them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct XfadeView {
    pub secs: f64,
    pub eq: bool,
    pub reverse: bool,
    pub auto: bool,
    pub auto_sync: bool,
}

/// A partly measured source's two pictures for one deck, with where each
/// sits and which of its columns are measured. Plain view data: no analysis
/// types.
#[derive(Clone)]
pub struct WaveView {
    pub overview: Arc<WaveData>,
    pub overview_coverage: WaveCoverage,
    pub detail: Arc<WaveData>,
    pub detail_coverage: WaveCoverage,
}

#[derive(Script, ScriptHook, Widget)]
pub struct MusicSurface {
    #[deref]
    view: View,
    #[rust]
    decks: [Option<DeckStripState>; 2],
    #[rust]
    dragging: [bool; 2],
    #[rust]
    wave_data: [Option<Arc<WaveData>>; 2],
    #[rust]
    wave_textures: [Option<Rc<MusicWaveTextures>>; 2],
    /// Each deck's measured pictures (overview, detail), uploaded once per
    /// report revision and shared by both decks when they show the same one.
    #[rust]
    placed: [Option<(Rc<PlacedWave>, Rc<PlacedWave>)>; 2],
    #[rust]
    waves_applied: [bool; 2],
    #[rust]
    pitch_ranges: [usize; 2],
    #[rust]
    pitch_applied: [Option<usize>; 2],
    #[rust]
    deck_peaks: [Option<Option<f64>>; 2],
    #[rust]
    crossfade: Option<f64>,
    #[rust]
    crossfade_enabled: Option<bool>,
    /// Which deck panel shows in a narrow window, and what moves it.
    #[rust]
    tabs: DeckTabs,
    /// The window is too narrow for both deck panels (as last drawn).
    #[rust]
    narrow: Option<bool>,
    #[rust]
    stems_available: [Option<bool>; 2],
    #[rust]
    stem_palette: [Option<u64>; 2],
    /// Each deck's marks as the pads show them, as last painted.
    #[rust]
    marks: [Option<DeckMarksView>; 2],
    /// DEL lit: the next pad pressed is cleared.
    #[rust]
    mark_del: [bool; 2],
    /// Each deck's beat loop length, an index into [`LOOP_BEATS`].
    #[rust([5, 5])]
    loop_beats: [usize; 2],
    /// The headphone buttons as last painted.
    #[rust]
    headphones: [Option<bool>; 2],
    /// Each deck's key lock, key shift and slip as last painted.
    #[rust]
    keys: [Option<DeckKeyView>; 2],
    /// The timed fades' settings as shown.
    #[rust]
    xfade: Option<XfadeView>,
}

impl MusicSurface {
    /// The headphone buttons lit while their deck is cued, the split as
    /// set and the cue's level.
    /// A deck's KEY (lit when locked, the shift as its text), SLIP, QUANT
    /// (its unit as its text) and ½ (lit while flipped).
    pub fn set_deck_key(&mut self, cx: &mut Cx, deck: usize, view: DeckKeyView) {
        if deck > 1 || self.keys[deck] == Some(view) {
            return;
        }
        self.keys[deck] = Some(view);
        let p = makepad_look::palette::get(cx);
        let (lock, slip, quant, phase) = if deck == 0 {
            (live_id!(deck_a_keylock), live_id!(deck_a_slip), live_id!(quant_a), live_id!(deck_a_phase))
        } else {
            (live_id!(deck_b_keylock), live_id!(deck_b_slip), live_id!(quant_b), live_id!(deck_b_phase))
        };
        let text = if view.key == 0 { "KEY".to_string() } else { format!("{:+}", view.key) };
        let unit = if view.quant == 0 { "—".to_string() } else { view.quant.to_string() };
        for (id, lit, text) in [(lock, view.keylock, Some(text)), (slip, view.slip, None), (quant, false, Some(unit)), (phase, view.phase_flipped, None)] {
            let (look, hot) = (p.pick(lit, false, p.control), p.pick(lit, true, p.control));
            let (bg, hover, border, ink) = (Vec4f::from_u32(look.fill), Vec4f::from_u32(hot.fill), Vec4f::from_u32(look.edge), Vec4f::from_u32(look.ink));
            let mut button = self.button(cx, &[id]);
            if let Some(text) = text {
                button.set_text(cx, &text);
            }
            script_apply_eval!(cx, button, {
                draw_bg +: {color: #(bg) color_focus: #(bg) color_hover: #(hover) border_color: #(border)}
                draw_text +: {color: #(ink) color_focus: #(ink) color_hover: #(ink)}
            });
        }
        self.view.redraw(cx);
    }

    pub fn set_headphones(&mut self, cx: &mut Cx, cued: [bool; 2], split: bool, level: Option<f64>) {
        // Called every pump: only a change redraws.
        let palette = makepad_look::palette::get(cx);
        let mut changed = false;
        for (deck, on) in cued.into_iter().enumerate() {
            if self.headphones[deck] == Some(on) {
                continue;
            }
            self.headphones[deck] = Some(on);
            changed = true;
            let id = if deck == 0 { live_id!(deck_a_hp) } else { live_id!(deck_b_hp) };
            if let Some(mut button) = self.button(cx, &[id]).borrow_mut() {
                button.draw_icon.color = Vec4f::from_u32(if on { palette.accent } else { palette.label });
            }
        }
        let split_box = self.check_box(cx, ids!(cue_split));
        if split_box.active(cx) != split {
            split_box.set_active(cx, split, Animate::No);
        }
        let text = match level {
            Some(level) if level > 1e-4 => format!("CUE {:.0} dB", 20.0 * level.log10()),
            _ => "CUE —".to_string(),
        };
        let label = self.label(cx, ids!(cue_level));
        if label.text() != text {
            label.set_text(cx, &text);
        }
        if changed {
            self.view.redraw(cx);
        }
    }

    /// The timed fades' settings: the duration, EQ, REV and AUTO DJ lit,
    /// and the fader's ends named the way round it runs.
    pub fn set_xfade(&mut self, cx: &mut Cx, view: XfadeView) {
        if self.xfade == Some(view) {
            return;
        }
        let reversed = self.xfade.map(|held| held.reverse) != Some(view.reverse);
        self.xfade = Some(view);
        self.slider(cx, ids!(xfade_secs)).set_value(cx, view.secs);
        for (id, on) in [(ids!(xfade_eq), view.eq), (ids!(xfade_rev), view.reverse), (ids!(xfade_auto_dj), view.auto), (ids!(xfade_auto_sync), view.auto_sync)] {
            self.check_box(cx, id).set_active(cx, on, Animate::No);
        }
        if reversed {
            let (left, right) = if view.reverse { ("B", "A") } else { ("A", "B") };
            self.label(cx, ids!(xfade_label_a)).set_text(cx, left);
            self.label(cx, ids!(xfade_label_b)).set_text(cx, right);
            self.button(cx, ids!(xfade_to_a)).set_text(cx, &format!("◀ {left}"));
            self.button(cx, ids!(xfade_to_b)).set_text(cx, &format!("{right} ▶"));
            // The slider shows the fader the other way round now.
            if let Some(value) = self.crossfade {
                let shown = if view.reverse { 1.0 - value } else { value };
                self.slider(cx, ids!(crossfader)).set_value(cx, shown.clamp(0.0, 1.0));
            }
        }
        self.view.redraw(cx);
    }

    fn reversed(&self) -> bool {
        self.xfade.is_some_and(|view| view.reverse)
    }

    /// A deck's marks, painted onto its pads: a set pad lit.
    pub fn set_marks(&mut self, cx: &mut Cx, deck: usize, marks: DeckMarksView) {
        if self.marks[deck] == Some(marks) {
            return;
        }
        self.marks[deck] = Some(marks);
        self.paint_marks(cx, deck);
    }

    /// The pads as the marks say, DEL as armed: a press toggles a pad's own
    /// light, so it is put back from the model after every press.
    fn paint_marks(&mut self, cx: &mut Cx, deck: usize) {
        let marks = self.marks[deck].unwrap_or_default();
        let (hot, loops, del, ..) = mark_ids(deck);
        let lit: Vec<(LiveId, bool)> = hot.iter().zip(marks.hot).map(|(id, at)| (*id, at.is_some()))
            .chain(loops.iter().zip(marks.loops).map(|(id, span)| (*id, span.is_some())))
            .chain([(del, self.mark_del[deck])]).collect();
        for (id, on) in lit {
            self.check_box(cx, &[id]).set_active(cx, on, Animate::No);
        }
        self.view.redraw(cx);
    }

    fn handle_mark_actions(&mut self, cx: &mut Cx, actions: &Actions, deck: usize, transport: bool) {
        let uid = self.widget_uid();
        let (hot, loops, del, shorter, len, longer) = mark_ids(deck);
        let emit = |cx: &mut Cx, ask: MarkAsk| cx.widget_action(uid, MusicSurfaceAction::Mark(deck, ask));
        let pressed = |cx: &mut Cx, this: &Self, id: LiveId| this.check_box(cx, &[id]).changed(actions).is_some();
        let mut touched = false;
        if pressed(cx, self, del) {
            self.mark_del[deck] = !self.mark_del[deck];
            touched = true;
        }
        let clearing = self.mark_del[deck];
        let mut used = false;
        for (slot, id) in hot.iter().enumerate() {
            if pressed(cx, self, *id) {
                touched = true;
                if transport {
                    emit(cx, if clearing { MarkAsk::ClearHotCue(slot) } else { MarkAsk::HotCue(slot) });
                    used = true;
                }
            }
        }
        for (slot, id) in loops.iter().enumerate() {
            if pressed(cx, self, *id) {
                touched = true;
                if transport {
                    emit(cx, if clearing { MarkAsk::ClearLoopSlot(slot) } else { MarkAsk::LoopSlot(slot) });
                    used = true;
                }
            }
        }
        if used && clearing {
            self.mark_del[deck] = false;
        }
        if touched {
            self.paint_marks(cx, deck);
        }
        let step = |at: usize, by: isize| (at as isize + by).clamp(0, LOOP_BEATS.len() as isize - 1) as usize;
        let mut relabel = false;
        if self.button(cx, &[shorter]).clicked(actions) {
            self.loop_beats[deck] = step(self.loop_beats[deck], -1);
            relabel = true;
        }
        if self.button(cx, &[longer]).clicked(actions) {
            self.loop_beats[deck] = step(self.loop_beats[deck], 1);
            relabel = true;
        }
        if relabel {
            self.button(cx, &[len]).set_text(cx, &beats_label(LOOP_BEATS[self.loop_beats[deck]]));
        }
        if self.button(cx, &[len]).clicked(actions) && transport {
            emit(cx, MarkAsk::BeatLoop(LOOP_BEATS[self.loop_beats[deck]]));
        }
    }
    pub fn prepared_wave(&self, deck: usize) -> Option<PreparedWaveHandle> {
        self.wave_textures.get(deck)?.clone().map(PreparedWaveHandle)
    }
    /// The stem columns' gain, mute and solo of `deck`: the controls a deck
    /// without playable stems holds disabled (the host gates its binder
    /// with them).
    pub fn stem_controls(&self, cx: &Cx, deck: usize) -> Vec<WidgetUid> {
        let mut controls = Vec::new();
        for id in stem_columns(deck) {
            let column = self.widget(cx, &[id]);
            for control in [ids!(gain), ids!(mute), ids!(solo)] {
                controls.push(column.widget(cx, control).widget_uid());
            }
        }
        controls
    }

    /// Reassert after the host's binding refresh. The binder owns admitted
    /// values and enabling; absent source stems only add a disabled gate
    /// (the same one the host gave the binder, see `stem_controls`).
    pub fn set_stems_available(&mut self, cx: &mut Cx, deck: usize, available: bool) {
        let Some(previous) = self.stems_available.get(deck).copied() else { return };
        let palette = makepad_look::palette::get(cx);
        let generation = makepad_look::palette::generation(cx);
        for (id, color) in stem_columns(deck).into_iter().zip(palette.stem_colors()) {
            let column = self.widget(cx, &[id]);
            if !available {
                for control in [ids!(gain), ids!(mute), ids!(solo)] {
                    column.widget(cx, control).set_disabled(cx, true);
                }
            }
            if previous != Some(available) || self.stem_palette[deck] != Some(generation) {
                let color = Vec4f::from_u32(if available { color } else { palette.dim });
                let pointer = Vec4f::from_u32(if available { palette.text_bright } else { palette.dim });
                let mut caption = column.widget(cx, ids!(caption));
                script_apply_eval!(cx, caption, { draw_text +: {color: #(color)} });
                let mut gain = column.widget(cx, ids!(gain));
                script_apply_eval!(cx, gain, { draw_bg +: {val_color: #(color) pointer_color: #(pointer)} });
                column.tip(cx, ids!(gain_tip)).set_text(if available {
                    "Gain of this measured stem"
                } else { "This track has no complete playable stem set" });
                column.tip(cx, ids!(mute_tip)).set_text(if available {
                    "Mute this stem without changing its gain"
                } else { "This track has no complete playable stem set" });
                column.tip(cx, ids!(solo_tip)).set_text(if available {
                    "Hear this stem; multiple solos combine"
                } else { "This track has no complete playable stem set" });
                column.redraw(cx);
            }
        }
        if !available {
            self.widget(cx, &[deck_ids(deck).lane]).as_music_wave().set_stem_levels(cx, [1.0; 4]);
        }
        self.stems_available[deck] = Some(available);
        self.stem_palette[deck] = Some(generation);
    }

    pub fn set_stem_levels(&mut self, cx: &mut Cx, deck: usize, levels: [f32; 4]) {
        if deck >= self.decks.len() { return; }
        let levels = if self.stems_available[deck] == Some(false) { [1.0; 4] } else { levels };
        self.widget(cx, &[deck_ids(deck).lane]).as_music_wave().set_stem_levels(cx, levels);
    }

    pub fn set_lyrics(&mut self, cx: &mut Cx, deck: usize, data: Option<Arc<LyricsData>>) {
        if deck >= self.decks.len() { return; }
        if let Some(mut lyrics) = self.widget(cx, &[deck_ids(deck).karaoke]).borrow_mut::<MusicLyrics>() {
            lyrics.set_data(cx, data);
        }
    }

    /// The deck's own picture well, for the host to bind.
    pub fn deck_video(&self, cx: &Cx, deck: usize) -> crate::music_video::StageMusicVideoRef {
        use crate::music_video::StageMusicVideoWidgetRefExt;
        self.widget(cx, &[deck_ids(deck.min(1)).video]).as_stage_music_video()
    }

    /// Show the deck's picture under its lyrics; while it shows, a deck
    /// without timed lyrics draws no placeholder over it.
    pub fn set_deck_picture(&mut self, cx: &mut Cx, deck: usize, shown: bool) {
        if deck >= self.decks.len() { return; }
        let ids = deck_ids(deck);
        self.widget(cx, &[ids.video]).set_visible(cx, shown);
        if let Some(mut lyrics) = self.widget(cx, &[ids.karaoke]).borrow_mut::<MusicLyrics>() {
            if lyrics.over_video != shown {
                lyrics.over_video = shown;
                lyrics.area.redraw(cx);
            }
        }
    }

    /// A debugger picture borrows the loaded picture, never uploads it again.
    pub fn sync_waveform_preview(&self, cx: &mut Cx, deck: usize, preview: &MusicWaveRef) {
        let Some(textures) = self.wave_textures.get(deck) else { return };
        preview.set_textures(cx, textures.clone());
        if let Some(state) = &self.decks[deck] {
            preview.set_state(cx, deck, state);
        }
        preview.set_disabled(cx, true);
    }

    // Range selection changes the ruler only. The declarative binder owns
    // the value and queued intent, including after an automatic expansion.
    fn sync_pitch_range(&mut self, cx: &mut Cx, deck: usize, speed: f64) {
        const RANGES: [(f64, f64, &str); 4] = [
            (0.92, 1.08, "±8%"), (0.84, 1.16, "±16%"),
            (0.5, 1.5, "±50%"), (0.25, 4.0, "WIDE"),
        ];
        let mut selected = self.pitch_ranges[deck];
        if speed.is_finite() {
            while selected < RANGES.len() - 1
                && !(RANGES[selected].0..=RANGES[selected].1).contains(&speed)
            {
                selected += 1;
            }
        }
        self.pitch_ranges[deck] = selected;
        if self.pitch_applied[deck] == Some(selected) { return; }
        let (min, max, label) = RANGES[selected];
        let mut slider = self.widget(cx, &[deck_ids(deck).tempo]);
        script_apply_eval!(cx, slider, { min: #(min) max: #(max) });
        let range_id = if deck == 0 { live_id!(deck_a_range) } else { live_id!(deck_b_range) };
        self.button(cx, &[range_id]).set_text(cx, label);
        self.pitch_applied[deck] = Some(selected);
        slider.redraw(cx);
    }

    /// A measured stereo peak after this deck's volume and mute. Missing
    /// observations clear the meter and identify its unavailable readout.
    pub fn set_deck_peak(&mut self, cx: &mut Cx, deck: usize, peak: Option<f64>) {
        let peak = peak.filter(|value| value.is_finite()).map(|value| value.max(0.0));
        if self.deck_peaks[deck] == Some(peak) { return; }
        let (meter_id, tip_id) = if deck == 0 {
            (live_id!(deck_a_vu), live_id!(deck_a_vu_tip))
        } else {
            (live_id!(deck_b_vu), live_id!(deck_b_vu_tip))
        };
        let level = peak.unwrap_or(0.0).min(1.0) as f32;
        let mut meter = self.widget(cx, &[meter_id]);
        // The mark is this observation, not an invented hold or another bus.
        script_apply_eval!(cx, meter, { draw_bg +: { level: #(level) hold: #(level) } });
        self.tip(cx, &[tip_id]).set_text(if peak.is_some() {
            "Stereo peak after this deck's volume and mute"
        } else { "Deck peak is waiting for audio" });
        meter.redraw(cx);
        self.deck_peaks[deck] = Some(peak);
    }

    pub fn set_deck_state(&mut self, cx: &mut Cx, deck: usize, state: &DeckStripState) {
        self.sync_pitch_range(cx, deck, state.speed);
        let ids = deck_ids(deck);
        let prev = self.decks[deck].clone();
        let transport = state.ready && state.has_track;
        let prev_transport = prev.as_ref().map(|p| p.ready && p.has_track);
        if prev.as_ref().map(|p| p.active_title.as_str()) != Some(state.active_title.as_str()) {
            let title = if state.has_track { state.active_title.as_str() } else { "empty" };
            self.label(cx, &[ids.title]).set_text(cx, title);
        }
        let note = if transport { state.status.clone() } else if state.status.is_empty() {
            format!("deck {} — load a track from the list below", if deck == 0 { "A" } else { "B" })
        } else {
            state.status.clone()
        };
        if prev.as_ref().map(|p| (p.ready && p.has_track, p.status.as_str())) != Some((transport, state.status.as_str())) {
            self.label(cx, &[ids.lane_note]).set_text(cx, &note);
        }
        let clock_changed = prev.as_ref().map_or(true, |p| clock_secs(p.position) != clock_secs(state.position) || clock_secs(p.duration) != clock_secs(state.duration));
        if clock_changed {
            self.label(cx, &[ids.time]).set_text(cx, &format!("{} / {}", clock(state.position), clock(state.duration)));
        }
        if prev.as_ref().map(|p| p.playing) != Some(state.playing) {
            if let Some(mut button) = self.button(cx, &[ids.play]).borrow_mut() {
                // Keep the donor SVG icon and the same Button action. Update
                // only when accepted play state changes, never on the draw path.
                button.draw_icon.svg = None;
                button.draw_icon.load_from_str(if state.playing {
                    include_str!("../resources/icons/pause.svg")
                } else {
                    include_str!("../resources/icons/play.svg")
                });
                button.redraw(cx);
            }
        }
        if prev.as_ref().map(|p| p.speed) != Some(state.speed) {
            self.label(cx, &[ids.pitch]).set_text(cx, &format!("{:+.1}%", (state.speed - 1.0) * 100.0));
        }
        for id in [ids.overview, ids.lane] {
            // Each renderer retains its own in-flight preview while accepted
            // engine snapshots continue to arrive through the shared host.
            self.widget(cx, &[id]).as_music_wave().set_state(cx, deck, state);
        }
        if !transport {
            self.dragging[deck] = false;
        }
        if prev.as_ref().map(|p| p.loop_enabled) != Some(state.loop_enabled) {
            let palette = makepad_look::palette::get(cx);
            if let Some(mut button) = self.button(cx, &[ids.loop_toggle]).borrow_mut() {
                button.draw_icon.color = Vec4f::from_u32(if state.loop_enabled {
                    palette.accent
                } else {
                    palette.label
                });
                button.redraw(cx);
            }
        }
        if prev_transport != Some(transport) {
            for id in [ids.overview, ids.lane, ids.play, ids.cue, ids.eject, ids.loop_toggle, ids.loop_in, ids.loop_out] {
                self.widget(cx, &[id]).set_disabled(cx, !transport);
            }
            for id in track_controls(deck) {
                self.widget(cx, &[id]).set_disabled(cx, !transport);
            }
            // The icon buttons draw their SVG in one colour, so a deck with
            // no track dims its play and cue icons itself.
            let palette = makepad_look::palette::get(cx);
            for id in [ids.play, ids.cue] {
                if let Some(mut button) = self.button(cx, &[id]).borrow_mut() {
                    button.draw_icon.color = Vec4f::from_u32(if transport { palette.label } else { palette.dim_lo });
                    button.redraw(cx);
                }
            }
        }
        if prev.as_ref().map(|p| p.ready) != Some(state.ready) {
            self.widget(cx, &[ids.tempo_reset]).set_disabled(cx, !state.ready);
        }
        if let Some(mut lyrics) = self.widget(cx, &[ids.karaoke]).borrow_mut::<MusicLyrics>() {
            lyrics.set_position(cx, state.position, state.has_track);
        }
        self.decks[deck] = Some(state.clone());
    }

    /// A partly measured source: its overview (the whole source, coarsened)
    /// and its detail window (full resolution), each placed on its own grid
    /// with every measured run. A picture already shown by either deck is
    /// reused, so only a new report revision uploads; `None` clears both.
    pub fn set_wave_view(&mut self, cx: &mut Cx, deck: usize, view: Option<WaveView>) {
        if deck >= self.decks.len() { return; }
        let ids = deck_ids(deck);
        let Some(view) = view else {
            if self.placed[deck].take().is_some() {
                for id in [ids.overview, ids.lane] {
                    self.widget(cx, &[id]).as_music_wave().set_placed(cx, None, None);
                }
            }
            return;
        };
        if self.placed[deck].as_ref().is_some_and(|(overview, detail)| {
            overview.shows(&view.overview, &view.overview_coverage) && detail.shows(&view.detail, &view.detail_coverage)
        }) {
            return;
        }
        let find = |placed: &[Option<(Rc<PlacedWave>, Rc<PlacedWave>)>; 2], data: &Arc<WaveData>, coverage: &WaveCoverage| {
            placed.iter().flatten().flat_map(|(overview, detail)| [overview, detail])
                .find(|shown| shown.shows(data, coverage)).cloned()
        };
        let overview = find(&self.placed, &view.overview, &view.overview_coverage)
            .or_else(|| PlacedWave::upload(cx, view.overview.clone(), view.overview_coverage.clone()));
        let detail = find(&self.placed, &view.detail, &view.detail_coverage)
            .or_else(|| PlacedWave::upload(cx, view.detail.clone(), view.detail_coverage.clone()));
        let (Some(overview), Some(detail)) = (overview, detail) else { return };
        // The whole-track picture of this deck, if any, gives way.
        self.wave_data[deck] = None;
        self.wave_textures[deck] = None;
        self.widget(cx, &[ids.overview]).as_music_wave().set_placed(cx, Some(overview.clone()), None);
        self.widget(cx, &[ids.lane]).as_music_wave().set_placed(cx, Some(detail.clone()), Some(overview.clone()));
        self.placed[deck] = Some((overview, detail));
        self.waves_applied[deck] = true;
    }

    pub fn set_waveform(&mut self, cx: &mut Cx, deck: usize, data: Option<Arc<WaveData>>) {
        let ids = deck_ids(deck);
        // A whole analysed track replaces any measured view.
        if self.placed[deck].take().is_some() {
            self.waves_applied[deck] = false;
            for id in [ids.overview, ids.lane] {
                self.widget(cx, &[id]).as_music_wave().set_placed(cx, None, None);
            }
        }
        let same = match (&self.wave_data[deck], &data) {
            (Some(previous), Some(next)) => Arc::ptr_eq(previous, next),
            (None, None) => true,
            _ => false,
        };
        if same && self.waves_applied[deck] {
            return;
        }
        if !same {
            self.wave_textures[deck] = data.as_ref().and_then(|data| MusicWaveTextures::upload(cx, data.clone()));
            self.wave_data[deck] = data;
        }
        // One upload per source, two views. A theme reapply only restores
        // these same handles to the reapplied widgets.
        for id in [ids.overview, ids.lane] {
            self.widget(cx, &[id]).as_music_wave().set_textures(cx, self.wave_textures[deck].clone());
        }
        self.waves_applied[deck] = true;
    }

    /// The tabs row and which deck panel shows, from the mode and the tab.
    fn sync_tabs(&mut self, cx: &mut Cx) {
        let narrow = self.narrow == Some(true);
        let shown = self.tabs.shown();
        self.view(cx, ids!(deck_tabs_row)).set_visible(cx, narrow);
        self.view(cx, ids!(deck_a_panel)).set_visible(cx, !narrow || shown == 0);
        self.view(cx, ids!(deck_b_panel)).set_visible(cx, !narrow || shown == 1);
        self.check_box(cx, ids!(tab_deck_a)).set_active(cx, shown == 0, Animate::No);
        self.check_box(cx, ids!(tab_deck_b)).set_active(cx, shown == 1, Animate::No);
        self.drop_down(cx, ids!(tab_follow)).set_selected_item(cx, usize::from(self.tabs.follow() == TabFollow::Audible));
        self.view.redraw(cx);
    }

    /// The crossfader as the graph has it (0 = A, 1 = B); `enabled` is
    /// whether the graph has the fade gains at all.
    pub fn set_crossfade(&mut self, cx: &mut Cx, value: f64, enabled: bool) {
        if self.crossfade_enabled != Some(enabled) {
            for id in [live_id!(crossfader), live_id!(xfade_to_a), live_id!(xfade_to_b)] {
                self.widget(cx, &[id]).set_disabled(cx, !enabled);
            }
            self.crossfade_enabled = Some(enabled);
        }
        if self.crossfade != Some(value) && self.tabs.audible_moved(audible_deck(value)) {
            self.sync_tabs(cx);
        }
        if self.crossfade != Some(value) {
            let shown = if self.reversed() { 1.0 - value } else { value };
            self.slider(cx, ids!(crossfader)).set_value(cx, shown.clamp(0.0, 1.0));
            self.crossfade = Some(value);
        }
    }

    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        let uid = self.widget_uid();
        for deck in 0..2 {
            let range_id = if deck == 0 { live_id!(deck_a_range) } else { live_id!(deck_b_range) };
            if self.button(cx, &[range_id]).clicked(actions) {
                self.pitch_ranges[deck] = (self.pitch_ranges[deck] + 1) % 4;
                let speed = self.decks[deck].as_ref().map_or(1.0, |state| state.speed);
                self.sync_pitch_range(cx, deck, speed);
            }
            let ids = deck_ids(deck);
            let (ready, transport, position, duration) = match self.decks[deck].as_ref() {
                Some(state) => (state.ready, state.ready && state.has_track, state.position, state.duration),
                None => (false, false, 0.0, 0.0),
            };
            let playing = self.decks[deck].as_ref().is_some_and(|state| state.playing);
            let looping = self.decks[deck].as_ref().is_some_and(|state| state.loop_enabled);
            let emit = |cx: &mut Cx, action: DeckStripAction| cx.widget_action(uid, MusicSurfaceAction::Deck(deck, action));
            if self.button(cx, &[ids.play]).clicked(actions) && transport {
                emit(cx, DeckStripAction::Play(!playing));
            }
            if self.button(cx, &[ids.cue]).clicked(actions) && transport {
                emit(cx, DeckStripAction::Cue);
            }
            if self.button(cx, &[ids.eject]).clicked(actions) && transport {
                emit(cx, DeckStripAction::Eject);
            }
            if self.button(cx, &[ids.loop_toggle]).clicked(actions) && transport {
                emit(cx, DeckStripAction::LoopEnabled(!looping));
            }
            // `[` and `]` set the loop ends at the playhead, as the console's
            // bracket keys did.
            let max = if duration.is_finite() && duration > 0.0 { duration.min(36000.0) } else { 36000.0 };
            if self.button(cx, &[ids.loop_in]).clicked(actions) && transport {
                emit(cx, DeckStripAction::LoopIn(position.clamp(0.0, max)));
            }
            if self.button(cx, &[ids.loop_out]).clicked(actions) && transport {
                emit(cx, DeckStripAction::LoopOut(position.clamp(0.0, max)));
            }
            if self.button(cx, &[ids.tempo_reset]).clicked(actions) && ready {
                emit(cx, DeckStripAction::Speed(1.0));
            }
            self.handle_mark_actions(cx, actions, deck, transport);
            let hp = if deck == 0 { live_id!(deck_a_hp) } else { live_id!(deck_b_hp) };
            if self.button(cx, &[hp]).clicked(actions) {
                cx.widget_action(uid, MusicSurfaceAction::Headphones(deck));
            }
            let overview = self.widget(cx, &[ids.overview]).as_music_wave();
            let lane = self.widget(cx, &[ids.lane]).as_music_wave();
            if overview.grabbed(actions) || lane.grabbed(actions) {
                self.dragging[deck] = true;
            }
            if overview.cancelled(actions) || lane.cancelled(actions) {
                self.dragging[deck] = false;
            }
            // The scrolling lane is the record under the hand.
            if let Some(at) = lane.scratched(actions) {
                if transport {
                    let at = if duration.is_finite() { at.clamp(0.0, duration.max(0.0)) } else { 0.0 };
                    emit(cx, DeckStripAction::Scratch(at));
                }
            }
            // The lane reports every end of a scratch (release, focus loss,
            // source replacement) as its own action.
            if lane.scratch_ended(actions) {
                self.dragging[deck] = false;
                emit(cx, DeckStripAction::ScratchEnd);
            }
            for at in [overview.seeked(actions), lane.seeked(actions)].into_iter().flatten() {
                self.dragging[deck] = false;
                if transport {
                    let at = if duration.is_finite() { at.clamp(0.0, duration.max(0.0)) } else { 0.0 };
                    emit(cx, DeckStripAction::Seek(at));
                }
            }
        }
        for (deck, id) in [(0, ids!(tab_deck_a)), (1, ids!(tab_deck_b))] {
            if self.check_box(cx, id).changed(actions).is_some() {
                self.tabs.press(deck);
                self.sync_tabs(cx);
            }
        }
        if let Some(index) = self.drop_down(cx, ids!(tab_follow)).selected(actions) {
            let follow = if index == 1 { TabFollow::Audible } else { TabFollow::Manual };
            self.tabs.set_follow(follow, None, self.crossfade.and_then(audible_deck));
            self.sync_tabs(cx);
        }
        if let Some(value) = self.slider(cx, ids!(cue_blend)).slided(actions) {
            cx.widget_action(uid, MusicSurfaceAction::CueBlend(value.clamp(0.0, 1.0)));
        }
        if let Some(on) = self.check_box(cx, ids!(cue_split)).changed(actions) {
            cx.widget_action(uid, MusicSurfaceAction::CueSplit(on));
        }
        if let Some(value) = self.slider(cx, ids!(crossfader)).slided(actions) {
            let value = if self.reversed() { 1.0 - value } else { value };
            cx.widget_action(uid, MusicSurfaceAction::Crossfade(value.clamp(0.0, 1.0)));
        }
        if let Some(value) = self.slider(cx, ids!(xfade_secs)).slided(actions) {
            cx.widget_action(uid, MusicSurfaceAction::FadeSecs(value.max(0.0)));
        }
        if self.button(cx, ids!(xfade_now)).clicked(actions) {
            cx.widget_action(uid, MusicSurfaceAction::FadeNow);
        }
        if self.button(cx, ids!(xfade_switch)).clicked(actions) {
            cx.widget_action(uid, MusicSurfaceAction::Cut);
        }
        let toggles: [(_, fn(bool) -> MusicSurfaceAction); 4] = [
            (ids!(xfade_auto_sync), MusicSurfaceAction::AutoSync),
            (ids!(xfade_eq), MusicSurfaceAction::EqFade),
            (ids!(xfade_rev), MusicSurfaceAction::Reverse),
            (ids!(xfade_auto_dj), MusicSurfaceAction::AutoDj),
        ];
        for (id, action) in toggles {
            if let Some(on) = self.check_box(cx, id).changed(actions) {
                cx.widget_action(uid, action(on));
            }
        }
        if self.button(cx, ids!(xfade_swap)).clicked(actions) {
            cx.widget_action(uid, MusicSurfaceAction::Deck(0, DeckStripAction::SwapDecks));
        }
        for (deck, lock, down, up, slip, quant, phase) in [
            (0, ids!(deck_a_keylock), ids!(deck_a_key_down), ids!(deck_a_key_up), ids!(deck_a_slip), ids!(quant_a), ids!(deck_a_phase)),
            (1, ids!(deck_b_keylock), ids!(deck_b_key_down), ids!(deck_b_key_up), ids!(deck_b_slip), ids!(quant_b), ids!(deck_b_phase)),
        ] {
            if self.button(cx, quant).clicked(actions) {
                cx.widget_action(uid, MusicSurfaceAction::Quant(deck));
            }
            if self.button(cx, phase).clicked(actions) {
                cx.widget_action(uid, MusicSurfaceAction::PhaseFlip(deck));
            }
            if self.button(cx, lock).clicked(actions) {
                cx.widget_action(uid, MusicSurfaceAction::KeyLock(deck));
            }
            if self.button(cx, down).clicked(actions) {
                cx.widget_action(uid, MusicSurfaceAction::Key(deck, -1));
            }
            if self.button(cx, up).clicked(actions) {
                cx.widget_action(uid, MusicSurfaceAction::Key(deck, 1));
            }
            if self.button(cx, slip).clicked(actions) {
                cx.widget_action(uid, MusicSurfaceAction::Slip(deck));
            }
        }
        for (deck, sync, back, fwd) in [
            (0, ids!(deck_a_sync), ids!(deck_a_nudge_back), ids!(deck_a_nudge_fwd)),
            (1, ids!(deck_b_sync), ids!(deck_b_nudge_back), ids!(deck_b_nudge_fwd)),
        ] {
            if self.button(cx, sync).clicked(actions) {
                cx.widget_action(uid, MusicSurfaceAction::Sync(deck));
            }
            for (id, dir) in [(back, -1i8), (fwd, 1i8)] {
                let button = self.button(cx, id);
                if button.pressed(actions) {
                    cx.widget_action(uid, MusicSurfaceAction::Nudge(deck, dir, true));
                }
                // A press let go over the button is a click, elsewhere a release.
                if button.released(actions) || button.clicked(actions) {
                    cx.widget_action(uid, MusicSurfaceAction::Nudge(deck, dir, false));
                }
            }
        }
        for (deck, bake) in [(0, ids!(deck_a_bake)), (1, ids!(deck_b_bake))] {
            if self.button(cx, bake).clicked(actions) {
                cx.widget_action(uid, MusicSurfaceAction::Bake(deck));
            }
        }
        // The fader's left end is A, or B turned round.
        let (left, right) = if self.reversed() { (1, 0) } else { (0, 1) };
        if self.button(cx, ids!(xfade_to_a)).clicked(actions) {
            cx.widget_action(uid, MusicSurfaceAction::FadeTo(left));
        }
        if self.button(cx, ids!(xfade_to_b)).clicked(actions) {
            cx.widget_action(uid, MusicSurfaceAction::FadeTo(right));
        }
    }
}

impl MusicSurfaceRef {
    pub fn set_xfade(&self, cx: &mut Cx, view: XfadeView) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.set_xfade(cx, view);
        }
    }

    pub fn set_deck_key(&self, cx: &mut Cx, deck: usize, view: DeckKeyView) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.set_deck_key(cx, deck, view);
        }
    }

    pub fn set_headphones(&self, cx: &mut Cx, cued: [bool; 2], split: bool, level: Option<f64>) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.set_headphones(cx, cued, split, level);
        }
    }

    pub fn set_marks(&self, cx: &mut Cx, deck: usize, marks: DeckMarksView) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.set_marks(cx, deck, marks);
        }
    }

    /// Let go of any gesture on either deck's pictures (a scratch releases):
    /// the decks are about to trade what they play.
    pub fn end_gestures(&self, cx: &mut Cx) {
        if let Some(inner) = self.borrow() {
            for deck in 0..2 {
                let ids = deck_ids(deck);
                inner.widget(cx, &[ids.overview]).as_music_wave().end_gesture(cx);
                inner.widget(cx, &[ids.lane]).as_music_wave().end_gesture(cx);
            }
        }
    }

    pub fn reject_seek(&self, cx: &mut Cx, deck: usize) {
        if let Some(inner) = self.borrow() {
            let ids = deck_ids(deck.min(1));
            inner.widget(cx, &[ids.overview]).as_music_wave().reject_seek(cx);
            inner.widget(cx, &[ids.lane]).as_music_wave().reject_seek(cx);
        }
    }

    /// Bind the deck's paged seek preview to the application's request.
    pub fn assign_seek(&self, cx: &mut Cx, deck: usize, source: u64, request: u64) {
        if let Some(inner) = self.borrow() {
            let ids = deck_ids(deck.min(1));
            inner.widget(cx, &[ids.overview]).as_music_wave().assign_seek(source, request);
            inner.widget(cx, &[ids.lane]).as_music_wave().assign_seek(source, request);
        }
    }

    /// Settle exactly that paged seek request's preview on the deck.
    pub fn settle_seek(&self, cx: &mut Cx, deck: usize, source: u64, request: u64) {
        if let Some(inner) = self.borrow() {
            let ids = deck_ids(deck.min(1));
            inner.widget(cx, &[ids.overview]).as_music_wave().settle_seek(cx, source, request);
            inner.widget(cx, &[ids.lane]).as_music_wave().settle_seek(cx, source, request);
        }
    }

    pub fn prepared_wave(&self, deck: usize) -> Option<PreparedWaveHandle> {
        self.borrow().and_then(|surface| surface.prepared_wave(deck))
    }
    pub fn set_stem_levels(&self, cx: &mut Cx, deck: usize, levels: [f32; 4]) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.set_stem_levels(cx, deck, levels);
        }
    }

    pub fn set_stems_available(&self, cx: &mut Cx, deck: usize, available: bool) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.set_stems_available(cx, deck, available);
        }
    }

    pub fn stem_controls(&self, cx: &Cx, deck: usize) -> Vec<WidgetUid> {
        self.borrow().map_or_else(Vec::new, |inner| inner.stem_controls(cx, deck))
    }

    pub fn set_lyrics(&self, cx: &mut Cx, deck: usize, data: Option<Arc<LyricsData>>) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.set_lyrics(cx, deck, data);
        }
    }

    pub fn deck_video(&self, cx: &Cx, deck: usize) -> crate::music_video::StageMusicVideoRef {
        match self.borrow() {
            Some(inner) => inner.deck_video(cx, deck),
            None => crate::music_video::StageMusicVideoRef::default(),
        }
    }

    pub fn set_deck_picture(&self, cx: &mut Cx, deck: usize, shown: bool) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.set_deck_picture(cx, deck, shown);
        }
    }

    pub fn sync_waveform_preview(&self, cx: &mut Cx, deck: usize, preview: &MusicWaveRef) {
        if let Some(inner) = self.borrow() {
            inner.sync_waveform_preview(cx, deck, preview);
        }
    }

    /// Restore authoritative values on the next sync after a refused edit.
    pub fn invalidate_state(&self, deck: usize) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.decks[deck] = None;
            inner.dragging[deck] = false;
            inner.crossfade = None;
        }
    }

    pub fn set_deck_peak(&self, cx: &mut Cx, deck: usize, peak: Option<f64>) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.set_deck_peak(cx, deck, peak);
        }
    }

    pub fn set_deck_state(&self, cx: &mut Cx, deck: usize, state: &DeckStripState) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.set_deck_state(cx, deck, state);
        }
    }

    pub fn set_waveform(&self, cx: &mut Cx, deck: usize, data: Option<Arc<WaveData>>) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.set_waveform(cx, deck, data);
        }
    }

    pub fn set_wave_view(&self, cx: &mut Cx, deck: usize, view: Option<WaveView>) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.set_wave_view(cx, deck, view);
        }
    }

    pub fn set_crossfade(&self, cx: &mut Cx, value: f64, enabled: bool) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.set_crossfade(cx, value, enabled);
        }
    }
}

impl Widget for MusicSurface {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if matches!(event, Event::ScriptReapply) {
            self.decks = [None, None];
            self.dragging = [false, false];
            self.waves_applied = [false, false];
            self.pitch_applied = [None, None];
            self.deck_peaks = [None, None];
            self.crossfade = None;
            self.crossfade_enabled = None;
            self.stems_available = [None, None];
            self.marks = [None, None];
            self.mark_del = [false, false];
            self.loop_beats = [5, 5];
            self.headphones = [None, None];
            self.keys = [None, None];
            self.narrow = None;
            self.xfade = None;
        }
        self.view.handle_event(cx, event, scope);
        if let Event::Actions(actions) = event {
            self.handle_actions(cx, actions);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        // The room the parent gives the surface decides its layout; a
        // change of mode redraws once more.
        let width = cx.turtle().inner_width();
        if width > 0.0 {
            let narrow = is_narrow(width);
            if self.narrow != Some(narrow) {
                log!("DJ surface: {width:.0} points wide, {}", if narrow { "one deck panel behind tabs" } else { "both deck panels" });
                self.narrow = Some(narrow);
                self.sync_tabs(cx);
                self.view.redraw(cx);
            }
        }
        self.view.draw_walk(cx, scope, walk)
    }
}

/// Both deck panels (316 points each), the gaps and room for the lanes.
const NARROW_BELOW: f64 = 316.0 * 2.0 + 8.0 * 2.0 + 300.0;

/// The surface is too narrow to stand both deck panels beside the lanes.
pub fn is_narrow(width: f64) -> bool {
    width < NARROW_BELOW
}

/// The deck the room hears from a fader (0 = A): one clearly, else none.
pub fn audible_deck(fader: f64) -> Option<usize> {
    if fader < 0.4 {
        Some(0)
    } else if fader > 0.6 {
        Some(1)
    } else {
        None
    }
}

fn clock_secs(seconds: f64) -> u64 {
    if seconds.is_finite() { seconds.max(0.0).floor() as u64 } else { 0 }
}

fn clock(seconds: f64) -> String {
    let whole = clock_secs(seconds);
    format!("{}:{:02}", whole / 60, whole % 60)
}

fn stem_columns(deck: usize) -> [LiveId; 4] {
    if deck == 0 {
        [live_id!(deck_a_stem_vocals), live_id!(deck_a_stem_drums), live_id!(deck_a_stem_bass), live_id!(deck_a_stem_other)]
    } else {
        [live_id!(deck_b_stem_vocals), live_id!(deck_b_stem_drums), live_id!(deck_b_stem_bass), live_id!(deck_b_stem_other)]
    }
}

/// A compact reading copy of the saved transcript. It follows accepted source
/// time, and only word timings explicitly marked confident light words.
#[derive(Script, ScriptHook, Widget)]
struct MusicLyrics {
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    #[redraw]
    #[area]
    area: Area,
    #[live]
    draw_text: DrawText,
    #[live]
    draw_progress: DrawColor,
    #[rust]
    data: Option<Arc<LyricsData>>,
    #[rust]
    position: f64,
    #[rust]
    loaded: bool,
    /// The deck's own picture shows under these lyrics.
    #[rust]
    over_video: bool,
}

impl MusicLyrics {
    fn set_data(&mut self, cx: &mut Cx, data: Option<Arc<LyricsData>>) {
        let same = match (&self.data, &data) {
            (Some(previous), Some(next)) => Arc::ptr_eq(previous, next),
            (None, None) => true,
            _ => false,
        };
        if !same {
            self.data = data;
            self.area.redraw(cx);
        }
    }

    fn set_position(&mut self, cx: &mut Cx, seconds: f64, loaded: bool) {
        let seconds = if seconds.is_finite() { seconds.max(0.0) } else { 0.0 };
        if self.position != seconds || self.loaded != loaded {
            self.position = seconds;
            self.loaded = loaded;
            self.area.redraw(cx);
        }
    }

    fn draw_line(&mut self, cx: &mut Cx2d, rect: Rect, line: &LyricsLine, active: bool) -> f64 {
        let palette = makepad_look::palette::get(cx);
        let normal = Vec4f::from_u32(if active { palette.glass_label } else { palette.glass_ink_dim });
        let sung = Vec4f::from_u32(palette.glass_live);
        let confident = active && line.confident
            && line.words.len() == line.text.split_whitespace().count();
        let spoken = if confident { line.words.partition_point(|time| *time <= self.position) } else { 0 };
        let left = rect.pos.x;
        let right = rect.pos.x + rect.size.x;
        let bottom = rect.pos.y + rect.size.y;
        let line_height = self.draw_text.text_style.font_size as f64 * 1.7;
        let space = self.draw_text.prepare_single_line_run(cx, " ")
            .map_or(3.0, |run| run.width_in_lpxs as f64);
        let mut at = rect.pos;
        for (index, word) in line.text.split_whitespace().enumerate() {
            let width = self.draw_text.prepare_single_line_run(cx, word)
                .map_or(0.0, |run| run.width_in_lpxs as f64);
            if at.x > left && at.x + width > right {
                at.x = left;
                at.y += line_height;
            }
            if at.y + line_height > bottom { break; }
            self.draw_text.color = if index < spoken { sung } else { normal };
            self.draw_text.draw_abs(cx, at, word);
            at.x += width + space;
        }
        let mut end = at.y + line_height;
        if active && !confident && line.end_secs > line.start_secs && end + 2.0 < bottom {
            // Only a line interval was measured: a line progress rule says
            // that honestly, without assigning invented starts to its words.
            let progress = ((self.position - line.start_secs) / (line.end_secs - line.start_secs)).clamp(0.0, 1.0);
            self.draw_progress.color = Vec4f { w: 0.6, ..sung };
            self.draw_progress.draw_abs(cx, Rect {
                pos: dvec2(left, end), size: dvec2(rect.size.x * progress, 1.0),
            });
            end += 3.0;
        }
        end
    }
}

impl Widget for MusicLyrics {
    fn handle_event(&mut self, _cx: &mut Cx, _event: &Event, _scope: &mut Scope) {}

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        let area = cx.walk_turtle_with_area(&mut self.area, walk);
        let rect = Rect {
            pos: area.pos + dvec2(7.0, 5.0),
            size: dvec2((area.size.x - 14.0).max(0.0), (area.size.y - 10.0).max(0.0)),
        };
        if rect.size.x <= 0.0 || rect.size.y <= 0.0 { return DrawStep::done(); }
        cx.push_clip_rect(rect);
        let data = self.data.clone().filter(|data| self.loaded && !data.lines.is_empty());
        if let Some(data) = data {
            let next = data.lines.partition_point(|line| line.start_secs <= self.position);
            let active = next.checked_sub(1).filter(|index| self.position < data.lines[*index].end_secs);
            let mut y = rect.pos.y;
            if let Some(index) = active {
                y = self.draw_line(cx, rect, &data.lines[index], true) + 5.0;
            }
            if let Some(line) = data.lines.get(next) {
                let remaining = Rect {
                    pos: dvec2(rect.pos.x, y),
                    size: dvec2(rect.size.x, (rect.pos.y + rect.size.y - y).max(0.0)),
                };
                self.draw_line(cx, remaining, line, false);
            }
        } else if !self.over_video {
            self.draw_text.color = Vec4f::from_u32(makepad_look::palette::get(cx).glass_ink_dim);
            self.draw_text.draw_abs(cx, rect.pos, "No timed lyrics");
        }
        cx.pop_clip_rect();
        DrawStep::done()
    }
}

// Donor music_view.rs::VjWrapStrip. The center sweep gets the first row
// when shaping and automation cannot flank it; only free group widths are
// remembered, so a constrained frame cannot oscillate its own fit decision.
const STRIP_SWEEP_MAX: f64 = 384.0;

const STRIP_SWEEP_MIN: f64 = 150.0;

const STRIP_ROW_SLACK: f64 = 8.0;

const STRIP_FADER_LABELS_MIN: f64 = 320.0;


#[derive(Clone, Copy, Debug, PartialEq)]
struct MusicStripPlan {
    row: f64,
    flanked: bool,
    cues_inline: bool,
    sweep_w: f64,
}

fn music_strip_sweep_plan(
    available: f64,
    shaping_w: f64,
    automation_w: f64,
    cue_w: f64,
    spacing: f64,
) -> MusicStripPlan {
    let row = if available.is_finite() { available } else { STRIP_SWEEP_MAX };
    let cues = 2.0 * cue_w + 2.0 * spacing;
    let flanked = available.is_finite()
        && available >= shaping_w + automation_w + cues + STRIP_SWEEP_MIN + 4.0 * spacing;
    let cues_inline = flanked || row - cues - STRIP_ROW_SLACK >= STRIP_SWEEP_MIN;
    let sweep_w = if flanked {
        available - shaping_w - automation_w - cues - 4.0 * spacing
    } else if cues_inline {
        row - cues - STRIP_ROW_SLACK
    } else {
        row
    };
    MusicStripPlan {
        row,
        flanked,
        cues_inline,
        sweep_w: sweep_w.clamp(0.0, STRIP_SWEEP_MAX),
    }
}

fn remembered_width(last: f64, measured: f64, drawn_free: bool) -> f64 {
    if drawn_free && measured > 1.0 {
        measured
    } else {
        last
    }
}

fn strip_walk(width: f64) -> Walk {
    Walk {
        abs_pos: None,
        margin: Inset::default(),
        width: Size::Fixed(width.max(0.0)),
        height: Size::fit(),
        ..Default::default()
    }
}
#[derive(Script, ScriptHook, Widget)]
pub struct MusicWrapStrip {
    #[deref]
    view: View,
    #[rust]
    area: Area,
    #[rust]
    group_w: Option<(f64, f64, f64)>,
}

impl Widget for MusicWrapStrip {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if matches!(event, Event::LiveEdit | Event::ScriptReapply) {
            self.group_w = None;
        }
        self.view.handle_event(cx, event, scope);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let shaping = self.view.widget(cx, ids!(strip_shaping));
        let cue_a = self.view.widget(cx, ids!(fader_cue_a));
        let sweep = self.view.widget(cx, ids!(fader_sweep));
        let cue_b = self.view.widget(cx, ids!(fader_cue_b));
        let automation = self.view.widget(cx, ids!(strip_automation));
        let layout = self.view.layout;
        let spacing = layout.spacing;
        let wrap_spacing = layout.wrap_spacing;

        let (shaping_w, automation_w, cue_w) =
            self.group_w.unwrap_or((302.0, 200.0, 58.0));
        let measured = self.group_w.is_some();

        cx.begin_turtle(walk, layout);
        let available = cx.turtle().inner_width();
        let plan = music_strip_sweep_plan(available, shaping_w, automation_w, cue_w, spacing);
        let row = plan.row;

        let labels = row >= STRIP_FADER_LABELS_MIN;
        for (group, id) in [(&cue_a, ids!(xfade_label_a)), (&cue_b, ids!(xfade_label_b))] {
            let label = group.widget(cx, id);
            if label.visible() != labels {
                label.set_visible(cx, labels);
            }
        }
        for group in [&cue_a, &cue_b] {
            if group.visible() != plan.cues_inline {
                group.set_visible(cx, plan.cues_inline);
            }
        }

        let shaping_free = plan.flanked || !measured || shaping_w <= row;
        let automation_free = plan.flanked || !measured || automation_w <= row;
        if plan.flanked {
            shaping.draw_all(cx, scope);
            cue_a.draw_all(cx, scope);
            sweep.draw_walk_all(cx, scope, strip_walk(plan.sweep_w));
            cue_b.draw_all(cx, scope);
            automation.draw_all(cx, scope);
        } else if plan.cues_inline {
            cue_a.draw_all(cx, scope);
            sweep.draw_walk_all(cx, scope, strip_walk(plan.sweep_w));
            cue_b.draw_all(cx, scope);
            cx.turtle_new_line_with_spacing(wrap_spacing);
            if shaping_free {
                shaping.draw_all(cx, scope);
            } else {
                shaping.draw_walk_all(cx, scope, strip_walk(row));
            }
            if automation_free {
                automation.draw_all(cx, scope);
            } else {
                automation.draw_walk_all(cx, scope, strip_walk(row));
            }
        } else {
            sweep.draw_walk_all(cx, scope, strip_walk(plan.sweep_w));
            cx.turtle_new_line_with_spacing(wrap_spacing);
            if shaping_free {
                shaping.draw_all(cx, scope);
            } else {
                shaping.draw_walk_all(cx, scope, strip_walk(row));
            }
            if automation_free {
                automation.draw_all(cx, scope);
            } else {
                automation.draw_walk_all(cx, scope, strip_walk(row));
            }
        }

        cx.end_turtle_with_area(&mut self.area);

        let fresh = (
            remembered_width(shaping_w, shaping.area().rect(cx).size.x, shaping_free),
            remembered_width(
                automation_w,
                automation.area().rect(cx).size.x,
                automation_free,
            ),
            remembered_width(
                cue_w,
                cue_a.area().rect(cx).size.x.max(cue_b.area().rect(cx).size.x),
                plan.cues_inline,
            ),
        );
        let settled = self.group_w.is_some_and(|was| {
            (was.0 - fresh.0).abs() < 0.5
                && (was.1 - fresh.1).abs() < 0.5
                && (was.2 - fresh.2).abs() < 0.5
        });
        self.group_w = Some(fresh);
        if !settled {
            self.area.redraw(cx);
        }
        DrawStep::done()
    }
}

#[cfg(test)]
mod narrow_tests {
    use super::*;

    #[test]
    fn a_narrow_window_tabs_the_decks_and_audible_names_one_deck_or_none() {
        assert!(is_narrow(900.0));
        assert!(!is_narrow(1000.0));
        assert!(!is_narrow(1680.0));
        assert_eq!(audible_deck(0.1), Some(0));
        assert_eq!(audible_deck(0.5), None, "both heard: the tab holds");
        assert_eq!(audible_deck(0.9), Some(1));
    }
}

#[cfg(test)]
mod track_control_tests {
    use super::*;

    #[test]
    fn each_deck_gates_its_own_twenty_track_controls() {
        let (a, b) = (track_controls(0), track_controls(1));
        assert_eq!((a.len(), b.len()), (20, 20));
        assert!(a.iter().all(|id| !b.contains(id)), "a control is shared by both decks");
        assert!(a.contains(&live_id!(deck_a_hot_7)) && a.contains(&live_id!(deck_a_nudge_fwd)));
        assert!(b.contains(&live_id!(deck_b_lslot_3)) && b.contains(&live_id!(deck_b_sync)));
    }
}
