use makepad_widgets::*;

script_mod! {
    use mod.prelude.workspace.*
    use mod.widgets.*

    mod.widgets.PairingForm = View{
        width: Fill height: Fit flow: Down spacing: 8
        // The two steps show only while they can be followed; otherwise one plain line says why.
        pair_steps := View{
            width: Fill height: Fit flow: Down spacing: 8
            View{
                width: Fill height: Fit flow: Right{wrap: true} spacing: 7 align: Align{y: 0.5}
                BodyNote{width: Fit text: "1  Send these words"}
                pair_copy := PanelButton{visible: false text: "Copy"}
            }
            pair_words := BodyNote{width: Fill}
            BodyNote{text: "2  Type the words you get back"}
            connect_peer_words := PanelInput{
                is_multiline: true empty_text: "" height: 66 align: Align{x: 0 y: 0}
                a11y: {label: "Type the words you get back"}
            }
        }
        pair_retry := PanelButton{visible: false text: "Try again"}
        pair_status := BodyNote{width: Fill visible: false}
    }
    mod.widgets.PairingPrompt = View{
        width: Fill height: Fit flow: Down spacing: 8
        offer_line := BodyNote{width: Fill}
        View{
            width: Fill height: Fit flow: Right{wrap: true} spacing: 7
            share_join := PanelButton{text: "Add it here"}
            share_decline := PanelButton{text: "Don't add it"}
        }
    }
    let Details = FoldHeader{
        width: Fill height: Fit
        animator +: {active: {default: @off}}
        header: View{
            width: Fill height: Fit flow: Overlay align: Align{y: 0.5}
            Kicker{margin: Inset{left: 16} text: "Details"}
            fold_button := FoldButton{width: Fill height: 24 animator +: {active: {default: @off}}}
        }
    }
    mod.widgets.PairStep = View{
        width: Fill height: Fit flow: Down spacing: 7
        View{
            width: Fill height: Fit flow: Right spacing: 7
            pair_title := ProjectHeading{width: Fill}
            pair_close := PanelButton{text: "×" a11y: {label: "Close pairing"}}
        }
        connect_devices := View{
            visible: false width: Fill height: Fit flow: Down spacing: 7
            BodyNote{text: "Town couldn’t read its list of known devices." draw_text.color: palette.warn}
            Details{body: View{width: Fill height: Fit connect_devices_detail := BodyNote{}}}
            connect_devices_actions := View{
                width: Fill height: Fit flow: Right{wrap: true} spacing: 7
                connect_devices_retry := PanelButton{text: "Try again"}
                connect_devices_forget := PanelButton{text: "Forget known devices…"}
            }
            connect_devices_confirmation := View{
                visible: false width: Fill height: Fit flow: Down spacing: 7
                BodyNote{text: "Keep the old list and start an empty one?"}
                View{
                    width: Fill height: Fit flow: Right{wrap: true} spacing: 7
                    connect_devices_confirm := PanelButton{text: "Forget them"}
                    connect_devices_cancel := PanelButton{text: "Cancel"}
                }
            }
        }
        connect_form := PairingForm{}
    }
    // Joining starts a new town, so its page holds the two steps and nothing of the open town.
    mod.widgets.JoinPage = ScrollYView{
        visible: false width: Fill height: Fill flow: Down
        padding: Inset{left: 24 right: 24 top: 22 bottom: 8}
        PairStep{}
    }
    mod.widgets.AddAiStep = View{
        width: Fill height: Fit flow: Down spacing: 7
        View{
            width: Fill height: Fit flow: Right spacing: 7
            ai_preview := BodyNote{width: Fill}
            ai_status := BodyNote{visible: false width: Fill}
            ai_show := PanelButton{text: "Show" a11y_name: "Show the join code"}
            ai_copy := PanelButton{text: "Copy"}
            ai_close := PanelButton{text: "×" a11y: {label: "Close AI invitation"}}
        }
        ai_commands := BodyNote{text: "Paste it into Claude Code. It appears here when it joins."}
        ai_retry := PanelButton{visible: false text: "Try again"}
    }
}

