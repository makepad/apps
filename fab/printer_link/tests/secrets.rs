use fab_printer_link::secrets::{PrinterConfig, Secret};

#[test]
fn secret_debug_and_display_are_redacted() {
    let secret = Secret::new(b"TESTCODE".to_vec());
    assert_eq!(format!("{secret}"), "***");
    assert_eq!(format!("{secret:?}"), "***");
}

#[cfg(unix)]
#[test]
fn configuration_checks_open_file_permissions_before_reading() {
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let model = format!("{}Fixture", makepad_fab_settings::family::DEFAULT.models[0]);
    let path = std::env::temp_dir().join(format!("fab-printer-conf-test-{}", std::process::id()));
    let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path).unwrap();
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup { fn drop(&mut self) { let _ = fs::remove_file(&self.0); } }
    let _cleanup = Cleanup(path.clone());
    file.write_all(format!("host=printer.invalid\nserial=TESTSERIAL0001\nmodel={model}\naccess_code=TESTCODE\n").as_bytes()).unwrap();
    file.set_permissions(fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(PrinterConfig::load_file(&path), Err("printer configuration must be private (mode 0600)")));
    file.set_permissions(fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(PrinterConfig::load_file(&path).unwrap().model, model);
    assert!(PrinterConfig::load("../escape").is_err());
    fs::write(&path, format!("host=printer.invalid\nmodel={model}\naccess_code=TESTCODE\n")).unwrap();
    // The serial is optional: the CA-verified certificate names the printer.
    assert_eq!(PrinterConfig::load_file(&path).unwrap().model, model);
    fs::write(&path, format!("host=printer.invalid\nserial=bad serial\nmodel={model}\naccess_code=TESTCODE\n")).unwrap();
    assert!(matches!(PrinterConfig::load_file(&path), Err("invalid printer serial")));
}
