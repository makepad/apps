//! The twin's firmware network on this machine (main 11-19, `fab::firmware_net`). A bench names
//! the servers its firmware's connections reach; the twin plays a fake for each unless the person
//! turned the real network on for the project, and then only the hosts named when they did.
//!
//! - main's side ([`ElecNet`]): the Firmware pane's switch and tokens change the settings, which
//!   reach the runner with the project on the bed; the runner's note of a real connection is the
//!   project's last use.
//! - the runner's side ([`FwNet`]): what a run reads when it starts (the maker's [`OpenNetwork`]),
//!   the key that restarts the run when what it would reach changes, and the pane's Network row.
//! - A hidden window (`MAKEPAD_HIDE_WINDOWS`) never reaches the real network: an agent's or a
//!   test's instance plays every connection. Its switch still works and still restarts the run,
//!   so it differs from a visible window only in the socket it never opens.

use crate::elec_host::ElecHost;
use fab::api::AppState;
use fab::firmware_net::{self, FirmwareNet, Secret, TokenEntry};
use fab::firmware_transport::{self, Transport};
use fab_elec_bench::net::{NetEndpoint, Real};
use fab_elec_fw::OpenNetwork;
use fab_elec_mcu_cosim::net::{Arrival, Network, Request};
use fab_elec_mcu_cosim::Upstream;
use fab_elec_view::firmware::{FirmwareNetSwitch, FirmwareNetToken};
use fab_elec_view::live::sim::{NetServerLive, NetworkLive, TokenLive};
use makepad_widgets::makepad_platform::thread::ThreadOptions;
use makepad_widgets::{log, Cx, Event};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

/// What the twin's network depends on outside the image: the project on the bed and this
/// machine's choice for it.
#[derive(Clone, Debug, PartialEq)]
pub struct NetContext {
    /// The project's file, its key in the settings (None: a plate not saved yet, or a demo).
    pub project: Option<String>,
    pub net: FirmwareNet,
    /// The saved tokens' names and hosts, or why they cannot be read.
    pub tokens: Result<Vec<TokenEntry>, String>,
    /// Counts the person's token saves and forgets: a run that reaches the real network starts
    /// again with the token as it now stands.
    pub tokens_rev: u64,
}

impl Default for NetContext {
    fn default() -> Self {
        NetContext { project: None, net: FirmwareNet::default(), tokens: Ok(Vec::new()), tokens_rev: 0 }
    }
}

/// The runner's side of the network.
pub struct FwNet {
    ctx: NetContext,
    /// Not a hidden window: this instance may reach the real network.
    visible: bool,
    /// What a run's network reads when the run starts (on the simulation's thread).
    shared: Arc<Mutex<NetContext>>,
    /// When the twin last opened a real connection (Unix seconds; 0: not yet).
    used: Arc<AtomicI64>,
    used_seen: i64,
    open: Option<OpenNetwork>,
}

impl Default for FwNet {
    fn default() -> Self {
        FwNet {
            ctx: NetContext::default(), visible: std::env::var_os("MAKEPAD_HIDE_WINDOWS").is_none(),
            shared: Arc::default(), used: Arc::default(), used_seen: 0, open: None,
        }
    }
}

impl FwNet {
    /// The choice as it stands; true when it changed.
    pub fn set(&mut self, ctx: NetContext) -> bool {
        if ctx == self.ctx {
            return false;
        }
        *self.shared.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = ctx.clone();
        self.ctx = ctx;
        true
    }

    /// The key of what a run of these bench connections would reach (0: nothing real): a run whose
    /// key changes starts again. A hidden window's runs are keyed alike, though they reach nothing.
    pub fn key(&self, endpoints: &[NetEndpoint]) -> u128 {
        let dials = dials(&self.ctx, true, endpoints);
        if dials.is_empty() {
            return 0;
        }
        let mut h = std::hash::DefaultHasher::new();
        (&self.ctx.project, self.ctx.tokens_rev, self.visible).hash(&mut h);
        for dial in &dials {
            (dial.c, &dial.endpoint, match &dial.real { Real::Server(server) => server.url(), Real::Hub { .. } => dial.real.shown() }).hash(&mut h);
        }
        u128::from(h.finish()) | 1 << 64
    }

