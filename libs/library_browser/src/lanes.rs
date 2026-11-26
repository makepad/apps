//! The library's lanes: what each lists, its rows and their thumbnails,
//! read from the asset store's catalogue on a worker (Story's cut store,
//! Stage's [`crate::catalog`]).

use crate::list::CardKind;
use makepad_asset_client::api::CatalogQuery;
use makepad_asset_client::dto::CatalogHit;
use makepad_asset_client::{AssetClient, CatalogPage};
use makepad_asset_connection::{bounded_png_metadata, redact};
use makepad_asset_data::{AssetId, AssetKind, AssetRevisionRef, MediaType, ThumbnailMedia};
use makepad_widgets::*;
use std::collections::HashSet;
use std::sync::Arc;

/// What the cut lanes' rows add (Story's): the cut file of a project tree
/// and the render of a cut or an app run.
pub trait CutRows {
    /// A project tree's cut: its file revision and its owner.
    fn tree_cut(&self, client: &mut AssetClient, reference: AssetRevisionRef) -> Result<(AssetRevisionRef, TreeDocument), String>;
    /// The newest render of `source` (its file `path` in a tree).
    fn render(&self, client: &mut AssetClient, source: &AssetRevisionRef, path: Option<&str>) -> Result<RenderState, String>;
}

pub const PAGE_SIZE: u32 = 48;

const THUMB_EDGE: u32 = 1024;

const MAX_THUMB_ENCODED: u64 = 2 * 1024 * 1024;

/// An image asset decoded for its own thumbnail: the texture ceiling.
const IMAGE_EDGE: u32 = 8192;

const MAX_IMAGE_ENCODED: u64 = 64 * 1024 * 1024;

/// Thumbnails keep their aspect inside this box.
const THUMB_BOX: usize = 256;

/// Imported cuts without a source picture use a placeholder until the visible
/// card's final still is cached. The cache never starts playback.
pub const POSTER_PENDING: &str = "poster-pending";

/// Tree files are opened through their project card, with its pinned context.
pub const TREE_FILE: &str = "tree-file";

/// The page's library lanes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum LibLane {
    #[default]
    Video,
    Image,
    Audio,
    Cuts,
    AppRuns,
    /// Decks (DECK's library, and a lane of CUT's).
    Decks,
    /// SCORE's explorer (EDIT does not list these): scores, audio as
    /// samples, SoundFonts and singers (audio tagged `soundfont`, `singer`).
    /// Only SCORE opens these lanes.
    Scores,
    Samples,
    SoundFonts,
    Voices,
}

impl LibLane {
    /// The tag a lane's items carry besides their kind.
    pub fn tag(self) -> Option<&'static str> {
        match self {
            Self::SoundFonts => Some("soundfont"),
            Self::Voices => Some("singer"),
            Self::Decks => Some("deck"),
            _ => None,
        }
    }

    pub fn kind(self) -> AssetKind {
        match self {
            Self::Video => AssetKind::Video,
            Self::Image => AssetKind::Texture,
            Self::Audio => AssetKind::Audio,
            Self::Cuts => AssetKind::Cut,
            Self::Decks => AssetKind::Tree,
            Self::AppRuns => AssetKind::AppRun,
            Self::Scores => AssetKind::Score,
            Self::Samples | Self::SoundFonts | Self::Voices => AssetKind::Audio,
        }
    }

    /// What a lane's cards stand for.
    pub fn card(self) -> CardKind {
        match self {
            Self::Video => CardKind::Video,
            Self::Image => CardKind::Image,
            Self::Audio | Self::Scores | Self::Samples | Self::SoundFonts | Self::Voices => CardKind::Audio,
            Self::Cuts => CardKind::Cut,
            Self::AppRuns => CardKind::AppRun,
            Self::Decks => CardKind::Deck,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Video => "VIDEO",
            Self::Image => "IMAGE",
            Self::Audio => "AUDIO",
            Self::Cuts => "CUTS",
            Self::Decks => "DECKS",
            Self::AppRuns => "APP RUNS",
            Self::Scores => "SCORES",
            Self::Samples => "SAMPLES",
            Self::SoundFonts => "SOUNDFONTS",
            Self::Voices => "VOICES",
        }
    }
}

