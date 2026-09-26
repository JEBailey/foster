//! Authoritative intrinsic identities and metadata shared by compiler, tooling, and runtimes.

mod registry;

pub use registry::{
    BUILTINS, Builtin, BuiltinDescriptor, BuiltinExecution, Intrinsic, IntrinsicArgumentMode,
    IntrinsicParameter, IntrinsicParameters, IntrinsicReceiverMode, IntrinsicSignature,
    IntrinsicType, NativeInlineIntrinsic, NativeIntrinsic, NativeReceiverKind, OPCODE_INTRINSICS,
    OpcodeIntrinsic, OpcodeIntrinsicDescriptor, native_member_runtime,
};