    /// The maker's network: the choice read when each run starts, a transport for the connections
    /// it allows. One for the runner's life.
    pub fn open(&mut self, cx: &mut Cx) -> OpenNetwork {
        if let Some(open) = &self.open {
            return open.clone();
        }
        let spawner = cx.thread_spawner();
        // each connection's worker lives as long as its connection, fed through its channel
        let spawn: firmware_transport::Spawn = Arc::new(move |job| {
            let options = ThreadOptions { name: Some("firmware network".into()), ..ThreadOptions::default() };
            match spawner.spawn_worker(options, job) {
                Ok(task) => task.detach(),
                Err(error) => log!("firmware network: a connection's worker did not start ({error:?}); it stays unanswered"),
            }
        });
        let say: firmware_transport::Log = Arc::new(|line| log!("{line}"));
        let (shared, used, visible) = (self.shared.clone(), self.used.clone(), self.visible);
        let open: OpenNetwork = Arc::new(move |endpoints: &[NetEndpoint]| {
            let ctx = shared.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
            let dials = dials(&ctx, visible, endpoints);
            if dials.is_empty() {
                return None;
            }
            let upstream = dials.iter().map(|dial| dial.c).collect();
            let transport = Transport::new(dials, firmware_transport::settings_tokens(), firmware_transport::checked_connect(),
                firmware_transport::machine_hubs(), say.clone(), spawn.clone());
            Some((upstream, Box::new(Noted { inner: transport, used: used.clone() }) as Upstream))
        });
        self.open = Some(open.clone());
        open
    }

    /// When the twin last opened a real connection, once per new time.
    pub fn take_used(&mut self) -> Option<i64> {
        let at = self.used.load(Ordering::Relaxed);
        (at > self.used_seen).then(|| {
            self.used_seen = at;
            at
        })
    }

    /// The Firmware pane's Network row for these bench connections (None: they name no real server).
    pub fn live(&self, endpoints: &[NetEndpoint]) -> Option<NetworkLive> {
        let servers: Vec<_> = endpoints.iter().filter_map(|e| Some((e, e.real.as_ref()?))).collect();
        if servers.is_empty() {
            return None;
        }
        let project = self.ctx.project.as_deref();
        let entry = project.and_then(|p| self.ctx.net.get(p));
        let held = if !self.visible {
            Some("this window is hidden".to_string())
        } else if project.is_none() {
            Some("the choice is kept per project file: save the plate first".to_string())
        } else {
            None
        };
        Some(NetworkLive {
            servers: servers.iter().map(|&(e, real)| NetServerLive {
                c: e.c,
                name: e.name.clone(),
                host: real.approval(),
                address: match real { Real::Server(server) => server.url(), Real::Hub { .. } => real.shown() },
                approved: project.is_some_and(|p| self.ctx.net.allowed(p, &real.approval())),
                token: match (&e.name, &self.ctx.tokens) {
                    (None, _) => TokenLive::NoName,
                    (Some(_), Err(why)) => TokenLive::Unreadable(why.clone()),
                    (Some(name), Ok(saved)) => match saved.iter().find(|t| &t.name == name) {
                        Some(t) if t.host.eq_ignore_ascii_case(&real.approval()) => TokenLive::Saved,
                        Some(t) => TokenLive::OtherHost(t.host.clone()),
                        None => TokenLive::Missing,
                    },
                },
            }).collect(),
            switchable: project.is_some(),
            on: entry.is_some_and(|p| p.on),
            since: entry.and_then(|p| p.accepted).map(fab::model_sources::date),
            last_used: entry.and_then(|p| p.last_used).map(fab::model_sources::date),
            held,
        })
    }
}

