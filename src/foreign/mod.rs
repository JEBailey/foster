//! Explicitly built C bridges. Loading an artifact never invokes a compiler.
mod bridge;
pub use bridge::{Manifest, build};
pub use foster_host::foreign as runtime;
