//! Stage's asset store: Stage hosts its own library in-process.
//!
//! One asset library per app: Stage's is the checkout's
//! `local/asset-library/store` (`paths::app_store_root("stage")`, the library
//! it always had). The decision is `makepad_app_asset_server::embed`'s,
//! shared with the workbench window: Stage hosts that store on loopback,
//! unless the SAME store is already served — by another Stage (the store's
//! lock admits one host, so of two Stages starting together one hosts and
//! the other attaches) or by `makepad-asset-server --app stage`. A server of
//! any other store (Sandbox's, a scratch one) is never joined.
//! `STAGE_ASSET_EMBED=never` keeps Stage attach-only, for debugging against
//! an out-of-process server.
//!
//! Before hosting a store whose catalog predates this build's schema, the
//! host copies it aside once (`<store>.backup-v<old>-<secs>`).
//!
//! Attached, Stage keeps its library when the host goes away: it waits for
//! the store's lock (`embed::resolve_when_freed_notify`) and, the moment the
//! host quits, hosts the store itself (or attaches to whichever other Stage
//! claimed it first), publishes the new decision, and the libraries ask
//! again. No restart, no Refresh.
//!
//! Resolution may wait on a loopback health probe (up to 400 ms, for a
//! `listen` port that no longer answers), so it runs off the UI thread (`embed::resolve_async`; a takeover's
//! `embed::resolve_when_freed_notify`); this handle
//! then keeps the hosted store alive until Stage stops. The first decision is
//! published by a thread of its own ([`decide`]): a worker that needs the
//! store (an agent's tool call at start) waits for it there, never for the UI.

use makepad_app_asset_server::embed::{self, LocalStore, ResolveHandle, Resolved};
use makepad_widgets::*;
use std::sync::mpsc::{Receiver, TryRecvError};

pub struct AssetHost {
    /// The first decision, published already, until the UI takes it.
    first: Option<Receiver<Resolved>>,
    /// A later one (a takeover), published when the UI takes it.
    resolving: Option<ResolveHandle>,
    store: Option<LocalStore>,
    /// An explicit server was named, or embedding is off: attach only, and
    /// never take a store over.
    attach_only: bool,
}

impl AssetHost {
    pub fn start() -> Self {
        let pinned = ["STAGE_ASSET_SERVER", "VJ_ASSET_SERVER"]
            .iter()
            .any(|key| std::env::var(key).is_ok_and(|value| !value.trim().is_empty()));
        // The resolver probes the store's own `listen` as well.
        let first = decide(embed::resolve_async("STAGE", "stage", pinned, None));
        let attach_only = pinned || embed::embed_policy("STAGE") == embed::EmbedPolicy::Never;
        Self { first: Some(first), resolving: None, store: None, attach_only }
    }

    /// Each decision, once, when it is made: the store is then either
    /// hosted here and listening, or served by someone else. The first comes
    /// at start; a later one when this Stage takes over a store whose host
    /// quit.
    pub fn take_resolved(&mut self) -> Option<String> {
        let resolved = match &self.first {
            Some(first) => match first.try_recv() {
                Ok(resolved) => resolved,
                Err(TryRecvError::Empty) => return None,
                Err(TryRecvError::Disconnected) => {
                    self.first = None;
                    return Some("no decision: the thread making it stopped without one".into());
                }
            },
            None => {
                let resolved = self.resolving.as_mut()?.poll()?;
                publish(&resolved);
                resolved
            }
        };
        self.first = None;
        self.resolving = None;
        // Attached: be ready to host the store the moment its host quits.
        if resolved.local.is_none() && !self.attach_only {
            self.resolving = Some(embed::resolve_when_freed_notify("STAGE", &resolved.root, SignalToUI::set_ui_signal));
        }
        self.store = resolved.local;
        Some(resolved.note)
    }

    /// Stop hosting, after everything that talks to the store has stopped.
    /// The host shuts down on its own thread, never holding up the UI; a
    /// resolution still in flight is dropped with its answer.
    pub fn stop(&mut self) {
        self.first = None;
        self.resolving = None;
        if let Some(store) = self.store.take() {
            let spawned = std::thread::Builder::new().name("stage-asset-host-stop".into()).spawn(move || drop(store));
            if let Err(error) = spawned {
                log!("asset store: could not stop it off the UI thread: {error}");
            }
        }
    }
}

/// Every worker's next connect goes where `resolved` says.
fn publish(resolved: &Resolved) {
    let hosted = resolved.local.as_ref().map(|store| (store.endpoints(), store.server_id(), store.token().to_string()));
    crate::publish_resolution(crate::Resolution { hosted, root: resolved.root.clone() });
}

/// The first decision, on a thread of its own: it waits for the resolver,
/// publishes the answer, then hands it to the UI and wakes it. Workers'
/// connects wait until this thread has ended, however it ends (a thread that
/// could not start ends the wait at once: the closure drops unrun).
fn decide(resolving: ResolveHandle) -> Receiver<Resolved> {
    let deciding = crate::Deciding::start();
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new().name("stage-asset-decide".into()).spawn(move || {
        let _deciding = deciding;
        if let Some(resolved) = resolving.wait() {
            publish(&resolved);
            // A host stopped meanwhile drops the answer here, and with it any
            // store hosted for nobody.
            let _ = tx.send(resolved);
        }
        SignalToUI::set_ui_signal();
    });
    if let Err(error) = spawned {
        log!("asset store: could not start the thread that decides where it is: {error}");
    }
    rx
}
