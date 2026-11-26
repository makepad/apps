//! A new video from one ask: the song written and
//! sung on the AI Hub, its words timed, a storyboard over its sections,
//! one Motion scene per section and the cut that lays them over the song.
//!
//! Pure: the production record and its steps ([`plan`]), the lyric
//! writer's brief and answer ([`lyrics`]), the song timing module
//! ([`timing`]), the storyboard's brief and answer ([`story`]) and the
//! edit's source ([`cut`]). Stage runs the steps (`video_cut/new_video.rs`).

pub mod edit;
pub mod lyrics;
pub mod plan;
pub mod story;
pub mod timing;

pub use plan::{AssetRef, LineSpan, Production, Scene, ScenePlan, Section, Status, Step, StepRecord};