/// A source's render: none, one made from an older revision,
/// or one of the current head.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderState {
    None,
    Stale(AssetRevisionRef),
    Fresh(AssetRevisionRef),
}

impl RenderState {
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "no render",
            Self::Stale(_) => "stale render",
            Self::Fresh(_) => "fresh render",
        }
    }

    pub fn render(self) -> Option<AssetRevisionRef> {
        match self {
            Self::None => None,
            Self::Stale(render) | Self::Fresh(render) => Some(render),
        }
    }
}

#[derive(Clone, Debug)]
pub struct LibRow {
    pub reference: AssetRevisionRef,
    pub tree: Option<TreeDocument>,
    pub title: String,
    pub detail: String,
    /// Media length, when the manifest records one.
    pub seconds: Option<f64>,
    /// Stored size in bytes.
    pub bytes: u64,
    /// A cut's or app run's render.
    pub render: Option<RenderState>,
    /// When its head was published, epoch ms (0: not recorded).
    pub published: u64,
    /// Its main file's media type (`mp4`, `png`, `wav`, `text`).
    pub format: String,
    /// The revision has a stored picture of its own (a video without one
    /// gets its filmstrip's frame, when the card is shown).
    pub pictured: bool,
    /// A cut published without its poster (see [`POSTER_PENDING`]).
    pub poster_pending: bool,
    /// The cut's stable project name.
    pub alias: Option<makepad_asset_data::AssetAlias>,
}

impl LibRow {
    /// A project card belongs to its tree, even when opening its root file.
    pub fn asset_id(&self) -> AssetId {
        self.tree.as_ref().map_or(self.reference.asset_id, |tree| tree.reference.asset_id)
    }
}

/// A decoded thumbnail at its own aspect, `0xAARRGGBB` pixels.
#[derive(Clone, Debug)]
pub struct Thumb {
    pub width: usize,
    pub height: usize,
    pub pixels: Arc<Vec<u32>>,
}

/// A file revision alone cannot identify the revisions of its sibling files.
/// Carry its owning tree revision and logical path all the way to the loader.
#[derive(Clone, Debug)]
pub struct TreeDocument {
    pub reference: AssetRevisionRef,
    pub path: String,
}

/// Query each kind once. DECK opens trees; CUT also opens standalone cuts.
pub fn catalogue_queries(lane: LibLane, text: &str, tag: Option<&str>) -> Vec<CatalogQuery> {
    let trimmed = text.trim();
    [Some(lane.kind()), (lane == LibLane::Cuts).then_some(AssetKind::Tree)]
        .into_iter().flatten().map(|kind| {
            let mut query = if trimmed.is_empty() { CatalogQuery::browse(PAGE_SIZE) } else { CatalogQuery::text(trimmed, PAGE_SIZE) };
            let role = match (lane, kind) {
                (LibLane::Decks, AssetKind::Tree) => Some("deck"),
                (LibLane::Cuts, AssetKind::Tree) => Some("cut"),
                _ => None,
            };
            query.kind = Some(kind);
            query.newest = true;
            // Earlier saves have the role tag but no category. The default
            // Decks query already requires that tag; a chosen tag needs both.
            query.category = role.filter(|_| lane == LibLane::Cuts || tag.is_some()).map(str::to_string);
            query.tag = tag.or(role).or(lane.tag()).map(str::to_string);
            query.exclude_tag = Some(TREE_FILE.into());
            query.facets = 12;
            query
        }).collect()
}

/// Keep the first hit for an asset, including duplicates within one response.
pub fn append_catalogue_hits(hits: &mut Vec<CatalogHit>, added: Vec<CatalogHit>) {
    let mut seen: HashSet<_> = hits.iter().map(|hit| hit.asset_id).collect();
    hits.extend(added.into_iter().filter(|hit| seen.insert(hit.asset_id)));
}

