use crate::Error;
use makepad_core_util::sha256::{to_hex, Sha256};
use std::collections::HashSet;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    pub id: String,
    pub chip: String,
    pub license: String,
    pub license_sha256: String,
    pub import_map_version: String,
    pub repositories: Vec<Repository>,
    pub files: Vec<PackFile>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Repository {
    pub name: String,
    pub commit: String,
    pub url_base: String,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PackFile {
    pub repository: String,
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

impl Manifest {
    pub fn total_bytes(&self) -> u64 {
        self.files.iter().map(|file| file.size).sum()
    }

    /// SHA-256 of UTF-8 fields in declaration order. Each field is written as
    /// decimal byte length, colon, value, newline. Lists start with a count
    /// field and their records are sorted lexicographically by their fields.
    pub fn key(&self) -> String {
        let mut hash = Sha256::new();
        let mut field = |value: &str| {
            hash.update(value.len().to_string().as_bytes());
            hash.update(b":");
            hash.update(value.as_bytes());
            hash.update(b"\n");
        };
        field(&self.id);
        field(&self.chip);
        field(&self.license);
        field(&self.license_sha256);
        field(&self.import_map_version);

        let mut repositories: Vec<_> = self.repositories.iter().collect();
        repositories.sort_unstable();
        field(&repositories.len().to_string());
        for repo in repositories {
            field(&repo.name);
            field(&repo.commit);
            field(&repo.url_base);
        }
        let mut files: Vec<_> = self.files.iter().collect();
        files.sort_unstable();
        field(&files.len().to_string());
        for file in files {
            field(&file.repository);
            field(&file.path);
            field(&file.size.to_string());
            field(&file.sha256);
        }
        to_hex(&hash.finalize())
    }

    pub(crate) fn validate(&self) -> Result<(), Error> {
        fn component(name: &str) -> bool {
            !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', ':'])
        }
        let mut repos = HashSet::new();
        for repo in &self.repositories {
            if !component(&repo.name) || repo.name.eq_ignore_ascii_case("PROVENANCE.txt") {
                return Err(Error::InvalidManifest("invalid repository directory"));
            }
            if !repos.insert(repo.name.as_str()) {
                return Err(Error::InvalidManifest("duplicate repository name"));
            }
        }
        let mut paths = HashSet::new();
        let mut total = 0u64;
        for file in &self.files {
            if !repos.contains(file.repository.as_str()) {
                return Err(Error::InvalidManifest(
                    "file refers to an unknown repository",
                ));
            }
            if !file.path.split('/').all(component) {
                return Err(Error::InvalidManifest(
                    "invalid repository-relative file path",
                ));
            }
            if !paths.insert((&file.repository, file.path.clone()))
                || !paths.insert((&file.repository, format!("{}.part", file.path)))
            {
                return Err(Error::InvalidManifest("overlapping file paths"));
            }
            total = total
                .checked_add(file.size)
                .ok_or(Error::InvalidManifest("pack size overflows"))?;
        }
        Ok(())
    }
}

/// Espressif's ESP32-S3 Wi-Fi and PHY archives for IDF v6.1.1.
pub fn espressif_s3() -> Manifest {
    const LICENSE: &str = "cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30";
    const WIFI: &str = "esp32-wifi-lib";
    const PHY: &str = "esp-phy-lib";

    fn repo(name: &str, commit: &str) -> Repository {
        Repository {
            name: name.into(),
            commit: commit.into(),
            url_base: format!("https://raw.githubusercontent.com/espressif/{name}/{commit}/"),
        }
    }
    fn file(repository: &str, path: &str, size: u64, sha256: &str) -> PackFile {
        PackFile {
            repository: repository.into(),
            path: path.into(),
            size,
            sha256: sha256.into(),
        }
    }
    Manifest {
        id: "espressif-radio-esp32s3-idf-v6.1.1".into(),
        chip: "ESP32-S3".into(),
        license: "Apache-2.0".into(),
        license_sha256: LICENSE.into(),
        import_map_version: "v1".into(),
        repositories: vec![
            repo(WIFI, "ec08f0cee472c46e9e9c2c5f8180f74653e8c2dd"),
            repo(PHY, "20f1db053a0e6cb9f1c09d255c43bf42483041d0"),
        ],
        files: vec![
            file(WIFI, "LICENSE", 11358, LICENSE),
            file(
                WIFI,
                "esp32s3/libcore.a",
                4848,
                "9d0b54be0f26b9ff96d9723220f2b6212c5fd58ca10928d5b1d1ca215f1b2431",
            ),
            file(
                WIFI,
                "esp32s3/libespnow.a",
                67280,
                "c719cd9d8b1c066be7719b6b32bfb0e83119b5ffacf3f0eeed4ea437cc146aef",
            ),
            file(
                WIFI,
                "esp32s3/libmesh.a",
                995216,
                "065c30fc564ccb2935d04c6ca351cacb6258056cbf4cb29f44cce2f2df80cdb6",
            ),
            file(
                WIFI,
                "esp32s3/libnet80211.a",
                1491226,
                "04b3b2179877d3fe496c1371758d99b0bb0ea4e4a47369b8ba79160b2a1150c4",
            ),
            file(
                WIFI,
                "esp32s3/libpp.a",
                708188,
                "4408e504e4c0721d59f4fa733ed4cf0219c718ef787fea00fd8adfdc45fcebe6",
            ),
            file(
                WIFI,
                "esp32s3/libsmartconfig.a",
                197726,
                "c1d731f85708279d0913e4e317cd02851e5edfe372b47e2a27b065355d12e66f",
            ),
            file(
                WIFI,
                "esp32s3/libwapi.a",
                52394,
                "aab91917e37224343407ba14a226291cb219aab5c2889a494beb0efd45b6b7b6",
            ),
            file(PHY, "LICENSE", 11358, LICENSE),
            file(
                PHY,
                "esp32s3/libphy.a",
                257184,
                "038ac2bc37dc297e295811bc095d2b360a49aa0dddaa51ccb4b60ce2d1b2b73f",
            ),
            file(
                PHY,
                "esp32s3/libbtbb.a",
                23346,
                "4d60de87cad0fa9519d68e0e40778a68dc61af040853fd0b5c45efa2a010ee94",
            ),
        ],
    }
}
