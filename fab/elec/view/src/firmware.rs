//! The Electronics workspace's Firmware pane: the bench's firmware document in the code editor,
//! under the sheet, with how its build stands. Lower than [`FOLD_UNDER`] it shows only its head
//! (the document, its build and an open button); its fold and open buttons ask the host to move
//! its split ([`FirmwareFold`]). It follows the electronics host (`ElecLive`): the
//! document the run compiles (`SimLive::firmware`'s `doc`), or before the run is made the one the
//! run's bench binds (`snapshot::run_bench`), and its text from the snapshot's documents. Beside
//! the source it shows the device's state at the playhead (`crate::fw_state`). The head holds
//! the firmware's steps (`crate::live::fw_steps`, the transport's own).
//!
//! The person types in it: each change goes to the engine as one replacement
//! ([`ViewAction::Text`]) addressed to the revision the pane holds, one at a time. What is typed
//! while one is on the way goes as the next, once the engine's text shows the first applied, so an
//! edit always addresses a text the pane has seen. Changes closer than [`TYPING_PAUSE`] are one
//! undo step. Typing is never lost: when the engine refuses the pane's edit (its refusal names the
//! edit's tag) or the document moves another way (another writer, an undo, a new load), the
//! person's typing moves onto the engine's text and goes again; where the two changes meet, the
//! editor keeps the typing, the pane says it is not in the document, and the person keeps it
//! (sent over the document's text) or takes the document's.
//!
//! Cost: per signal or frame four pointer compares; when the firmware status, the benches, the
//! documents or the refusal changed, one lookup of the document by name (O(benches + documents))
//! and a text compare or two when the document's text is a new Arc; when the document moved
//! another way, two trimmed replacements and one merged text (O(text)) and one load into the
//! editor. Per typed change, one read of the editor's text and one copy (O(text)); per edit sent,
//! one trimmed replacement (O(text)). Nothing per item. The source's heat: per publication while
//! playing O(statements that have run: the device's counts against those the pane read before,
//! so a publication replaced unread still pulses its statements); per frame while a line is hot O(hot lines) and one redraw of the
//! editor; paused, one marker per publication; nothing while unchanged or folded. A pointer over
//! the state panel's pins costs one division per move and one action per change of pin.
//! Drawing the state rows lays out each name and displayed value line, including wrapped words.
//! The monitor's list: per signal or frame one pointer compare and one uncontended lock; per line
//! that arrived, one push into the list (it virtualises: rows on screen only, at most
//! `MONITOR_HELD` lines kept, the rest in the monitor's log file).

use crate::fw_rows::FwRows;
use crate::fw_state::{cond_marks, line_at, marker, ran_lines, state_columns, state_text, unrun_lines, value_inlays, CondMark, Heat, Mark, SourceLines, StateText, COND_WASH, MARK_WASH, SOURCE_MIN, STATE_COLUMN, STATE_PANEL, STATE_PANEL_TWO};
use crate::live::sim::{FlashDevice, FlashLive, FlashState, FwLive, MicLive, MonitorFeed, MonitorState, NetServerLive, NetworkLive, RadioLive, RadioState, SimAction, MIC_LEVEL_DB, MONITOR_HELD};
use crate::snapshot::{next_tag, run_bench, DocText, DocsView, Refusal, ViewAction};
use crate::ElecLive;
use fab_elec_core::DocId;
use fab_elec_design::TextEdit;
use fab_elec_lang::BenchView;
use fab_elec_mcu_cosim::FwState;
use fab_elec_sim::cosim::CoState;
use makepad_code_editor::code_view::{CodeView, CodeViewAction};
use makepad_code_editor::inlays::InlineInlay;
use makepad_code_editor::text::Position;
use makepad_widgets_core::makepad_platform::log::LogLevel;
use makepad_widgets_core::*;
use makepad_widgets_rich_text::log_list::LogListWidgetExt;
use std::sync::Arc;

