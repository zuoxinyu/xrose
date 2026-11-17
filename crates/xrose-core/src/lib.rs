//! Xrose core: everything that is not UI.
//!
//! The crate is organised around a UI-agnostic [`model::AppModel`] (user intent),
//! a [`core::CoreManager`] (binaries + process lifecycle) and two
//! [`adapters::CoreAdapter`] implementations that compile the model into the
//! very different wire formats of Xray and sing-box.

pub mod adapters;
pub mod bundled;
pub mod core;
pub mod diagnostics;
pub mod dns;
pub mod error;
pub mod flow;
pub mod mesh;
pub mod model;
pub mod paths;
pub mod privacy;
pub mod route;
pub mod runtime;
pub mod service;
pub mod settings;
pub mod storage;
pub mod subscription;
pub mod system;
pub mod trace;
pub mod traffic_statistics;
pub mod updates;
pub mod util;

pub use error::{Error, Result};
pub use model::AppModel;
pub use service::AppService;
pub use settings::Settings;
