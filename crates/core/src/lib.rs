//! Shared types and the `Driver` trait for OurDB.
//!
//! Every database driver crate implements [`Driver`]. The UI crate only talks to
//! `Arc<dyn Driver>` and never knows which database is behind it.

pub mod export;
pub mod model;
pub mod sql;
pub mod tunnel;
pub mod value;

pub use model::*;
pub use value::Value;
