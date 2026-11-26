//! Renders: an edit's or an app run's MP4 published as an ordinary `Video`
//! asset whose ONLY dependency is the exact source revision it was made
//! from. It lives in the SOURCE's namespace, and the alias
//! `<namespace>/render/<source asset id>` points at the source's newest
//! render, so publishing and repointing a render takes the same grants as
//! publishing the source itself. Consumers that need a particular render
//! pin its revision instead.
//!
//! The pure helpers build on every target; publishing and lookup need the
//! blocking native client.

#[cfg(all(not(target_arch = "wasm32"), not(feature = "web")))]
use crate::client::AssetClient;
use crate::error::{ClientError, ClientResult};
#[cfg(all(not(target_arch = "wasm32"), not(feature = "web")))]
use crate::publish::PublishedBundle;
use crate::publish::PublishBundle;
use makepad_asset_data::{
    AssetAlias, AssetId, AssetKind, AssetManifest, AssetRevisionId, AssetRevisionRef, FileRole,
};

/// The alias naming the newest render of `source` (an asset in namespace
/// `namespace`): `<namespace>/render/<asset id>`.
pub fn render_alias(namespace: &str, source: &AssetId) -> String {
    format!("{namespace}/render/{source}")
}

fn render_alias_typed(namespace: &str, source: &AssetId) -> ClientResult<AssetAlias> {
    AssetAlias::new(render_alias(namespace, source))
        .map_err(|_| ClientError::InvalidInput { what: "render alias" })
}

/// Shape `bundle` into the render of `source` (an asset in `namespace`):
/// kind `Video`, exactly the one dependency, published in the source's
/// namespace under its render alias. Files, thumbnail, title, rights and
/// annotation text stay the caller's.
pub fn render_bundle(
    source: AssetRevisionRef,
    namespace: &str,
    mut bundle: PublishBundle,
) -> ClientResult<PublishBundle> {
    if !bundle.files.iter().any(|f| f.role == FileRole::Video) {
        return Err(ClientError::InvalidInput { what: "render has no video file" });
    }
    if !bundle.dependencies.is_empty() && bundle.dependencies != [source] {
        return Err(ClientError::InvalidInput { what: "render dependencies" });
    }
    let alias = render_alias_typed(namespace, &source.asset_id)?;
    bundle.namespace = namespace.to_string();
    bundle.kind = AssetKind::Video;
    bundle.dependencies = vec![source];
    bundle.alias = Some(alias);
    Ok(bundle)
}

/// The `(render, source)` pair a render alias head describes, or a protocol
/// refusal when the head is not a render of `source_asset`.
#[cfg_attr(any(target_arch = "wasm32", feature = "web"), allow(dead_code))]
fn render_pair(
    source_asset: &AssetId,
    render: AssetRevisionRef,
    manifest: &AssetManifest,
) -> ClientResult<(AssetRevisionRef, AssetRevisionRef)> {
    match manifest.dependencies.as_slice() {
        [dep] if manifest.kind == AssetKind::Video
            && manifest.asset_id == render.asset_id
            && dep.asset_id == *source_asset =>
        {
            Ok((render, *dep))
        }
        _ => Err(ClientError::Protocol { what: "render alias target" }),
    }
}

/// Whether the render alias should stay where it is: it already names a
/// render of a NEWER revision of the source than `ours` (a slower render of
/// an older revision finished last). Order is the source's revision order
/// (staged time, then revision id); a revision the detail does not list
/// never outranks ours.
#[cfg_attr(any(target_arch = "wasm32", feature = "web"), allow(dead_code))]
fn render_alias_is_newer(
    detail: &crate::dto::AssetDetailDto,
    current: &AssetRevisionId,
    ours: &AssetRevisionId,
) -> bool {
    let order = |rev: &AssetRevisionId| {
        detail.candidates.iter().find(|c| &c.revision == rev).map(|c| (c.staged_ms, c.revision))
    };
    match (order(current), order(ours)) {
        (Some(current), Some(ours)) => current > ours,
        _ => false,
    }
}

#[cfg(all(not(target_arch = "wasm32"), not(feature = "web")))]
impl AssetClient {
    /// Publish `bundle` as the render of `source` (see [`render_bundle`])
    /// and point `<namespace>/render/<source asset id>` at it. A bundle
    /// without an `asset_id` mints a new render asset, so older renders an
    /// edit still pins stay live heads of their own assets.
    ///
    /// Renders of one source can finish out of order. When the alias
    /// already names a render of a newer source revision, the render is
    /// published WITHOUT moving the alias (the result's `alias` is `None`);
    /// callers that pin it by revision are unaffected. The check and the
    /// publish are two requests, so two renders finishing within that
    /// window can still race; the loser then shows as stale, never wrong.
    pub fn publish_render(
        &mut self,
        source: AssetRevisionRef,
        bundle: PublishBundle,
    ) -> ClientResult<PublishedBundle> {
        let detail = self.asset_detail(&source.asset_id)?;
        // The store refuses a dependency on a retired revision; say so before
        // any bytes move.
        let retired = detail.retired
            || detail.candidates.iter().any(|c| c.revision == source.revision && c.retired_ms.is_some());
        if retired {
            return Err(ClientError::InvalidInput { what: "render source retired" });
        }
        let mut bundle = render_bundle(source, &detail.namespace, bundle)?;
        if let Some((_, current)) = self.render_head(&source.asset_id, &detail.namespace)? {
            if render_alias_is_newer(&detail, &current.revision, &source.revision) {
                bundle.alias = None;
            }
        }
        self.publish_bundle(&bundle)
    }

