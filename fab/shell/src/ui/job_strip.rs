//! The caption's job strip shows the newest long operation and opens the list of jobs.

use makepad_widgets_core::*;

/// The number of characters the caption shows before it shortens a job's line: enough for who
/// asked, the job and the part it is on ("AI: Rebuilding the plate · building mount · …").
pub const JOB_LINE_CHARS: usize = 50;

script_mod! {
    use mod.prelude.fab.*
    use mod.widgets.*

    // The job strip in Fab's colours: what runs, in words, over a thin bar,
    // and the list of every job below the caption.
    mod.widgets.FabJobStrip = mod.widgets.JobStrip{
        // A rebuild shows here only when it takes longer than a live drag step.
        show_after: 0.4
        pop +: {
            placement: BottomEnd
            line +: {
                draw_text +: {
                    color: fab.color_text
                    text_style: theme.font_regular{font_size: fab.font_size_small}
                }
                draw_bg +: {
                    color_track: fab.color_input
                    color_bar: fab.color_accent
                    color_fail: fab.color_error
                }
            }
            more +: {
                draw_text +: {
                    color: fab.color_text_dim
                    text_style: theme.font_regular{font_size: fab.font_size_small}
                }
            }
            cancel +: {
                draw_text +: {
                    color: fab.color_text_dim
                    text_style: theme.font_regular{font_size: fab.font_size_ui}
                }
            }
            content +: {
                list +: {
                    width: 360
                    Heading +: {
                        draw_text +: {
                            color: fab.color_text_dim
                            text_style: theme.font_regular{font_size: fab.font_size_small}
                        }
                    }
                    Row +: {
                        head +: {
                            title +: {
                                draw_text +: {
                                    color: fab.color_text
                                    text_style: theme.font_regular{font_size: fab.font_size_ui}
                                }
                            }
                            elapsed +: {
                                draw_text +: {
                                    color: fab.color_text_dim
                                    text_style: theme.font_regular{font_size: fab.font_size_small}
                                }
                            }
                        }
                        detail +: {
                            draw_text +: {
                                color: fab.color_text_dim
                                text_style: theme.font_regular{font_size: fab.font_size_small}
                            }
                        }
                        failure +: {
                            draw_text +: {
                                color: fab.color_error
                                text_style: theme.font_regular{font_size: fab.font_size_small}
                            }
                        }
                        bar +: {
                            draw_bg +: {
                                track_color: fab.color_input
                                color_primary: fab.color_accent
                            }
                        }
                    }
                }
            }
        }
    }
}