/// How long lines an outside edit changed stay washed in the AI colour, fading, seconds.
pub const FLASH_SECONDS: f64 = 3.0;
/// The AI wash's opacity at the flash's start.
const FLASH_WASH: f32 = 0.35;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*

    let step_face = ButtonFlatter.draw_bg{
        color: theme.prop_field color_focus: theme.prop_field
        color_hover: theme.prop_field_hover color_down: theme.prop_field_active
        border_size: 0.0 border_radius: theme.corner_radius
    }
    let step_text = ButtonFlatter.draw_text{
        color: theme.color_text color_focus: theme.color_text
        color_hover: theme.color_text color_down: theme.color_text
        text_style: theme.font_regular {font_size: theme.prop_font_size}
    }
    let step_pad = Inset{left: 9 right: 9}
    let StateHead = Label {
        padding: 0
        draw_text +: {color: theme.color_text_meta text_style: theme.font_bold {font_size: 7.5}}
    }
    let SectionHead = StateHead {
        visible: false margin: Inset{top: 9 right: 3 bottom: 3 left: 3}
    }

    mod.widgets.FirmwarePaneBase = #(FirmwarePane::register_widget(vm))
    mod.widgets.FirmwarePane = set_type_default() do mod.widgets.FirmwarePaneBase {
        width: Fill height: Fill
        flow: Down
        show_bg: true
        // A condition's newest value in the source.
        color_true: theme.color_success
        color_false: theme.color_error
        color_inlay: theme.color_text_meta
        // A Rust View's quad draws nothing by itself, so the pane fills its own pixel.
        draw_bg +: {
            color: instance(theme.color_surface_container)
            pixel: fn() {
                return Pal.premul(self.color)
            }
        }
        head := View {
            width: Fill height: Fit
            // One control-high row that wraps only when the pane runs out of width. Every child
            // is Fit: a Fill child in a wrapping row got a sliver of width and stood its text
            // one glyph a line, hundreds of points tall. (The radio row's Fill label sits inside
            // that row's own bounded width, never directly in this row.)
            flow: Right{wrap: true}
            align: Align{x: 0.0 y: 0.5}
            padding: Inset{left: 10 right: 10 top: 3 bottom: 3}
            spacing: 10
            steps := FwSteps{
                words +: {
                    statement +: {padding: step_pad draw_bg: step_face draw_text: step_text}
                    entry +: {padding: step_pad draw_bg: step_face draw_text: step_text}
                    out +: {padding: step_pad draw_bg: step_face draw_text: step_text}
                }
            }
            doc_title := Label {
                text: "Firmware"
                draw_text +: {color: theme.color_text text_style: theme.font_bold {font_size: 9}}
            }
            status := Label {
                width: Fit
                text: ""
                draw_text +: {color: theme.color_text_meta text_style: theme.font_regular {font_size: theme.prop_font_size}}
            }
            // Typing kept in the editor, not in the document: the person decides.
            keep := ButtonFlatter {
                visible: false
                width: Fit height: 22 padding: Inset{left: 8 right: 8} margin: 0 text: "Keep mine"
                a11y: {label: "Send your typing over the document's text"}
            }
            take := ButtonFlatter {
                visible: false
                width: Fit height: 22 padding: Inset{left: 8 right: 8} margin: 0 text: "Take the document's"
                a11y: {label: "Drop your typing for the document's text"}
            }
            // The device's EEPROM the project keeps between runs, back to a new chip's.
            erase := ButtonFlatter {
                visible: false
                width: Fit height: 22 padding: Inset{left: 8 right: 8} margin: 0 text: "Erase EEPROM"
                a11y: {label: "Erase the EEPROM kept for this chip: its run starts as a new chip"}
            }
            // Flash to: the running image onto the board chosen beside it (a board that may not
            // be flashed is greyed, with why).
            flash := ButtonFlatter {
                visible: false
                width: Fit height: 22 padding: Inset{left: 8 right: 8} margin: 0 text: "Flash to"
                a11y: {label: "Flash the running firmware to the chosen board"}
            }
            flash_to := DropDownFlat {
                visible: false
                width: Fit height: 22 margin: 0
                labels: []
                a11y: {label: "The board to flash"}
            }
            // Read: the chosen board's chip and its whole flash's MD5, nothing written.
            flash_read := ButtonFlatter {
                visible: false
                width: Fit height: 22 padding: Inset{left: 8 right: 8} margin: 0 text: "Read"
                a11y: {label: "Read the chosen board: its chip and its whole flash's MD5, nothing written"}
            }
            // Back up: the chosen board's whole flash into a new folder of Fab's data (Cancel ends it).
            backup := ButtonFlatter {
                visible: false
                width: Fit height: 22 padding: Inset{left: 8 right: 8} margin: 0 text: "Back up"
                a11y: {label: "Back up the chosen board's whole flash to a file, about 12 minutes for 16 MB"}
            }
            // Monitor: the chosen board's own watch stream, from a restart (Stop ends it).
            monitor := ButtonFlatter {
                visible: false
                width: Fit height: 22 padding: Inset{left: 8 right: 8} margin: 0 text: "Monitor"
                a11y: {label: "Monitor the chosen board: restart it and show what it reports"}
            }
            radio := Tip {
                visible: false width: 390 max_width: FitBound.Rel{base: Base.Parent factor: 1.0}
                height: 22 flow: Right align: Align{y: 0.5} spacing: 6
                text: #(crate::fw_state::radio_source()) wrap_width: 360
                a11y: {label: #(crate::fw_state::radio_source())}
                Label {
                    width: Fit text: "Radio"
                    draw_text +: {color: theme.color_text text_style: theme.font_bold {font_size: 9}}
                }
                face := Label {
                    width: Fill text: "" max_lines: 1 text_overflow: Ellipsis hover_actions_enabled: true
                    draw_text +: {color: theme.color_text_meta text_style: theme.font_regular {font_size: theme.prop_font_size}}
                }
                get := ButtonFlatter {
                    width: Fit height: 22 padding: Inset{left: 8 right: 8} margin: 0 text: "Get"
                    a11y: {label: "Get Wi-Fi libraries" description: #(crate::fw_state::radio_source())}
                }
                cancel := ButtonFlatter {
                    visible: false width: Fit height: 22 padding: Inset{left: 8 right: 8} margin: 0 text: "Cancel"
                    a11y: {label: "Cancel Wi-Fi library download" description: #(crate::fw_state::radio_source())}
                }
            }
            mic := View {
                visible: false width: 580 max_width: FitBound.Rel{base: Base.Parent factor: 1.0}
                height: Fit flow: Right{wrap: true} align: Align{y: 0.5} spacing: 6
                Label {
                    width: Fit text: "Mic"
                    draw_text +: {color: theme.color_text text_style: theme.font_bold {font_size: 9}}
                }
                pick := ButtonFlatter {
                    width: Fit height: 22 padding: Inset{left: 8 right: 8} margin: 0 text: "Choose WAV…"
                    a11y: {label: "Choose a WAV for the board microphone"}
                }
                file := Label {
                    width: 104 text: "No file selected" max_lines: 1 text_overflow: Ellipsis
                    draw_text +: {color: theme.color_text_meta text_style: theme.font_regular {font_size: theme.prop_font_size}}
                }
                level := Slider {
                    width: 100 height: 22 margin: 0 text: "" min: -60.0 max: 0.0
                    default: #(f64::from(MIC_LEVEL_DB)) step: 1.0 precision: 0 unit: "dB"
                    a11y: {label: "Microphone input level in dB"}
                }
                play := ButtonFlatter {
                    width: Fit height: 22 padding: Inset{left: 8 right: 8} margin: 0 text: "Play into mic"
                    a11y: {label: "Play the WAV into the board microphone"}
                }
                computer := Toggle {
                    width: Fit height: 22 margin: 0 text: "Computer mic"
                    a11y: {label: "Computer mic"}
                }
                capture_status := Label {
                    visible: false width: Fit text: ""
                    draw_text +: {color: theme.color_text_meta text_style: theme.font_regular {font_size: theme.prop_font_size}}
                }
            }
            // The real network: off, the twin plays the bench's fake servers; on, the hosts the
            // person allowed for this project, with the tokens this computer's settings hold.
            network := View {
                visible: false width: 580 max_width: FitBound.Rel{base: Base.Parent factor: 1.0}
                height: Fit flow: Down spacing: 4
                View {
                    width: Fill height: 22 flow: Right align: Align{y: 0.5} spacing: 6
                    Label {
                        width: Fit text: "Network"
                        draw_text +: {color: theme.color_text text_style: theme.font_bold {font_size: 9}}
                    }
                    face := Label {
                        width: Fill text: "" max_lines: 1 text_overflow: Ellipsis
                        draw_text +: {color: theme.color_text_meta text_style: theme.font_regular {font_size: theme.prop_font_size}}
                    }
                    allow := ButtonFlatter {
                        visible: false width: Fit height: 22 padding: Inset{left: 8 right: 8} margin: 0 text: "Allow new hosts"
                        a11y: {label: "Allow the twin to reach the bench's new hosts"}
                    }
                    // As wide as its longer label, so a toggle moves nothing beside it.
                    switch := ButtonFlatter {
                        width: 150 height: 22 padding: Inset{left: 8 right: 8} margin: 0 text: "Use the real network"
                        a11y: {label: "Let the twin reach the bench's real servers, or go back to its fakes"}
                    }
                }
                servers := Label {
                    width: Fill text: "" padding: 0
                    draw_text +: {color: theme.color_text_meta text_style: theme.font_code {font_size: 8.5}}
                }
                tokens := View {
                    visible: false width: Fill height: Fit flow: Right{wrap: true} align: Align{y: 0.5} spacing: 6
                    endpoint := DropDownFlat {
                        width: Fit height: 22 margin: 0
                        labels: []
                        a11y: {label: "The endpoint whose token to save"}
                    }
                    token := TextInput {
                        width: 180 height: 22 margin: 0 text: "" is_password: true empty_text: "token"
                        a11y: {label: "The endpoint's token, kept in this computer's Fab settings"}
                    }
                    save := ButtonFlatter {
                        width: Fit height: 22 padding: Inset{left: 8 right: 8} margin: 0 text: "Save token"
                        a11y: {label: "Save the token in this computer's Fab settings"}
                    }
                    forget := ButtonFlatter {
                        width: Fit height: 22 padding: Inset{left: 8 right: 8} margin: 0 text: "Forget"
                        a11y: {label: "Forget the endpoint's saved token"}
                    }
                }
            }
            // The host gives the two buttons their icons.
            fold := ButtonFlatterIcon {
                width: 22 height: 22 text: ""
                a11y: {label: "Fold the firmware to its name"}
                label_walk: Walk{width: 0 height: 0}
                icon_walk: Walk{width: 14 height: 14}
            }
            unfold := ButtonFlatterIcon {
                visible: false
                width: 22 height: 22 text: ""
                a11y: {label: "Open the firmware"}
                label_walk: Walk{width: 0 height: 0}
                icon_walk: Walk{width: 14 height: 14}
            }
        }
        empty := Label {
            visible: false
            width: Fill
            padding: Inset{left: 10 right: 10 top: 10 bottom: 10}
            text: ""
            draw_text +: {color: theme.color_text_meta text_style: theme.font_regular {font_size: 11}}
        }
        source := View {
            width: Fill height: Fill
            flow: Right
            // Its own list: the state beside it changes with each publication while the source
            // stays (or only its marks move), so the source records only when it changed.
            code_list := View {
                width: Fill height: Fill
                new_batch: true
                code := CodeView {
                    editor +: {
                        width: Fill height: Fill
                        read_only: false show_gutter: true word_wrap: false
                        draw_text +: {text_style: theme.font_code {font_size: 9}}
                    }
                }
            }
            // The same device state that `elec.fw.state` reads.
            panel := View {
                width: 280 height: Fill
                flow: Down
                show_bg: true
                draw_bg +: {
                    color: instance(theme.color_surface_container)
                    edge: uniform(theme.color_bevel_inset_2)
                    pixel: fn() {
                        if self.pos.x * self.rect_size.x < 1.0 {
                            return Pal.premul(self.edge)
                        }
                        return Pal.premul(self.color)
                    }
                }
                // The two groups sit side by side when one column is too tall.
                state := ScrollYView {
                    width: Fill height: Fill flow: Down
                    padding: Inset{left: 14 right: 14 top: 12 bottom: 12}
                    first := View {
                        width: Fill height: Fit flow: Down
                        head := StateHead{width: Fill text: "AT THE PLAYHEAD"}
                        rows := mod.widgets.FwRows{margin: Inset{bottom: 10}}
                        at := Label {
                            width: Fill text: "" padding: 0
                            draw_text +: {color: theme.color_text_meta text_style: theme.font_code {font_size: 8.5}}
                        }
                    }
                    second := View {
                        width: Fill height: Fit flow: Down
                        pins_head := SectionHead{text: "PINS"}
                        // A pin hovered here lights its net in every view.
                        pins_box := View {
                            width: Fill height: Fit
                            pins := Label {
                                visible: false width: Fill text: "" padding: 0
                                draw_text +: {color: theme.color_text text_style: theme.font_code {font_size: 8.5}}
                            }
                        }
                        pins_idle := Label {
                            visible: false width: Fill text: "" padding: 0 margin: theme.mspace_1
                            draw_text +: {color: theme.color_text_meta text_style: theme.font_code {font_size: 8.5}}
                        }
                        timers_head := SectionHead{text: "TIMERS"}
                        timers := Label {
                            visible: false width: Fill text: "" padding: 0 margin: theme.mspace_1
                            draw_text +: {color: theme.color_text text_style: theme.font_code {font_size: 8.5}}
                        }
                        pending_head := SectionHead{text: "PENDING"}
                        pending := Label {
                            visible: false width: Fill text: "" padding: 0 margin: theme.mspace_1
                            draw_text +: {color: theme.color_text text_style: theme.font_code {font_size: 8.5}}
                        }
                        faults_head := SectionHead{text: "FAULTS"}
                        faults := Label {
                            visible: false width: Fill text: "" padding: 0 margin: theme.mspace_1
                            draw_text +: {color: theme.color_text text_style: theme.font_code {font_size: 8.5}}
                        }
                        net_head := SectionHead{text: "NETWORK"}
                        net := Label {
                            visible: false width: Fill text: "" padding: 0 margin: theme.mspace_1
                            draw_text +: {color: theme.color_text text_style: theme.font_code {font_size: 8.5}}
                        }
                        regs_head := SectionHead{text: "REGISTERS"}
                        regs := Label {
                            visible: false width: Fill text: "" padding: 0 margin: theme.mspace_1
                            draw_text +: {color: theme.color_text text_style: theme.font_code {font_size: 8.5}}
                        }
                    }
                }
            }
        }
        // The board's watch stream while Monitor runs or after it ended by itself; every line is
        // also in the monitor's log file.
        monitor_lines := LogList {
            visible: false
            width: Fill height: 200
            cap: #(MONITOR_HELD)
        }
    }
}

/// Lower than this the pane shows only its head: folded.
pub const FOLD_UNDER: f64 = 100.0;

/// Whether a pane this high shows only its head.
pub fn shows_folded(height: f64) -> bool {
    height < FOLD_UNDER
}

/// The pane asks to be opened (true) or folded to its head (false): the host moves its split.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FirmwareFold(pub bool);

/// The person erases the EEPROM the project keeps for the device of this component path: the
/// host drops the kept bytes, and its run (started again at once, when one runs) starts as a new
/// chip.
#[derive(Clone, Debug, PartialEq)]
pub struct FirmwareErase(pub String);

/// The person chooses the board Flash to writes to, by its port's path.
#[derive(Clone, Debug, PartialEq)]
pub struct FirmwareFlashChoose(pub String);

/// The person flashes the running image to the chosen board.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FirmwareFlash;

/// The person reads the chosen board: its chip and its whole flash's MD5, nothing written.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FirmwareFlashRead;

/// The person backs up the chosen board's whole flash, or cancels the back-up under way.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FirmwareBackup;

/// The person starts the monitor on the chosen board, or stops it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FirmwareMonitor;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FirmwareRadioGet;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FirmwareRadioCancel;

/// The person lets the twin reach these hosts of the bench's servers for the project (Some), or
/// sends it back to the bench's fakes (None).
#[derive(Clone, Debug, PartialEq)]
pub struct FirmwareNetSwitch(pub Option<Vec<String>>);

/// The person saves a token for an endpoint name and its host in this computer's Fab settings, or
/// forgets it. Its Debug never shows the token.
pub struct FirmwareNetToken {
    pub name: String,
    pub host: String,
    token: Option<Vec<u8>>,
}

impl FirmwareNetToken {
    pub fn save(name: String, host: String, token: Vec<u8>) -> Self {
        FirmwareNetToken { name, host, token: Some(token) }
    }

    pub fn forget(name: String, host: String) -> Self {
        FirmwareNetToken { name, host, token: None }
    }

    /// The token to save (None: forget the saved one).
    pub fn token(&self) -> Option<&[u8]> {
        self.token.as_deref()
    }
}

impl std::fmt::Debug for FirmwareNetToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let what = if self.token.is_some() { "save" } else { "forget" };
        write!(f, "FirmwareNetToken {{ name: {:?}, host: {:?}, {what} }}", self.name, self.host)
    }
}

