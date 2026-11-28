use super::{AppKey, LocalApp};
use makepad_remote::{
    discovery::{self, App},
    link::Target,
};
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub(super) struct Refused(pub &'static str);

/// Run metadata describes the local process. Routes and test runs aren't selectable apps.
pub(super) fn local_endpoint(app: &App) -> Result<Target, Refused> {
    if app
        .fields
        .iter()
        .any(|(key, raw)| key == "test_run" && raw == "true")
    {
        return Err(Refused("test run"));
    }
    if app
        .fields
        .iter()
        .any(|(key, _)| matches!(key.as_str(), "via" | "relay" | "route" | "hub" | "host"))
        || app.serves("route")
        || app.serves("relay")
    {
        return Err(Refused("routed endpoint"));
    }
    if app.unreachable().is_some() || !app.serves("cmd") || !app.serves("frames") {
        return Err(Refused("missing local app services"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        let path = Path::new(&app.socket);
        let dir = makepad_remote::carrier::unix::socket_dir();
        if path.parent() != Some(dir.as_path())
            || !std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_socket())
        {
            return Err(Refused("outside the user's sockets"));
        }
        let _owned = makepad_remote::carrier::unix::own_socket_dir()
            .map_err(|_| Refused("socket directory ownership"))?;
        Ok(Target::Unix(path.to_owned()))
    }
    #[cfg(windows)]
    {
        windows_endpoint(app)
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err(Refused("no local carrier"))
    }
}

#[cfg(any(windows, all(test, not(target_arch = "wasm32"))))]
pub(super) fn windows_endpoint(app: &App) -> Result<Target, Refused> {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
    let addr: SocketAddr = app
        .socket
        .strip_prefix("tcp:")
        .and_then(|s| s.parse().ok())
        .ok_or(Refused("expected numeric loopback address"))?;
    if addr.port() == 0
        || !matches!(
            addr.ip(),
            IpAddr::V4(Ipv4Addr::LOCALHOST) | IpAddr::V6(Ipv6Addr::LOCALHOST)
        )
    {
        return Err(Refused("outside loopback"));
    }
    let token = app
        .text("token")
        .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or(Refused("missing registry token"))?;
    Ok(Target::Loopback(addr.to_string(), token))
}

pub(super) fn key(app: &App) -> AppKey {
    AppKey {
        pid: app.pid,
        started_ms: app.started_ms,
        socket: PathBuf::from(&app.socket),
        run_dir: app.text("run_dir").filter(|dir| !dir.is_empty()),
        restart_parent: app.restart_parent.filter(|pid| *pid != 0),
    }
}

impl LocalApp {
    fn could_replace(&self, old: &Self) -> bool {
        let (a, b) = (&self.key, &old.key);
        self.name == old.name
            && !self.name.is_empty()
            && a.pid != b.pid
            && a.socket != b.socket
            && a.started_ms >= b.started_ms
            && (a.run_dir.is_some() && a.run_dir == b.run_dir
                || a.restart_parent.is_some() && a.restart_parent == b.restart_parent
                || a.run_dir.is_none()
                    && b.run_dir.is_none()
                    && a.restart_parent.is_none()
                    && b.restart_parent.is_none())
    }
}

/// Remember other instances seen alongside the linked process. Their continued presence after
/// its exit cannot identify a restart, even when two instances share a name and a run directory.
#[derive(Default)]
pub(super) struct RestartWatch {
    alongside: Vec<AppKey>,
}
impl RestartWatch {
    pub fn replacement<'a>(
        &mut self,
        old: &LocalApp,
        ended: bool,
        apps: &'a [LocalApp],
    ) -> Option<&'a LocalApp> {
        let candidates = apps.iter().filter(|app| app.could_replace(old));
        if apps
            .iter()
            .any(|app| app.pid == old.pid && app.key.socket == old.key.socket)
        {
            if ended {
                // The departing process can still be visible during a replacement's startup.
                return None;
            }
            for sibling in candidates {
                if !self.alongside.contains(&sibling.key) {
                    self.alongside.push(sibling.key.clone());
                }
            }
        } else if ended {
            let mut unseen = candidates.filter(|app| !self.alongside.contains(&app.key));
            let one = unseen.next()?;
            if unseen.next().is_none() {
                return Some(one);
            }
        }
        None
    }
}

/// The apps Town can link, and each running app it cannot reach with the reason, for the picker.
pub(super) fn discover_all(registry: &Path, own: u32) -> (Vec<LocalApp>, Vec<(String, String)>) {
    let mut unreachable = Vec::new();
    let apps = discovery::apps_in(registry)
        .into_iter()
        .filter(|app| !app.answers_to(own))
        .filter_map(|app| {
            if let Err(why) = local_endpoint(&app) {
                makepad_widgets::log!("App link: skipped {}: {}", app.app, why.0);
                // Test runs, routed endpoints and service-only processes are not apps a person shares.
                if !matches!(why.0, "test run" | "routed endpoint" | "missing local app services") {
                    unreachable.push((app.title().unwrap_or_else(|| app.app.clone()), why.0.to_string()));
                }
                return None;
            }
            Some(LocalApp {
                key: key(&app),
                visible_windows: app.visible_windows().into_iter().map(|(id, title)| (super::WindowId(id), title)).collect(),
                title: app.title(),
                name: app.app,
                pid: app.pid,
                started_ms: app.started_ms,
            })
        })
        .collect();
    (apps, unreachable)
}

pub(super) fn resolve(registry: &Path, selected: &AppKey, own: u32) -> Result<Target, String> {
    let app = discovery::apps_in(registry)
        .into_iter()
        .find(|a| !a.answers_to(own) && key(a) == *selected)
        .ok_or("the selected app has ended")?;
    local_endpoint(&app).map_err(|why| why.0.to_string())
}