/// Tree files have their own revisions but appear through their project card.
pub fn catalogue_page(client: &mut AssetClient, lane: LibLane, text: &str, tag: Option<&str>) -> Result<CatalogPage, String> {
    let mut queries = catalogue_queries(lane, text, tag).into_iter();
    let mut page = client.catalog_search(&queries.next().expect("every lane has a kind"), None).map_err(redact)?;
    let hits = std::mem::take(&mut page.hits);
    append_catalogue_hits(&mut page.hits, hits);
    for query in queries {
        let other = client.catalog_search(&query, None).map_err(redact)?;
        page.total += other.total;
        append_catalogue_hits(&mut page.hits, other.hits);
        for facet in other.facets {
            if let Some(old) = page.facets.iter_mut().find(|old| old.kind == facet.kind && old.label == facet.label) {
                old.count += facet.count;
            } else { page.facets.push(facet); }
        }
    }
    page.hits.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| b.updated_ms.cmp(&a.updated_ms)).then_with(|| a.asset_id.cmp(&b.asset_id)));
    Ok(page)
}

/// A lane's rows matching `text` (and `tag`), newest first or A–Z, and the
/// lane's tags with their counts. `current`: the query is still wanted (a
/// newer one, or a stop, ends it early). `cuts`: what the cut lanes add;
/// without it their rows have no tree file and no render.
pub fn catalogue(client: &mut AssetClient, lane: LibLane, text: &str, tag: Option<&str>, az: bool, current: &dyn Fn() -> bool, cuts: Option<&dyn CutRows>) -> Result<(Vec<LibRow>, Vec<(String, u64)>), String> {
    let page = catalogue_page(client, lane, text, tag)?;
    let tags = page.facets.iter().filter(|facet| facet.kind == makepad_asset_client::dto::FacetKind::Tag && facet.label != POSTER_PENDING && facet.label != TREE_FILE).map(|facet| (facet.label.clone(), facet.count)).collect::<Vec<_>>();
    let mut rows = Vec::new();
    for hit in page.hits.into_iter().take(PAGE_SIZE as usize) {
        if !current() {
            return Err("cancelled".into());
        }
        let detail = client.asset_detail(&hit.asset_id).map_err(redact)?;
        if detail.retired {
            continue;
        }
        let Some(head) = detail.latest_published() else { continue };
        let reference = AssetRevisionRef { asset_id: detail.asset_id, revision: head.revision };
        let (reference, tree) = match cuts.filter(|_| lane == LibLane::Cuts && hit.kind == Some(AssetKind::Tree)) {
            Some(cuts) => match cuts.tree_cut(client, reference) {
                Ok((file, owner)) => (file, Some(owner)),
                Err(error) => { log!("cut library: {}: {error}", hit.asset_id); continue; }
            },
            None => (reference, None),
        };
        // A head whose revision the server no longer holds opens as "not
        // found: server object": it is not listed, so no card leads to it.
        let manifest = match client.resolve_ref(&reference) {
            Ok(manifest) => Some(manifest),
            Err(makepad_asset_client::ClientError::NotFound { .. }) => { log!("library: {}: its head revision is gone; not listed", hit.asset_id); continue; }
            Err(_) => None,
        };
        let pictured = manifest.as_ref().is_some_and(|manifest| manifest.thumbnail.is_some());
        let poster_pending = lane == LibLane::Cuts && tree.is_none() && poster_pending(client, &hit.asset_id);
        let mut seconds = manifest.as_ref().map(|manifest| manifest.metrics.media_millis).filter(|millis| *millis > 0).map(|millis| millis as f64 / 1000.0);
        let render = match (lane, cuts) {
            (LibLane::Cuts | LibLane::AppRuns, Some(cuts)) => Some(match &tree {
                Some(tree) => cuts.render(client, &tree.reference, Some(&tree.path)),
                None => cuts.render(client, &reference, None),
            }.unwrap_or(RenderState::None)),
            _ => None,
        };
        // A cut's length: its render's, when it has one.
        if seconds.is_none() {
            if let Some(render) = render.and_then(RenderState::render) {
                seconds = client.resolve_ref(&render).ok().map(|manifest| manifest.metrics.media_millis).filter(|millis| *millis > 0).map(|millis| millis as f64 / 1000.0);
            }
        }
        let published = detail.latest_published().and_then(|head| head.published_ms).unwrap_or(0);
        let format = manifest.as_ref().and_then(|manifest| manifest.files.first().map(|file| match &file.dims {
            Some(dims) => format!("{:?} · {}×{}", file.media, dims.width, dims.height),
            None => format!("{:?}", file.media),
        })).unwrap_or_default();
        let title = if hit.title.is_empty() { hit.asset_id.to_string() } else { hit.title };
        let mut detail = match seconds {
            Some(seconds) => format!("{seconds:.1} s"),
            None => if hit.namespace.is_empty() { hit.creator } else { hit.namespace },
        };
        if let Some(render) = render {
            detail = render.label().to_string();
        }
        let bytes = manifest.as_ref().map_or(0, |manifest| manifest.metrics.total_bytes);
        let alias = if tree.is_some() { None } else { hit.alias };
        rows.push(LibRow { reference, tree, title, detail, seconds, bytes, render, published, format, pictured, poster_pending, alias });
    }
    if az {
        rows.sort_by_key(|row| row.title.to_lowercase());
    }
    Ok((rows, tags))
}

