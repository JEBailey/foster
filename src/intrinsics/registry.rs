//! Stable builtin tags and source intrinsic bindings.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    /// Whether execution must cross the VM host-capability boundary.
    pub host: bool,
}

macro_rules! builtin_descriptors {
    ($(($builtin:ident, $tag:literal, $source:expr, $key:expr, $module:expr, $host:literal)),+ $(,)?) => {
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
                host: $host,
            }),+
        ];
    };
}

builtin_descriptors!(
    (Print, 0, Some("print"), None, None, false),
    (Println, 1, Some("println"), None, None, false),
    (FromCodePoint, 2, Some("from_code_point"), None, None, false),
    (ParseFloat, 3, Some("parse_float"), None, None, false),
    (
        FormatFloat,
        4,
        None,
        Some("float.format"),
        Some("core.float"),
        false
    ),
    (
        ByteValid,
        5,
        None,
        Some("byte.valid"),
        Some("core.byte"),
        false
    ),
    (
        ByteUnchecked,
        6,
        None,
        Some("byte.unchecked"),
        Some("core.byte"),
        false
    ),
    (
        BytesEmpty,
        7,
        None,
        Some("bytes.empty"),
        Some("core.bytes"),
        false
    ),
    (
        BytesFromList,
        8,
        None,
        Some("bytes.from_list"),
        Some("core.bytes"),
        false
    ),
    (
        BytesConcat,
        9,
        None,
        Some("bytes.concat"),
        Some("core.bytes"),
        false
    ),
    (
        BytesSlice,
        10,
        None,
        Some("bytes.slice"),
        Some("core.bytes"),
        false
    ),
    (
        BytesToList,
        11,
        None,
        Some("bytes.to_list"),
        Some("core.bytes"),
        false
    ),
    (
        BytesHex,
        12,
        None,
        Some("bytes.hex"),
        Some("core.bytes"),
        false
    ),
    (
        BytesFromHex,
        13,
        None,
        Some("bytes.from_hex"),
        Some("core.bytes"),
        false
    ),
    (
        StringUtf8,
        14,
        None,
        Some("bytes.encode_utf8"),
        Some("core.bytes"),
        false
    ),
    (
        BytesUtf8Valid,
        15,
        None,
        Some("bytes.utf8_valid"),
        Some("core.bytes"),
        false
    ),
    (
        BytesDecodeUtf8,
        16,
        None,
        Some("bytes.decode_utf8"),
        Some("core.bytes"),
        false
    ),
    (
        ByteBufferEmpty,
        17,
        None,
        Some("byte_buffer.empty"),
        Some("core.bytes.buffer"),
        false
    ),
    (
        ByteBufferWithCapacity,
        18,
        None,
        Some("byte_buffer.with_capacity"),
        Some("core.bytes.buffer"),
        false
    ),
    (
        ByteBufferPush,
        19,
        None,
        Some("byte_buffer.push"),
        Some("core.bytes.buffer"),
        false
    ),
    (
        ByteBufferExtend,
        20,
        None,
        Some("byte_buffer.extend"),
        Some("core.bytes.buffer"),
        false
    ),
    (
        ByteBufferClear,
        21,
        None,
        Some("byte_buffer.clear"),
        Some("core.bytes.buffer"),
        false
    ),
    (
        ByteBufferTruncate,
        22,
        None,
        Some("byte_buffer.truncate"),
        Some("core.bytes.buffer"),
        false
    ),
    (
        ByteBufferReserve,
        23,
        None,
        Some("byte_buffer.reserve"),
        Some("core.bytes.buffer"),
        false
    ),
    (
        ByteBufferFreeze,
        24,
        None,
        Some("byte_buffer.freeze"),
        Some("core.bytes.buffer"),
        false
    ),
    (
        ByteBufferSnapshot,
        25,
        None,
        Some("byte_buffer.snapshot"),
        Some("core.bytes.buffer"),
        false
    ),
    (
        IoReadText,
        26,
        None,
        Some("io.read_text"),
        Some("std.fs"),
        true
    ),
    (
        IoWriteText,
        27,
        None,
        Some("io.write_text"),
        Some("std.fs"),
        true
    ),
    (
        IoReadBytes,
        28,
        None,
        Some("io.read_bytes"),
        Some("std.fs"),
        true
    ),
    (
        IoWriteBytes,
        29,
        None,
        Some("io.write_bytes"),
        Some("std.fs"),
        true
    ),
    (
        IoListDirectory,
        30,
        None,
        Some("io.list_directory"),
        Some("std.fs"),
        true
    ),
    (IoExists, 31, None, Some("io.exists"), Some("std.fs"), true),
    (IoIsFile, 32, None, Some("io.is_file"), Some("std.fs"), true),
    (
        IoIsDirectory,
        33,
        None,
        Some("io.is_directory"),
        Some("std.fs"),
        true
    ),
    (
        IoCreateDirectory,
        34,
        None,
        Some("io.create_directory"),
        Some("std.fs"),
        true
    ),
    (
        IoCreateDirectoryAll,
        35,
        None,
        Some("io.create_directory_all"),
        Some("std.fs"),
        true
    ),
    (
        IoRemoveFile,
        36,
        None,
        Some("io.remove_file"),
        Some("std.fs"),
        true
    ),
    (
        IoRemoveDirectory,
        37,
        None,
        Some("io.remove_directory"),
        Some("std.fs"),
        true
    ),
    (IoRename, 38, None, Some("io.rename"), Some("std.fs"), true),
    (
        IoCopyFile,
        39,
        None,
        Some("io.copy_file"),
        Some("std.fs"),
        true
    ),
    (IoJoin, 40, None, Some("io.join"), Some("std.path"), true),
    (
        IoParent,
        41,
        None,
        Some("io.parent"),
        Some("std.path"),
        true
    ),
    (
        IoFileName,
        42,
        None,
        Some("io.file_name"),
        Some("std.path"),
        true
    ),
    (
        IoExtension,
        43,
        None,
        Some("io.extension"),
        Some("std.path"),
        true
    ),
    (
        IoCanonicalize,
        44,
        None,
        Some("io.canonicalize"),
        Some("std.path"),
        true
    ),
    (
        IoCurrentDirectory,
        45,
        None,
        Some("io.current_directory"),
        Some("std.env"),
        true
    ),
    (
        TcpListen,
        46,
        None,
        Some("tcp.listen"),
        Some("std.net.tcp"),
        true
    ),
    (
        TcpConnect,
        47,
        None,
        Some("tcp.connect"),
        Some("std.net.tcp"),
        true
    ),
    (
        TcpAccept,
        48,
        None,
        Some("tcp.accept"),
        Some("std.net.tcp"),
        true
    ),
    (
        TcpRead,
        49,
        None,
        Some("tcp.read"),
        Some("std.net.tcp"),
        true
    ),
    (
        TcpWrite,
        50,
        None,
        Some("tcp.write"),
        Some("std.net.tcp"),
        true
    ),
    (
        TcpReadBytes,
        51,
        None,
        Some("tcp.read_bytes"),
        Some("std.net.tcp"),
        true
    ),
    (
        TcpWriteBytes,
        52,
        None,
        Some("tcp.write_bytes"),
        Some("std.net.tcp"),
        true
    ),
    (
        TcpSetTimeout,
        53,
        None,
        Some("tcp.set_timeout"),
        Some("std.net.tcp"),
        true
    ),
    (
        TcpCloseListener,
        54,
        None,
        Some("tcp.close_listener"),
        Some("std.net.tcp"),
        true
    ),
    (
        TcpCloseConnection,
        55,
        None,
        Some("tcp.close_connection"),
        Some("std.net.tcp"),
        true
    ),
    (
        IoReadRange,
        56,
        None,
        Some("io.read_range"),
        Some("std.fs"),
        true
    ),
    (
        IoAppendBytes,
        57,
        None,
        Some("io.append_bytes"),
        Some("std.fs"),
        true
    ),
    (
        IoFileLength,
        58,
        None,
        Some("io.file_length"),
        Some("std.fs"),
        true
    ),
    (
        TimeWallNow,
        59,
        None,
        Some("time.wall_now"),
        Some("std.time"),
        true
    ),
    (
        TimeMonotonicNow,
        60,
        None,
        Some("time.monotonic_now"),
        Some("std.time"),
        true
    ),
    (
        RandomBytes,
        61,
        None,
        Some("random.bytes"),
        Some("std.random"),
        true
    ),
);

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
        self.descriptor().host
    }
}

/// Compiler lowering selected by a source `intrinsic("...")` declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intrinsic {
    Builtin(Builtin),
    ListPush,
    ListAppend,
}

impl Intrinsic {
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "list.push" => Some(Self::ListPush),
            "list.append" => Some(Self::ListAppend),
            _ => Builtin::from_intrinsic_key(key).map(Self::Builtin),
        }
    }

    pub fn for_module(module: &str, key: &str) -> Option<Self> {
        let intrinsic = Self::from_key(key)?;
        let expected = match intrinsic {
            Self::ListPush | Self::ListAppend => "core.list",
            Self::Builtin(builtin) => builtin.descriptor().module?,
        };
        (module == expected).then_some(intrinsic)
    }

    pub fn builtin(self) -> Option<Builtin> {
        match self {
            Self::Builtin(builtin) => Some(builtin),
            Self::ListPush | Self::ListAppend => None,
        }
    }

    pub fn is_list_operation(self) -> bool {
        matches!(self, Self::ListPush | Self::ListAppend)
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
        assert!(keys.insert("list.push"));
        assert!(keys.insert("list.append"));
    }
}