#[derive(Clone, Copy, Debug)]
pub enum FirmwareMic {
    Pick,
    Level(i32),
    Play,
    ComputerOn(ComputerMicClick),
    ComputerOff,
}

/// Only the pane's toggle creates this action. Keep the origin of the event that flipped it,
/// since the app receives the action after the platform's injected-input scope has ended.
#[derive(Clone, Copy, Debug)]
pub struct ComputerMicClick { local: bool }

impl ComputerMicClick {
    pub fn from_person(self) -> bool { self.local }
}

/// Typed changes closer than this (seconds) are one undo step.
pub const TYPING_PAUSE: f64 = 1.0;

/// The label of the pane's typing in document `name`: its undo step's name, and how the pane
/// knows a refusal for its own.
pub fn typing_label(name: &str) -> String {
    format!("Type in {name}")
}

/// How the bound firmware's build stands, in a few words: what runs, what compiles, what failed.
/// `shown_rev` is the revision of the document text the pane shows; None while it shows typing the
/// engine has not taken.
pub fn firmware_status(fw: &FwLive, shown_rev: Option<u64>) -> String {
    if let Some(problem) = &fw.problem {
        return problem.clone();
    }
    let mut parts = Vec::new();
    // A failure older than the running build is history: that text was fixed since.
    let failed = fw.failed.as_ref().filter(|(rev, _)| fw.running_rev.is_none_or(|running| *rev > running));
    if let Some((_, diags)) = failed {
        let first = diags.first().map_or("it has errors", |d| d.message.as_str());
        parts.push(match diags.len().saturating_sub(1) {
            0 => format!("does not compile: {first}"),
            more => format!("does not compile: {first} (and {more} more)"),
        });
    }
    if fw.compiling_rev.is_some() {
        parts.push("compiling…".into());
    }
    match fw.running_rev {
        Some(running) if failed.is_none() && fw.compiling_rev.is_none() && shown_rev == Some(running) => {
            parts.push("running".into());
        }
        Some(_) => parts.push("the last build runs".into()),
        None => {}
    }
    if let Some(bytes) = fw.eeprom {
        parts.push(format!("EEPROM kept ({bytes} bytes)"));
    }
    match fw.flash.as_ref().map(|flash| &flash.state) {
        Some(FlashState::Flashing(step)) => parts.push(format!("flashing: {step}")),
        Some(FlashState::Done(text)) => parts.push(text.clone()),
        Some(FlashState::Failed(why)) => parts.push(format!("flash failed: {why}")),
        Some(FlashState::Reading(step)) => parts.push(format!("reading the board: {step}")),
        Some(FlashState::ReadFailed(why)) => parts.push(format!("read failed: {why}")),
        Some(FlashState::BackingUp(step)) => parts.push(format!("backing up the board: {step}")),
        Some(FlashState::BackupFailed(why)) => parts.push(format!("back-up failed: {why}")),
        Some(FlashState::Idle) | None => {}
    }
    match fw.flash.as_ref().map(|flash| &flash.monitor) {
        Some(MonitorState::Starting) => parts.push("monitor: restarting the board".into()),
        Some(MonitorState::On) => parts.push("monitoring".into()),
        Some(MonitorState::Paused) => parts.push("monitor paused".into()),
        Some(MonitorState::Ended(why)) => parts.push(format!("monitor ended: {why}")),
        Some(MonitorState::Off) | None => {}
    }
    match fw.not_running.len() {
        0 => {}
        1 => parts.push(format!("{} is bound too, but one device runs", fw.not_running[0])),
        n => parts.push(format!("{n} more are bound, but one device runs")),
    }
    parts.join(" · ")
}

/// What the pane shows after a change of the firmware status or the documents.
#[derive(Clone, Debug, PartialEq)]
pub struct FirmwareUpdate {
    pub title: String,
    pub status: String,
    /// In place of the code: why there is none (no firmware bound, or its document is missing).
    pub empty: Option<String>,
    /// Text for the editor, with the document's name (its extension picks the highlighting): only
    /// when the editor should show something else than it does.
    pub load: Option<(String, Arc<str>)>,
    /// The person's typing is kept in the editor but is not in the document (the status says
    /// why): the pane offers to send it over the document's text or to take the document's.
    pub kept: bool,
    /// The device whose kept EEPROM the person can erase (`FirmwareErase`), by component path.
    pub erase: Option<String>,
    /// Flash to, while the running image can go to a board.
    pub flash: Option<FlashLive>,
    pub radio: Option<RadioLive>,
    pub network: Option<NetworkLive>,
}

/// The document the editor holds.
struct Held {
    doc: DocId,
    name: String,
    /// The engine's text the editor's text was made from, and its revision.
    base: Arc<str>,
    base_rev: u64,
    /// The editor's text as the person left it at the newest change.
    mine: Arc<str>,
    /// The person typed since the newest edit sent: the next edit takes it.
    unsent: bool,
    /// The edit on the way: its tag and the text the document has once it applied.
    on_way: Option<(u64, Arc<str>)>,
    /// Why the person's typing is kept in the editor and not in the document; sending waits for
    /// the person (`FirmwareShown::keep_mine`, `FirmwareShown::take_document`).
    kept: Option<String>,
}

fn same_text(a: &Arc<str>, b: &Arc<str>) -> bool {
    Arc::ptr_eq(a, b) || **a == **b
}

/// The person's typing (`base` to `mine`) moved onto the document's new text (`base` to
/// `theirs`), when the two changes touch different parts of `base`. Cost: O(text).
fn rebase(base: &str, mine: &str, theirs: &str) -> Option<String> {
    if base == theirs {
        return Some(mine.to_owned());
    }
    if base == mine {
        return Some(theirs.to_owned());
    }
    let my = TextEdit::trimmed(DocId(0), base, mine);
    let their = TextEdit::trimmed(DocId(0), base, theirs);
    let (my_span, their_span) = (my.span.start as usize..my.span.end as usize, their.span.start as usize..their.span.end as usize);
    // Changes that meet at a point are not ordered: they conflict too.
    let (first, second) = if my_span.end < their_span.start {
        ((my_span, &my.text), (their_span, &their.text))
    } else if their_span.end < my_span.start {
        ((their_span, &their.text), (my_span, &my.text))
    } else {
        return None;
    };
    Some([&base[..first.0.start], first.1, &base[first.0.end..second.0.start], second.1, &base[second.0.end..]].concat())
}

