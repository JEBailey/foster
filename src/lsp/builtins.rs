use std::fs;

use lsp_types::Location;

use crate::hir::Builtin;

use super::byte_range_to_lsp;
use super::workspace::path_to_uri;

pub(super) struct BuiltinInfo {
    pub name: &'static str,
    pub signature: &'static str,
    pub parameters: &'static [&'static str],
    pub documentation: &'static str,
}

pub(super) fn info(intrinsic: Builtin) -> BuiltinInfo {
    match intrinsic {
        Builtin::Print => builtin(
            "print",
            "print(values...) -> Unit",
            &["value"],
            "Prints values without a trailing newline.",
        ),
        Builtin::Println => builtin(
            "println",
            "println(values...) -> Unit",
            &["value"],
            "Prints values followed by a newline.",
        ),
        Builtin::CodePoint => builtin(
            "code_point",
            "code_point(character: CodePoint) -> Int",
            &["character"],
            "Legacy explicit conversion to a Unicode scalar value. CodePoint participates directly in integer operators.",
        ),
        Builtin::FromCodePoint => builtin(
            "from_code_point",
            "from_code_point(value: Int) -> CodePoint",
            &["value"],
            "Constructs the code point for a valid Unicode scalar value.",
        ),
        Builtin::ParseFloat => builtin(
            "parse_float",
            "parse_float(source: String) -> Float",
            &["source"],
            "Parses a binary64 floating-point value from text.",
        ),
        Builtin::ByteValid => builtin(
            "Byte.valid",
            "Byte.valid(value: Int) -> Bool",
            &["value"],
            "Checks whether an integer fits in Byte.",
        ),
        Builtin::ByteUnchecked => builtin(
            "Byte.unchecked",
            "Byte.unchecked(value: Int) -> Byte",
            &["value"],
            "Constructs a Byte after bounds validation.",
        ),
        Builtin::BytesEmpty => builtin(
            "BytesHost.empty",
            "BytesHost.empty() -> Bytes",
            &[],
            "Creates an empty immutable byte sequence.",
        ),
        Builtin::BytesFromList => builtin(
            "BytesHost.from_list",
            "BytesHost.from_list(values: List<Byte>) -> Bytes",
            &["values"],
            "Packs byte values into compact immutable storage.",
        ),
        Builtin::BytesConcat => builtin(
            "BytesHost.concat",
            "BytesHost.concat(left: Bytes, right: Bytes) -> Bytes",
            &["left", "right"],
            "Concatenates immutable byte sequences.",
        ),
        Builtin::BytesSlice => builtin(
            "BytesHost.slice",
            "BytesHost.slice(values: Bytes, start: Int, end: Int) -> Bytes",
            &["values", "start", "end"],
            "Returns a bounded byte slice.",
        ),
        Builtin::BytesToList => builtin(
            "BytesHost.to_list",
            "BytesHost.to_list(values: Bytes) -> List<Byte>",
            &["values"],
            "Expands compact bytes into a Foster list.",
        ),
        Builtin::BytesHex => builtin(
            "BytesHost.hex",
            "BytesHost.hex(values: Bytes) -> String",
            &["values"],
            "Encodes bytes as lowercase hexadecimal text.",
        ),
        Builtin::BytesFromHex => builtin(
            "BytesHost.from_hex",
            "BytesHost.from_hex(value: String) -> Result<Bytes, HexError>",
            &["value"],
            "Decodes an even-length hexadecimal string.",
        ),
        Builtin::StringUtf8 => builtin(
            "BytesHost.encode_utf8",
            "BytesHost.encode_utf8(value: String) -> Bytes",
            &["value"],
            "Encodes a string as UTF-8 bytes.",
        ),
        Builtin::BytesUtf8Valid => builtin(
            "BytesHost.utf8_valid",
            "BytesHost.utf8_valid(value: Bytes) -> Bool",
            &["value"],
            "Checks whether bytes contain valid UTF-8.",
        ),
        Builtin::BytesDecodeUtf8 => builtin(
            "BytesHost.decode_utf8",
            "BytesHost.decode_utf8(value: Bytes) -> String",
            &["value"],
            "Decodes bytes already validated as UTF-8.",
        ),
        Builtin::ByteBufferEmpty => builtin(
            "RawByteBuffer.empty",
            "RawByteBuffer.empty() -> RawByteBuffer",
            &[],
            "Creates an empty mutable byte buffer.",
        ),
        Builtin::ByteBufferWithCapacity => builtin(
            "RawByteBuffer.with_capacity",
            "RawByteBuffer.with_capacity(capacity: Int) -> RawByteBuffer",
            &["capacity"],
            "Creates a byte buffer with reserved capacity.",
        ),
        Builtin::ByteBufferPush => builtin(
            "RawByteBuffer.push",
            "RawByteBuffer.push(buffer: RawByteBuffer, value: Byte) -> RawByteBuffer",
            &["buffer", "value"],
            "Appends one byte to a buffer.",
        ),
        Builtin::ByteBufferExtend => builtin(
            "RawByteBuffer.extend",
            "RawByteBuffer.extend(buffer: RawByteBuffer, values: Bytes) -> RawByteBuffer",
            &["buffer", "values"],
            "Appends immutable bytes to a buffer.",
        ),
        Builtin::ByteBufferClear => builtin(
            "RawByteBuffer.clear",
            "RawByteBuffer.clear(buffer: RawByteBuffer) -> RawByteBuffer",
            &["buffer"],
            "Removes all buffered bytes.",
        ),
        Builtin::ByteBufferTruncate => builtin(
            "RawByteBuffer.truncate",
            "RawByteBuffer.truncate(buffer: RawByteBuffer, length: Int) -> RawByteBuffer",
            &["buffer", "length"],
            "Truncates a buffer to at most the supplied length.",
        ),
        Builtin::ByteBufferReserve => builtin(
            "RawByteBuffer.reserve",
            "RawByteBuffer.reserve(buffer: RawByteBuffer, additional: Int) -> RawByteBuffer",
            &["buffer", "additional"],
            "Reserves additional byte capacity.",
        ),
        Builtin::ByteBufferFreeze => builtin(
            "RawByteBuffer.freeze",
            "RawByteBuffer.freeze(buffer: RawByteBuffer) -> Bytes",
            &["buffer"],
            "Transfers a buffer into immutable bytes.",
        ),
        Builtin::ByteBufferSnapshot => builtin(
            "RawByteBuffer.snapshot",
            "RawByteBuffer.snapshot(buffer: RawByteBuffer) -> Bytes",
            &["buffer"],
            "Copies the current buffer into immutable bytes.",
        ),
        Builtin::IoReadText => builtin(
            "IoHost.read_text",
            "IoHost.read_text(path: String) -> Result<String, IoError>",
            &["path"],
            "Reads a UTF-8 text file through the host filesystem boundary.",
        ),
        Builtin::IoWriteText => builtin(
            "IoHost.write_text",
            "IoHost.write_text(path: String, contents: String) -> Result<Unit, IoError>",
            &["path", "contents"],
            "Writes a UTF-8 text file through the host filesystem boundary.",
        ),
        Builtin::IoReadBytes => builtin(
            "IoHost.read_bytes",
            "IoHost.read_bytes(path: String) -> Result<Bytes, IoError>",
            &["path"],
            "Reads raw bytes through the host filesystem boundary.",
        ),
        Builtin::IoWriteBytes => builtin(
            "IoHost.write_bytes",
            "IoHost.write_bytes(path: String, contents: Bytes) -> Result<Unit, IoError>",
            &["path", "contents"],
            "Writes raw bytes through the host filesystem boundary.",
        ),
        Builtin::IoListDirectory => builtin(
            "IoHost.list_directory",
            "IoHost.list_directory(path: String) -> Result<List<String>, IoError>",
            &["path"],
            "Lists directory entries through the host filesystem boundary.",
        ),
        Builtin::IoExists => builtin(
            "IoHost.exists",
            "IoHost.exists(path: String) -> Bool",
            &["path"],
            "Reports whether a host filesystem path exists.",
        ),
        Builtin::IoIsFile => builtin(
            "IoHost.is_file",
            "IoHost.is_file(path: String) -> Bool",
            &["path"],
            "Reports whether a host filesystem path is a file.",
        ),
        Builtin::IoIsDirectory => builtin(
            "IoHost.is_directory",
            "IoHost.is_directory(path: String) -> Bool",
            &["path"],
            "Reports whether a host filesystem path is a directory.",
        ),
        Builtin::IoJoin => builtin(
            "IoHost.join",
            "IoHost.join(left: String, right: String) -> String",
            &["left", "right"],
            "Joins two path components using host path rules.",
        ),
        Builtin::IoParent => builtin(
            "IoHost.parent",
            "IoHost.parent(path: String) -> String",
            &["path"],
            "Returns the parent of a host filesystem path.",
        ),
        Builtin::IoFileName => builtin(
            "IoHost.file_name",
            "IoHost.file_name(path: String) -> String",
            &["path"],
            "Returns the final component of a host filesystem path.",
        ),
        Builtin::IoExtension => builtin(
            "IoHost.extension",
            "IoHost.extension(path: String) -> String",
            &["path"],
            "Returns the extension of a host filesystem path.",
        ),
        Builtin::IoCanonicalize => builtin(
            "IoHost.canonicalize",
            "IoHost.canonicalize(path: String) -> Result<String, IoError>",
            &["path"],
            "Canonicalizes a host filesystem path.",
        ),
        Builtin::IoCurrentDirectory => builtin(
            "IoHost.current_directory",
            "IoHost.current_directory() -> Result<String, IoError>",
            &[],
            "Returns the host process's current directory.",
        ),
        Builtin::TcpListen => builtin(
            "TcpHost.listen",
            "TcpHost.listen(host: String, port: Int) -> Result<Int, NetworkError>",
            &["host", "port"],
            "Creates a host TCP listener and returns its opaque handle.",
        ),
        Builtin::TcpConnect => builtin(
            "TcpHost.connect",
            "TcpHost.connect(host: String, port: Int) -> Result<Int, NetworkError>",
            &["host", "port"],
            "Connects to a TCP endpoint and returns its opaque handle.",
        ),
        Builtin::TcpAccept => builtin(
            "TcpHost.accept",
            "TcpHost.accept(listener: Int) -> Result<Int, NetworkError>",
            &["listener"],
            "Accepts one connection from a TCP listener.",
        ),
        Builtin::TcpRead => builtin(
            "TcpHost.read",
            "TcpHost.read(connection: Int, maximum: Int) -> Result<String, NetworkError>",
            &["connection", "maximum"],
            "Reads UTF-8 text from a TCP connection.",
        ),
        Builtin::TcpWrite => builtin(
            "TcpHost.write",
            "TcpHost.write(connection: Int, contents: String) -> Result<Unit, NetworkError>",
            &["connection", "contents"],
            "Writes UTF-8 text to a TCP connection.",
        ),
        Builtin::TcpReadBytes => builtin(
            "TcpHost.read_bytes",
            "TcpHost.read_bytes(connection: Int, maximum: Int) -> Result<Bytes, NetworkError>",
            &["connection", "maximum"],
            "Reads raw bytes from a TCP connection.",
        ),
        Builtin::TcpWriteBytes => builtin(
            "TcpHost.write_bytes",
            "TcpHost.write_bytes(connection: Int, contents: Bytes) -> Result<Unit, NetworkError>",
            &["connection", "contents"],
            "Writes raw bytes to a TCP connection.",
        ),
        Builtin::TcpSetTimeout => builtin(
            "TcpHost.set_timeout",
            "TcpHost.set_timeout(connection: Int, milliseconds: Int) -> Result<Unit, NetworkError>",
            &["connection", "milliseconds"],
            "Sets a TCP connection timeout.",
        ),
        Builtin::TcpCloseListener => builtin(
            "TcpHost.close_listener",
            "TcpHost.close_listener(listener: Int) -> Result<Unit, NetworkError>",
            &["listener"],
            "Closes a host TCP listener.",
        ),
        Builtin::TcpCloseConnection => builtin(
            "TcpHost.close_connection",
            "TcpHost.close_connection(connection: Int) -> Result<Unit, NetworkError>",
            &["connection"],
            "Closes a host TCP connection.",
        ),
    }
}

pub(super) fn definition_location(builtin: Builtin) -> Option<Location> {
    let info = info(builtin);
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/core-library.md");
    let source = fs::read_to_string(&path).ok()?;
    let marker = format!("`{}`", info.name);
    let marker_start = source.find(&marker)?;
    let start = marker_start + 1;
    let end = start + info.name.len();
    Some(Location::new(
        path_to_uri(&path)?,
        byte_range_to_lsp(&source, start..end),
    ))
}

fn builtin(
    name: &'static str,
    signature: &'static str,
    parameters: &'static [&'static str],
    documentation: &'static str,
) -> BuiltinInfo {
    BuiltinInfo {
        name,
        signature,
        parameters,
        documentation,
    }
}