/// The cut was published without its poster (the tag is its asset's).
pub fn poster_pending(client: &mut AssetClient, asset: &AssetId) -> bool {
    client.get_annotation(asset).is_ok_and(|note| note.tags.iter().any(|tag| tag == POSTER_PENDING))
}

pub fn load_thumb(client: &mut AssetClient, reference: &AssetRevisionRef) -> Option<Thumb> {
    let manifest = client.resolve_ref(reference).ok()?;
    let Some(meta) = manifest.thumbnail.clone() else { return image_thumb(client, &manifest) };
    if meta.width == 0 || meta.height == 0 || meta.width > THUMB_EDGE || meta.height > THUMB_EDGE || meta.byte_len == 0 || meta.byte_len > MAX_THUMB_ENCODED {
        return image_thumb(client, &manifest);
    }
    let bytes = client.fetch_blob_bytes(&meta.blob, Some(meta.byte_len)).ok()?;
    let image = match meta.media {
        ThumbnailMedia::Jpeg => ImageBuffer::from_jpg(&bytes).ok()?,
        ThumbnailMedia::Png => {
            bounded_png_metadata(&bytes)?;
            ImageBuffer::from_png(&bytes).ok()?
        }
    };
    if image.animation.is_some() || image.width != meta.width as usize || image.height != meta.height as usize || image.data.len() != image.width * image.height {
        return None;
    }
    Some(shrink(&image.data, image.width, image.height))
}

/// An image asset without a usable thumbnail: its own picture, shrunk
/// (within the image asset's own ceilings: `IMAGE_EDGE`, `MAX_IMAGE_ENCODED`).
pub fn image_thumb(client: &mut AssetClient, manifest: &makepad_asset_data::AssetManifest) -> Option<Thumb> {
    let file = manifest.files.iter().find(|file| matches!(file.media, MediaType::Png | MediaType::Jpeg))?;
    let dims = file.dims?;
    if dims.width == 0 || dims.height == 0 || dims.width > IMAGE_EDGE || dims.height > IMAGE_EDGE || file.byte_len > MAX_IMAGE_ENCODED {
        return None;
    }
    let bytes = client.fetch_blob_bytes(&file.blob, Some(file.byte_len)).ok()?;
    let image = match file.media {
        MediaType::Jpeg => ImageBuffer::from_jpg(&bytes).ok()?,
        _ => {
            bounded_png_metadata(&bytes)?;
            ImageBuffer::from_png(&bytes).ok()?
        }
    };
    if image.animation.is_some() || image.data.len() != image.width * image.height {
        return None;
    }
    Some(shrink(&image.data, image.width, image.height))
}

