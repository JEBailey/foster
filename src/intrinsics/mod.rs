//! Authoritative intrinsic identities and metadata shared by compiler, tooling, and runtimes.

mod registry;

pub use registry::{BUILTINS, Builtin, BuiltinDescriptor, Intrinsic};
