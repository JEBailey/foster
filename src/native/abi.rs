//! Stable symbols shared by generated native code and the linked platform runtime.
//!
//! The version is part of every imported symbol. An object compiled against a different runtime
//! therefore fails at link time instead of silently calling an incompatible function.

pub const VERSION: u16 = 2;

pub const ALLOC: &str = "foster_rt_v2_alloc";
pub const DEALLOC: &str = "foster_rt_v2_dealloc";
pub const ASSERT: &str = "foster_rt_v2_assert";
pub const FAIL: &str = "foster_rt_v2_fail";

pub const STRING_CONSTANT: &str = "foster_rt_v2_string_constant";
pub const STRING_EMPTY: &str = "foster_rt_v2_string_empty";
pub const STRING_LENGTH: &str = "foster_rt_v2_string_length";
pub const STRING_HEAD: &str = "foster_rt_v2_string_head";
pub const STRING_REST: &str = "foster_rt_v2_string_rest";
pub const STRING_WHITESPACE: &str = "foster_rt_v2_string_whitespace";
pub const STRING_CONCAT: &str = "foster_rt_v2_string_concat";
pub const STRING_GET: &str = "foster_rt_v2_string_get";
pub const STRING_EQUAL: &str = "foster_rt_v2_string_equal";
pub const OBJECT_EQUAL: &str = "foster_rt_v2_object_equal";
pub const COPY_BYTES: &str = "foster_rt_v2_copy_bytes";
pub const CODE_POINT_WHITESPACE: &str = "foster_rt_v2_code_point_whitespace";
pub const CODE_POINT_STRING: &str = "foster_rt_v2_code_point_string";
pub const PARSE_FLOAT: &str = "foster_rt_v2_parse_float";
pub const FORMAT_FLOAT: &str = "foster_rt_v2_format_float";

// Platform services use a small family of argument-shape entry points. Every call returns an
// opaque temporary response; generated code copies its contents into descriptor-backed Foster
// values and then releases it.
pub const HOST_CALL_NULLARY: &str = "foster_rt_v2_host_call_nullary";
pub const HOST_CALL_STRING: &str = "foster_rt_v2_host_call_string";
pub const HOST_CALL_STRINGS: &str = "foster_rt_v2_host_call_strings";
pub const HOST_CALL_STRING_INTS: &str = "foster_rt_v2_host_call_string_ints";
pub const HOST_CALL_INT: &str = "foster_rt_v2_host_call_int";
pub const HOST_CALL_INTS: &str = "foster_rt_v2_host_call_ints";
pub const HOST_CALL_STRING_BYTES: &str = "foster_rt_v2_host_call_string_bytes";
pub const HOST_CALL_INT_BYTES: &str = "foster_rt_v2_host_call_int_bytes";
pub const HOST_CALL_INT_STRING: &str = "foster_rt_v2_host_call_int_string";
pub const HOST_REQUIRE_OK: &str = "foster_rt_v2_host_require_ok";
pub const HOST_OK: &str = "foster_rt_v2_host_ok";
pub const HOST_INTEGER: &str = "foster_rt_v2_host_integer";
pub const HOST_ERROR_VALUE: &str = "foster_rt_v2_host_error_value";
pub const HOST_STRING: &str = "foster_rt_v2_host_string";
pub const HOST_BYTES_LENGTH: &str = "foster_rt_v2_host_bytes_length";
pub const HOST_COPY_BYTES: &str = "foster_rt_v2_host_copy_bytes";
pub const HOST_STRINGS_LENGTH: &str = "foster_rt_v2_host_strings_length";
pub const HOST_RELEASE: &str = "foster_rt_v2_host_release";

// Remote actors use fixed-width words so callback thunks have one signature for every Foster
// scalar-or-pointer specialization. Futures own completed managed results until `await` transfers
// the word back into generated code.
pub const REMOTE_SPAWN: &str = "foster_rt_v2_remote_spawn";
pub const REMOTE_CALL: &str = "foster_rt_v2_remote_call";
pub const FUTURE_AWAIT: &str = "foster_rt_v2_future_await";
pub const REMOTE_RELEASE: &str = "foster_rt_v2_remote_release";
pub const FUTURE_RELEASE: &str = "foster_rt_v2_future_release";

