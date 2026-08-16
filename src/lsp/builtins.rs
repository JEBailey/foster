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
        Builtin::IoReadText => builtin(
            "__io_read_text",
            "__io_read_text(path: String) -> Result[String, IoError]",
            &["path"],
            "Reads a UTF-8 text file through the host filesystem boundary.",
        ),
        Builtin::IoWriteText => builtin(
            "__io_write_text",
            "__io_write_text(path: String, contents: String) -> Result[Unit, IoError]",
            &["path", "contents"],
            "Writes a UTF-8 text file through the host filesystem boundary.",
        ),
        Builtin::IoListDirectory => builtin(
            "__io_list_directory",
            "__io_list_directory(path: String) -> Result[List[String], IoError]",
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
            "__io_canonicalize(path: String) -> Result[String, IoError]",
            &["path"],
            "Canonicalizes a host filesystem path.",
        ),
        Builtin::IoCurrentDirectory => builtin(
            "__io_current_directory",
            "__io_current_directory() -> Result[String, IoError]",
            &[],
            "Returns the host process's current directory.",
        ),
        Builtin::TcpListen => builtin(
            "__tcp_listen",
            "__tcp_listen(host: String, port: Int) -> Result[Int, NetworkError]",
            &["host", "port"],
            "Creates a host TCP listener and returns its opaque handle.",
        ),
        Builtin::TcpConnect => builtin(
            "__tcp_connect",
            "__tcp_connect(host: String, port: Int) -> Result[Int, NetworkError]",
            &["host", "port"],
            "Connects to a TCP endpoint and returns its opaque handle.",
        ),
        Builtin::TcpAccept => builtin(
            "__tcp_accept",
            "__tcp_accept(listener: Int) -> Result[Int, NetworkError]",
            &["listener"],
            "Accepts one connection from a TCP listener.",
        ),
        Builtin::TcpRead => builtin(
            "__tcp_read",
            "__tcp_read(connection: Int, maximum: Int) -> Result[String, NetworkError]",
            &["connection", "maximum"],
            "Reads UTF-8 text from a TCP connection.",
        ),
        Builtin::TcpWrite => builtin(
            "__tcp_write",
            "__tcp_write(connection: Int, contents: String) -> Result[Unit, NetworkError]",
            &["connection", "contents"],
            "Writes UTF-8 text to a TCP connection.",
        ),
        Builtin::TcpSetTimeout => builtin(
            "__tcp_set_timeout",
            "__tcp_set_timeout(connection: Int, milliseconds: Int) -> Result[Unit, NetworkError]",
            &["connection", "milliseconds"],
            "Sets a TCP connection timeout.",
        ),
        Builtin::TcpCloseListener => builtin(
            "__tcp_close_listener",
            "__tcp_close_listener(listener: Int) -> Result[Unit, NetworkError]",
            &["listener"],
            "Closes a host TCP listener.",
        ),
        Builtin::TcpCloseConnection => builtin(
            "__tcp_close_connection",
            "__tcp_close_connection(connection: Int) -> Result[Unit, NetworkError]",
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
