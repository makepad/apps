//! Device-local asset library locations.
//!
//! One asset library per app: Stage's is the library below (its store is
//! [`store_root`]); every other app has its own under it, `<library>/<app>`
//! with `<library>/<app>/store` ([`app_store_root`], [`app_library_root`]).
//! Apps never share a store.
//!
//! Generated source files and their index live in the asset library; the
//! catalog, content-addressed blobs and server identity live in its `store`.
//! App caches remain separate. Explicit deployment overrides are preserved;
//! absent directories never select a different library behind the user's back.
//!
//! Which directory is the library, first match wins:
//!
//! 1. `MAKEPAD_ASSET_LIBRARY` names the library itself.
//! 2. `STAGE_DATA_ROOT` names a parent data directory; the library is its
//!    `asset-library`.
//! 3. `MAKEPAD_HOME` names an isolated Makepad home (a test instance); the
//!    library is its `asset-library`, so such an instance never touches the
//!    library people use.
//! 4. `MAKEPAD_ROOT` names a checkout; the library is its
//!    `local/asset-library` (the long-standing explicit override).
//! 5. Stage built inside a repository cloned into a Makepad checkout's
//!    `apps/<name>/` uses that checkout's `local/asset-library`.
//! 6. A standalone Stage checkout: Stage's own `local/asset-library`. A
//!    standalone build never writes into some Makepad tree.
//!
//! Under rules 5 and 6 a build in a linked git worktree uses the library of
//! that worktree's main checkout: `local/` is untracked, so a worktree's own
//! is empty, and a worktree build is the same program on the same device.

use std::path::{Path, PathBuf};

fn override_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).filter(|value| !value.is_empty()).map(PathBuf::from)
}

/// The `stage/` directory beside the `engine/` this crate was built in.
fn stage_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("asset client lives under engine/asset/client")
        .join("stage")
}

/// The Makepad checkout around `apps/<name>/stage`, identified by its framework crates.
fn enclosing_makepad_checkout(stage_root: &Path) -> Option<PathBuf> {
    let apps = stage_root.parent()?.parent()?;
    if apps.file_name()? != "apps" {
        return None;
    }
    let checkout = apps.parent()?;
    let is_makepad = checkout.join("platform/Cargo.toml").is_file()
        && checkout.join("widgets/Cargo.toml").is_file();
    is_makepad.then(|| checkout.to_path_buf())
}

/// The checkout this build sits in (rules 5 and 6).
fn build_checkout() -> PathBuf {
    let stage = stage_root();
    enclosing_makepad_checkout(&stage).unwrap_or(stage)
}

/// The main working tree of the repository whose linked worktree `checkout`
/// is (its `.git` is a file naming `<main>/.git/worktrees/<name>`, whose
/// `commondir` leads to `<main>/.git`); `checkout` itself otherwise.
fn main_checkout(checkout: &Path) -> PathBuf {
    let linked = std::fs::read_to_string(checkout.join(".git")).ok()
        .and_then(|text| text.strip_prefix("gitdir:").map(|dir| checkout.join(dir.trim())));
    linked
        .and_then(|gitdir| {
            let common = std::fs::read_to_string(gitdir.join("commondir")).ok()?;
            std::fs::canonicalize(gitdir.join(common.trim())).ok()
        })
        .filter(|common| common.file_name().is_some_and(|name| name == ".git"))
        .and_then(|common| common.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| checkout.to_path_buf())
}

/// The checkout whose `local/` holds this device's data (rules 4 to 6).
pub fn checkout_root() -> PathBuf {
    override_path("MAKEPAD_ROOT").unwrap_or_else(|| main_checkout(&build_checkout()))
}

pub fn library_root() -> PathBuf {
    override_path("MAKEPAD_ASSET_LIBRARY")
        .or_else(|| override_path("STAGE_DATA_ROOT").map(|data| data.join("asset-library")))
        .or_else(|| override_path("MAKEPAD_HOME").map(|home| home.join("asset-library")))
        .unwrap_or_else(|| library_at(&checkout_root()))
}

/// Stage's store root, kept under its old name for compatibility: the
/// catalog, CAS and server identity of the library Stage has always used.
/// `AI_CONTENT_ASSET_ROOT` remains an explicit override of it. New code names
/// its app with [`app_store_root`]; this is `app_store_root(STAGE_APP)`
/// without the `STAGE_ASSET_ROOT` override.
pub fn store_root() -> PathBuf {
    override_path("AI_CONTENT_ASSET_ROOT")
        .unwrap_or_else(|| library_root().join("store"))
}

/// The app whose library is the one this checkout always had.
pub const STAGE_APP: &str = "stage";

/// One asset library per app: `app`'s generation library (the directory
/// whose `index.json` its host publishes). Stage's is the library itself;
/// any other app's is its own child `<library>/<app>`.
pub fn app_library_root(app: &str) -> PathBuf {
    let app = app_dir_name(app);
    if app == STAGE_APP {
        library_root()
    } else {
        library_root().join(app)
    }
}

