//! Container runtime + app store.
//!
//! Everything that talks to the Docker daemon lives in [`docker`]; the shipped
//! template catalog and the on-disk compose files live in [`store`]; [`host`]
//! answers "what address does this box serve on" so the UI can build links.

pub mod docker;
pub mod error;
pub mod host;
pub mod model;
pub mod store;