/// The connections a run may open on the real network now.
fn dials(ctx: &NetContext, visible: bool, endpoints: &[NetEndpoint]) -> Vec<firmware_transport::Dial> {
    match (&ctx.project, visible) {
        (Some(project), true) => firmware_transport::dials(&ctx.net, project, endpoints),
        _ => Vec::new(),
    }
}

/// The transport, noting when the twin opens a real connection.
struct Noted {
    inner: Transport,
    used: Arc<AtomicI64>,
}

impl Network for Noted {
    fn send(&mut self, request: &Request) {
        if matches!(request, Request::Open { .. }) {
            self.used.store(makepad_civil_time::now_secs(), Ordering::Relaxed);
        }
        self.inner.send(request);
    }
    fn close_all(&mut self) {
        self.inner.close_all();
    }
    fn answers(&mut self, out: &mut Vec<Arrival>) {
        self.inner.answers(out);
    }
}

/// main's side: the pane's actions on the settings, and the settings to the runner.
#[derive(Default)]
pub struct ElecNet {
    tokens_rev: u64,
    /// The project file, settings and token count last handed to the runner.
    sent: Option<(Option<PathBuf>, FirmwareNet, Result<Vec<TokenEntry>, String>, u64)>,
}

impl ElecNet {
    /// Once per event: the pane's actions change the settings, and a change of the project on the
    /// bed (`file`: where it is saved; None for a demo or a document not saved yet) or of its
    /// settings reaches the runner. Per event without either, one compare of the settings with
    /// those last sent.
    pub fn handle_event(&mut self, cx: &mut Cx, event: &Event, state: &mut AppState, file: Option<&Path>, elec: Option<&mut ElecHost>) {
        let project = || file.map(|file| file.to_string_lossy().into_owned());
        let Some(elec) = elec else {
            self.sent = None;
            return;
        };
        if let Event::Actions(actions) = event {
            for action in actions.iter() {
                if let Some(FirmwareNetSwitch(hosts)) = action.downcast_ref::<FirmwareNetSwitch>() {
                    let Some(project) = project() else { continue };
                    match hosts {
                        Some(hosts) => {
                            state.firmware_net.turn_on(&project, hosts, makepad_civil_time::now_secs());
                            log!("firmware network: on for {project}: {}", hosts.join(", "));
                        }
                        None => {
                            state.firmware_net.turn_off(&project);
                            log!("firmware network: off for {project}");
                        }
                    }
                    firmware_net::save(&state.firmware_net);
                }
                if let Some(token) = action.downcast_ref::<FirmwareNetToken>() {
                    let secret = token.token().map(|bytes| Secret::new(bytes.to_vec()));
                    let forget = secret.is_none();
                    match firmware_net::set_token(&token.name, &token.host, secret) {
                        Ok(()) if forget => log!("firmware network: the token for {} ({}) is forgotten", token.name, token.host),
                        Ok(()) => log!("firmware network: a token for {} ({}) is saved in this computer's Fab settings", token.name, token.host),
                        Err(why) => log!("firmware network: the token for {} was not saved: {why}", token.name),
                    }
                    state.firmware_tokens = firmware_net::tokens();
                    self.tokens_rev += 1;
                }
            }
        }
        if let Some(at) = elec.take_net_used() {
            if let Some(project) = project() {
                state.firmware_net.used(&project, at);
                firmware_net::save(&state.firmware_net);
            }
        }
        let same = self.sent.as_ref().is_some_and(|(sent_file, net, tokens, rev)| {
            sent_file.as_deref() == file && *net == state.firmware_net && *tokens == state.firmware_tokens && *rev == self.tokens_rev
        });
        if !same {
            let project = project();
            self.sent = Some((file.map(Path::to_path_buf), state.firmware_net.clone(), state.firmware_tokens.clone(), self.tokens_rev));
            elec.set_network(cx, NetContext {
                project, net: state.firmware_net.clone(), tokens: state.firmware_tokens.clone(), tokens_rev: self.tokens_rev,
            });
        }
    }
}