/// String slots exposed by [`HOST_STRING`].
pub mod host_string {
    pub const VALUE: i64 = 0;
    pub const ERROR_OPERATION: i64 = 1;
    pub const ERROR_PATH: i64 = 2;
    pub const ERROR_MESSAGE: i64 = 3;
    pub const LIST_VALUE: i64 = 4;
}

pub const REF_LOAD_I8: &str = "foster_rt_v2_ref_load_i8";
pub const REF_LOAD_I32: &str = "foster_rt_v2_ref_load_i32";
pub const REF_LOAD_I64: &str = "foster_rt_v2_ref_load_i64";
pub const REF_LOAD_F64: &str = "foster_rt_v2_ref_load_f64";
pub const REF_LOAD_PTR: &str = "foster_rt_v2_ref_load_ptr";
pub const REF_STORE_I8: &str = "foster_rt_v2_ref_store_i8";
pub const REF_STORE_I32: &str = "foster_rt_v2_ref_store_i32";
pub const REF_STORE_I64: &str = "foster_rt_v2_ref_store_i64";
pub const REF_STORE_F64: &str = "foster_rt_v2_ref_store_f64";
pub const REF_STORE_PTR: &str = "foster_rt_v2_ref_store_ptr";

pub const WRITE_UNIT: &str = "foster_rt_v2_write_unit";
pub const WRITE_BOOL: &str = "foster_rt_v2_write_bool";
pub const WRITE_INT: &str = "foster_rt_v2_write_int";
pub const WRITE_FLOAT: &str = "foster_rt_v2_write_float";
pub const WRITE_CODE_POINT: &str = "foster_rt_v2_write_code_point";
pub const WRITE_BYTE: &str = "foster_rt_v2_write_byte";
pub const WRITE_STRING: &str = "foster_rt_v2_write_string";
pub const WRITE_OBJECT: &str = "foster_rt_v2_write_object";
pub const WRITE_SEPARATOR: &str = "foster_rt_v2_write_separator";
pub const WRITE_NEWLINE: &str = "foster_rt_v2_write_newline";

/// Stable error categories accepted by [`FAIL`].
pub mod failure {
    pub const INTEGER_OVERFLOW: i64 = 1;
    pub const DIVISION: i64 = 2;
    pub const INVALID_SHIFT: i64 = 3;
    pub const INDEX_OUT_OF_BOUNDS: i64 = 4;
    pub const INVALID_CODE_POINT: i64 = 5;
    pub const INVALID_BYTE: i64 = 6;
    pub const CONTRACT_DISPATCH: i64 = 7;
}

pub const HOST_INITIALIZE: &str = "foster_rt_v2_host_initialize";

/// Exact Rust/C wire types. Signedness is retained for runtime compile-time checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireType {
    U8,
    U32,
    I64,
    U64,
    F64,
    Pointer,
}

