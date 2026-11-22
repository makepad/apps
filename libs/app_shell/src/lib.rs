//! The product shell: what every product window shares (Stage, Story).
//!
//! * [`chat`] — the window's one chat: the panel's plumbing over the product's tools.
//! * [`pages`] — the window's pages (built once, one per pump after the first frame) and its header tabs.
//! * [`user_state`] — where the person's own work is kept between launches.
//! * [`theme_choice`] — the house theme or a catalogue theme, kept.
//! * [`output_window`] — the output window: a picture the product renders, on a second screen.
//! * [`pump_pace`] — the pump's clock (30 Hz while busy, a 2 Hz idle check).
//! * [`lazy_component`] — a component started on first use and closed with the window.
//! * [`tool_host`] — the tool host core: the product's tools for the chat, agents and remote clients, and
//!   the [`tool_host::Documents`] a product's document tools act on.

pub mod chat;
pub mod lazy_component;
pub mod output_window;
pub mod pages;
pub mod pump_pace;
pub mod theme_choice;
pub mod tool_host;
pub mod user_state;
