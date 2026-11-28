//! Backpressure for the panel's Sets. Only the newest unsent value matters.
use crate::tweak::RequestId;
use std::rc::Rc;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::tweak) enum Phase { Off, Step, End }

struct Property {
    path: Rc<str>,
    prop: String,
    phase: Phase,
    request: Option<RequestId>,
    last: Option<String>,
    waiting: Option<String>,
}

#[derive(Default)]
pub(in crate::tweak) struct LiveSets {
    properties: Vec<Property>,
}

impl LiveSets {
    pub(in crate::tweak) fn open_on(&self, path: &str) -> bool {
        self.properties.iter().any(|p| p.path.as_ref() == path && p.phase != Phase::Off)
    }

    pub(in crate::tweak) fn waiting(&self, path: &str, prop: &str) -> Option<&str> {
        self.properties.iter().find(|p| p.path.as_ref() == path && p.prop == prop)?.waiting.as_deref()
    }

    /// Both offer and reply report the path once its gesture ends. The sender
    /// returns None for a local refusal (there will be no later wire reply).
    pub(in crate::tweak) fn offer(&mut self, path: &Rc<str>, prop: &str, value: String, phase: Phase,
        send: impl FnOnce(&Rc<str>, &str, &str) -> Option<RequestId>) -> Option<Rc<str>> {
        let at = self.properties.iter().position(|p| p.path == *path && p.prop == prop).unwrap_or_else(|| {
            self.properties.push(Property {
                path: path.clone(), prop: prop.into(), phase, request: None, last: None, waiting: None,
            });
            self.properties.len() - 1
        });
        let p = &mut self.properties[at];
        p.phase = if phase == Phase::Off && p.phase != Phase::Off { Phase::End } else { phase };
        p.waiting = (p.last.as_ref() != Some(&value)).then_some(value);
        self.advance(at, send)
    }

    pub(in crate::tweak) fn reply(&mut self, req: RequestId,
        send: impl FnOnce(&Rc<str>, &str, &str) -> Option<RequestId>) -> Option<Rc<str>> {
        let at = self.properties.iter().position(|p| p.request == Some(req))?;
        self.properties[at].request = None;
        self.advance(at, send)
    }

    fn advance(&mut self, at: usize, send: impl FnOnce(&Rc<str>, &str, &str) -> Option<RequestId>) -> Option<Rc<str>> {
        let p = &mut self.properties[at];
        if p.request.is_some() { return None }
        if let Some(value) = p.waiting.take() {
            if let Some(req) = send(&p.path, &p.prop, &value) {
                p.request = Some(req);
                p.last = Some(value);
                return None
            }
        } else if p.phase == Phase::Step {
            return None // Caught up with the hand. Keep the last value until End.
        }
        let p = self.properties.swap_remove(at);
        (p.phase != Phase::Off).then_some(p.path)
    }
}