impl WireType {
    pub fn representation(self) -> crate::codegen::ir::Representation {
        use crate::codegen::ir::Representation;
        match self {
            Self::U8 => Representation::I8,
            Self::U32 => Representation::I32,
            Self::I64 | Self::U64 => Representation::I64,
            Self::F64 => Representation::F64,
            Self::Pointer => Representation::Pointer,
        }
    }
    fn rust_name(self) -> &'static str {
        match self {
            Self::U8 => "u8",
            Self::U32 => "u32",
            Self::I64 => "i64",
            Self::U64 => "u64",
            Self::F64 => "f64",
            Self::Pointer => "usize",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgumentOwnership {
    Value,
    Borrowed,
    Consumed,
    /// A payload word whose release callback determines whether ownership is transferred.
    CallbackManaged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultOwnership {
    Scalar,
    Borrowed,
    Owned,
    Transferred,
}

#[derive(Debug)]
pub struct RuntimeFunction {
    pub symbol: &'static str,
    pub parameters: &'static [(WireType, ArgumentOwnership)],
    pub result: WireType,
    pub result_ownership: ResultOwnership,
}

macro_rules! runtime_functions {
    ($($name:ident: ($($ty:ident $ownership:ident),*) -> $result:ident $result_ownership:ident;)*) => {
        pub static FUNCTIONS: &[RuntimeFunction] = &[$(
            RuntimeFunction { symbol: $name, parameters: &[$((WireType::$ty, ArgumentOwnership::$ownership)),*],
                result: WireType::$result, result_ownership: ResultOwnership::$result_ownership },
        )*];
    }
}

// This is the single signature/ownership contract used by imports and checked against runtime exports.
runtime_functions! {
    ALLOC: (I64 Value, I64 Value) -> Pointer Owned;
    DEALLOC: (Pointer Consumed, I64 Value, I64 Value) -> U8 Scalar;
    ASSERT: (U8 Value, Pointer Borrowed) -> U8 Scalar;
    FAIL: (I64 Value, I64 Value, I64 Value) -> U8 Scalar;
    WRITE_UNIT: () -> U8 Scalar;
    WRITE_BOOL: (U8 Value) -> U8 Scalar;
    WRITE_INT: (I64 Value) -> U8 Scalar;
    WRITE_FLOAT: (F64 Value) -> U8 Scalar;
    WRITE_CODE_POINT: (U32 Value) -> U8 Scalar;
    WRITE_BYTE: (U8 Value) -> U8 Scalar;
    WRITE_STRING: (Pointer Borrowed) -> U8 Scalar;
    WRITE_OBJECT: (Pointer Borrowed) -> U8 Scalar;
    WRITE_SEPARATOR: () -> U8 Scalar;
    WRITE_NEWLINE: () -> U8 Scalar;
    STRING_CONSTANT: (I64 Value) -> Pointer Owned;
    STRING_EMPTY: (Pointer Borrowed) -> U8 Scalar;
    STRING_LENGTH: (Pointer Borrowed) -> I64 Scalar;
    STRING_HEAD: (Pointer Borrowed) -> U32 Scalar;
    STRING_REST: (Pointer Borrowed) -> Pointer Owned;
    STRING_WHITESPACE: (Pointer Borrowed) -> U8 Scalar;
    STRING_CONCAT: (Pointer Borrowed, Pointer Borrowed) -> Pointer Owned;
    COPY_BYTES: (Pointer Borrowed, Pointer Borrowed, I64 Value) -> U8 Scalar;
    CODE_POINT_WHITESPACE: (U32 Value) -> U8 Scalar;
    CODE_POINT_STRING: (U32 Value) -> Pointer Owned;
    STRING_GET: (Pointer Borrowed, I64 Value) -> U32 Scalar;
    STRING_EQUAL: (Pointer Borrowed, Pointer Borrowed) -> U8 Scalar;
    PARSE_FLOAT: (Pointer Borrowed) -> F64 Scalar;
    FORMAT_FLOAT: (F64 Value) -> Pointer Owned;
    REF_LOAD_I8: (Pointer Borrowed) -> U8 Scalar;
    REF_LOAD_I32: (Pointer Borrowed) -> U32 Scalar;
    REF_LOAD_I64: (Pointer Borrowed) -> I64 Scalar;
    REF_LOAD_F64: (Pointer Borrowed) -> F64 Scalar;
    REF_LOAD_PTR: (Pointer Borrowed) -> Pointer Borrowed;
    REF_STORE_I8: (Pointer Borrowed, U8 Value) -> U8 Scalar;
    REF_STORE_I32: (Pointer Borrowed, U32 Value) -> U8 Scalar;
    REF_STORE_I64: (Pointer Borrowed, I64 Value) -> U8 Scalar;
    REF_STORE_F64: (Pointer Borrowed, F64 Value) -> U8 Scalar;
    REF_STORE_PTR: (Pointer Borrowed, Pointer Borrowed) -> U8 Scalar;
    HOST_INITIALIZE: () -> U8 Scalar;
    HOST_CALL_NULLARY: (I64 Value) -> Pointer Owned;
    HOST_CALL_STRING: (I64 Value, Pointer Borrowed) -> Pointer Owned;
    HOST_CALL_STRINGS: (I64 Value, Pointer Borrowed, Pointer Borrowed) -> Pointer Owned;
    HOST_CALL_STRING_INTS: (I64 Value, Pointer Borrowed, I64 Value, I64 Value) -> Pointer Owned;
    HOST_CALL_INT: (I64 Value, I64 Value) -> Pointer Owned;
    HOST_CALL_INTS: (I64 Value, I64 Value, I64 Value) -> Pointer Owned;
    HOST_CALL_STRING_BYTES: (I64 Value, Pointer Borrowed, Pointer Borrowed, I64 Value) -> Pointer Owned;
    HOST_CALL_INT_BYTES: (I64 Value, I64 Value, Pointer Borrowed, I64 Value) -> Pointer Owned;
    HOST_CALL_INT_STRING: (I64 Value, I64 Value, Pointer Borrowed) -> Pointer Owned;
    HOST_REQUIRE_OK: (Pointer Borrowed) -> U8 Scalar;
    HOST_OK: (Pointer Borrowed) -> U8 Scalar;
    HOST_INTEGER: (Pointer Borrowed, I64 Value) -> I64 Scalar;
    HOST_ERROR_VALUE: (Pointer Borrowed) -> I64 Scalar;
    HOST_STRING: (Pointer Borrowed, I64 Value, I64 Value) -> Pointer Owned;
    HOST_BYTES_LENGTH: (Pointer Borrowed) -> I64 Scalar;
    HOST_COPY_BYTES: (Pointer Borrowed, Pointer Borrowed) -> U8 Scalar;
    HOST_STRINGS_LENGTH: (Pointer Borrowed) -> I64 Scalar;
    HOST_RELEASE: (Pointer Consumed) -> U8 Scalar;
    REMOTE_SPAWN: (U64 CallbackManaged, Pointer Borrowed, U8 Value) -> Pointer Owned;
    REMOTE_CALL: (Pointer Borrowed, Pointer Borrowed, Pointer Borrowed, I64 Value, U8 Value, Pointer Borrowed) -> Pointer Owned;
    FUTURE_AWAIT: (Pointer Borrowed) -> U64 Transferred;
    REMOTE_RELEASE: (Pointer Consumed) -> U8 Scalar;
    FUTURE_RELEASE: (Pointer Consumed) -> U8 Scalar;
    OBJECT_EQUAL: (Pointer Borrowed, Pointer Borrowed) -> U8 Scalar;
}

pub fn lookup(symbol: &str) -> Option<&'static RuntimeFunction> {
    FUNCTIONS.iter().find(|function| function.symbol == symbol)
}

/// Verify an IR call before lowering it to the platform C ABI.
pub(crate) fn verify_call(
    symbol: &str,
    signature: &crate::codegen::ir::Signature,
) -> Result<&'static RuntimeFunction, String> {
    let function =
        lookup(symbol).ok_or_else(|| format!("unregistered runtime helper `{symbol}`"))?;
    if function.parameters.len() != signature.parameters.len()
        || function
            .parameters
            .iter()
            .zip(&signature.parameters)
            .any(|((wire, _), ty)| wire.representation() != ty.representation())
        || function.result.representation() != signature.result.representation()
    {
        return Err(format!(
            "runtime helper `{symbol}` has an incompatible call signature"
        ));
    }
    Ok(function)
}

/// Rust checks every linked export against the same contracts used by code generation.
pub(super) fn runtime_assertions() -> String {
    use std::fmt::Write;
    let mut output = String::new();
    for function in FUNCTIONS {
        let parameters = function
            .parameters
            .iter()
            .map(|(ty, _)| ty.rust_name())
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(
            output,
            "const _: extern \"C\" fn({parameters}) -> {} = {};",
            function.result.rust_name(),
            function.symbol
        )
        .unwrap();
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn contracts_have_unique_versioned_symbols_and_pointer_ownership() {
        let mut seen = std::collections::HashSet::new();
        for function in FUNCTIONS {
            assert!(seen.insert(function.symbol), "duplicate runtime symbol");
            assert!(
                function
                    .symbol
                    .starts_with(&format!("foster_rt_v{VERSION}_"))
            );
            if function.result == WireType::Pointer {
                assert_ne!(function.result_ownership, ResultOwnership::Scalar);
            }
        }
    }
    #[test]
    fn rejects_unknown_helpers_and_mismatched_wire_types() {
        use crate::codegen::ir::{Signature, Type};
        let valid = Signature {
            parameters: vec![Type::String, Type::String],
            result: Type::Bool,
        };
        assert!(verify_call(STRING_EQUAL, &valid).is_ok());
        assert!(verify_call("missing", &valid).is_err());
        let invalid = Signature {
            parameters: vec![Type::Float, Type::String],
            result: Type::Bool,
        };
        assert!(verify_call(STRING_EQUAL, &invalid).is_err());
    }
}
