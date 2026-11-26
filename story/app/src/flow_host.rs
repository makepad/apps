//! The generation studio (makepad-flow-studio) as Story's video maker
//! drives it: New video, the storyboard and the generative segments fire its
//! presets through the generation tools. Story shows no FLOW page; the
//! studio runs on its API only, with the library as its result store.

use makepad_asset_connection as asset_connection;
use makepad_flow_assets::{AssetStoreConfig, AssetWorker, StoreLocator};
use makepad_flow_studio::host::{Opening, Place, StudioHost};
use makepad_flow_studio::presets::Destination;
use std::sync::Arc;

/// Install Story as the studio's host, before the studio first starts.
pub(crate) fn install_host() {
    let _ = makepad_flow_studio::host::install(StudioHost {
        open_results: Box::new(open_library),
        // The built-in node types and the recipe checks: Story runs no live
        // owner of its own under the studio.
        registries: || makepad_flow_studio::nodes::registries_with(|_, _| Ok(())),
        values: makepad_app_shell::user_state::dir(None, "studio-values"),
        presets: Vec::new(),
        place: story_place,
        library: "the library",
    });
}

/// The asset store, once it answers (it starts with the app): the same
/// endpoints, server id and credential the media workers use, its own cache
/// root, following a takeover that moves the store.
fn open_library() -> Opening {
    let (endpoints, server_id, token) = match asset_connection::store_endpoints() {
        Ok(found) => found,
        Err(error) => return Opening::Starting(format!("the asset store: {error}")),
    };
    // The store's `listen` file outlives its server: only a store that
    // answers now is the one this window hosts or attached to.
    if std::net::TcpStream::connect_timeout(&endpoints.control, std::time::Duration::from_millis(500)).is_err() {
        return Opening::Starting("the asset store is starting".into());
    }
    let mut config = AssetStoreConfig::new(makepad_ai_hub::home::makepad_home().join("stage-product").join("asset-cache").join("flow-studio-story"));
    config.endpoints = Some(endpoints);
    config.server_id = Some(server_id);
    config.token = Some(token);
    config.discovery_wait_ms = 0;
    config.locate = Some(StoreLocator(Arc::new(asset_connection::store_endpoints)));
    match AssetWorker::start_detached(config) {
        Ok(handle) => Opening::Open(handle.store()),
        Err(error) => Opening::Failed(error),
    }
}

/// Where Story's pages find each kind of result: the CUT library's lanes.
fn story_place(destination: Destination) -> Place {
    let (label, tags): (&'static str, &'static [&'static str]) = match destination {
        Destination::Image => ("CUT · IMAGE", &["image"]),
        Destination::Video => ("CUT · VIDEO", &["video"]),
        Destination::Music => ("CUT · AUDIO", &["music"]),
        Destination::Voice => ("VOICE TAKES", &["speech", "voiceover"]),
        Destination::Sound => ("CUT · AUDIO", &["sfx"]),
        Destination::Stem => ("CUT · AUDIO", &["stem"]),
        Destination::Mesh => ("3D", &["mesh"]),
        Destination::World => ("WORLDS", &["world"]),
        Destination::Data => ("DATA", &["text"]),
    };
    Place { label, tags }
}
