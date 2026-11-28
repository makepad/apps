pub mod build_script;
mod config;
pub mod inventory;
pub mod manifest;
pub mod modules;
pub mod notices;
pub mod rules;
pub mod stage;
pub mod strip;
pub mod table;

pub type SyncError = String;
mod files;
pub mod patch;
pub mod install;
pub mod summary;
pub mod doc;
