//! Explicitly built C bridges. Loading an artifact never invokes a compiler.
mod bridge;
pub mod runtime;
pub use bridge::{Manifest, build};