    /// The newest render of `source` and the exact source revision it was
    /// made from, or `None` when there is none. Fresh when that revision is
    /// still the source's head (and, for an app run, its build identity
    /// matches).
    pub fn render_of(
        &mut self,
        source: &AssetId,
    ) -> ClientResult<Option<(AssetRevisionRef, AssetRevisionRef)>> {
        let namespace = self.asset_detail(source)?.namespace;
        self.render_head(source, &namespace)
    }

    fn render_head(
        &mut self,
        source: &AssetId,
        namespace: &str,
    ) -> ClientResult<Option<(AssetRevisionRef, AssetRevisionRef)>> {
        let alias = render_alias_typed(namespace, source)?;
        let head = match self.resolve_alias(&alias) {
            Ok(head) => head,
            Err(ClientError::NotFound { .. }) => return Ok(None),
            Err(e) => return Err(e),
        };
        let render = AssetRevisionRef { asset_id: head.asset_id, revision: head.head_revision };
        let manifest = self.fetch_asset_manifest(&render.revision)?;
        render_pair(source, render, &manifest).map(Some)
    }
}

#[cfg(all(test, not(target_arch = "wasm32"), not(feature = "web")))]
mod tests {
    use super::*;
    use crate::dto::{AssetDetailDto, CandidateDto, CandidateStateDto};
    use crate::publish::{PublishBundleFile, PublishRights, PublishThumbnail};
    use makepad_asset_data::{BlobId, MediaType, ThumbnailMedia};

    fn source() -> AssetRevisionRef {
        AssetRevisionRef {
            asset_id: AssetId::from_bytes([3; 16]),
            revision: AssetRevisionId::from_bytes([4; 32]),
        }
    }

    fn bundle() -> PublishBundle {
        let mut b = PublishBundle::new(
            "elsewhere",
            AssetKind::Prop,
            "Intro render",
            vec![
                PublishBundleFile::bytes(FileRole::Video, MediaType::Mp4, vec![0x55; 9_000], None),
                PublishBundleFile::bytes(FileRole::Audio, MediaType::Wav, vec![0x66; 3_000], None),
            ],
            PublishThumbnail::plain(vec![7; 900], ThumbnailMedia::Png, 512, 512),
            PublishRights::generated_cc0(),
        );
        b.media_millis = 4_000;
        b
    }

    #[test]
    fn a_render_is_a_video_in_its_sources_namespace_depending_only_on_it() {
        let source = source();
        let alias = render_alias("rik2", &source.asset_id);
        assert_eq!(alias, format!("rik2/render/{}", source.asset_id));
        assert_eq!(AssetAlias::new(alias.clone()).unwrap().namespace(), "rik2");

        let b = bundle();
        let render = render_bundle(source, "rik2", b.clone()).unwrap();
        assert_eq!(render.kind, AssetKind::Video);
        assert_eq!(render.dependencies, vec![source]);
        assert_eq!(render.namespace, "rik2");
        assert_eq!(render.alias.as_ref().map(AssetAlias::as_str), Some(alias.as_str()));
        render.validate().unwrap();
        let render_id = AssetId::from_bytes([9; 16]);
        let resolved: Vec<(BlobId, u64)> = render
            .files
            .iter()
            .map(|f| (BlobId::hash_of(&f.bytes), f.bytes.len() as u64))
            .collect();
        let (bytes, revision, _) = render.manifest_with(render_id, &resolved).unwrap();
        let manifest = AssetManifest::from_canonical_bytes(&bytes).unwrap();
        let render_ref = AssetRevisionRef { asset_id: render_id, revision };
        assert_eq!(
            render_pair(&source.asset_id, render_ref, &manifest).unwrap(),
            (render_ref, source)
        );
        // A head that is not a render of THIS source is refused, not believed.
        let stranger = AssetId::from_bytes([5; 16]);
        assert!(render_pair(&stranger, render_ref, &manifest).is_err());

        // Idempotent over a bundle that already names the source; refuses a
        // bundle with no video or with other dependencies.
        let mut again = b.clone();
        again.dependencies = vec![source];
        assert_eq!(render_bundle(source, "rik2", again).unwrap(), render);
        let mut other = b.clone();
        other.dependencies = vec![render_ref];
        assert!(render_bundle(source, "rik2", other).is_err());
        let mut silent = b;
        silent.files.retain(|f| f.role != FileRole::Video);
        assert!(render_bundle(source, "rik2", silent).is_err());
    }

    #[test]
    fn an_older_sources_render_does_not_take_the_alias_back() {
        let old = AssetRevisionId::from_bytes([1; 32]);
        let new = AssetRevisionId::from_bytes([2; 32]);
        let unknown = AssetRevisionId::from_bytes([9; 32]);
        let candidate = |revision, staged_ms| CandidateDto {
            revision,
            state: CandidateStateDto::Published,
            staged_ms,
            published_ms: Some(staged_ms),
            quarantined_ms: None,
            retired_ms: None,
        };
        let detail = AssetDetailDto {
            asset_id: source().asset_id,
            namespace: "rik2".into(),
            retired: false,
            retired_ms: None,
            candidates: vec![candidate(new, 20), candidate(old, 10)],
        };
        // The alias names a render of `new`; a late render of `old` keeps off.
        assert!(render_alias_is_newer(&detail, &new, &old));
        // The alias names a render of `old`; a render of `new` moves it.
        assert!(!render_alias_is_newer(&detail, &old, &new));
        // Re-rendering the same revision moves it; an unlisted one never wins.
        assert!(!render_alias_is_newer(&detail, &new, &new));
        assert!(!render_alias_is_newer(&detail, &unknown, &old));
    }
}