/// What the pane last showed, so an unchanged frame costs four pointer compares, and its typing.
#[derive(Default)]
pub struct FirmwareShown {
    seen: Option<(Option<Arc<FwLive>>, Arc<Vec<BenchView>>, Option<Arc<DocsView>>, Option<Arc<Refusal>>)>,
    choice: Option<Arc<str>>,
    held: Option<Held>,
    /// The newest refusal looked at (its `seq`).
    refusal_seen: u64,
    /// The typing's undo group and when its newest edit was sent.
    group: Option<(u64, f64)>,
}

fn same<T: ?Sized>(a: Option<&Arc<T>>, b: Option<&Arc<T>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    }
}

impl FirmwareShown {
    /// The person changed the editor's text to `now` at `at` seconds: the edit for the engine,
    /// unless one is on the way or the typing is kept (then it goes later, see
    /// [`FirmwareShown::sends`]) or nothing changed. Cost: O(text).
    pub fn typed(&mut self, at: f64, now: impl FnOnce() -> String) -> Option<ViewAction> {
        let held = self.held.as_mut()?;
        let now = now();
        if *held.mine != *now {
            held.mine = Arc::from(now);
            held.unsent = true;
        }
        self.send(at)
    }

    /// The editor shows exactly the document's text at revision `rev`: no typing is unsent, on
    /// the way or kept. The source's heat and marker name lines of that text only.
    pub fn shows_rev(&self, rev: u64) -> bool {
        self.held.as_ref().is_some_and(|held| {
            held.base_rev == rev && !held.unsent && held.on_way.is_none() && held.kept.is_none() && same_text(&held.mine, &held.base)
        })
    }

    /// The text at revision `rev`, when the editor shows exactly it ([`FirmwareShown::shows_rev`]).
    pub fn text_at(&self, rev: u64) -> Option<&Arc<str>> {
        self.shows_rev(rev).then(|| self.held.as_ref().map(|held| &held.base)).flatten()
    }

    /// Typing waits to be sent and nothing holds it: the pane calls [`FirmwareShown::typed`] again.
    pub fn sends(&self) -> bool {
        self.held.as_ref().is_some_and(|held| held.unsent && held.on_way.is_none() && held.kept.is_none())
    }

    /// The kept typing goes over the document's text, as the person asked.
    pub fn keep_mine(&mut self, at: f64) -> Option<ViewAction> {
        self.held.as_mut()?.kept = None;
        self.seen = None;
        self.send(at)
    }

    /// The person drops the kept typing for the document's text: the text to load.
    pub fn take_document(&mut self) -> Option<(String, Arc<str>)> {
        let held = self.held.as_mut()?;
        held.kept.take()?;
        self.seen = None;
        held.mine = held.base.clone();
        held.unsent = false;
        Some((held.name.clone(), held.base.clone()))
    }

    /// The edit from the document's text to the person's, when it may go now.
    fn send(&mut self, at: f64) -> Option<ViewAction> {
        let held = self.held.as_mut().filter(|held| held.unsent && held.on_way.is_none() && held.kept.is_none())?;
        held.unsent = false;
        if same_text(&held.base, &held.mine) {
            return None;
        }
        let group = match self.group {
            Some((group, last)) if at - last < TYPING_PAUSE => group,
            _ => next_tag(),
        };
        self.group = Some((group, at));
        let tag = next_tag();
        held.on_way = Some((tag, held.mine.clone()));
        Some(ViewAction::Text {
            doc: held.doc, base_rev: held.base_rev, edits: vec![TextEdit::trimmed(held.doc, &held.base, &held.mine)],
            group: Some(group), label: typing_label(&held.name), tag,
        })
    }

    /// The pane's next state for this firmware status (None until the run is made), the evaluated
    /// benches, the documents and the newest refusal; None when none of them changed since the
    /// last call.
    pub fn update(&mut self, fw: Option<&Arc<FwLive>>, benches: &Arc<Vec<BenchView>>, docs: Option<&Arc<DocsView>>,
        refusal: Option<&Arc<Refusal>>) -> Option<FirmwareUpdate> {
        if self.seen.as_ref().is_some_and(|(seen_fw, seen_benches, seen_docs, seen_refusal)| {
            same(seen_fw.as_ref(), fw) && Arc::ptr_eq(seen_benches, benches) && same(seen_docs.as_ref(), docs)
                && same(seen_refusal.as_ref(), refusal)
        }) {
            return None;
        }
        self.seen = Some((fw.cloned(), benches.clone(), docs.cloned(), refusal.cloned()));
        // The engine refused the edit on the way, found by its kind and tag.
        let mut refused = None;
        if let Some(refusal) = refusal.filter(|refusal| refusal.seq > self.refusal_seen) {
            self.refusal_seen = refusal.seq;
            let on_way = self.held.as_ref().and_then(|held| held.on_way.as_ref()).map(|(tag, _)| *tag);
            if refusal.op == "text" && refusal.sample.is_some() && refusal.sample == on_way {
                refused = Some(refusal.reason.clone());
            }
        }
        // While the new scope lowers, the old run can still be publishing its firmware.
        let bench = run_bench(benches, self.choice.as_deref());
        let fw = fw.filter(|fw| benches.is_empty() || bench.is_some_and(|b| b.out.spec.firmware.iter().any(|binding| binding.doc == fw.doc)));
        let bound = fw.map(|fw| fw.doc.as_str())
            .or_else(|| bench?.out.spec.firmware.first().map(|binding| binding.doc.as_str()));
        let Some(doc) = bound else {
            return Some(FirmwareUpdate {
                title: "Firmware".into(),
                status: String::new(),
                empty: Some("No firmware runs on this bench. A microcontroller bound to a firmware document shows its code here.".into()),
                load: None,
                kept: false,
                erase: None,
                flash: None,
                radio: None,
                network: None,
            });
        };
        let text = docs.and_then(|docs| docs.texts.iter().find(|text| text.name == doc));
        let load = text.and_then(|text| self.follow(text, refused));
        let held = self.held.as_ref().filter(|_| text.is_some());
        let kept = held.and_then(|held| held.kept.as_ref());
        // The revision the editor shows; None while it shows typing the engine has not taken.
        let shown_rev = held.filter(|held| !held.unsent && held.on_way.is_none() && held.kept.is_none()).map(|held| held.base_rev);
        let status = match kept {
            Some(why) => {
                let stop = if why.ends_with(['.', '?', '!']) { "" } else { "." };
                format!("Not in the document: {why}{stop} Keep yours, or take the document's.")
            }
            None => fw.map_or_else(|| "compiles when the bench runs".to_owned(), |fw| firmware_status(fw, shown_rev)),
        };
        Some(FirmwareUpdate {
            title: doc.to_owned(),
            status,
            empty: text.is_none().then(|| format!("The firmware document {doc} is not in the project.")),
            kept: kept.is_some(),
            erase: fw.filter(|fw| fw.eeprom.is_some()).map(|fw| fw.part.clone()),
            flash: fw.and_then(|fw| fw.flash.clone()),
            radio: fw.and_then(|fw| fw.radio.clone()),
            network: fw.and_then(|fw| fw.network.clone()),
            load,
        })
    }

    /// Follows the engine's text of the bound document: what the editor loads, if anything. The
    /// edit on the way arrives when the document shows its text one revision on; while the
    /// document is still at the text it addressed, the pane waits. Any other text (another
    /// writer, an undo, a new load) or a refusal of the edit on the way moves the person's typing
    /// onto the document's text: it loads with the typing in it and goes as the next edit, or,
    /// when the two changes meet, stays in the editor, kept, until the person decides.
    fn follow(&mut self, now: &DocText, refused: Option<String>) -> Option<(String, Arc<str>)> {
        // The same document by name: a new load may number it anew.
        let Some(held) = self.held.as_mut().filter(|held| held.name == now.name) else {
            let held = Held {
                doc: now.doc, name: now.name.clone(), base: now.text.clone(), base_rev: now.rev, mine: now.text.clone(),
                unsent: false, on_way: None, kept: None,
            };
            self.held = Some(held);
            self.group = None;
            return Some((now.name.clone(), now.text.clone()));
        };
        let at_base = now.doc == held.doc && now.rev == held.base_rev && same_text(&held.base, &now.text);
        if refused.is_none() {
            // In step, or the edit on the way still waits.
            if at_base {
                return None;
            }
            let arrived = held.on_way.as_ref()
                .is_some_and(|(_, sent)| now.doc == held.doc && now.rev == held.base_rev + 1 && same_text(sent, &now.text));
            if arrived {
                held.base = now.text.clone();
                held.base_rev = now.rev;
                held.on_way = None;
                return None;
            }
        }
        // The person's typing: what the editor holds beyond the document's text it was made from.
        held.on_way = None;
        let typed = !same_text(&held.base, &held.mine);
        if at_base {
            // Refused with the document as it was: sending it again would be refused again.
            held.kept = refused.filter(|_| typed).or(held.kept.take());
            held.unsent = typed;
            return None;
        }
        let moved = rebase(&held.base, &held.mine, &now.text);
        held.doc = now.doc;
        held.base = now.text.clone();
        held.base_rev = now.rev;
        self.group = None;
        if !typed {
            held.mine = now.text.clone();
            held.unsent = false;
            held.kept = None;
            return Some((now.name.clone(), now.text.clone()));
        }
        match moved {
            Some(merged) if held.kept.is_none() => {
                held.mine = Arc::from(merged);
                held.unsent = true;
                Some((now.name.clone(), held.mine.clone()))
            }
            _ => {
                held.kept = Some(format!("{} changed where you typed", now.name));
                held.unsent = true;
                None
            }
        }
    }
}

