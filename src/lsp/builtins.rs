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
            "__byte_valid",
            "__byte_valid(value: Int) -> Bool",
            &["value"],
            "Checks whether an integer fits in Byte.",
        ),
        Builtin::ByteUnchecked => builtin(
            "__byte_unchecked",
            "__byte_unchecked(value: Int) -> Byte",
            &["value"],
            "Constructs a Byte after bounds validation.",
        ),
        Builtin::BytesEmpty => builtin(
            "__bytes_empty",
            "__bytes_empty() -> Bytes",
            &[],
            "Creates an empty immutable byte sequence.",
        ),
        Builtin::BytesFromList => builtin(
            "__bytes_from_list",
            "__bytes_from_list(values: List<Byte>) -> Bytes",
            &["values"],
            "Packs byte values into compact immutable storage.",
        ),
        Builtin::BytesConcat => builtin(
            "__bytes_concat",
            "__bytes_concat(left: Bytes, right: Bytes) -> Bytes",
            &["left", "right"],
            "Concatenates immutable byte sequences.",
        ),
        Builtin::BytesSlice => builtin(
            "__bytes_slice",
            "__bytes_slice(values: Bytes, start: Int, end: Int) -> Bytes",
            &["values", "start", "end"],
            "Returns a bounded byte slice.",
        ),
        Builtin::BytesToList => builtin(
            "__bytes_to_list",
            "__bytes_to_list(values: Bytes) -> List<Byte>",
            &["values"],
            "Expands compact bytes into a Foster list.",
        ),
        Builtin::BytesHex => builtin(
            "__bytes_hex",
            "__bytes_hex(values: Bytes) -> String",
            &["values"],
            "Encodes bytes as lowercase hexadecimal text.",
        ),
        Builtin::BytesFromHex => builtin(
            "__bytes_from_hex",
            "__bytes_from_hex(value: String) -> Result<Bytes, HexError>",
            &["value"],
            "Decodes an even-length hexadecimal string.",
        ),
        Builtin::StringUtf8 => builtin(
            "__string_utf8",
            "__string_utf8(value: String) -> Bytes",
            &["value"],
            "Encodes a string as UTF-8 bytes.",
        ),
        Builtin::BytesUtf8Valid => builtin(
            "__bytes_utf8_valid",
            "__bytes_utf8_valid(value: Bytes) -> Bool",
            &["value"],
            "Checks whether bytes contain valid UTF-8.",
        ),
        Builtin::BytesDecodeUtf8 => builtin(
            "__bytes_decode_utf8",
            "__bytes_decode_utf8(value: Bytes) -> String",
            &["value"],
            "Decodes bytes already validated as UTF-8.",
        ),
        Builtin::ByteBufferEmpty => builtin(
            "__byte_buffer_empty",
            "__byte_buffer_empty() -> ByteBuffer",
            &[],
            "Creates an empty mutable byte buffer.",
        ),
        Builtin::ByteBufferWithCapacity => builtin(
            "__byte_buffer_with_capacity",
            "__byte_buffer_with_capacity(capacity: Int) -> ByteBuffer",
            &["capacity"],
            "Creates a byte buffer with reserved capacity.",
        ),
        Builtin::ByteBufferPush => builtin(
            "__byte_buffer_push",
            "__byte_buffer_push(buffer: ref ByteBuffer, value: Byte) -> Unit",
            &["buffer", "value"],
            "Appends one byte to a buffer.",
        ),
        Builtin::ByteBufferExtend => builtin(
            "__byte_buffer_extend",
            "__byte_buffer_extend(buffer: ref ByteBuffer, values: Bytes) -> Unit",
            &["buffer", "values"],
            "Appends immutable bytes to a buffer.",
        ),
        Builtin::ByteBufferClear => builtin(
            "__byte_buffer_clear",
            "__byte_buffer_clear(buffer: ref ByteBuffer) -> Unit",
            &["buffer"],
            "Removes all buffered bytes.",
        ),
        Builtin::ByteBufferTruncate => builtin(
            "__byte_buffer_truncate",
            "__byte_buffer_truncate(buffer: ref ByteBuffer, length: Int) -> Unit",
            &["buffer", "length"],
            "Truncates a buffer to at most the supplied length.",
        ),
        Builtin::ByteBufferReserve => builtin(
            "__byte_buffer_reserve",
            "__byte_buffer_reserve(buffer: ref ByteBuffer, additional: Int) -> Unit",
            &["buffer", "additional"],
            "Reserves additional byte capacity.",
        ),
        Builtin::ByteBufferFreeze => builtin(
            "__byte_buffer_freeze",
            "__byte_buffer_freeze(buffer: ByteBuffer) -> Bytes",
            &["buffer"],
            "Transfers a buffer into immutable bytes.",
        ),
        Builtin::ByteBufferSnapshot => builtin(
            "__byte_buffer_snapshot",
            "__byte_buffer_snapshot(buffer: ByteBuffer) -> Bytes",
            &["buffer"],
            "Copies the current buffer into immutable bytes.",
        ),
        Builtin::IoReadText => builtin(
            "__io_read_text",
            "__io_read_text(path: String) -> Result<String, IoError>",
            &["path"],
            "Reads a UTF-8 text file through the host filesystem boundary.",
        ),
        Builtin::IoWriteText => builtin(
            "__io_write_text",
            "__io_write_text(path: String, contents: String) -> Result<Unit, IoError>",
            &["path", "contents"],
            "Writes a UTF-8 text file through the host filesystem boundary.",
        ),
        Builtin::IoReadBytes => builtin(
            "__io_read_bytes",
            "__io_read_bytes(path: String) -> Result<Bytes, IoError>",
            &["path"],
            "Reads raw bytes through the host filesystem boundary.",
        ),
        Builtin::IoWriteBytes => builtin(
            "__io_write_bytes",
            "__io_write_bytes(path: String, contents: Bytes) -> Result<Unit, IoError>",
            &["path", "contents"],
            "Writes raw bytes through the host filesystem boundary.",
        ),
        Builtin::IoListDirectory => builtin(
            "__io_list_directory",
            "__io_list_directory(path: String) -> Result<List<String>, IoError>",
            &["path"],
            "Lists directory entries through the host filesystem boundary.",
        ),
        Builtin::IoExists => builtin(
            "__io_exists",
            "__io_exists(path: String) -> Bool",
            &["path"],
            "Reports whether a host filesystem path exists.",
        ),
        Builtin::IoIsFile => builtin(
            "__io_is_file",
            "__io_is_file(path: String) -> Bool",
            &["path"],
            "Reports whether a host filesystem path is a file.",
        ),
        Builtin::IoIsDirectory => builtin(
            "__io_is_directory",
            "__io_is_directory(path: String) -> Bool",
            &["path"],
            "Reports whether a host filesystem path is a directory.",
        ),
        Builtin::IoJoin => builtin(
            "__io_join",
            "__io_join(left: String, right: String) -> String",
            &["left", "right"],
            "Joins two path components using host path rules.",
        ),
        Builtin::IoParent => builtin(
            "__io_parent",
            "__io_parent(path: String) -> String",
            &["path"],
            "Returns the parent of a host filesystem path.",
        ),
        Builtin::IoFileName => builtin(
            "__io_file_name",
            "__io_file_name(path: String) -> String",
            &["path"],
            "Returns the final component of a host filesystem path.",
        ),
        Builtin::IoExtension => builtin(
            "__io_extension",
            "__io_extension(path: String) -> String",
            &["path"],
            "Returns the extension of a host filesystem path.",
        ),
        Builtin::IoCanonicalize => builtin(
            "__io_canonicalize",
            "__io_canonicalize(path: String) -> Result<String, IoError>",
            &["path"],
            "Canonicalizes a host filesystem path.",
        ),
        Builtin::IoCurrentDirectory => builtin(
            "__io_current_directory",
            "__io_current_directory() -> Result<String, IoError>",
            &[],
            "Returns the host process's current directory.",
        ),
        Builtin::TcpListen => builtin(
            "__tcp_listen",
            "__tcp_listen(host: String, port: Int) -> Result<Int, NetworkError>",
            &["host", "port"],
            "Creates a host TCP listener and returns its opaque handle.",
        ),
        Builtin::TcpConnect => builtin(
            "__tcp_connect",
            "__tcp_connect(host: String, port: Int) -> Result<Int, NetworkError>",
            &["host", "port"],
            "Connects to a TCP endpoint and returns its opaque handle.",
        ),
        Builtin::TcpAccept => builtin(
            "__tcp_accept",
            "__tcp_accept(listener: Int) -> Result<Int, NetworkError>",
            &["listener"],
            "Accepts one connection from a TCP listener.",
        ),
        Builtin::TcpRead => builtin(
            "__tcp_read",
            "__tcp_read(connection: Int, maximum: Int) -> Result<String, NetworkError>",
            &["connection", "maximum"],
            "Reads UTF-8 text from a TCP connection.",
        ),
        Builtin::TcpWrite => builtin(
            "__tcp_write",
            "__tcp_write(connection: Int, contents: String) -> Result<Unit, NetworkError>",
            &["connection", "contents"],
            "Writes UTF-8 text to a TCP connection.",
        ),
        Builtin::TcpReadBytes => builtin(
            "__tcp_read_bytes",
            "__tcp_read_bytes(connection: Int, maximum: Int) -> Result<Bytes, NetworkError>",
            &["connection", "maximum"],
            "Reads raw bytes from a TCP connection.",
        ),
        Builtin::TcpWriteBytes => builtin(
            "__tcp_write_bytes",
            "__tcp_write_bytes(connection: Int, contents: Bytes) -> Result<Unit, NetworkError>",
            &["connection", "contents"],
            "Writes raw bytes to a TCP connection.",
        ),
        Builtin::TcpSetTimeout => builtin(
            "__tcp_set_timeout",
            "__tcp_set_timeout(connection: Int, milliseconds: Int) -> Result<Unit, NetworkError>",
            &["connection", "milliseconds"],
            "Sets a TCP connection timeout.",
        ),
        Builtin::TcpCloseListener => builtin(
            "__tcp_close_listener",
            "__tcp_close_listener(listener: Int) -> Result<Unit, NetworkError>",
            &["listener"],
            "Closes a host TCP listener.",
        ),
        Builtin::TcpCloseConnection => builtin(
            "__tcp_close_connection",
            "__tcp_close_connection(connection: Int) -> Result<Unit, NetworkError>",
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
