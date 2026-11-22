use fab_elec_mcu_cosim::eeprom;

#[test]
fn eeprom_file_round_trip_new_chip_and_clear() {
    let dir = std::env::temp_dir().join(format!("fab-eeprom-file-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = eeprom::path(&dir, "drum16", "U1").unwrap();
    assert_eq!(path, dir.join("drum16.U1.eeprom"), "one file per circuit and MCU part, beside the benches");
    for (circuit, part) in [("drum16", "../U1"), ("..", "U1"), ("drum16", ""), ("a\\b", "U1")] {
        assert!(eeprom::path(&dir, circuit, part).is_err(), "`{circuit}`/`{part}` must not leave the project folder");
    }
    assert_eq!(eeprom::load(&path, 1024), Ok(None), "no file is a new chip");
    let bytes: Vec<u8> = (0..1024).map(|i| (i * 7) as u8).collect();
    eeprom::save(&path, &bytes).unwrap();
    assert_eq!(eeprom::load(&path, 1024), Ok(Some(bytes)), "saved bytes load back exactly");
    assert!(!path.with_extension("eeprom.tmp").exists(), "the temp file is renamed over the file");
    let error = eeprom::load(&path, 512).unwrap_err();
    assert!(error.contains("1024 bytes") && error.contains("512 bytes"), "a wrong length names both lengths: {error}");
    eeprom::clear(&path).unwrap();
    assert_eq!(eeprom::load(&path, 1024), Ok(None), "clear gives a new chip");
    eeprom::clear(&path).unwrap();
    std::fs::remove_dir(&dir).unwrap();
}