/// The Firmware pane, registered as `mod.widgets.FirmwarePane`.
#[derive(Script, ScriptHook, Widget)]
pub struct FirmwarePane {
    #[deref]
    view: View,
    #[rust]
    shown: FirmwareShown,
    /// The pane shows only its head (it was lower than [`FOLD_UNDER`] at the last draw).
    #[rust]
    folded: bool,
    /// There is code to show (a bound document in the project).
    #[rust]
    has_code: bool,
    /// The person's typing is kept in the editor, not in the document.
    #[rust]
    kept: bool,
    /// The device whose kept EEPROM the head's Erase button erases.
    #[rust]
    erase: Option<String>,
    /// Flash to as the head shows it: the dropdown's rows are its devices, in order.
    #[rust]
    flash: Option<FlashLive>,
    #[rust]
    radio: Option<RadioLive>,
    /// The Network row as shown: the token row's endpoints are its named servers, in order.
    #[rust]
    network: Option<NetworkLive>,
    #[rust]
    mic: Arc<MicLive>,
    #[rust]
    build_status: String,
    /// The monitor's stream the list shows.
    #[rust]
    monitor_feed: Option<Arc<MonitorFeed>>,
    /// The name of the document the editor shows.
    #[rust]
    loaded: Option<String>,
    /// The device state the state panel last read (`SimLive::co`; None: it reads the next one
    /// whatever it is) and what it shows.
    #[rust]
    state_seen: Option<Option<CoState>>,
    #[rust]
    state_shown: StateText,
    /// The state panel shows two columns (`fw_state::state_columns`).
    #[rust]
    state_two: bool,
    /// The pane's height less the state panel's visible height (its head), from the
    /// last draw that showed the panel; None before one.
    #[rust]
    state_chrome: Option<f64>,
    /// The device state the source's heat and marker last read (none after the source was
    /// folded away), the heat, a scratch list of the lines that ran since, and what the editor
    /// shows (washes, outlines, marker).
    #[rust]
    source_seen: Option<CoState>,
    #[rust]
    source_playing: bool,
    #[rust]
    heat: Heat,
    /// A declaration's ghost value brightens when it changes, even while paused, and fades.
    #[rust]
    value_heat: Heat,
    #[rust]
    inlays_shown: Vec<(usize, String)>,
    /// The ghost values brightened as last handed to the editor.
    #[rust]
    lifts_shown: Vec<(usize, f32)>,
    /// The pin of the state panel under the pointer, which the host emphasises.
    #[rust]
    hovered_pin: Option<&'static str>,
    #[rust]
    ran: Vec<usize>,
    #[rust]
    source_shown: (Vec<(std::ops::Range<usize>, f32)>, Vec<std::ops::Range<usize>>, Option<Mark>),
    /// The text the running image was built from and its line starts, the conditions in their
    /// newest values and the lines that never ran (from the newest publication read), as shown.
    #[rust]
    source_lines: Option<(Arc<str>, SourceLines)>,
    #[rust]
    run_shown: (Vec<CondMark>, Vec<std::ops::Range<usize>>),
    /// Lines an edit from outside the app changed ([`FirmwarePane::flash_lines`]): the document,
    /// its 0-based lines and when the flash began. They wash in the AI colour and fade.
    #[rust]
    flashed: Option<(String, std::ops::Range<usize>, f64)>,
    /// The flashed lines and their levels as last handed to the editor.
    #[rust]
    flash_shown: Vec<(usize, f32)>,
    #[live]
    color_true: Vec4f,
    #[live]
    color_false: Vec4f,
    #[live]
    color_inlay: Vec4f,
}

impl FirmwarePane {
    /// Takes the host's newest firmware status and documents; true when the pane changed.
    fn follow(&mut self, cx: &mut Cx) -> bool {
        let Some(live) = cx.get_global_ref::<ElecLive>() else { return false };
        if self.shown.choice != live.chosen_bench {
            self.shown.choice.clone_from(&live.chosen_bench);
            self.shown.seen = None;
        }
        let (fw, benches, docs, refusal) =
            (live.sim.firmware.clone(), live.snap.benches.clone(), live.snap.docs.clone(), live.snap.last_refusal.clone());
        let mic = live.sim.mic.clone();
        let changed = !Arc::ptr_eq(&mic, &self.mic);
        if changed {
            self.view.widget(cx, ids!(head.mic)).set_visible(cx, mic.rate.is_ok());
            let file = mic.file.as_deref().and_then(|p| p.file_name()).map(|name| name.to_string_lossy());
            self.view.label(cx, ids!(head.mic.file)).set_text(cx, file.as_deref().unwrap_or("No file selected"));
            if mic.level_db != self.mic.level_db {
                self.view.slider(cx, ids!(head.mic.level)).set_value(cx, f64::from(mic.level_db));
            }
            self.view.button(cx, ids!(head.mic.play)).set_enabled(cx, mic.ready && mic.rate.is_ok() && !mic.computer);
            let toggle = self.view.check_box(cx, ids!(head.mic.computer));
            if toggle.active(cx) != mic.computer { toggle.set_active(cx, mic.computer, Animate::No); }
            self.view.widget(cx, ids!(head.mic.capture_status)).set_visible(cx, !mic.capture_status.is_empty());
            self.view.label(cx, ids!(head.mic.capture_status)).set_text(cx, &mic.capture_status);
            self.mic = mic;
        }
        let update = self.shown.update(fw.as_ref(), &benches, docs.as_ref(), refusal.as_ref());
        if changed || update.is_some() {
            if let Some(update) = &update { self.build_status.clone_from(&update.status); }
            let mic_status = match (&self.mic.file, self.mic.rate) {
                (Some(_), Err(why)) => why,
                _ => self.mic.status.as_str(),
            };
            let status = match (self.build_status.is_empty(), mic_status.is_empty()) {
                (false, false) => format!("{} · {mic_status}", self.build_status),
                (false, true) => self.build_status.clone(),
                (true, _) => mic_status.to_owned(),
            };
            self.view.label(cx, ids!(head.status)).set_text(cx, &status);
        }
        let Some(update) = update else { return changed };
        self.view.widget(cx, ids!(head.doc_title)).set_text(cx, &update.title);
        self.view.widget(cx, ids!(empty)).set_text(cx, update.empty.as_deref().unwrap_or(""));
        self.has_code = update.empty.is_none();
        self.kept = update.kept;
        self.erase = update.erase;
        if self.flash != update.flash {
            self.flash = update.flash;
            self.sync_flash(cx);
        }
        if self.radio != update.radio {
            self.radio = update.radio;
            self.sync_radio(cx);
        }
        if self.network != update.network {
            self.network = update.network;
            self.sync_network(cx);
        }
        self.sync_forms(cx);
        if let Some((name, text)) = update.load {
            self.load(cx, &name, &text);
        }
        true
    }

    /// The pin under the pointer in the state panel: the host emphasises it and the net its pad is
    /// on in every view that draws them. One action per change of pin, none while it stays.
    fn hover_pin(&mut self, cx: &mut Cx, pin: Option<&'static str>) {
        if pin != self.hovered_pin {
            self.hovered_pin = pin;
            cx.action(SimAction::EmphasisDevicePin(pin.map(str::to_owned)));
        }
    }

    /// Takes the device's newest state into the state panel (`crate::fw_state`): nothing while
    /// folded or when the publication is the one it read; a label changes only when its text did.
    /// True when the panel changed.
    fn follow_state(&mut self, cx: &mut Cx) -> bool {
        if self.folded || !self.has_code {
            return false;
        }
        let Some(live) = cx.get_global_ref::<ElecLive>() else { return false };
        let co = live.sim.co.clone();
        if self.state_seen.as_ref() == Some(&co) {
            return false;
        }
        let text = match co.as_ref().and_then(|co| co.get::<FwState>()) {
            Some(s) => state_text(s),
            None => StateText::idle(),
        };
        self.state_seen = Some(co);
        if text == self.state_shown {
            return false;
        }
        let old = std::mem::replace(&mut self.state_shown, text);
        let new = &self.state_shown;
        if old.head != new.head {
            self.view.widget(cx, ids!(state.head)).set_text(cx, &new.head);
        }
        if let Some(mut rows) = self.view.widget(cx, ids!(state.rows)).borrow_mut::<FwRows>() {
            rows.set_rows(cx, &new.rows);
        }
        if old.at != new.at {
            self.view.widget(cx, ids!(state.at)).set_text(cx, &new.at);
        }
        if old.idle_pins != new.idle_pins {
            let idle = self.view.widget(cx, ids!(state.pins_idle));
            idle.set_visible(cx, !new.idle_pins.is_empty());
            idle.set_text(cx, &new.idle_pins);
        }
        for (head, body, was, now) in [
            (ids!(state.pins_head), ids!(state.pins), &old.pins, &new.pins),
            (ids!(state.timers_head), ids!(state.timers), &old.timers, &new.timers),
            (ids!(state.pending_head), ids!(state.pending), &old.pending, &new.pending),
            (ids!(state.faults_head), ids!(state.faults), &old.faults, &new.faults),
            (ids!(state.net_head), ids!(state.net), &old.net, &new.net),
            (ids!(state.regs_head), ids!(state.regs), &old.regs, &new.regs),
        ] {
            if was == now {
                continue;
            }
            if was.is_empty() != now.is_empty() {
                self.view.widget(cx, head).set_visible(cx, !now.is_empty());
                self.view.widget(cx, body).set_visible(cx, !now.is_empty());
            }
            self.view.widget(cx, body).set_text(cx, &now.join("\n"));
        }
        // The head names the pins in use and the idle line alike.
        if old.pins != new.pins || old.idle_pins != new.idle_pins {
            self.view.widget(cx, ids!(state.pins_head)).set_visible(cx, !new.pins.is_empty() || !new.idle_pins.is_empty());
        }
        true
    }