/// One asset library per app: `app`'s store root (catalog, CAS, `server-id`,
/// `listen`, `server.lock`). Apps never share a store.
///
/// 1. `<APP>_ASSET_ROOT` (the app name upper-cased, `-` as `_`) names it.
/// 2. Stage: [`store_root`], so Stage keeps its library byte for byte.
/// 3. Any other app: `<library>/<app>/store`.
pub fn app_store_root(app: &str) -> PathBuf {
    let app = app_dir_name(app);
    override_path(&app_env_var(&app, "ASSET_ROOT")).unwrap_or_else(|| {
        if app == STAGE_APP {
            store_root()
        } else {
            app_library_root(&app).join("store")
        }
    })
}

/// Stage's store holds a video library (edit renders, long recordings),
/// so it admits blobs up to 16 GiB; uploads stream, so memory stays flat.
pub const STAGE_MAX_BLOB_BYTES: u64 = 16 << 30;

/// The largest blob `app`'s store admits, when the app asks for other than
/// the store default (256 MiB). A store setting: whoever hosts the app's
/// store applies it, and clients learn it from the server's health answer.
pub fn app_max_blob_bytes(app: &str) -> Option<u64> {
    (app_dir_name(app) == STAGE_APP).then_some(STAGE_MAX_BLOB_BYTES)
}

/// `<APP>_<suffix>`: the app's environment name for one setting.
pub fn app_env_var(app: &str, suffix: &str) -> String {
    format!("{}_{suffix}", app_dir_name(app).to_ascii_uppercase().replace('-', "_"))
}

/// An app name as a directory name: lower-case ASCII letters, digits, `-`
/// and `_`. Anything else is a programming error (the name comes from code,
/// never from input); release builds replace it with `_` rather than let a
/// name like `../x` leave the library.
fn app_dir_name(app: &str) -> String {
    debug_assert!(
        !app.is_empty() && app.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_'),
        "asset app name must be lower-case [a-z0-9_-]: {app:?}"
    );
    let name: String = app
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c.to_ascii_lowercase() } else { '_' })
        .collect();
    if name.is_empty() { "_".to_string() } else { name }
}

fn library_at(checkout: &Path) -> PathBuf {
    checkout.join("local/asset-library")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_and_store_share_one_root_even_before_it_exists() {
        let library = library_at(Path::new("/checkout"));
        assert_eq!(library, PathBuf::from("/checkout/local/asset-library"));
        assert_eq!(library.join("store"), PathBuf::from("/checkout/local/asset-library/store"));
    }

    #[test]
    fn stage_keeps_the_library_and_every_other_app_gets_its_own() {
        // Environment-independent part: the name rules.
        assert_eq!(app_dir_name("sandbox"), "sandbox");
        assert_eq!(app_env_var("sandbox", "ASSET_ROOT"), "SANDBOX_ASSET_ROOT");
        assert_eq!(app_env_var("my-app", "ASSET_ROOT"), "MY_APP_ASSET_ROOT");
        // The layout, unless the environment overrides it for this run.
        if std::env::var_os("STAGE_ASSET_ROOT").is_none() {
            assert_eq!(app_store_root(STAGE_APP), store_root());
        }
        if std::env::var_os("SANDBOX_ASSET_ROOT").is_none() {
            assert_eq!(app_store_root("sandbox"), library_root().join("sandbox/store"));
            assert_ne!(app_store_root("sandbox"), app_store_root(STAGE_APP));
        }
        assert_eq!(app_library_root(STAGE_APP), library_root());
        assert_eq!(app_library_root("sandbox"), library_root().join("sandbox"));
        assert_eq!(app_max_blob_bytes(STAGE_APP), Some(16 << 30));
        assert_eq!(app_max_blob_bytes("sandbox"), None, "the store default");
    }

    #[test]
    fn stage_in_a_nested_repository_finds_the_enclosing_makepad_checkout() {
        let base = std::env::temp_dir().join(format!("asset-paths-nested-{}", std::process::id()));
        for crate_dir in ["platform", "widgets"] {
            std::fs::create_dir_all(base.join(crate_dir)).unwrap();
            std::fs::write(base.join(crate_dir).join("Cargo.toml"), "").unwrap();
        }
        for path in ["apps/commercial/stage", "apps/apps/stage"] {
            let stage = base.join(path);
            std::fs::create_dir_all(&stage).unwrap();
            assert_eq!(enclosing_makepad_checkout(&stage), Some(base.clone()));
        }
        // Stage anywhere else is its own checkout, not a guessed one.
        std::fs::create_dir_all(base.join("apps/stage")).unwrap();
        assert_eq!(enclosing_makepad_checkout(&base.join("apps/stage")), None);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_linked_worktree_attaches_through_its_main_checkout() {
        let base = std::env::temp_dir().join(format!("asset-paths-worktree-{}", std::process::id()));
        let main = base.join("main");
        let gitdir = main.join(".git/worktrees/wt");
        let worktree = base.join("wt");
        std::fs::create_dir_all(&gitdir).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(gitdir.join("commondir"), "../..\n").unwrap();
        std::fs::write(worktree.join(".git"), format!("gitdir: {}\n", gitdir.display())).unwrap();
        let main = std::fs::canonicalize(&main).unwrap();
        assert_eq!(main_checkout(&worktree), main);
        // The main checkout and a plain directory are their own.
        assert_eq!(main_checkout(&main), main);
        assert_eq!(main_checkout(&base), base);
        std::fs::remove_dir_all(&base).unwrap();
    }
}