/// Fit inside `THUMB_BOX` at the source's aspect, nearest sample.
pub fn shrink(src: &[u32], width: usize, height: usize) -> Thumb {
    let scale = (THUMB_BOX as f64 / width.max(height) as f64).min(1.0);
    let (w, h) = (((width as f64 * scale) as usize).max(1), ((height as f64 * scale) as usize).max(1));
    let mut pixels = Vec::with_capacity(w * h);
    for y in 0..h {
        let sy = y * height / h;
        for x in 0..w {
            pixels.push(src[sy * width + x * width / w] | 0xff00_0000);
        }
    }
    Thumb { width: w, height: h, pixels: Arc::new(pixels) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_lanes_query_each_kind_once() {
        let decks = catalogue_queries(LibLane::Decks, "", None);
        assert_eq!(decks.len(), 1);
        assert_eq!(decks[0].kind, Some(AssetKind::Tree));
        assert_eq!(decks[0].category, None);
        assert_eq!(decks[0].tag.as_deref(), Some("deck"));
        assert_eq!(catalogue_queries(LibLane::Decks, "", Some("favorite"))[0].category.as_deref(), Some("deck"));
        let cuts = catalogue_queries(LibLane::Cuts, "", None);
        assert_eq!(cuts.iter().map(|query| query.kind).collect::<Vec<_>>(), [Some(AssetKind::Cut), Some(AssetKind::Tree)]);
        assert_eq!(cuts[0].category, None);
        assert_eq!(cuts[0].tag, None);
        assert_eq!(cuts[1].category.as_deref(), Some("cut"));
        assert_eq!(cuts[1].tag.as_deref(), Some("cut"));
        for lane in [LibLane::Decks, LibLane::Cuts, LibLane::Video] {
            for query in catalogue_queries(lane, "  example  ", Some("favorite")) {
                assert_eq!(query.text, "example");
                assert_eq!(query.tag.as_deref(), Some("favorite"));
                assert_eq!(query.exclude_tag.as_deref(), Some(TREE_FILE));
            }
        }
    }

    /// DECK's library is the Decks lane alone, with or without a search:
    /// one query, for deck trees, that must carry the `deck` role tag, so no
    /// media, cut or other tree kind can be listed.
    #[test]
    fn deck_library_query_returns_only_deck_kinds() {
        for text in ["", "  quarterly  "] {
            let queries = catalogue_queries(LibLane::Decks, text, None);
            assert_eq!(queries.len(), 1);
            assert_eq!(queries[0].kind, Some(AssetKind::Tree));
            assert_eq!(queries[0].tag.as_deref(), Some("deck"));
            assert_eq!(queries[0].text, text.trim());
        }
    }

    #[test]
    fn catalogue_hits_are_unique_by_asset_id_not_title_or_alias() {
        let hit = |id, title: &str| CatalogHit {
            asset_id: AssetId::from_bytes([id; 16]), namespace: "stage".into(),
            kind: Some(AssetKind::Tree), title: title.into(), creator: String::new(),
            artist: String::new(), artist_url: String::new(), album: String::new(),
            source_url: String::new(), license: String::new(), license_url: String::new(),
            snippet: String::new(), score: 0, live: true, alias: None, updated_ms: 0,
        };
        let first = hit(1, "Stage tour");
        let same_title = hit(2, "Stage tour");
        let mut duplicate = hit(1, "Renamed tour");
        duplicate.alias = Some("stage/examples/tour/tree".parse().unwrap());
        let mut hits = Vec::new();
        append_catalogue_hits(&mut hits, vec![first.clone(), same_title.clone(), duplicate.clone()]);
        append_catalogue_hits(&mut hits, vec![duplicate, same_title.clone(), hit(3, "Motion"), hit(3, "Motion")]);
        assert_eq!(hits, vec![first, same_title, hit(3, "Motion")]);
    }

    #[test]
    fn tree_cards_are_keyed_by_their_project_asset() {
        let reference = |id| AssetRevisionRef {
            asset_id: AssetId::from_bytes([id; 16]),
            revision: makepad_asset_data::AssetRevisionId::from_bytes([id; 32]),
        };
        let mut row = LibRow {
            reference: reference(1), tree: None, title: "Harbour tide".into(), detail: String::new(),
            seconds: None, bytes: 0, render: None, published: 0, format: String::new(),
            pictured: false, poster_pending: false, alias: None,
        };
        assert_eq!(row.asset_id(), reference(1).asset_id);
        row.tree = Some(TreeDocument { reference: reference(2), path: "cut.splash".into() });
        let mut other = row.clone();
        other.tree.as_mut().unwrap().reference = reference(3);
        assert_eq!(row.asset_id(), reference(2).asset_id);
        assert_ne!(row.asset_id(), other.asset_id());
        assert_eq!(row.reference, other.reference);
        assert_eq!(row.title, other.title);
    }

    #[test]
    fn thumbnails_keep_their_aspect() {
        let thumb = shrink(&vec![0xff112233; 1024 * 512], 1024, 512);
        assert_eq!((thumb.width, thumb.height), (256, 128));
        let small = shrink(&vec![0; 100 * 50], 100, 50);
        assert_eq!((small.width, small.height), (100, 50));
    }
}
