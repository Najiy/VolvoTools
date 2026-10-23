//! Backend logic of the VolvoTools UI.
//!
//! The ECU protocol itself lives in the C++ `VolvoFlasher`. This crate drives it through a
//! JSON-lines protocol and adds what a GUI needs around it: a verified backup library, a
//! flash plan check, and safety gates that are enforced here rather than in the front end.

mod backup;
pub mod core;
pub mod error;
pub mod events;
mod flash;
pub mod flasher;
pub mod hashing;
pub mod history;
mod jobs;
pub mod library;
pub mod profile;
pub mod protocol;
pub mod runner;
pub mod settings;
mod text;

pub use crate::core::{Core, CoreOptions, Dirs};
pub use crate::error::CoreError;
pub use crate::flash::{
    CONFIRM_PHRASE, ChecksumStatus, ChecksumVerdict, FileInfo, FlashPreview, FlashRequest,
    PREFLIGHT_ITEMS,
};
