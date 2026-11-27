use super::Home;
use std::io;

impl Home {
    pub fn computer_label(&self) -> io::Result<String> {
        let path = self.dir.join("local/computer-label");
        match self.files().read(&path).and_then(|bytes| String::from_utf8(bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))) {
            Ok(label) if usable(&label) => Ok(label),
            Ok(_) => Err(io::Error::new(io::ErrorKind::InvalidData, "invalid stored computer label")),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let label = Self::default_computer_label();
                self.files().replace(&path, label.as_bytes().to_vec())?;
                Ok(label)
            }
            Err(e) => Err(e),
        }
    }

    /// Reading the fallback label during pairing does not complete the name prompt.
    pub fn chosen_computer_label(&self) -> io::Result<Option<String>> {
        if self.files().exists(&self.dir.join("local/name-chosen"))? {
            self.computer_label().map(Some)
        } else {
            Ok(None)
        }
    }

    pub fn name_prompt_prefill(&self) -> Option<String> {
        self.files().read(&self.dir.join("local/computer-label")).ok().and_then(|bytes| String::from_utf8(bytes).ok())
            .filter(|label| usable(label) && *label != Self::default_computer_label())
            .or_else(Self::suggested_person_name)
    }

    pub fn default_computer_label() -> String {
        short_label(os_name().as_deref()).to_owned()
    }

    pub fn suggested_person_name() -> Option<String> {
        os_full_name()
    }

    pub fn set_computer_label(&self, label: &str) -> io::Result<()> {
        if !usable(label) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid computer label"));
        }
        self.files().replace(&self.dir.join("local/computer-label"), label.as_bytes().to_vec())?;
        self.files().replace(&self.dir.join("local/name-chosen"), Vec::new())
    }
}

fn usable(label: &str) -> bool {
    // The proof's length-prefixed KEYS body includes a 32-byte contribution and a u16 name length.
    !label.trim().is_empty() && !label.chars().any(char::is_control) && label.len() <= u16::MAX as usize - 34
}

fn short_label(host: Option<&str>) -> &str {
    host.and_then(|name| name.split('.').next()).map(str::trim)
        .filter(|label| usable(label)).unwrap_or("This computer")
}

#[cfg(any(target_os = "macos", target_os = "linux", test))]
fn gecos_name(gecos: &[u8]) -> Option<&str> {
    let first = gecos.split(|byte| *byte == b',').next()?;
    let name = std::str::from_utf8(first).ok()?.trim();
    usable(name).then_some(name)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn os_full_name() -> Option<String> {
    use std::ffi::{c_char, CStr};
    #[repr(C)]
    struct Passwd {
        name: *const c_char,
        passwd: *const c_char,
        uid: u32,
        gid: u32,
        #[cfg(target_os = "macos")]
        change: std::ffi::c_long,
        #[cfg(target_os = "macos")]
        class: *const c_char,
        gecos: *const c_char,
        dir: *const c_char,
        shell: *const c_char,
        #[cfg(target_os = "macos")]
        expire: std::ffi::c_long,
    }
    extern "C" {
        fn getuid() -> u32;
        fn getpwuid(uid: u32) -> *const Passwd;
    }
    // Copy the C library's account entry before releasing it for another lookup.
    static LOOKUP: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOOKUP.lock().ok()?;
    unsafe {
        let entry = getpwuid(getuid()).as_ref()?;
        if entry.gecos.is_null() { return None }
        gecos_name(CStr::from_ptr(entry.gecos).to_bytes()).map(str::to_owned)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn os_full_name() -> Option<String> { None }

#[cfg(unix)]
fn os_name() -> Option<String> {
    extern "C" { fn gethostname(name: *mut u8, len: usize) -> i32; }
    let mut buf = [0u8; 256];
    if unsafe { gethostname(buf.as_mut_ptr(), buf.len()) } != 0 { return None }
    let end = buf.iter().position(|b| *b == 0)?;
    String::from_utf8(buf[..end].to_vec()).ok()
}

#[cfg(windows)]
fn os_name() -> Option<String> {
    #[link(name = "kernel32")]
    extern "system" { fn GetComputerNameExW(format: i32, name: *mut u16, size: *mut u32) -> i32; }
    const COMPUTER_NAME_DNS_HOSTNAME: i32 = 1;
    let mut size = 0;
    unsafe { GetComputerNameExW(COMPUTER_NAME_DNS_HOSTNAME, std::ptr::null_mut(), &mut size); }
    if size == 0 { return None }
    let mut buf = vec![0; size as usize];
    if unsafe { GetComputerNameExW(COMPUTER_NAME_DNS_HOSTNAME, buf.as_mut_ptr(), &mut size) } == 0 { return None }
    String::from_utf16(buf.get(..size as usize)?).ok()
}

#[cfg(not(any(unix, windows)))]
fn os_name() -> Option<String> { None }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_label_uses_the_first_component_with_its_original_case() {
        assert_eq!(gecos_name(b"Ana Ben,,,"), Some("Ana Ben"), "only the full-name field prefills the prompt");
        assert_eq!(gecos_name(b"Ana,\xff"), Some("Ana"), "unused fields do not affect the name");
        for empty in ["", ",x", "  ,x", "Ana\nBen"] {
            assert_eq!(gecos_name(empty.as_bytes()), None, "an unusable GECOS name leaves the field empty");
        }
        assert_eq!(short_label(Some("Mixed-Case.example")), "Mixed-Case");
        for missing in [None, Some(""), Some(".example"), Some(" \n")] {
            assert_eq!(short_label(missing), "This computer");
        }
    }
}