    /// Washes `lines` (0-based) of the document `doc` in the AI colour, fading over
    /// [`FLASH_SECONDS`]: an edit from outside the app landed there (V9.3). Nothing while the
    /// editor shows another document. O(1) now; O(lines) a frame while it fades.
    pub fn flash_lines(&mut self, cx: &mut Cx, doc: &str, lines: std::ops::Range<usize>) {
        self.flashed = Some((doc.to_owned(), lines, cx.seconds_since_app_start()));
        cx.new_next_frame();
    }

    /// The flashed lines at `now` with their level; empty once the flash has faded or while
    /// another document shows (the flash then ends).
    fn flash_levels(&mut self, now: f64) -> Vec<(usize, f32)> {
        let Some((doc, lines, at)) = &self.flashed else { return Vec::new() };
        let level = 1.0 - ((now - at) / FLASH_SECONDS).clamp(0.0, 1.0);
        if level <= 0.0 || self.loaded.as_deref() != Some(doc.as_str()) {
            self.flashed = None;
            return Vec::new();
        }
        lines.clone().map(|line| (line, level as f32)).collect()
    }

    /// The source's heat and marker (`crate::fw_state`): while playing, the statements each new
    /// publication names pulse and fade in wall time; paused or stopped, one marker on the
    /// statement about to execute (hollow on the last that ran while the device sleeps). Nothing
    /// unless the editor shows exactly the text the running image was built from. The editor
    /// changes only when what it shows did; frames keep coming only while a line is hot.
    fn follow_source(&mut self, cx: &mut Cx) {
        let live = cx.get_global_ref::<ElecLive>().filter(|_| self.has_code && !self.folded);
        let co = live.and_then(|l| l.sim.co.clone());
        let playing = live.is_some_and(|l| l.sim.playing);
        let now = cx.seconds_since_app_start();
        let fresh = self.source_seen != co;
        let seen = self.source_seen.as_ref().and_then(CoState::get::<FwState>);
        let source = co.as_ref().and_then(CoState::get::<FwState>)
            .and_then(|s| Some((s, self.shown.text_at(s.source_rev)?)));
        let text_changed = source.is_some_and(|(_, text)| self.source_lines.as_ref().is_none_or(|l| !Arc::ptr_eq(&l.0, text)));
        let (run, inlays) = match source {
            Some((s, text)) if fresh || text_changed => {
                if text_changed {
                    self.source_lines = Some((text.clone(), SourceLines::new(text)));
                }
                let lines = &self.source_lines.as_ref().expect("just made").1;
                let (mut conds, mut dims, mut values) = (Vec::new(), Vec::new(), Vec::new());
                cond_marks(s, seen, lines, &mut conds);
                unrun_lines(s, lines, &mut dims);
                value_inlays(s, lines, &mut values);
                if !text_changed && seen.is_some_and(|p| (p.image_id, p.source_rev) == (s.image_id, s.source_rev)) {
                    // The same image has the same declarations in the same order.
                    self.ran.clear();
                    self.ran.extend(values.iter().zip(&self.inlays_shown).filter_map(|((line, value), (was_line, was))|
                        (line == was_line && value != was).then_some(*line)));
                    self.value_heat.pulse(&self.ran, now);
                } else {
                    self.value_heat.clear();
                    self.heat.clear();
                }
                (Some((conds, dims)), Some(values))
            }
            Some(_) => (None, None),
            None => {
                self.heat.clear();
                self.value_heat.clear();
                (Some(Default::default()), Some(Vec::new()))
            }
        };
        let (mut washes, mut outlines, mut mark) = (Vec::new(), Vec::new(), None);
        match source {
            Some((s, _)) if playing => {
                if fresh {
                    ran_lines(s, seen, &mut self.ran);
                    self.heat.pulse(&self.ran, now);
                }
                washes = self.heat.washes(now);
            }
            Some((s, _)) => {
                self.heat.clear();
                mark = if fresh || text_changed || self.source_playing { marker(s) } else { self.source_shown.2 };
                match mark {
                    Some(Mark::Solid(line)) => washes.push((line..line + 1, MARK_WASH)),
                    Some(Mark::Hollow(line)) => outlines.push(line..line + 1),
                    None => {}
                }
            }
            None => {}
        }
        // a changed value lights only its ghost text, so the code stays calm while it plays
        let lifts = self.value_heat.levels(now);
        self.source_seen = if source.is_some() { co } else { None };
        self.source_playing = playing;
        let flash = self.flash_levels(now);
        if self.heat.is_hot() || self.value_heat.is_hot() || self.flashed.is_some() {
            cx.new_next_frame();
        }
        let run = run.filter(|run| *run != self.run_shown);
        // Edits can move an existing inlay's byte anchor even if its line and value still match.
        let inlays = inlays.filter(|values| *values != self.inlays_shown || (text_changed && !values.is_empty()));
        let decorations = (washes, outlines, mark);
        if decorations == self.source_shown && run.is_none() && inlays.is_none() && lifts == self.lifts_shown && flash == self.flash_shown {
            return;
        }
        let ai = crate::canvas::looks::Tokens::for_theme(base_theme(cx) != BaseTheme::Light).ai;
        if let Some(mut view) = self.view.widget(cx, ids!(code)).borrow_mut::<CodeView>() {
            view.editor.line_washes.clone_from(&decorations.0);
            view.editor.line_outlines.clone_from(&decorations.1);
            view.editor.inlay_color = Some(self.color_inlay);
            view.editor.inlay_lifts.clone_from(&lifts);
            let spans_changed = run.is_some() || flash != self.flash_shown;
            if let Some(run) = run {
                view.editor.line_dims.clone_from(&run.1);
                self.run_shown = run;
            }
            if spans_changed {
                let mut spans: Vec<_> = self.run_shown.0.iter().map(|c| {
                    let mut color = if c.value { self.color_true } else { self.color_false };
                    color.w *= COND_WASH;
                    (Position { line_index: c.line, byte_index: c.bytes.start }, Position { line_index: c.line, byte_index: c.bytes.end }, color)
                }).collect();
                // a flashed line is one span over its whole text, under the conditions' marks
                if let (false, Some(session)) = (flash.is_empty(), &view.session) {
                    let text = session.document().as_text();
                    for &(line, level) in &flash {
                        let Some(len) = text.as_lines().get(line).map(|l| l.len()) else { continue };
                        let color = vec4(ai[0], ai[1], ai[2], ai[3] * FLASH_WASH * level);
                        spans.push((Position { line_index: line, byte_index: 0 }, Position { line_index: line, byte_index: len }, color));
                    }
                    spans.sort_by_key(|(start, ..)| (start.line_index, start.byte_index));
                }
                view.editor.span_washes = spans;
                self.flash_shown = flash;
            }
            if let Some(values) = inlays {
                view.lazy_init_session();
                if let Some(session) = &view.session {
                    let document = session.document();
                    let inlays: Vec<_> = {
                        let text = document.as_text();
                        values.iter().filter_map(|(line, value)| {
                            let end = text.as_lines().get(*line)?.len();
                            Some((*line, end, InlineInlay::Text(value.clone())))
                        }).collect()
                    };
                    // A replacement in this session may also have carried every inlay to the
                    // correct end already. Only remeasure if the placements actually changed.
                    let already_placed = text_changed && values.len() == self.inlays_shown.len() && {
                        let layout = document.layout();
                        inlays.iter().all(|(line, end, value)| {
                            matches!(layout.inline_inlays[*line].as_slice(), [(at, shown)] if at == end && shown == value)
                        })
                    };
                    if !already_placed {
                        document.set_inline_inlays(inlays);
                        session.relayout_inline();
                    }
                    self.inlays_shown = values;
                }
            }
            // A new marker off screen comes into view.
            if let Some(line) = mark.filter(|m| Some(*m) != self.source_shown.2).map(Mark::line) {
                if !view.session.as_ref().is_some_and(|session| view.editor.shows_line(session, line)) {
                    view.editor.center_line(cx, line);
                }
            }
            view.redraw(cx);
            self.source_shown = decorations;
            self.lifts_shown = lifts;
        } else {
            self.source_seen = None;
        }
    }

