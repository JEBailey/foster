//! Stable builtin tags and declarative VM/native source-intrinsic metadata.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntrinsicType {
    Any,
    Unit,
    Bool,
    Integer,
    Float,
    CodePoint,
    Byte,
    Bytes,
    ByteBuffer,
    String,
    ListByte,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntrinsicArgumentMode {
    Read,
    Consume,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntrinsicParameter {
    pub ty: IntrinsicType,
    pub mode: IntrinsicArgumentMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntrinsicParameters {
    Fixed(&'static [IntrinsicParameter]),
    Variadic(IntrinsicParameter),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntrinsicSignature {
    pub parameters: IntrinsicParameters,
    pub result: IntrinsicType,
}

impl IntrinsicSignature {
    pub fn accepts_arity(self, arity: usize) -> bool {
        match self.parameters {
            IntrinsicParameters::Fixed(parameters) => parameters.len() == arity,
            IntrinsicParameters::Variadic(_) => true,
        }
    }

    pub fn parameter(self, index: usize) -> Option<IntrinsicParameter> {
        match self.parameters {
            IntrinsicParameters::Fixed(parameters) => parameters.get(index).copied(),
            IntrinsicParameters::Variadic(parameter) => Some(parameter),
        }
    }
}

/// How the VM obtains arguments before invoking a builtin handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinExecution {
    Direct,
    Host,
    TransformFirst,
    ConsumeFirst,
}

/// Native lowering selected from the same registry as VM execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeIntrinsic {
    Unavailable,
    Print {
        newline: bool,
    },
    Inline(NativeInlineIntrinsic),
    Runtime(&'static str),
    /// A typed call through the versioned native platform-service ABI.
    Host,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeInlineIntrinsic {
    IntegerToCodePoint,
    ByteIsValid,
    IntegerToByte,
    BytesFromList,
    BytesToList,
    BytesDecodeUtf8,
}

pub(crate) type DirectBuiltinHandler = fn(
    &[crate::vm::Value],
    Option<crate::hir::RecordId>,
) -> Result<crate::vm::Value, crate::error::RuntimeError>;

pub(crate) type ConsumingBuiltinHandler =
    fn(
        crate::vm::Value,
        &[crate::vm::Value],
        Option<crate::hir::RecordId>,
    ) -> Result<crate::vm::Value, crate::error::RuntimeError>;

#[derive(Debug, Clone, Copy)]
pub(crate) enum BuiltinHandler {
    Direct(DirectBuiltinHandler),
    ConsumeFirst(ConsumingBuiltinHandler),
}

#[derive(Debug, Clone, Copy)]
pub struct BuiltinDescriptor {
    pub builtin: Builtin,
    /// Stable tag stored in Foster bytecode. Existing values must never be renumbered.
    pub bytecode_tag: u8,
    /// Unqualified source name for builtins that do not use an intrinsic declaration.
    pub source_name: Option<&'static str>,
    /// Key used by `intrinsic("...")` declarations.
    pub intrinsic_key: Option<&'static str>,
    /// Module that is allowed to declare `intrinsic_key`.
    pub module: Option<&'static str>,
    pub signature: IntrinsicSignature,
    pub execution: BuiltinExecution,
    /// Native policy: scalar codegen, a typed platform import, or deliberately unavailable.
    pub native: NativeIntrinsic,
    pub(crate) handler: Option<BuiltinHandler>,
}

macro_rules! intrinsic_parameters {
    ([$($mode:ident $ty:ident),* $(,)?]) => {
        IntrinsicParameters::Fixed(&[
            $(IntrinsicParameter {
                ty: IntrinsicType::$ty,
                mode: IntrinsicArgumentMode::$mode,
            }),*
        ])
    };
    ((variadic $mode:ident $ty:ident)) => {
        IntrinsicParameters::Variadic(IntrinsicParameter {
            ty: IntrinsicType::$ty,
            mode: IntrinsicArgumentMode::$mode,
        })
    };
}

macro_rules! builtin_descriptors {
    ($(
        $builtin:ident = $tag:literal,
        source: $source:expr,
        intrinsic: $key:expr => $module:expr,
        execution: $execution:ident,
        signature: $parameters:tt -> $result:ident;
    )+) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Builtin {
            $($builtin),+
        }

        pub const BUILTINS: &[BuiltinDescriptor] = &[
            $(BuiltinDescriptor {
                builtin: Builtin::$builtin,
                bytecode_tag: $tag,
                source_name: $source,
                intrinsic_key: $key,
                module: $module,
                signature: IntrinsicSignature {
                    parameters: intrinsic_parameters!($parameters),
                    result: IntrinsicType::$result,
                },
                execution: BuiltinExecution::$execution,
                native: native_builtin!($builtin),
                handler: builtin_handler!($execution, $builtin),
            }),+
        ];
    };
}

macro_rules! native_builtin {
    (Print) => {
        NativeIntrinsic::Print { newline: false }
    };
    (Println) => {
        NativeIntrinsic::Print { newline: true }
    };
    (FromCodePoint) => {
        NativeIntrinsic::Inline(NativeInlineIntrinsic::IntegerToCodePoint)
    };
    (ByteValid) => {
        NativeIntrinsic::Inline(NativeInlineIntrinsic::ByteIsValid)
    };
    (ByteUnchecked) => {
        NativeIntrinsic::Inline(NativeInlineIntrinsic::IntegerToByte)
    };
    (ParseFloat) => {
        NativeIntrinsic::Runtime(crate::native::abi::PARSE_FLOAT)
    };
    (FormatFloat) => {
        NativeIntrinsic::Runtime(crate::native::abi::FORMAT_FLOAT)
    };
    (BytesFromList) => {
        NativeIntrinsic::Inline(NativeInlineIntrinsic::BytesFromList)
    };
    (BytesToList) => {
        NativeIntrinsic::Inline(NativeInlineIntrinsic::BytesToList)
    };
    (BytesDecodeUtf8) => {
        NativeIntrinsic::Inline(NativeInlineIntrinsic::BytesDecodeUtf8)
    };
    (IoReadText) => {
        NativeIntrinsic::Host
    };
    (IoWriteText) => {
        NativeIntrinsic::Host
    };
    (IoReadBytes) => {
        NativeIntrinsic::Host
    };
    (IoWriteBytes) => {
        NativeIntrinsic::Host
    };
    (IoListDirectory) => {
        NativeIntrinsic::Host
    };
    (IoExists) => {
        NativeIntrinsic::Host
    };
    (IoIsFile) => {
        NativeIntrinsic::Host
    };
    (IoIsDirectory) => {
        NativeIntrinsic::Host
    };
    (IoCreateDirectory) => {
        NativeIntrinsic::Host
    };
    (IoCreateDirectoryAll) => {
        NativeIntrinsic::Host
    };
    (IoRemoveFile) => {
        NativeIntrinsic::Host
    };
    (IoRemoveDirectory) => {
        NativeIntrinsic::Host
    };
    (IoRename) => {
        NativeIntrinsic::Host
    };
    (IoCopyFile) => {
        NativeIntrinsic::Host
    };
    (IoJoin) => {
        NativeIntrinsic::Host
    };
    (IoParent) => {
        NativeIntrinsic::Host
    };
    (IoFileName) => {
        NativeIntrinsic::Host
    };
    (IoExtension) => {
        NativeIntrinsic::Host
    };
    (IoCanonicalize) => {
        NativeIntrinsic::Host
    };
    (IoCurrentDirectory) => {
        NativeIntrinsic::Host
    };
    (TcpListen) => {
        NativeIntrinsic::Host
    };
    (TcpConnect) => {
        NativeIntrinsic::Host
    };
    (TcpAccept) => {
        NativeIntrinsic::Host
    };
    (TcpRead) => {
        NativeIntrinsic::Host
    };
    (TcpWrite) => {
        NativeIntrinsic::Host
    };
    (TcpReadBytes) => {
        NativeIntrinsic::Host
    };
    (TcpWriteBytes) => {
        NativeIntrinsic::Host
    };
    (TcpSetTimeout) => {
        NativeIntrinsic::Host
    };
    (TcpCloseListener) => {
        NativeIntrinsic::Host
    };
    (TcpCloseConnection) => {
        NativeIntrinsic::Host
    };
    (IoReadRange) => {
        NativeIntrinsic::Host
    };
    (IoAppendBytes) => {
        NativeIntrinsic::Host
    };
    (IoFileLength) => {
        NativeIntrinsic::Host
    };
    (TimeWallNow) => {
        NativeIntrinsic::Host
    };
    (TimeMonotonicNow) => {
        NativeIntrinsic::Host
    };
    (RandomBytes) => {
        NativeIntrinsic::Host
    };
    ($builtin:ident) => {
        NativeIntrinsic::Unavailable
    };
}

macro_rules! builtin_handler {
    (Direct, $builtin:ident) => {
        Some(BuiltinHandler::Direct(
            crate::vm::builtins::$builtin as DirectBuiltinHandler,
        ))
    };
    (TransformFirst, $builtin:ident) => {
        Some(BuiltinHandler::ConsumeFirst(
            crate::vm::builtins::$builtin as ConsumingBuiltinHandler,
        ))
    };
    (ConsumeFirst, $builtin:ident) => {
        Some(BuiltinHandler::ConsumeFirst(
            crate::vm::builtins::$builtin as ConsumingBuiltinHandler,
        ))
    };
    ($execution:ident, $builtin:ident) => {
        None
    };
}

builtin_descriptors! {
    Print = 0, source: Some("print"), intrinsic: None => None,
        execution: Direct, signature: (variadic Read Any) -> Unit;
    Println = 1, source: Some("println"), intrinsic: None => None,
        execution: Direct, signature: (variadic Read Any) -> Unit;
    FromCodePoint = 2, source: Some("from_code_point"), intrinsic: None => None,
        execution: Direct, signature: [Read Integer] -> CodePoint;
    ParseFloat = 3, source: Some("parse_float"), intrinsic: None => None,
        execution: Direct, signature: [Read String] -> Float;
    FormatFloat = 4, source: None, intrinsic: Some("float.format") => Some("core.float"),
        execution: Direct, signature: [Read Float] -> String;
    ByteValid = 5, source: None, intrinsic: None => None,
        execution: Direct, signature: [Read Integer] -> Bool;
    ByteUnchecked = 6, source: None, intrinsic: Some("byte.unchecked") => Some("core.byte"),
        execution: Direct, signature: [Read Integer] -> Byte;
    BytesEmpty = 7, source: None, intrinsic: None => None,
        execution: Direct, signature: [] -> Bytes;
    BytesFromList = 8, source: None, intrinsic: Some("bytes.from_list") => Some("core.bytes"),
        execution: Direct, signature: [Read ListByte] -> Bytes;
    BytesConcat = 9, source: None, intrinsic: None => None,
        execution: Direct, signature: [Read Bytes, Read Bytes] -> Bytes;
    BytesSlice = 10, source: None, intrinsic: None => None,
        execution: Direct, signature: [Read Bytes, Read Integer, Read Integer] -> Bytes;
    BytesToList = 11, source: None, intrinsic: Some("bytes.to_list") => Some("core.bytes"),
        execution: Direct, signature: [Read Bytes] -> ListByte;
    BytesHex = 12, source: None, intrinsic: None => None,
        execution: Direct, signature: [Read Bytes] -> String;
    BytesFromHex = 13, source: None, intrinsic: None => None,
        execution: Direct, signature: [Read String] -> Any;
    StringUtf8 = 14, source: None, intrinsic: None => None,
        execution: Direct, signature: [Read String] -> Bytes;
    BytesUtf8Valid = 15, source: None, intrinsic: None => None,
        execution: Direct, signature: [Read Bytes] -> Bool;
    BytesDecodeUtf8 = 16, source: None, intrinsic: Some("bytes.decode_utf8") => Some("core.bytes"),
        execution: Direct, signature: [Read Bytes] -> String;
    ByteBufferEmpty = 17, source: None, intrinsic: None => None,
        execution: Direct, signature: [] -> ByteBuffer;
    ByteBufferWithCapacity = 18, source: None, intrinsic: None => None,
        execution: Direct, signature: [Read Integer] -> ByteBuffer;
    ByteBufferPush = 19, source: None, intrinsic: None => None,
        execution: TransformFirst, signature: [Consume ByteBuffer, Read Byte] -> ByteBuffer;
    ByteBufferExtend = 20, source: None, intrinsic: None => None,
        execution: TransformFirst, signature: [Consume ByteBuffer, Read Bytes] -> ByteBuffer;
    ByteBufferClear = 21, source: None, intrinsic: None => None,
        execution: TransformFirst, signature: [Consume ByteBuffer] -> ByteBuffer;
    ByteBufferTruncate = 22, source: None, intrinsic: None => None,
        execution: TransformFirst, signature: [Consume ByteBuffer, Read Integer] -> ByteBuffer;
    ByteBufferReserve = 23, source: None, intrinsic: None => None,
        execution: TransformFirst, signature: [Consume ByteBuffer, Read Integer] -> ByteBuffer;
    ByteBufferFreeze = 24, source: None, intrinsic: None => None,
        execution: ConsumeFirst, signature: [Consume ByteBuffer] -> Bytes;
    ByteBufferSnapshot = 25, source: None, intrinsic: None => None,
        execution: Direct, signature: [Read ByteBuffer] -> Bytes;
    IoReadText = 26, source: None, intrinsic: None => None,
        execution: Host, signature: [Read String] -> Any;
    IoWriteText = 27, source: None, intrinsic: None => None,
        execution: Host, signature: [Read String, Read String] -> Any;
    IoReadBytes = 28, source: None, intrinsic: Some("io.read_bytes") => Some("std.fs"),
        execution: Host, signature: [Read String] -> Any;
    IoWriteBytes = 29, source: None, intrinsic: Some("io.write_bytes") => Some("std.fs"),
        execution: Host, signature: [Read String, Read Bytes] -> Any;
    IoListDirectory = 30, source: None, intrinsic: Some("io.list_directory") => Some("std.fs"),
        execution: Host, signature: [Read String] -> Any;
    IoExists = 31, source: None, intrinsic: Some("io.exists") => Some("std.fs"),
        execution: Host, signature: [Read String] -> Any;
    IoIsFile = 32, source: None, intrinsic: Some("io.is_file") => Some("std.fs"),
        execution: Host, signature: [Read String] -> Any;
    IoIsDirectory = 33, source: None, intrinsic: Some("io.is_directory") => Some("std.fs"),
        execution: Host, signature: [Read String] -> Any;
    IoCreateDirectory = 34, source: None, intrinsic: Some("io.create_directory") => Some("std.fs"),
        execution: Host, signature: [Read String] -> Any;
    IoCreateDirectoryAll = 35, source: None, intrinsic: Some("io.create_directory_all") => Some("std.fs"),
        execution: Host, signature: [Read String] -> Any;
    IoRemoveFile = 36, source: None, intrinsic: Some("io.remove_file") => Some("std.fs"),
        execution: Host, signature: [Read String] -> Any;
    IoRemoveDirectory = 37, source: None, intrinsic: Some("io.remove_directory") => Some("std.fs"),
        execution: Host, signature: [Read String] -> Any;
    IoRename = 38, source: None, intrinsic: Some("io.rename") => Some("std.fs"),
        execution: Host, signature: [Read String, Read String] -> Any;
    IoCopyFile = 39, source: None, intrinsic: Some("io.copy_file") => Some("std.fs"),
        execution: Host, signature: [Read String, Read String] -> Any;
    IoJoin = 40, source: None, intrinsic: Some("io.join") => Some("std.path"),
        execution: Host, signature: [Read String, Read String] -> Any;
    IoParent = 41, source: None, intrinsic: Some("io.parent") => Some("std.path"),
        execution: Host, signature: [Read String] -> Any;
    IoFileName = 42, source: None, intrinsic: Some("io.file_name") => Some("std.path"),
        execution: Host, signature: [Read String] -> Any;
    IoExtension = 43, source: None, intrinsic: Some("io.extension") => Some("std.path"),
        execution: Host, signature: [Read String] -> Any;
    IoCanonicalize = 44, source: None, intrinsic: Some("io.canonicalize") => Some("std.path"),
        execution: Host, signature: [Read String] -> Any;
    IoCurrentDirectory = 45, source: None, intrinsic: Some("io.current_directory") => Some("std.env"),
        execution: Host, signature: [] -> Any;
    TcpListen = 46, source: None, intrinsic: Some("tcp.listen") => Some("std.net.tcp"),
        execution: Host, signature: [Read String, Read Integer] -> Any;
    TcpConnect = 47, source: None, intrinsic: Some("tcp.connect") => Some("std.net.tcp"),
        execution: Host, signature: [Read String, Read Integer] -> Any;
    TcpAccept = 48, source: None, intrinsic: Some("tcp.accept") => Some("std.net.tcp"),
        execution: Host, signature: [Read Integer] -> Any;
    TcpRead = 49, source: None, intrinsic: None => None,
        execution: Host, signature: [Read Integer, Read Integer] -> Any;
    TcpWrite = 50, source: None, intrinsic: None => None,
        execution: Host, signature: [Read Integer, Read String] -> Any;
    TcpReadBytes = 51, source: None, intrinsic: Some("tcp.read_bytes") => Some("std.net.tcp"),
        execution: Host, signature: [Read Integer, Read Integer] -> Any;
    TcpWriteBytes = 52, source: None, intrinsic: Some("tcp.write_bytes") => Some("std.net.tcp"),
        execution: Host, signature: [Read Integer, Read Bytes] -> Any;
    TcpSetTimeout = 53, source: None, intrinsic: Some("tcp.set_timeout") => Some("std.net.tcp"),
        execution: Host, signature: [Read Integer, Read Integer] -> Any;
    TcpCloseListener = 54, source: None, intrinsic: Some("tcp.close_listener") => Some("std.net.tcp"),
        execution: Host, signature: [Read Integer] -> Any;
    TcpCloseConnection = 55, source: None, intrinsic: Some("tcp.close_connection") => Some("std.net.tcp"),
        execution: Host, signature: [Read Integer] -> Any;
    IoReadRange = 56, source: None, intrinsic: Some("io.read_range") => Some("std.fs"),
        execution: Host, signature: [Read String, Read Integer, Read Integer] -> Any;
    IoAppendBytes = 57, source: None, intrinsic: Some("io.append_bytes") => Some("std.fs"),
        execution: Host, signature: [Read String, Read Bytes] -> Any;
    IoFileLength = 58, source: None, intrinsic: Some("io.file_length") => Some("std.fs"),
        execution: Host, signature: [Read String] -> Any;
    TimeWallNow = 59, source: None, intrinsic: Some("time.wall_now") => Some("std.time"),
        execution: Host, signature: [] -> Any;
    TimeMonotonicNow = 60, source: None, intrinsic: Some("time.monotonic_now") => Some("std.time"),
        execution: Host, signature: [] -> Integer;
    RandomBytes = 61, source: None, intrinsic: Some("random.bytes") => Some("std.random"),
        execution: Host, signature: [Read Integer] -> Any;
}

impl Builtin {
    pub fn descriptor(self) -> &'static BuiltinDescriptor {
        BUILTINS
            .iter()
            .find(|descriptor| descriptor.builtin == self)
            .expect("every Builtin has a registry descriptor")
    }

    pub fn from_bytecode_tag(tag: u8) -> Option<Self> {
        BUILTINS
            .iter()
            .find(|descriptor| descriptor.bytecode_tag == tag)
            .map(|descriptor| descriptor.builtin)
    }

    pub fn from_source_name(name: &str) -> Option<Self> {
        BUILTINS
            .iter()
            .find(|descriptor| descriptor.source_name == Some(name))
            .map(|descriptor| descriptor.builtin)
    }

    pub fn from_intrinsic_key(key: &str) -> Option<Self> {
        BUILTINS
            .iter()
            .find(|descriptor| descriptor.intrinsic_key == Some(key))
            .map(|descriptor| descriptor.builtin)
    }

    pub fn bytecode_tag(self) -> u8 {
        self.descriptor().bytecode_tag
    }

    pub fn is_host(self) -> bool {
        self.descriptor().execution == BuiltinExecution::Host
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeReceiverKind {
    Arguments,
    String,
    StringList,
}

/// Registry-owned runtime member lookup used only by legacy host-backed ABI views.
pub fn native_member_runtime(receiver: NativeReceiverKind, member: &str) -> Option<&'static str> {
    match (receiver, member) {
        (NativeReceiverKind::Arguments, "executable") => Some(crate::native::abi::ARGS_EXECUTABLE),
        (NativeReceiverKind::Arguments, "values") => Some(crate::native::abi::ARGS_VALUES),
        (NativeReceiverKind::StringList, "empty?") => Some(crate::native::abi::STRING_LIST_EMPTY),
        (NativeReceiverKind::StringList, "length") => Some(crate::native::abi::STRING_LIST_LENGTH),
        (NativeReceiverKind::StringList, "head") => Some(crate::native::abi::STRING_LIST_HEAD),
        (NativeReceiverKind::String, "empty?") => Some(crate::native::abi::STRING_EMPTY),
        (NativeReceiverKind::String, "length") => Some(crate::native::abi::STRING_LENGTH),
        (NativeReceiverKind::String, "head") => Some(crate::native::abi::STRING_HEAD),
        (NativeReceiverKind::String, "rest") => Some(crate::native::abi::STRING_REST),
        (NativeReceiverKind::String, "whitespace?") => Some(crate::native::abi::STRING_WHITESPACE),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OpcodeIntrinsic {
    ListAt,
    ListPush,
    ListAppend,
}

/// How an opcode intrinsic accesses its method receiver.
///
/// This is separate from argument ownership: a mutating receiver must be lowered as a
/// place so an operation on a projected field updates the original aggregate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntrinsicReceiverMode {
    Read,
    Mutate,
    Consume,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpcodeIntrinsicDescriptor {
    pub intrinsic: OpcodeIntrinsic,
    pub intrinsic_key: &'static str,
    pub module: &'static str,
    pub receiver: IntrinsicReceiverMode,
    pub signature: IntrinsicSignature,
}

pub const OPCODE_INTRINSICS: &[OpcodeIntrinsicDescriptor] = &[
    OpcodeIntrinsicDescriptor {
        intrinsic: OpcodeIntrinsic::ListAt,
        intrinsic_key: "list.at",
        module: "core.list",
        receiver: IntrinsicReceiverMode::Read,
        signature: IntrinsicSignature {
            parameters: intrinsic_parameters!([Read Any, Read Integer]),
            result: IntrinsicType::Any,
        },
    },
    OpcodeIntrinsicDescriptor {
        intrinsic: OpcodeIntrinsic::ListPush,
        intrinsic_key: "list.push",
        module: "core.list",
        receiver: IntrinsicReceiverMode::Mutate,
        signature: IntrinsicSignature {
            parameters: intrinsic_parameters!([Read Any, Read Any]),
            result: IntrinsicType::Unit,
        },
    },
    OpcodeIntrinsicDescriptor {
        intrinsic: OpcodeIntrinsic::ListAppend,
        intrinsic_key: "list.append",
        module: "core.list",
        receiver: IntrinsicReceiverMode::Consume,
        signature: IntrinsicSignature {
            parameters: intrinsic_parameters!([Read Any, Read Any]),
            result: IntrinsicType::Any,
        },
    },
];

impl OpcodeIntrinsic {
    pub fn descriptor(self) -> &'static OpcodeIntrinsicDescriptor {
        OPCODE_INTRINSICS
            .iter()
            .find(|descriptor| descriptor.intrinsic == self)
            .expect("every opcode intrinsic has a registry descriptor")
    }
}

/// Compiler lowering selected by a source `intrinsic("...")` declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intrinsic {
    Builtin(Builtin),
    Opcode(OpcodeIntrinsic),
}

impl Intrinsic {
    pub fn from_key(key: &str) -> Option<Self> {
        Builtin::from_intrinsic_key(key)
            .map(Self::Builtin)
            .or_else(|| {
                OPCODE_INTRINSICS
                    .iter()
                    .find(|descriptor| descriptor.intrinsic_key == key)
                    .map(|descriptor| Self::Opcode(descriptor.intrinsic))
            })
    }

    pub fn for_module(module: &str, key: &str) -> Option<Self> {
        let intrinsic = Self::from_key(key)?;
        let expected = match intrinsic {
            Self::Builtin(builtin) => builtin.descriptor().module?,
            Self::Opcode(intrinsic) => intrinsic.descriptor().module,
        };
        (module == expected).then_some(intrinsic)
    }

    pub fn builtin(self) -> Option<Builtin> {
        match self {
            Self::Builtin(builtin) => Some(builtin),
            Self::Opcode(_) => None,
        }
    }

    pub fn opcode(self) -> Option<OpcodeIntrinsic> {
        match self {
            Self::Opcode(intrinsic) => Some(intrinsic),
            Self::Builtin(_) => None,
        }
    }

    pub fn is_list_operation(self) -> bool {
        self.opcode().is_some()
    }

    pub fn receiver_mode(self) -> Option<IntrinsicReceiverMode> {
        self.opcode()
            .map(|intrinsic| intrinsic.descriptor().receiver)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytecode_tags_are_unique_contiguous_and_round_trip() {
        assert_eq!(
            BUILTINS.len(),
            usize::from(u8::try_from(BUILTINS.len()).unwrap())
        );
        for (tag, descriptor) in BUILTINS.iter().enumerate() {
            assert_eq!(usize::from(descriptor.bytecode_tag), tag);
            assert_eq!(
                Builtin::from_bytecode_tag(descriptor.bytecode_tag),
                Some(descriptor.builtin)
            );
            assert_eq!(descriptor.builtin.bytecode_tag(), descriptor.bytecode_tag);
        }
    }

    #[test]
    fn intrinsic_keys_are_unique_and_owned_by_one_module() {
        let mut keys = std::collections::HashSet::new();
        for descriptor in BUILTINS {
            if let Some(key) = descriptor.intrinsic_key {
                assert!(keys.insert(key), "duplicate intrinsic key {key}");
                let module = descriptor
                    .module
                    .expect("source intrinsic has an owning module");
                assert_eq!(
                    Intrinsic::for_module(module, key),
                    Some(Intrinsic::Builtin(descriptor.builtin))
                );
                assert_eq!(Intrinsic::for_module("main", key), None);
            }
        }
        for descriptor in OPCODE_INTRINSICS {
            assert!(
                keys.insert(descriptor.intrinsic_key),
                "duplicate intrinsic key {}",
                descriptor.intrinsic_key
            );
            assert_eq!(
                Intrinsic::for_module(descriptor.module, descriptor.intrinsic_key),
                Some(Intrinsic::Opcode(descriptor.intrinsic))
            );
            assert_eq!(
                Intrinsic::for_module("main", descriptor.intrinsic_key),
                None
            );
        }
    }

    #[test]
    fn foster_replacements_are_not_source_intrinsics() {
        for key in [
            "byte.valid",
            "bytes.empty",
            "bytes.concat",
            "bytes.slice",
            "bytes.hex",
            "bytes.from_hex",
            "bytes.encode_utf8",
            "bytes.utf8_valid",
            "byte_buffer.empty",
            "byte_buffer.with_capacity",
            "byte_buffer.push",
            "byte_buffer.extend",
            "byte_buffer.clear",
            "byte_buffer.truncate",
            "byte_buffer.reserve",
            "byte_buffer.freeze",
            "byte_buffer.snapshot",
            "io.read_text",
            "io.write_text",
            "tcp.read",
            "tcp.write",
        ] {
            assert_eq!(Intrinsic::from_key(key), None, "retired source key {key}");
        }
    }

    #[test]
    fn opcode_receiver_modes_are_explicit() {
        assert_eq!(
            Intrinsic::Opcode(OpcodeIntrinsic::ListAt).receiver_mode(),
            Some(IntrinsicReceiverMode::Read)
        );
        assert_eq!(
            Intrinsic::Opcode(OpcodeIntrinsic::ListPush).receiver_mode(),
            Some(IntrinsicReceiverMode::Mutate)
        );
        assert_eq!(
            Intrinsic::Opcode(OpcodeIntrinsic::ListAppend).receiver_mode(),
            Some(IntrinsicReceiverMode::Consume)
        );
    }

    #[test]
    fn native_intrinsic_policies_are_explicit() {
        let lowered_in_process = BUILTINS
            .iter()
            .filter(|descriptor| {
                !matches!(
                    descriptor.native,
                    NativeIntrinsic::Unavailable | NativeIntrinsic::Host
                )
            })
            .map(|descriptor| descriptor.builtin)
            .collect::<Vec<_>>();
        assert_eq!(
            lowered_in_process,
            vec![
                Builtin::Print,
                Builtin::Println,
                Builtin::FromCodePoint,
                Builtin::ParseFloat,
                Builtin::FormatFloat,
                Builtin::ByteValid,
                Builtin::ByteUnchecked,
                Builtin::BytesFromList,
                Builtin::BytesToList,
                Builtin::BytesDecodeUtf8,
            ]
        );
        for descriptor in BUILTINS {
            assert_eq!(
                matches!(descriptor.native, NativeIntrinsic::Host),
                descriptor.execution == BuiltinExecution::Host,
                "host policy differs for {:?}",
                descriptor.builtin
            );
        }
        assert_eq!(
            native_member_runtime(NativeReceiverKind::String, "length"),
            Some(crate::native::abi::STRING_LENGTH)
        );
    }

    #[test]
    fn ownership_modes_agree_with_execution_policy() {
        for descriptor in BUILTINS {
            let consumes_first = descriptor
                .signature
                .parameter(0)
                .is_some_and(|parameter| parameter.mode == IntrinsicArgumentMode::Consume);
            assert_eq!(
                consumes_first,
                matches!(
                    descriptor.execution,
                    BuiltinExecution::TransformFirst | BuiltinExecution::ConsumeFirst
                ),
                "inconsistent execution policy for {:?}",
                descriptor.builtin
            );
            assert_eq!(
                descriptor.handler.is_some(),
                descriptor.execution != BuiltinExecution::Host,
                "handler registration for {:?}",
                descriptor.builtin
            );
            assert!(matches!(
                (descriptor.execution, descriptor.handler),
                (BuiltinExecution::Direct, Some(BuiltinHandler::Direct(_)))
                    | (
                        BuiltinExecution::TransformFirst | BuiltinExecution::ConsumeFirst,
                        Some(BuiltinHandler::ConsumeFirst(_))
                    )
                    | (BuiltinExecution::Host, None)
            ));
        }
    }
}
