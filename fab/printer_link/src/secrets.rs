use std::fmt;
#[cfg(all(unix, not(target_arch = "wasm32")))]
use std::{fs::File, io::Read};
use std::path::{Path, PathBuf};

/// Never format credential bytes, including in a failed assertion or panic.
pub struct Secret(Vec<u8>);

impl Secret {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        &self.0
    }

    /// Appends without a reallocation that would leave an unwiped copy behind.
    pub(crate) fn extend(&mut self, bytes: &[u8]) {
        let mut grown = Vec::with_capacity((self.0.len() + bytes.len()).max(64));
        grown.extend_from_slice(&self.0);
        grown.extend_from_slice(bytes);
        let old = std::mem::replace(&mut self.0, grown);
        drop(Secret(old));
    }

    pub(crate) fn redact(&self, text: &str) -> String {
        match std::str::from_utf8(&self.0) {
            Ok(code) if !code.is_empty() => text.replace(code, "***"),
            _ => text.to_string(),
        }
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        // Also used for the transient CONNECT packet and configuration buffer.
        for byte in &mut self.0 {
            unsafe { std::ptr::write_volatile(byte, 0) };
        }
        std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) enum Access {
    Code(Secret),
    Moonraker { port: u16, api_key: Option<Secret> },
}

pub struct PrinterConfig {
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) host: String,
    #[cfg(not(target_arch = "wasm32"))]
    /// Optional: without it the CA-verified certificate's CN is the serial (and the leaf pin keeps it).
    pub(crate) serial: Option<String>,
    pub model: String,
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) access: Access,
}

pub(crate) fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

pub(crate) fn directory() -> Option<PathBuf> {
    Some(makepad_fab_settings::home::user_home().join(".makepad").join("fab").join("printers"))
}

/// Lists names only. Call on an existing startup worker; never reads credentials.
pub fn first_config_name() -> Option<String> {
    let mut names: Vec<_> = std::fs::read_dir(directory()?).ok()?.filter_map(Result::ok).filter_map(|entry| {
        let path = entry.path();
        if path.extension()?.to_str()? != "conf" { return None; }
        let name = path.file_stem()?.to_str()?;
        valid_name(name).then(|| name.to_string())
    }).collect();
    names.sort();
    names.into_iter().next()
}

impl PrinterConfig {
    pub fn load(name: &str) -> Result<Self, &'static str> {
        if !valid_name(name) { return Err("invalid printer configuration name"); }
        Self::load_file(&directory().ok_or("home directory unavailable")?.join(format!("{name}.conf")))
    }

    /// Permissions are checked on the opened file, before reading any bytes. Where they cannot be
    /// checked, no configuration loads: the file holds the printer's access secret.
    pub fn load_file(path: &Path) -> Result<Self, &'static str> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let buffer = read_private(path)?;
            Self::parse(buffer.bytes())
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = path;
            Err("private printer configuration permission checks are unavailable on this platform")
        }
    }

    /// A configuration's entries, the same on every platform that can read one privately.
    #[cfg(not(target_arch = "wasm32"))]
    fn parse(bytes: &[u8]) -> Result<Self, &'static str> {
        let text = std::str::from_utf8(bytes).map_err(|_| "invalid printer configuration encoding")?;
        let (mut host, mut serial, mut model, mut access_code, mut port, mut api_key) = (None, None, None, None, None, None);
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            let (key, value) = line.split_once('=').ok_or("invalid printer configuration entry")?;
            let value = value.trim();
            let value = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')).unwrap_or(value);
            let target = match key.trim() {
                "host" => &mut host,
                "serial" => &mut serial,
                "model" => &mut model,
                "access_code" => &mut access_code,
                "port" => &mut port,
                "api_key" => &mut api_key,
                _ => return Err("unknown printer configuration entry"),
            };
            if target.replace(value).is_some() { return Err("duplicate printer configuration entry"); }
        }
        let host = host.filter(|s| !s.is_empty() && !s.chars().any(char::is_whitespace) && !s.contains('/') && !s.contains('@')).ok_or("invalid printer host")?;
        let model = model.filter(|s| !s.is_empty()).ok_or("missing printer model")?;
        use makepad_fab_settings::family::{of_reported, Link, UNKNOWN_PRINTER};
        let (serial, access) = match of_reported(model).ok_or(UNKNOWN_PRINTER)?.link {
            Link::Mqtt => {
                if port.is_some() || api_key.is_some() { return Err("unknown printer configuration entry"); }
                let serial = match serial {
                    Some(s) if s.is_empty() || !s.bytes().all(|b| b.is_ascii_alphanumeric()) => return Err("invalid printer serial"),
                    s => s.map(str::to_string),
                };
                let code = access_code.filter(|s| s.len() == 8 && s.bytes().all(|b| b.is_ascii_graphic())).ok_or("invalid printer access code")?;
                (serial, Access::Code(Secret::new(code.as_bytes().to_vec())))
            }
            Link::Moonraker => {
                if access_code.is_some() || serial.is_some() { return Err("unknown printer configuration entry"); }
                let port = match port {
                    None => 7125,
                    Some(p) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => p.parse::<u16>().ok().filter(|p| *p != 0).ok_or("invalid printer port")?,
                    Some(_) => return Err("invalid printer port"),
                };
                let api_key = match api_key {
                    Some(key) if key.is_empty() || !key.bytes().all(|b| b.is_ascii_graphic()) => return Err("invalid printer API key"),
                    key => key.map(|key| Secret::new(key.as_bytes().to_vec())),
                };
                (None, Access::Moonraker { port, api_key })
            }
        };
        let model = match &access {
            Access::Code(code) => code.redact(model),
            Access::Moonraker { api_key: Some(key), .. } => key.redact(model),
            Access::Moonraker { api_key: None, .. } => model.to_string(),
        };
        Ok(Self { host: host.to_string(), serial, model, access })
    }
}