    /// Shows `text` of the document `name`: the same document's new text replaces only the part
    /// that changed, so the cursor stays with the text around it while someone types.
    fn load(&mut self, cx: &mut Cx, name: &str, text: &str) {
        if let Some(mut view) = self.view.widget(cx, ids!(code)).borrow_mut::<CodeView>() {
            if self.loaded.as_deref() == Some(name) {
                view.replace_text(cx, text);
            } else {
                view.set_text_for_path(cx, name, text);
                self.inlays_shown.clear();
                self.loaded = Some(name.to_string());
            }
            self.source_lines = None;
        }
    }

    /// The editor's text as the person typed it, to the engine: the edit, or nothing while one is
    /// on the way (it goes once the engine's text shows that one applied).
    fn send_typed(&mut self, cx: &mut Cx) {
        let code = self.view.widget(cx, ids!(code));
        if let Some(edit) = self.shown.typed(cx.seconds_since_app_start(), || code.text()) {
            cx.action(edit);
        }
    }
}

impl FirmwarePane {
    /// The code or why there is none, unless folded; the button that changes the form.
    fn sync_forms(&mut self, cx: &mut Cx) {
        self.view.widget(cx, ids!(source)).set_visible(cx, !self.folded && self.has_code);
        self.view.widget(cx, ids!(empty)).set_visible(cx, !self.folded && !self.has_code);
        // a full-page tab: nothing to fold (the head keeps the buttons for a pane that is short)
        self.view.widget(cx, ids!(head.fold)).set_visible(cx, false);
        self.view.widget(cx, ids!(head.unfold)).set_visible(cx, self.folded);
        self.view.widget(cx, ids!(head.keep)).set_visible(cx, self.kept);
        self.view.widget(cx, ids!(head.take)).set_visible(cx, self.kept);
        self.view.widget(cx, ids!(head.erase)).set_visible(cx, self.erase.is_some());
        self.view.widget(cx, ids!(head.flash)).set_visible(cx, self.flash.is_some());
        self.view.widget(cx, ids!(head.flash_to)).set_visible(cx, self.flash.is_some());
        self.view.widget(cx, ids!(head.flash_read)).set_visible(cx, self.flash.is_some());
        self.view.widget(cx, ids!(head.backup)).set_visible(cx, self.flash.is_some());
        let backing = self.flash.as_ref().is_some_and(|flash| matches!(flash.state, FlashState::BackingUp(_)));
        self.view.widget(cx, ids!(head.backup)).set_text(cx, if backing { "Cancel" } else { "Back up" });
        let monitor = self.flash.as_ref().map(|flash| &flash.monitor);
        self.view.widget(cx, ids!(head.monitor)).set_visible(cx, monitor.is_some());
        let running = matches!(monitor, Some(MonitorState::Starting | MonitorState::On | MonitorState::Paused));
        self.view.widget(cx, ids!(head.monitor)).set_text(cx, if running { "Stop" } else { "Monitor" });
        let lines = monitor.is_some_and(|monitor| *monitor != MonitorState::Off);
        self.view.widget(cx, ids!(monitor_lines)).set_visible(cx, !self.folded && lines);
    }

    /// Takes the monitor's waiting lines into its list (a new stream empties it first); true when
    /// the list changed. Per signal or frame one pointer compare and one uncontended lock.
    fn follow_monitor(&mut self, cx: &mut Cx) -> bool {
        let feed = self.flash.as_ref().and_then(|flash| flash.feed.clone());
        let list = self.view.log_list(cx, ids!(monitor_lines));
        let mut changed = false;
        if feed.as_ref().map(Arc::as_ptr) != self.monitor_feed.as_ref().map(Arc::as_ptr) {
            if feed.is_some() {
                list.clear(cx);
            }
            self.monitor_feed = feed;
            changed = true;
        }
        let Some(feed) = &self.monitor_feed else { return changed };
        let (lines, spilled) = feed.take();
        if spilled > 0 {
            list.push(cx, LogLevel::Warning, &format!("{spilled} lines went only to the monitor's log file"));
            changed = true;
        }
        if !lines.is_empty() {
            list.extend(cx, lines.iter().map(|(level, line)| (*level, line.as_str())));
            changed = true;
        }
        changed
    }

    fn sync_radio(&mut self, cx: &mut Cx) {
        self.view.widget(cx, ids!(head.radio)).set_visible(cx, self.radio.is_some());
        let Some(radio) = &self.radio else { return };
        self.view.label(cx, ids!(head.radio.face)).set_text(cx, &crate::fw_state::radio_face(radio));
        self.view.widget(cx, ids!(head.radio.get)).set_visible(cx,
            matches!(radio.state, RadioState::Absent | RadioState::Failed(_) | RadioState::Cancelled));
        self.view.widget(cx, ids!(head.radio.cancel)).set_visible(cx, matches!(radio.state, RadioState::Fetching { .. }));
    }

    fn sync_network(&mut self, cx: &mut Cx) {
        self.view.widget(cx, ids!(head.network)).set_visible(cx, self.network.is_some());
        let Some(network) = &self.network else { return };
        self.view.label(cx, ids!(head.network.face)).set_text(cx, &crate::fw_state::network_face(network));
        self.view.label(cx, ids!(head.network.servers)).set_text(cx, &crate::fw_state::network_servers(network).join("\n"));
        let unapproved = network.on && network.servers.iter().any(|server| !server.approved);
        self.view.widget(cx, ids!(head.network.allow)).set_visible(cx, network.switchable && unapproved);
        let switch = self.view.button(cx, ids!(head.network.switch));
        switch.set_visible(cx, network.switchable);
        switch.set_text(cx, if network.on { "Back to the fakes" } else { "Use the real network" });
        let named: Vec<String> = self.named_servers().map(|server| {
            format!("{} ({}): {}", server.name.as_deref().unwrap_or_default(), server.host, crate::fw_state::token_text(&server.token))
        }).collect();
        self.view.widget(cx, ids!(head.network.tokens)).set_visible(cx, !named.is_empty());
        let list = self.view.drop_down(cx, ids!(head.network.endpoint));
        let selected = list.selected_item().min(named.len().saturating_sub(1));
        list.set_labels(cx, named);
        list.set_selected_item(cx, selected);
    }

    /// The servers whose tokens the token row saves: those with an endpoint name.
    fn named_servers(&self) -> impl Iterator<Item = &NetServerLive> {
        self.network.iter().flat_map(|network| network.servers.iter()).filter(|server| server.name.is_some())
    }

    /// The token row's choice: the endpoint name and host of its selected server.
    fn token_endpoint(&self, cx: &mut Cx) -> Option<(String, String)> {
        let row = self.view.drop_down(cx, ids!(head.network.endpoint)).selected_item();
        self.named_servers().nth(row).map(|server| (server.name.clone().unwrap_or_default(), server.host.clone()))
    }

    /// The dropdown's rows: each board with why it may not be flashed (greyed), the chosen one
    /// selected. While none is chosen a greyed first row says so (or, with no board, the hint),
    /// so the dropdown never shows a board the button would not write to.
    fn sync_flash(&mut self, cx: &mut Cx) {
        let Some(flash) = &self.flash else { return };
        let list = self.view.drop_down(cx, ids!(head.flash_to));
        let chosen = flash.devices.iter().position(|device| flash.chosen.as_deref() == Some(device.path.as_str()));
        let mut labels = Vec::with_capacity(flash.devices.len() + 1);
        let mut faces = Vec::with_capacity(flash.devices.len() + 1);
        let mut disabled = Vec::with_capacity(flash.devices.len() + 1);
        if chosen.is_none() {
            let hint = flash.hint.clone().unwrap_or_else(|| "choose a board".into());
            faces.push(hint.clone());
            labels.push(hint);
            disabled.push(true);
        }
        for device in &flash.devices {
            labels.push(match &device.off {
                Some(why) => format!("{}: not flashable, {why}", device.label),
                None => device.label.clone(),
            });
            faces.push(device.face.clone());
            disabled.push(device.off.is_some());
        }
        list.set_labels(cx, labels);
        list.set_face_labels(cx, faces);
        list.set_disabled_items(cx, disabled);
        list.set_selected_item(cx, chosen.unwrap_or(0));
    }

    /// The board at dropdown row `row` (`sync_flash`'s rows).
    fn flash_row(&self, row: usize) -> Option<&FlashDevice> {
        let flash = self.flash.as_ref()?;
        let chosen = flash.devices.iter().any(|device| flash.chosen.as_deref() == Some(device.path.as_str()));
        flash.devices.get(if chosen { row } else { row.checked_sub(1)? })
    }
}

