//! The user-installed Espressif pack and its linked ESP32-S3 station island.

use fab_elec_mcu_island::{Archive, Island};
use fab_elec_mcu_pack::{espressif_s3, verify};
use std::path::{Path, PathBuf};

pub fn packs_dir() -> PathBuf {
    std::env::var_os("FAB_PACKS_DIR")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| makepad_fab_settings::home::vendor_data_dir("Fab").join("packs"))
}

pub fn pack_dest(packs: &Path) -> PathBuf {
    packs.join(espressif_s3().id)
}

pub fn pack_staging(packs: &Path) -> PathBuf {
    packs.join(format!("{}.staging", espressif_s3().id))
}

pub fn link_installed(dest: &Path) -> Result<Island, String> {
    let manifest = espressif_s3();
    verify(&manifest, dest).map_err(|error| error.to_string())?;
    let mut contents = Vec::new();
    for file in manifest
        .files
        .iter()
        .filter(|file| file.path.ends_with(".a"))
    {
        let path = dest.join(&file.repository).join(&file.path);
        let data = std::fs::read(&path)
            .map_err(|error| format!("could not read {}: {error}", path.display()))?;
        let name = file
            .path
            .rsplit('/')
            .next()
            .expect("a verified archive has a name");
        contents.push((name, data));
    }
    let mut archives: Vec<_> = contents
        .iter()
        .map(|(name, data)| Archive { name, data })
        .collect();
    archives.sort_unstable_by_key(|archive| archive.name);
    fab_elec_mcu_emit_s3::radio::island::link_pack(&archives).map_err(|error| error.to_string())
}