/// The file's bytes, once it is known to be private: a regular file no one but its owner can read
/// or write (mode 0600). Permissions are checked on the opened file, before reading any bytes.
#[cfg(all(unix, not(target_arch = "wasm32")))]
fn read_private(path: &Path) -> Result<Secret, &'static str> {
    use std::os::unix::fs::PermissionsExt;
    let mut file = File::open(path).map_err(|_| "cannot open printer configuration")?;
    let metadata = file.metadata().map_err(|_| "cannot check printer configuration permissions")?;
    if !metadata.is_file() { return Err("printer configuration is not a regular file"); }
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err("printer configuration must be private (mode 0600)");
    }
    let mut buffer = Secret::new(Vec::new());
    file.read_to_end(&mut buffer.0).map_err(|_| "cannot read printer configuration")?;
    Ok(buffer)
}

/// No permission check exists here yet, so no configuration is read.
#[cfg(all(not(unix), not(target_arch = "wasm32")))]
fn read_private(path: &Path) -> Result<Secret, &'static str> {
    let _ = path;
    Err("private printer configuration permission checks are unavailable on this platform")
}

#[cfg(all(test, unix, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct ConfigFile { dir: PathBuf, path: PathBuf }
    impl ConfigFile {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!("fab-link-config-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
            fs::create_dir(&dir).unwrap();
            let path = dir.join("printer.conf");
            OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path).unwrap();
            Self { dir, path }
        }
        fn load(&self, text: &str) -> Result<PrinterConfig, &'static str> {
            let mut file = OpenOptions::new().write(true).truncate(true).open(&self.path).unwrap();
            file.write_all(text.as_bytes()).unwrap();
            PrinterConfig::load_file(&self.path)
        }
    }
    impl Drop for ConfigFile {
        fn drop(&mut self) { fs::remove_file(&self.path).unwrap(); fs::remove_dir(&self.dir).unwrap(); }
    }
    #[test]
    fn credentials_follow_the_models_protocol() {
        let file = ConfigFile::new();
        let second = &format!("host=printer.invalid\nmodel={}\n", makepad_fab_settings::family::FAMILIES[1].models[0]);
        let config = file.load(&format!("{second}api_key=TESTKEY\nport=8125\n")).unwrap();
        assert_eq!(config.host, "printer.invalid");
        assert!(matches!(config.access, Access::Moonraker { port: 8125, api_key: Some(_) }));
        assert!(config.serial.is_none());
        assert!(matches!(file.load(second).unwrap().access, Access::Moonraker { port: 7125, api_key: None }));
        for entry in ["access_code=TESTCODE", "serial=TESTSERIAL"] {
            assert!(matches!(file.load(&format!("{second}{entry}\n")), Err("unknown printer configuration entry")));
        }
        let first = &format!("host=printer.invalid\nmodel={}Fixture\naccess_code=TESTCODE\n", makepad_fab_settings::family::DEFAULT.models[0]);
        for entry in ["api_key=TESTKEY", "port=7125"] {
            assert!(matches!(file.load(&format!("{first}{entry}\n")), Err("unknown printer configuration entry")));
        }
        assert!(matches!(file.load(first).unwrap().access, Access::Code(_)));
        for port in ["0", "65536", "-1", "1.0", "", "+1"] {
            assert!(matches!(file.load(&format!("{second}port={port}\n")), Err("invalid printer port")));
        }
        for port in [1, 65535] { assert!(file.load(&format!("{second}port={port}\n")).is_ok()); }
    }
    /// A configuration names its printer as the printer reports itself: the full model, or the
    /// name after its maker, either case. A model no family claims says what the line needs.
    #[test]
    fn a_configuration_names_its_printer_by_its_model_or_the_name_after_its_maker() {
        use makepad_fab_settings::family::{DEFAULT, FAMILIES, UNKNOWN_PRINTER};
        let file = ConfigFile::new();
        let code = "access_code=TESTCODE\n";
        for model in ["Fixture".to_string(), format!("{}fixture", DEFAULT.maker.to_lowercase()), format!("{}FIXTURE", DEFAULT.maker.to_uppercase())] {
            let config = file.load(&format!("host=printer.invalid\nmodel={model}\n{code}")).unwrap_or_else(|e| panic!("{model}: {e}"));
            assert!(matches!(config.access, Access::Code(_)), "{model}");
        }
        let second = FAMILIES[1];
        let short = second.models[0][second.maker.len()..].to_lowercase();
        assert!(matches!(file.load(&format!("host=printer.invalid\nmodel={short}\n")).unwrap().access, Access::Moonraker { .. }));
        assert_eq!(file.load(&format!("host=printer.invalid\nmodel={}Fixture\n{code}", second.maker)).err(), Some(UNKNOWN_PRINTER));
    }

    #[test]
    fn keys_stay_out_of_models_and_every_configuration_error() {
        let file = ConfigFile::new();
        let key = "private-test-key";
        let text = format!("host=printer.invalid\nmodel={} {key}\napi_key={key}\n", makepad_fab_settings::family::FAMILIES[1].models[0]);
        let config = file.load(&text).unwrap();
        assert!(!config.model.contains(key));
        assert!(config.model.contains("***"));
        if let Access::Moonraker { api_key: Some(secret), .. } = config.access {
            assert_eq!(format!("{secret} {secret:?}"), "*** ***");
        } else { panic!("missing credential"); }
        for extra in [format!("{key}=value\n"), format!("port={key}\n"), format!("api_key={key}\n"), "serial=TESTSERIAL\n".into(), "bad entry\n".into()] {
            let error = file.load(&format!("{text}{extra}")).err().unwrap();
            assert!(!format!("{error} {error:?}").contains(key));
        }
        for key in ["", "two words", "tab\tkey", "é", "control\u{7f}"] {
            let error = file.load(&format!("host=printer.invalid\nmodel={}\napi_key=\"{key}\"\n", makepad_fab_settings::family::FAMILIES[1].models[0])).err().unwrap();
            assert_eq!(error, "invalid printer API key");
        }
        let first = file.load(&format!("host=printer.invalid\nmodel={}Fixture TESTCODE\naccess_code=TESTCODE\n", makepad_fab_settings::family::DEFAULT.models[0])).unwrap();
        assert_eq!(first.model, format!("{}Fixture ***", makepad_fab_settings::family::DEFAULT.models[0]));
        fs::set_permissions(&file.path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(PrinterConfig::load_file(&file.path), Err("printer configuration must be private (mode 0600)")));
    }
}