impl FirmwarePane {
    /// The height as one column, including wrapped rows, label margins and panel padding.
    /// Pending interrupts scroll into view without changing the layout.
    fn state_need(&self, cx: &mut Cx) -> f64 {
        let pad = self.view.widget(cx, ids!(state)).borrow::<View>().map(|state| state.layout.padding).unwrap_or_default();
        let width = STATE_PANEL - pad.left - pad.right;
        let mut need = pad.top + pad.bottom;
        let widget = self.view.widget(cx, ids!(state.rows));
        let m = widget.walk(cx).margin;
        if let Some(rows) = widget.borrow::<FwRows>() {
            need += rows.height_for(cx, width - m.left - m.right) + m.top + m.bottom;
        }
        for path in [
            ids!(state.head), ids!(state.at), ids!(state.pins_head),
            ids!(state.pins), ids!(state.pins_idle), ids!(state.timers_head), ids!(state.timers),
            ids!(state.faults_head), ids!(state.faults), ids!(state.net_head), ids!(state.net),
            ids!(state.regs_head), ids!(state.regs),
        ] {
            let widget = self.view.widget(cx, path);
            if !widget.visible() {
                continue;
            }
            let text = widget.text();
            let Some(label) = widget.borrow::<Label>() else { continue };
            let m = label.walk.margin;
            // An empty label draws a space (its line height).
            let shown = if text.is_empty() { " " } else { text.as_str() };
            // A label's margins count twice as it draws: it begins its own turtle with its walk,
            // margins included, and its parent walks that turtle with them again. So it takes its
            // text's height plus twice its top and bottom margins, and wraps at its width less twice
            // its side margins (measured against the drawn panel: exact in one column and in two).
            let laid = label.draw_text.layout(cx, 0.0, 0.0, Some((width - 2.0 * (m.left + m.right)) as f32), true, Align::default(), shown);
            need += f64::from(laid.size_in_lpxs.height) + 2.0 * (m.top + m.bottom);
        }
        need
    }

    /// The state panel's layout for the pane's size and the texts about to be drawn; None while
    /// the panel's surroundings are unknown (before it was first drawn).
    fn state_layout(&self, cx: &mut Cx, pane: DVec2) -> Option<bool> {
        let chrome = self.state_chrome.filter(|_| !self.folded)?;
        if pane.x < SOURCE_MIN + STATE_PANEL_TWO {
            // One column whatever its content: nothing to measure.
            return Some(false);
        }
        Some(state_columns(self.state_two, pane.x, pane.y - chrome, self.state_need(cx)))
    }

    /// One column (today's order, scrolling) or two side by side.
    fn lay_out_state(&mut self, cx: &mut Cx, two: bool) {
        if let Some(mut panel) = self.view.widget(cx, ids!(source.panel)).borrow_mut::<View>() {
            panel.walk.width = Size::Fixed(if two { STATE_PANEL_TWO } else { STATE_PANEL });
        }
        if let Some(mut state) = self.view.widget(cx, ids!(state)).borrow_mut::<View>() {
            state.layout.flow = if two { Flow::right() } else { Flow::Down };
            state.layout.spacing = if two { 10.0 } else { 0.0 };
        }
        if let Some(mut first) = self.view.widget(cx, ids!(state.first)).borrow_mut::<View>() {
            first.walk.width = if two { Size::Fixed(STATE_COLUMN) } else { Size::fill() };
        }
    }
}

impl Widget for FirmwarePane {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        // The host's snapshots arrive with signals; its firmware status changes on its frames.
        if matches!(event, Event::Signal | Event::NextFrame(_)) {
            // A new document, status or bench redraws the pane; the state panel's labels and rows
            // redraw themselves (a redraw of the pane would record the source again with them).
            if self.follow(cx) {
                self.view.redraw(cx);
            }
            self.follow_state(cx);
            self.follow_monitor(cx);
            self.follow_source(cx);
            if self.shown.sends() {
                self.send_typed(cx);
            }
        }
        let pins = self.view.widget(cx, ids!(state.pins_box)).area();
        match event.hits(cx, pins) {
            Hit::FingerHoverIn(e) | Hit::FingerHoverOver(e) => {
                let rect = pins.rect(cx);
                let names = &self.state_shown.pin_names;
                let pin = line_at(rect.pos.y, rect.size.y, names.len(), e.abs.y).and_then(|line| names.get(line).copied());
                self.hover_pin(cx, pin);
            }
            Hit::FingerHoverOut(_) => self.hover_pin(cx, None),
            _ => {}
        }
        let actions = cx.capture_actions(|cx| self.view.handle_event(cx, event, scope));
        let code = self.view.widget(cx, ids!(code)).widget_uid();
        if actions.find_widget_action(code).is_some_and(|action| matches!(action.cast::<CodeViewAction>(), CodeViewAction::Changed)) {
            self.send_typed(cx);
        }
        if self.view.button(cx, ids!(head.keep)).clicked(&actions) {
            if let Some(edit) = self.shown.keep_mine(cx.seconds_since_app_start()) {
                cx.action(edit);
            }
            self.follow(cx);
            self.view.redraw(cx);
        }
        if self.view.button(cx, ids!(head.take)).clicked(&actions) {
            if let Some((name, text)) = self.shown.take_document() {
                self.load(cx, &name, &text);
            }
            self.follow(cx);
            self.view.redraw(cx);
        }
        if self.view.button(cx, ids!(head.erase)).clicked(&actions) {
            if let Some(part) = self.erase.clone() {
                cx.action(FirmwareErase(part));
            }
        }
        if let Some(row) = self.view.drop_down(cx, ids!(head.flash_to)).changed(&actions) {
            if let Some(device) = self.flash_row(row).filter(|device| device.off.is_none()) {
                cx.action(FirmwareFlashChoose(device.path.clone()));
            }
        }
        if self.view.button(cx, ids!(head.flash)).clicked(&actions) {
            cx.action(FirmwareFlash);
        }
        if self.view.button(cx, ids!(head.flash_read)).clicked(&actions) {
            cx.action(FirmwareFlashRead);
        }
        if self.view.button(cx, ids!(head.backup)).clicked(&actions) {
            cx.action(FirmwareBackup);
        }
        if self.view.button(cx, ids!(head.monitor)).clicked(&actions) {
            cx.action(FirmwareMonitor);
        }
        if self.view.button(cx, ids!(head.radio.get)).clicked(&actions) {
            cx.action(FirmwareRadioGet);
        }
        if self.view.button(cx, ids!(head.radio.cancel)).clicked(&actions) {
            cx.action(FirmwareRadioCancel);
        }
        if let Some(network) = &self.network {
            let hosts = crate::fw_state::network_hosts(network);
            if self.view.button(cx, ids!(head.network.switch)).clicked(&actions) {
                cx.action(FirmwareNetSwitch(if network.on { None } else { Some(hosts.clone()) }));
            }
            if self.view.button(cx, ids!(head.network.allow)).clicked(&actions) {
                cx.action(FirmwareNetSwitch(Some(hosts)));
            }
        }
        let input = self.view.text_input(cx, ids!(head.network.token));
        let save = self.view.button(cx, ids!(head.network.save)).clicked(&actions) || input.returned(&actions).is_some();
        if save && !input.text().is_empty() {
            if let Some((name, host)) = self.token_endpoint(cx) {
                cx.action(FirmwareNetToken::save(name, host, input.text().into_bytes()));
            }
            // the typed token leaves the field once it is handed on
            input.set_text(cx, "");
        }
        if self.view.button(cx, ids!(head.network.forget)).clicked(&actions) {
            if let Some((name, host)) = self.token_endpoint(cx) {
                cx.action(FirmwareNetToken::forget(name, host));
            }
        }
        if self.view.button(cx, ids!(head.mic.pick)).clicked(&actions) {
            cx.action(FirmwareMic::Pick);
        }
        if let Some(level) = self.view.slider(cx, ids!(head.mic.level)).slided(&actions) {
            cx.action(FirmwareMic::Level(level.round() as i32));
        }
        if self.view.button(cx, ids!(head.mic.play)).clicked(&actions) {
            cx.action(FirmwareMic::Play);
        }
        if let Some(on) = self.view.check_box(cx, ids!(head.mic.computer)).changed(&actions) {
            if on {
                let local = matches!(event, Event::MouseUp(_) | Event::TouchUpdate(_) | Event::KeyDown(_)) && cx.injected_by().is_none();
                cx.action(FirmwareMic::ComputerOn(ComputerMicClick { local }));
            } else {
                cx.action(FirmwareMic::ComputerOff);
            }
        }
        if self.view.button(cx, ids!(head.fold)).clicked(&actions) {
            cx.action(FirmwareFold(false));
        }
        if self.view.button(cx, ids!(head.unfold)).clicked(&actions) {
            cx.action(FirmwareFold(true));
        }
        cx.extend_actions(actions);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.follow(cx);
        let height = cx.peek_walk_turtle(walk).size.y;
        if height > 0.0 && shows_folded(height) != self.folded {
            self.folded = shows_folded(height);
            self.sync_forms(cx);
            // Unfolded, the panel reads the newest state again.
            self.state_seen = None;
        }
        self.follow_state(cx);
        self.follow_source(cx);
        // The state panel's layout is decided before it draws, so no frame shows the other one.
        let pane = cx.peek_walk_turtle(walk).size;
        if let Some(two) = self.state_layout(cx, pane).filter(|&two| two != self.state_two) {
            self.state_two = two;
            self.lay_out_state(cx, two);
        }
        let step = self.view.draw_walk(cx, scope, walk);
        if step.is_done() && !self.folded {
            // The panel's surroundings, from this draw (before a draw, the last one's areas no
            // longer read). The first time they are known, the layout follows on the next frame.
            let shown = self.view.widget(cx, ids!(state)).area().rect(cx).size.y;
            if shown > 0.0 {
                let first = self.state_chrome.is_none();
                self.state_chrome = Some(pane.y - shown);
                if first && self.state_layout(cx, pane).is_some_and(|two| two != self.state_two) {
                    self.view.redraw(cx);
                }
            }
        }
        step
    }
}