use crate::link_worker::{
    ConnectView, Status, BAD_WORDS, BLOCKED_NETWORKS, NOT_REACHED_SECS, PAIR_PEER_OLDER, PAIR_UPDATING,
    SAME_NETWORK_LAN_OFF_HERE, SAME_NETWORK_NO_LOCAL_ADDRESS, SAME_NETWORK_PEER_LAN_OFF, START_WAIT_SECS,
    VERSION_TEXT, WORDS_OUT_OF_DATE,
};
use crate::model::PairSide;

const FINDING: &str = "Finding this computer's address…";
const RENEWED: &str = "These words are new. Send them again.";
pub const PAIR_FAILED: &str = "Couldn't finish pairing.";
pub const DECLINED: &str = "The other computer declined the offer.";
pub const ADDRESS_FAILED: &str = "Couldn't find this computer's address.";

// A diagnostic can contain a path or a peer's text. Keep it in the log.
pub fn pair_failure(why: &str) -> &'static str {
    match why {
        BAD_WORDS => BAD_WORDS,
        VERSION_TEXT => VERSION_TEXT,
        PAIR_UPDATING => PAIR_UPDATING,
        PAIR_PEER_OLDER => PAIR_PEER_OLDER,
        BLOCKED_NETWORKS => BLOCKED_NETWORKS,
        SAME_NETWORK_LAN_OFF_HERE => SAME_NETWORK_LAN_OFF_HERE,
        SAME_NETWORK_PEER_LAN_OFF => SAME_NETWORK_PEER_LAN_OFF,
        SAME_NETWORK_NO_LOCAL_ADDRESS => SAME_NETWORK_NO_LOCAL_ADDRESS,
        WORDS_OUT_OF_DATE => WORDS_OUT_OF_DATE,
        crate::model::THAT_CANNOT_REJOIN => crate::model::THAT_CANNOT_REJOIN,
        crate::model::DIFFERENT_INVITATION => crate::model::DIFFERENT_INVITATION,
        crate::model::ALREADY_IN_TOWN => crate::model::ALREADY_IN_TOWN,
        ADDRESS_FAILED => ADDRESS_FAILED,
        DECLINED => DECLINED,
        _ => PAIR_FAILED,
    }
}

fn label<'a>(name: &'a str, fallback: &'a str) -> &'a str {
    let name = name.trim();
    if name.is_empty() || name.contains(['/', '\\']) || name.chars().any(char::is_control) { fallback } else { name }
}

pub fn offer_line(peer: &str, town: Option<&str>) -> String {
    let peer = label(peer, "Another computer");
    let town = town.map(|s| label(s, "")).filter(|s| !s.is_empty());
    match town {
        Some(town) => format!("{peer} wants to add \"{town}\" to this computer."),
        None => format!("{peer} wants to add a town to this computer."),
    }
}

impl ConnectView {
    /// The worker can hold only one first contact. Reopening it must keep its words.
    pub fn can_open_pairing(&self, side: PairSide) -> bool {
        !self.first_contact || self.failed.is_some() || self.side.is_none() || self.side == Some(side)
    }

    /// Keep the sharing handshake alive after proof. Its words are finished as soon as the link is up.
    pub fn shows_pairing(&self, side: PairSide) -> bool {
        self.side == Some(side) && (!self.first_contact || self.linked.is_none() || self.failed.is_some())
    }

    pub fn wait_status(&self, now: u64) -> String {
        if let Some(why) = self.blocked { return why.into() }
        match self.status {
            Some(Status::Reaching { secs }) if secs >= NOT_REACHED_SECS => format!(
                "Haven't reached the other computer yet · {secs} s\nStill trying. Check the words on both sides."
            ),
            Some(Status::Reaching { secs }) => format!("Waiting for the other computer… {secs} s"),
            Some(Status::Securing | Status::Proving) => "Connecting to the other computer…".into(),
            _ if now.saturating_sub(self.waiting_since.unwrap_or(now)) >= START_WAIT_SECS =>
                "Connection hasn't started yet.\nClose pairing and try again.".into(),
            _ => "Starting connection…".into(),
        }
    }

