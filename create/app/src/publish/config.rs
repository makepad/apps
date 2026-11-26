use super::{field, invalid};
use makepad_work_core::home::Home;
use makepad_strict_json::{parse, Value};
use std::{collections::HashSet, fs, io, path::Path};

#[derive(Clone, Debug)]
pub struct Target {
    pub name: String,
    pub ssh: String,
    pub dir: String,
    pub url_base: Option<String>,
}

impl Target {
    pub(super) fn validate(&self) -> io::Result<()> {
        let (user, alias) = self.ssh.split_once('@').map_or((None, self.ssh.as_str()), |(u, a)| (Some(u), a));
        let token = |s: &str| !s.is_empty() && !s.starts_with('-') && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-+".contains(&b));
        let ipv6 = alias.strip_prefix('[').and_then(|s| s.strip_suffix(']')).and_then(|s| s.parse::<std::net::Ipv6Addr>().ok()).is_some();
        if !user.is_none_or(token) || !(token(alias) || ipv6) {
            return Err(invalid("expected an ssh alias or user@alias"));
        }
        if self.dir.trim().is_empty() || self.dir.chars().any(char::is_control) || self.name.trim().is_empty() {
            return Err(invalid("a publish target needs a name and a single-line directory"));
        }
        Ok(())
    }
}

#[derive(Default, Debug)]
pub struct Config {
    pub targets: Vec<Target>,
}

impl Config {
    pub fn load_default() -> io::Result<Self> {
        let home = Home::default_dir().ok_or_else(|| invalid("cannot locate the local home"))?;
        Self::load(&home)
    }

    pub fn load(home: &Path) -> io::Result<Self> {
        let bytes = match fs::read(home.join("publish.json")) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            result => result?,
        };
        let read = || {
            let value = parse(&bytes).map_err(invalid)?;
            let rows = value.get("targets").and_then(Value::as_arr).ok_or_else(|| invalid("expected targets array"))?;
            let mut names = HashSet::new();
            let mut targets = Vec::new();
            for row in rows {
                let target = Target {
                    name: field(row, "name")?.into(),
                    ssh: field(row, "ssh")?.into(),
                    dir: field(row, "dir")?.into(),
                    url_base: row.get("url_base").map(|_| field(row, "url_base").map(str::to_owned)).transpose()?,
                };
                target.validate()?;
                if !names.insert(target.name.clone()) {
                    return Err(invalid("duplicate target name"));
                }
                targets.push(target);
            }
            Ok(Self { targets })
        };
        read().map_err(|e: io::Error| invalid(format!("publish.json: {e}")))
    }

    pub fn has_publish_action(&self) -> bool {
        !self.targets.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn publish_action_requires_a_configured_target() {
        let home = crate::publish::Scratch::new(&std::env::temp_dir()).unwrap();
        use crate::publish::{session::LocalPublish, sftp::Published, store::{test_image, Author, Store}};
        let file = home.0.join("publish.json");
        assert!(!Config::load(&home.0).unwrap().has_publish_action());
        fs::write(&file, r#"{"targets":[]}"#).unwrap();
        assert!(!Config::load(&home.0).unwrap().has_publish_action());
        fs::write(&file, r#"{"targets":[{"name":"Demo","ssh":"example.invalid","dir":"/posts"}]}"#).unwrap();
        assert!(Config::load(&home.0).unwrap().has_publish_action());
        let store = Store::new(&home.0);
        let entry = store.import(&test_image(&home.0), Author::Local).unwrap();
        let mut state = LocalPublish::open(&home.0);
        state.entries = store.offered().unwrap();
        let args = makepad_strict_json::obj(vec![("ids", Value::Arr(vec![makepad_strict_json::s(&entry.id)])),
            ("title", makepad_strict_json::s("Demo")), ("text", makepad_strict_json::s("Notes"))]).to_json();
        let crate::tools::Request::Publish { ids, title, text } = crate::tools::read("publish", &args).unwrap() else { panic!("expected draft") };
        state.review(ids, title, text).unwrap();
        assert_eq!(state.draft.ids, [entry.id]);
        assert!(!state.pending(), "a tool draft scheduled no work");
        let cx = makepad_widgets::Cx::new(Box::new(|_,_| {}));
        let (release, held) = std::sync::mpsc::channel();
        let (done, observed) = std::sync::mpsc::channel();
        let ui_thread = std::thread::current().id();
        state.confirm_with(&cx.task_pool(), move |_,_| {
            assert_ne!(std::thread::current().id(), ui_thread, "upload work runs off the UI thread");
            held.recv().expect("Confirm returned before completion");
            done.send(()).unwrap();
            Ok(Published { folder: String::new(), url: None })
        }).unwrap();
        assert!(state.sending() && !state.can_confirm());
        assert!(!state.poll(), "progress polling returns while the worker is held");
        release.send(()).unwrap();
        observed.recv().unwrap();
        for bad in [r#"{"targets":[], "targets":[]}"#, r#"{"targets":[{"name":"Demo","ssh":"-F","dir":"/posts"}]}"#] {
            fs::write(&file, bad).unwrap();
            let state = LocalPublish::open(&home.0);
            assert!(!state.enabled() && state.config_problem.as_deref().unwrap().contains("publish.json"));
            assert!(Config::load(&home.0).unwrap_err().to_string().contains("publish.json"));
        }
    }
}