    pub fn show_form(&self, cx: &mut Cx, form: &WidgetRef, town: Option<&str>, unavailable: Option<&str>, now: u64) {
        let title = match self.side {
            Some(PairSide::Add(_, _)) => format!("Add a computer to {}", label(town.unwrap_or(""), "this town")),
            Some(PairSide::Join) => "Join a town on another computer".into(),
            None => return,
        };
        form.label(cx, ids!(pair_title)).set_text(cx, &title);
        form.widget(cx, ids!(pair_steps)).set_visible(cx, unavailable.is_none() && self.failed.is_none());
        form.button(cx, ids!(pair_retry)).set_visible(cx, unavailable.is_none() && self.failed.is_some());
        let words = self.own_words.as_deref().filter(|_| self.failed.is_none() && unavailable.is_none());
        form.button(cx, ids!(pair_copy)).set_visible(cx, words.is_some());
        let placeholder = if self.failed.is_some() || unavailable.is_some() { "" } else { FINDING };
        form.label(cx, ids!(pair_words)).set_text(cx, words.unwrap_or(placeholder));
        form.text_input(cx, ids!(connect_peer_words)).set_is_read_only(cx, self.waiting_since.is_some() || self.failed.is_some() || unavailable.is_some());
        let line = if let Some(why) = unavailable { why.into() }
            else if let Some(why) = &self.failed { pair_failure(why).into() }
            else if self.waiting_since.is_some() { self.wait_status(now) }
            else if let Some(why) = &self.bad_words { pair_failure(why).into() }
            else if self.renewed && words.is_some() { RENEWED.into() }
            else { String::new() };
        let status = form.label(cx, ids!(pair_status));
        let line = match line.as_str() {
            PAIR_UPDATING | PAIR_PEER_OLDER => line.replacen("; ", ";\n", 1),
            _ => line,
        };
        status.set_visible(cx, !line.is_empty());
        status.set_text(cx, &line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Walk the actual widget trees after each transition, including static instructions.
    fn check_copy(id: LiveId, widget: &WidgetRef) {
        if !widget.visible() { return }
        let text = widget.text();
        for line in text.lines() {
            if id != id!(pair_words) { assert!(line.chars().count() <= 60, "long line: {line}"); }
            assert!(!line.contains("MAKEPAD_"), "environment variable in main flow: {line}");
            assert!(!line.contains(['/', '\\']), "path in main flow: {line}");
        }
        if widget.borrow::<Button>().is_some() {
            assert!(!["ok", "join", "not now", "start", "connect", "yes", "no"].contains(&text.to_lowercase().as_str()));
            assert!(["Copy", "×", "Try again", "Add it here", "Don't add it"].contains(&text.as_str()),
                "button needs an action: {text}");
        }
        let mut buttons = 0;
        widget.children(&mut |id, child| {
            buttons += usize::from(child.visible() && child.borrow::<Button>().is_some());
            check_copy(id, &child);
        });
        assert!(buttons <= 2, "more than two buttons in a row");
    }

    #[test]
    fn pairing_form_and_prompt_copy_follow_the_actual_controls() {
        use crate::link_worker::{FromWorker, PairWords, LOCAL_NETWORK_OFF};
        use makepad_network::p2p::{discover::Hint, words::Invite};
        let mut pool = test_cx::checkout_test_cx(|vm| {
            makepad_widgets::script_mod(vm);
            crate::script_mod(vm);
        });
        let cx = &mut *pool;
        let (form, prompt) = cx.with_vm(|vm| {
            let form = script_eval!(vm, { use mod.widgets.* PairStep{} });
            let prompt = script_eval!(vm, { use mod.widgets.* PairingPrompt{} });
            (WidgetRef::script_from_value(vm, form), WidgetRef::script_from_value(vm, prompt))
        });
        let mut view = ConnectView::default();
        let homes = crate::model::tests::ShareHomes::new("pair-copy");
        let (mut model, _) = crate::model::Model::open(&homes.0).unwrap();
        view.side = Some(PairSide::Add(model.open_id(), crate::model::AddComputer::SomeoneElse));
        for (name, title, offer) in [
            (None, "Add a computer to Center", "River wants to add \"Center\" to this computer."),
            (Some("Studio"), "Add a computer to Studio", "River wants to add \"Studio\" to this computer."),
        ] {
            if let Some(name) = name { model.rename_town(name).unwrap(); }
            let town = model.town_tree();
            view.show_form(cx, &form, town.name.as_deref(), None, 15);
            check_copy(id!(connect_form), &form);
            assert_eq!(form.label(cx, ids!(pair_title)).text(), title);
            let line = offer_line("River", model.town_name(model.open_id()));
            prompt.label(cx, ids!(offer_line)).set_text(cx, &line);
            check_copy(id!(share_incoming), &prompt);
            assert_eq!(line, offer);
        }
        view.show_form(cx, &form, None, None, 15);
        assert_eq!(form.label(cx, ids!(pair_title)).text(), "Add a computer to this town", "an unnamed town");
        assert_eq!(offer_line("River", None), "River wants to add a town to this computer.");
        let long_name = "界".repeat(75);
        view.show_form(cx, &form, Some(&long_name), None, 15);
        assert_eq!(form.label(cx, ids!(pair_title)).text(), format!("Add a computer to {long_name}"));
        let show = |cx: &mut Cx, v: &ConnectView, reason| {
            v.show_form(cx, &form, Some("Studio"), reason, 15);
            check_copy(id!(connect_form), &form);
            form.label(cx, ids!(pair_status)).text()
        };
        show(cx, &view, None);
        assert!(form.widget(cx, ids!(pair_close)).visible(), "the panel can be closed while finding words");
        assert_eq!(form.label(cx, ids!(pair_words)).text(), FINDING);
        assert!(!form.widget(cx, ids!(pair_copy)).visible());
        let invite = Invite { nat: Hint::Easy, addr: "192.0.2.4:4242".parse().unwrap(), lan: None, commit: 17 };
        view.apply(&FromWorker::Invite { words: invite.to_words() });
        view.side = Some(PairSide::Join);
        show(cx, &view, None);
        assert!(form.widget(cx, ids!(pair_copy)).visible());
        let shown = form.label(cx, ids!(pair_words)).text();
        assert_eq!(Some(shown.as_str()), view.own_words.as_deref(), "display preserves the complete grouped words");
        let mut node = a11y_tree::Node::default();
        form.label(cx, ids!(pair_words)).borrow().unwrap().semantics(cx, &mut node);
        assert_eq!(Some(node.name.as_str()), view.own_words.as_deref(), "the accessible name contains every pairing word");
        assert!(a11y_tree::render::node_line(&node).contains(&format!("\"{shown}\"")), "ui find lists the complete pairing words");
        assert_eq!(PairWords::decode(&shown).unwrap(), invite);
        assert_eq!(PairWords::decode(&invite.to_words().join("_,.!").to_uppercase()).unwrap(), invite);
        let mut wrong = invite.to_words();
        let corrupt = makepad_network::p2p::words::list().iter().find_map(|word| {
            *wrong.last_mut()? = word;
            let draft = wrong.join(" ");
            matches!(Invite::from_words(&draft), Err(makepad_network::p2p::words::WordsError::Checksum)).then_some(draft)
        }).unwrap();
        view.edit_words(&corrupt);
        assert_eq!(show(cx, &view, None), BAD_WORDS);
        assert!(!form.text_input(cx, ids!(connect_peer_words)).is_read_only(), "incorrect words can be corrected in place");
        assert!(view.waiting_since.is_none(), "a wrong checksum never starts a wait");
        view.bad_words = None;
        view.waiting_since = Some(3);
        view.apply(&FromWorker::Phase(Status::Reaching { secs: 12 }));
        assert_eq!(show(cx, &view, None), "Waiting for the other computer… 12 s");
        assert!(form.text_input(cx, ids!(connect_peer_words)).is_read_only(), "the submitted words stay fixed during pairing");
        view.blocked = Some(LOCAL_NETWORK_OFF);
        assert_eq!(show(cx, &view, None), LOCAL_NETWORK_OFF);
        view.apply(&FromWorker::WordsChanged);
        view.apply(&FromWorker::Invite { words: invite.to_words() });
        assert_eq!(show(cx, &view, None), RENEWED);
        assert!(form.widget(cx, ids!(pair_steps)).visible(), "the steps show while they can be followed");
        for why in ["This town is read-only now.", "This town is unavailable."] {
            assert_eq!(show(cx, &view, Some(why)), why);
            assert!(!form.widget(cx, ids!(pair_steps)).visible() && !form.widget(cx, ids!(pair_retry)).visible(),
                "an unavailable town hides the words and retry");
        }
        for why in [BLOCKED_NETWORKS, VERSION_TEXT, PAIR_PEER_OLDER, PAIR_UPDATING, crate::model::THAT_CANNOT_REJOIN, crate::model::DIFFERENT_INVITATION,
            crate::model::ALREADY_IN_TOWN, DECLINED, ADDRESS_FAILED, PAIR_FAILED] {
            assert_eq!(pair_failure(why), why);
            view.failed = Some(why.into());
            let shown = show(cx, &view, None);
            if matches!(why, PAIR_PEER_OLDER | PAIR_UPDATING) {
                assert_eq!(shown.replace('\n', " "), why, "the form wraps the complete version message");
            } else { assert_eq!(shown, why); }
            assert!(form.button(cx, ids!(pair_retry)).visible(), "a failed pairing can be retried");
            assert!(!form.widget(cx, ids!(pair_steps)).visible(), "a failure shows its line, never an empty step");
        }
        assert_eq!(pair_failure("error at private/file"), PAIR_FAILED);
        let text = offer_line("private/file", Some("private/file"));
        prompt.label(cx, ids!(offer_line)).set_text(cx, &text);
        check_copy(id!(share_incoming), &prompt);
        prompt.label(cx, ids!(offer_line)).set_text(cx, &offer_line(&long_name, Some(&long_name)));
        assert_eq!(prompt.label(cx, ids!(offer_line)).text(), format!("{long_name} wants to add \"{long_name}\" to this computer."));
    }

    #[test]
    fn pairing_form_copy_covers_starting_dialing_not_reached_and_lan_off_states() {
        use crate::link_worker::{FromWorker, WordsRejected, LOCAL_NETWORK_OFF};
        let mut pool = test_cx::checkout_test_cx(|vm| {
            makepad_widgets::script_mod(vm);
            crate::script_mod(vm);
        });
        let cx = &mut *pool;
        let form = cx.with_vm(|vm| {
            let value = script_eval!(vm, { use mod.widgets.* PairStep{} });
            WidgetRef::script_from_value(vm, value)
        });
        let mut view = ConnectView { side: Some(PairSide::Join), first_contact: true, ..Default::default() };
        let mut check = |v: &ConnectView, now, text: &str, retry| {
            v.show_form(cx, &form, None, None, now);
            check_copy(id!(connect_form), &form);
            assert_eq!(form.label(cx, ids!(pair_status)).text(), text);
            assert!(form.widget(cx, ids!(pair_status)).visible());
            assert_eq!(form.widget(cx, ids!(pair_retry)).visible(), retry);
            assert_ne!(form.widget(cx, ids!(pair_steps)).visible(), retry);
        };
        view.apply(&FromWorker::Phase(Status::WaitingForWords));
        view.waiting_since = Some(11);
        check(&view, 15, "Starting connection…", false);
        view.waiting_since = Some(10);
        check(&view, 15, "Connection hasn't started yet.\nClose pairing and try again.", false);
        for (secs, text) in [
            (0, "Waiting for the other computer… 0 s"),
            (14, "Waiting for the other computer… 14 s"),
            (15, "Haven't reached the other computer yet · 15 s\nStill trying. Check the words on both sides."),
            (42, "Haven't reached the other computer yet · 42 s\nStill trying. Check the words on both sides."),
        ] {
            view.apply(&FromWorker::Phase(Status::Reaching { secs }));
            view.apply(&FromWorker::WordsRejected(WordsRejected::AlreadyPairing));
            check(&view, 109, text, false);
        }
        for why in [LOCAL_NETWORK_OFF, SAME_NETWORK_LAN_OFF_HERE, SAME_NETWORK_PEER_LAN_OFF, SAME_NETWORK_NO_LOCAL_ADDRESS] {
            view.apply(&FromWorker::Blocked(why));
            check(&view, 109, why, false);
        }
        for status in [Status::Securing, Status::Proving] {
            view.apply(&FromWorker::Blocked(SAME_NETWORK_PEER_LAN_OFF));
            view.apply(&FromWorker::Phase(status));
            assert!(view.blocked.is_none());
            check(&view, 109, "Connecting to the other computer…", false);
        }
        for reason in [WordsRejected::StaleAttempt, WordsRejected::OlderInvite, WordsRejected::NotReady] {
            view.waiting_since = Some(10);
            view.apply(&FromWorker::Phase(Status::Reaching { secs: 0 }));
            view.apply(&FromWorker::Blocked(SAME_NETWORK_LAN_OFF_HERE));
            view.apply(&FromWorker::WordsRejected(reason));
            assert!(view.waiting_since.is_none() && view.blocked.is_none());
            check(&view, 109, WORDS_OUT_OF_DATE, true);
        }
    }

    #[test]
    fn the_caption_contains_only_the_window_title() {
        let mut pool = test_cx::checkout_test_cx(|vm| {
            makepad_widgets::script_mod(vm);
            crate::script_mod(vm);
        });
        let cx = &mut *pool;
        let caption = cx.with_vm(|vm| {
            let value = script_eval!(vm, { use mod.widgets.* WorkCaptionTitle{} });
            WidgetRef::script_from_value(vm, value)
        });
        let mut visible = Vec::new();
        caption.children(&mut |id, child| {
            if child.visible() && id != id!(caption_window_gap) { visible.push((id, child.text())); }
        });
        assert_eq!(visible, [(id!(caption_title), "Makepad Town".to_owned())]);
        use crate::{lists::{Page, ViewData}, model::tree::{TownTree, TreeProject}};
        let project = [2; 32];
        let mut data = ViewData::default();
        data.tree.set(Vec::new(), TownTree {
            town: Some([1; 32]), label: "You and Studio".into(),
            projects: vec![TreeProject { id: project, title: "Drawings".into(), tasks: 0, rows: vec![] }],
            ..TownTree::default()
        });
        data.workspace.current = Some(project);
        for (page, title) in [(Page::Home, "You and Studio"), (Page::Project, "Drawings"),
            (Page::Board, "Drawings"), (Page::Home, "You and Studio")] {
            data.page = page;
            data.show_caption(cx, &caption);
            assert_eq!(caption.label(cx, ids!(caption_title)).text(), format!("Makepad Town · {title}"));
        }
        data.page = Page::Join;
        data.show_caption(cx, &caption);
        assert_eq!(caption.label(cx, ids!(caption_title)).text(), "Makepad Town", "joining another town names none");
    }
}
