use std::sync::{Arc, OnceLock};

use crate::error::RuntimeError;
use crate::hir::Builtin;

use super::Value;
use super::value::RecordFields;

pub(super) fn native_host() -> Arc<dyn HostServices> {
    Arc::new(NativeHost)
}

pub(super) fn dispatch(
    host: &dyn HostServices,
    builtin: Builtin,
    arguments: &[Value],
    string_record: Option<crate::hir::RecordId>,
) -> Result<Value, RuntimeError> {
    match (builtin, arguments) {
        (Builtin::Print | Builtin::Println, arguments) => {
            let rendered = arguments
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(" ");
            if builtin == Builtin::Println {
                println!("{rendered}");
            } else {
                print!("{rendered}");
            }
            Ok(Value::Unit)
        }
        (Builtin::CodePoint, [Value::CodePoint(value)]) => Ok(Value::Integer(*value as i64)),
        (Builtin::FromCodePoint, [Value::Integer(value)]) => u32::try_from(*value)
            .ok()
            .and_then(char::from_u32)
            .map(Value::CodePoint)
            .ok_or_else(|| RuntimeError::runtime("invalid Unicode scalar value")),
        (Builtin::ParseFloat, [value]) if value.string_bytes().is_some() => value
            .string_text()?
            .parse::<f64>()
            .map(Value::Float)
            .map_err(|_| RuntimeError::runtime("invalid Float text")),
        (Builtin::ByteValid, [Value::Integer(value)]) => {
            Ok(Value::Bool(u8::try_from(*value).is_ok()))
        }
        (Builtin::ByteUnchecked, [Value::Integer(value)]) => u8::try_from(*value)
            .map(Value::Byte)
            .map_err(|_| RuntimeError::runtime("Byte is outside 0..255")),
        (Builtin::BytesEmpty, []) => Ok(Value::bytes(Vec::new())),
        (Builtin::BytesFromList, [value]) if value.list_value().is_some() => {
            let values = value.list_value().unwrap();
            let bytes = values
                .iter()
                .map(|value| match value {
                    Value::Byte(value) => Ok(*value),
                    _ => Err(RuntimeError::runtime("Bytes.from requires List<Byte>")),
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Value::bytes(bytes))
        }
        (Builtin::BytesConcat, [left, right])
            if left.bytes_value().is_some() && right.bytes_value().is_some() =>
        {
            let left = left.bytes_value().unwrap();
            let right = right.bytes_value().unwrap();
            let mut bytes = Vec::with_capacity(left.len() + right.len());
            bytes.extend_from_slice(left);
            bytes.extend_from_slice(right);
            Ok(Value::bytes(bytes))
        }
        (Builtin::BytesSlice, [values, Value::Integer(start), Value::Integer(end)])
            if values.bytes_value().is_some() =>
        {
            let values = values.bytes_value().unwrap();
            let start = usize::try_from(*start)
                .map_err(|_| RuntimeError::runtime("byte slice start is out of bounds"))?;
            let end = usize::try_from(*end)
                .map_err(|_| RuntimeError::runtime("byte slice end is out of bounds"))?;
            let slice = values
                .get(start..end)
                .ok_or_else(|| RuntimeError::runtime("byte slice is out of bounds"))?;
            Ok(Value::bytes(slice.to_vec()))
        }
        (Builtin::BytesToList, [values]) if values.bytes_value().is_some() => Ok(Value::list(
            values
                .bytes_value()
                .unwrap()
                .iter()
                .copied()
                .map(Value::Byte)
                .collect(),
        )),
        (Builtin::BytesHex, [values]) if values.bytes_value().is_some() => Ok(Value::string(
            string_record,
            values
                .bytes_value()
                .unwrap()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
                .into_bytes(),
        )),
        (Builtin::BytesFromHex, [value]) if value.string_bytes().is_some() => {
            Ok(match decode_hex(value.string_text()?) {
                Ok(bytes) => result_ok(Value::bytes(bytes)),
                Err((offset, message)) => result_error(Value::Record {
                    record: None,
                    name: "HexError".into(),
                    fields: RecordFields::from_pairs([
                        ("offset".into(), Value::Integer(offset as i64)),
                        (
                            "message".into(),
                            Value::string(string_record, message.into_bytes()),
                        ),
                    ]),
                }),
            })
        }
        (Builtin::StringUtf8, [value]) if value.string_bytes().is_some() => {
            Ok(Value::bytes(value.string_bytes().unwrap().to_vec()))
        }
        (Builtin::BytesUtf8Valid, [value]) if value.bytes_value().is_some() => Ok(Value::Bool(
            std::str::from_utf8(value.bytes_value().unwrap()).is_ok(),
        )),
        (Builtin::BytesDecodeUtf8, [value]) if value.bytes_value().is_some() => {
            std::str::from_utf8(value.bytes_value().unwrap())
                .map(|value| Value::string(string_record, value.as_bytes().to_vec()))
                .map_err(|_| RuntimeError::runtime("Bytes are not valid UTF-8"))
        }
        (Builtin::ByteBufferEmpty, []) => Ok(Value::RawByteBuffer(Vec::new())),
        (Builtin::ByteBufferWithCapacity, [Value::Integer(capacity)]) => {
            let capacity = usize::try_from(*capacity)
                .map_err(|_| RuntimeError::runtime("ByteBuffer capacity cannot be negative"))?;
            Ok(Value::RawByteBuffer(Vec::with_capacity(capacity)))
        }
        (Builtin::ByteBufferPush, [Value::RawByteBuffer(buffer), Value::Byte(value)]) => {
            let mut buffer = buffer.clone();
            buffer.push(*value);
            Ok(Value::RawByteBuffer(buffer))
        }
        (Builtin::ByteBufferExtend, [Value::RawByteBuffer(buffer), values])
            if values.bytes_value().is_some() =>
        {
            let mut buffer = buffer.clone();
            buffer.extend_from_slice(values.bytes_value().unwrap());
            Ok(Value::RawByteBuffer(buffer))
        }
        (Builtin::ByteBufferClear, [Value::RawByteBuffer(buffer)]) => {
            let mut buffer = buffer.clone();
            buffer.clear();
            Ok(Value::RawByteBuffer(buffer))
        }
        (Builtin::ByteBufferTruncate, [Value::RawByteBuffer(buffer), Value::Integer(length)]) => {
            let length = usize::try_from(*length)
                .map_err(|_| RuntimeError::runtime("truncate length cannot be negative"))?;
            let mut buffer = buffer.clone();
            buffer.truncate(length);
            Ok(Value::RawByteBuffer(buffer))
        }
        (
            Builtin::ByteBufferReserve,
            [Value::RawByteBuffer(buffer), Value::Integer(additional)],
        ) => {
            let additional = usize::try_from(*additional)
                .map_err(|_| RuntimeError::runtime("reserve amount cannot be negative"))?;
            let mut buffer = buffer.clone();
            buffer.reserve(additional);
            Ok(Value::RawByteBuffer(buffer))
        }
        (
            Builtin::ByteBufferFreeze | Builtin::ByteBufferSnapshot,
            [Value::RawByteBuffer(value)],
        ) => Ok(Value::bytes(value.clone())),
        _ => host.dispatch(builtin, arguments, string_record),
    }
}

pub(super) trait HostServices: Send + Sync {
    fn dispatch(
        &self,
        builtin: Builtin,
        arguments: &[Value],
        string_record: Option<crate::hir::RecordId>,
    ) -> Result<Value, RuntimeError>;
}

struct NativeHost;

impl HostServices for NativeHost {
    fn dispatch(
        &self,
        builtin: Builtin,
        arguments: &[Value],
        string_record: Option<crate::hir::RecordId>,
    ) -> Result<Value, RuntimeError> {
        match (builtin, arguments) {
            (Builtin::IoReadText, [path]) if path.string_bytes().is_some() => {
                let path = path.string_text()?;
                Ok(io_result(
                    "read_text",
                    path,
                    std::fs::read(path).map(|bytes| Value::string(string_record, bytes)),
                    string_record,
                ))
            }
            (Builtin::IoWriteText, [path, text])
                if path.string_bytes().is_some() && text.string_bytes().is_some() =>
            {
                let path = path.string_text()?;
                Ok(io_result(
                    "write_text",
                    path,
                    std::fs::write(path, text.string_bytes().unwrap()).map(|()| Value::Unit),
                    string_record,
                ))
            }
            (Builtin::IoReadBytes, [path]) if path.string_bytes().is_some() => {
                let path = path.string_text()?;
                Ok(io_result(
                    "read_bytes",
                    path,
                    std::fs::read(path).map(Value::bytes),
                    string_record,
                ))
            }
            (Builtin::IoWriteBytes, [path, bytes])
                if path.string_bytes().is_some() && bytes.bytes_value().is_some() =>
            {
                let path = path.string_text()?;
                Ok(io_result(
                    "write_bytes",
                    path,
                    std::fs::write(path, bytes.bytes_value().unwrap()).map(|()| Value::Unit),
                    string_record,
                ))
            }
            (Builtin::IoListDirectory, [path]) if path.string_bytes().is_some() => {
                let path = path.string_text()?;
                let entries = std::fs::read_dir(path).and_then(|entries| {
                    let mut names = Vec::new();
                    for entry in entries {
                        let name = entry?.file_name().into_string().map_err(|_| {
                            std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                "directory entry name is not valid UTF-8",
                            )
                        })?;
                        names.push(Value::string(string_record, name.into_bytes()));
                    }
                    names.sort_by_key(|name| name.to_string());
                    Ok(Value::list(names))
                });
                Ok(io_result("list_directory", path, entries, string_record))
            }
            (Builtin::IoExists, [path]) if path.string_bytes().is_some() => Ok(Value::Bool(
                std::path::Path::new(path.string_text()?).exists(),
            )),
            (Builtin::IoIsFile, [path]) if path.string_bytes().is_some() => Ok(Value::Bool(
                std::path::Path::new(path.string_text()?).is_file(),
            )),
            (Builtin::IoIsDirectory, [path]) if path.string_bytes().is_some() => Ok(Value::Bool(
                std::path::Path::new(path.string_text()?).is_dir(),
            )),
            (Builtin::IoJoin, [left, right])
                if left.string_bytes().is_some() && right.string_bytes().is_some() =>
            {
                path_value(
                    std::path::Path::new(left.string_text()?).join(right.string_text()?),
                    string_record,
                )
            }
            (Builtin::IoParent, [path]) if path.string_bytes().is_some() => {
                optional_path_component(
                    std::path::Path::new(path.string_text()?)
                        .parent()
                        .map(std::path::Path::to_path_buf),
                    string_record,
                )
            }
            (Builtin::IoFileName, [path]) if path.string_bytes().is_some() => {
                optional_os_component(
                    std::path::Path::new(path.string_text()?).file_name(),
                    string_record,
                )
            }
            (Builtin::IoExtension, [path]) if path.string_bytes().is_some() => {
                optional_os_component(
                    std::path::Path::new(path.string_text()?).extension(),
                    string_record,
                )
            }
            (Builtin::IoCanonicalize, [path]) if path.string_bytes().is_some() => {
                let path = path.string_text()?;
                Ok(io_result(
                    "canonicalize",
                    path,
                    std::fs::canonicalize(path).and_then(|path| path_value_io(path, string_record)),
                    string_record,
                ))
            }
            (Builtin::IoCurrentDirectory, []) => Ok(io_result(
                "current_directory",
                "",
                std::env::current_dir().and_then(|path| path_value_io(path, string_record)),
                string_record,
            )),
            (Builtin::TcpListen, [address, Value::Integer(port)])
                if address.string_bytes().is_some() =>
            {
                Ok(tcp_result(
                    "listen",
                    super::host::listen(address.string_text()?, *port).map(Value::Integer),
                    string_record,
                ))
            }
            (Builtin::TcpConnect, [address, Value::Integer(port)])
                if address.string_bytes().is_some() =>
            {
                Ok(tcp_result(
                    "connect",
                    super::host::connect(address.string_text()?, *port).map(Value::Integer),
                    string_record,
                ))
            }
            (Builtin::TcpAccept, [Value::Integer(listener)]) => Ok(tcp_result(
                "accept",
                super::host::accept(*listener).map(Value::Integer),
                string_record,
            )),
            (Builtin::TcpRead, [Value::Integer(connection), Value::Integer(maximum)]) => {
                Ok(tcp_result(
                    "read",
                    super::host::read(*connection, *maximum)
                        .map(|value| Value::string(string_record, value.into_bytes())),
                    string_record,
                ))
            }
            (Builtin::TcpWrite, [Value::Integer(connection), text])
                if text.string_bytes().is_some() =>
            {
                Ok(tcp_result(
                    "write",
                    super::host::write(*connection, text.string_text()?).map(|()| Value::Unit),
                    string_record,
                ))
            }
            (Builtin::TcpReadBytes, [Value::Integer(connection), Value::Integer(maximum)]) => {
                Ok(tcp_result(
                    "read_bytes",
                    super::host::read_bytes(*connection, *maximum).map(Value::bytes),
                    string_record,
                ))
            }
            (Builtin::TcpWriteBytes, [Value::Integer(connection), bytes])
                if bytes.bytes_value().is_some() =>
            {
                Ok(tcp_result(
                    "write_bytes",
                    super::host::write_bytes(*connection, bytes.bytes_value().unwrap())
                        .map(|()| Value::Unit),
                    string_record,
                ))
            }
            (
                Builtin::TcpSetTimeout,
                [Value::Integer(connection), Value::Integer(milliseconds)],
            ) => Ok(tcp_result(
                "set_timeout",
                super::host::set_timeout(*connection, *milliseconds).map(|()| Value::Unit),
                string_record,
            )),
            (Builtin::TcpCloseListener, [Value::Integer(listener)]) => Ok(tcp_result(
                "close_listener",
                super::host::close_listener(*listener).map(|()| Value::Unit),
                string_record,
            )),
            (Builtin::TcpCloseConnection, [Value::Integer(connection)]) => Ok(tcp_result(
                "close_connection",
                super::host::close_connection(*connection).map(|()| Value::Unit),
                string_record,
            )),
            _ => Err(RuntimeError::runtime("invalid builtin arguments")),
        }
    }
}

fn decode_hex(value: &str) -> Result<Vec<u8>, (usize, String)> {
    if !value.len().is_multiple_of(2) {
        return Err((
            value.len(),
            "hexadecimal byte text must have even length".into(),
        ));
    }
    let mut decoded = Vec::with_capacity(value.len() / 2);
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let offset = index * 2;
        let high = hex_nibble(pair[0]).ok_or_else(|| {
            (
                offset,
                format!("invalid hexadecimal digit `{}`", pair[0] as char),
            )
        })?;
        let low = hex_nibble(pair[1]).ok_or_else(|| {
            (
                offset + 1,
                format!("invalid hexadecimal digit `{}`", pair[1] as char),
            )
        })?;
        decoded.push((high << 4) | low);
    }
    Ok(decoded)
}

fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn io_result(
    operation: &str,
    path: &str,
    result: Result<Value, std::io::Error>,
    string_record: Option<crate::hir::RecordId>,
) -> Value {
    match result {
        Ok(value) => result_ok(value),
        Err(error) => result_error(Value::Record {
            record: None,
            name: "IoError".into(),
            fields: RecordFields::from_pairs([
                (
                    "operation".into(),
                    Value::string(string_record, operation.as_bytes().to_vec()),
                ),
                (
                    "path".into(),
                    Value::string(string_record, path.as_bytes().to_vec()),
                ),
                (
                    "message".into(),
                    Value::string(string_record, error.to_string().into_bytes()),
                ),
            ]),
        }),
    }
}

fn tcp_result(
    operation: &str,
    result: Result<Value, String>,
    string_record: Option<crate::hir::RecordId>,
) -> Value {
    match result {
        Ok(value) => result_ok(value),
        Err(message) => result_error(Value::Record {
            record: None,
            name: "NetworkError".into(),
            fields: RecordFields::from_pairs([
                (
                    "operation".into(),
                    Value::string(string_record, operation.as_bytes().to_vec()),
                ),
                (
                    "message".into(),
                    Value::string(string_record, message.into_bytes()),
                ),
            ]),
        }),
    }
}

fn result_ok(value: Value) -> Value {
    let (type_name, alternative) = result_variant_names(true);
    Value::Variant {
        variant: None,
        type_name,
        alternative,
        payload: vec![value],
    }
}

fn result_error(error: Value) -> Value {
    let (type_name, alternative) = result_variant_names(false);
    Value::Variant {
        variant: None,
        type_name,
        alternative,
        payload: vec![error],
    }
}

fn result_variant_names(ok: bool) -> (Arc<str>, Arc<str>) {
    static RESULT: OnceLock<Arc<str>> = OnceLock::new();
    static OK: OnceLock<Arc<str>> = OnceLock::new();
    static ERROR: OnceLock<Arc<str>> = OnceLock::new();
    let type_name = RESULT.get_or_init(|| Arc::from("Result")).clone();
    let alternative = if ok {
        OK.get_or_init(|| Arc::from("Ok")).clone()
    } else {
        ERROR.get_or_init(|| Arc::from("Error")).clone()
    };
    (type_name, alternative)
}

fn path_value(
    path: std::path::PathBuf,
    string_record: Option<crate::hir::RecordId>,
) -> Result<Value, RuntimeError> {
    path.into_os_string()
        .into_string()
        .map(|value| Value::string(string_record, value.into_bytes()))
        .map_err(|_| RuntimeError::runtime("path is not valid UTF-8"))
}

fn path_value_io(
    path: std::path::PathBuf,
    string_record: Option<crate::hir::RecordId>,
) -> Result<Value, std::io::Error> {
    path.into_os_string()
        .into_string()
        .map(|value| Value::string(string_record, value.into_bytes()))
        .map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "path is not valid UTF-8")
        })
}

fn optional_path_component(
    path: Option<std::path::PathBuf>,
    string_record: Option<crate::hir::RecordId>,
) -> Result<Value, RuntimeError> {
    match path {
        Some(path) => path_value(path, string_record),
        None => Ok(Value::string(string_record, Vec::new())),
    }
}

fn optional_os_component(
    value: Option<&std::ffi::OsStr>,
    string_record: Option<crate::hir::RecordId>,
) -> Result<Value, RuntimeError> {
    match value {
        Some(value) => value
            .to_str()
            .map(|value| Value::string(string_record, value.as_bytes().to_vec()))
            .ok_or_else(|| RuntimeError::runtime("path component is not valid UTF-8")),
        None => Ok(Value::string(string_record, Vec::new())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DeniedHost;

    impl HostServices for DeniedHost {
        fn dispatch(
            &self,
            _builtin: Builtin,
            _arguments: &[Value],
            _string_record: Option<crate::hir::RecordId>,
        ) -> Result<Value, RuntimeError> {
            Err(RuntimeError::runtime("host access denied"))
        }
    }

    #[test]
    fn core_builtins_do_not_require_host_access() {
        assert_eq!(
            dispatch(
                &DeniedHost,
                Builtin::ByteValid,
                &[Value::Integer(255)],
                None
            )
            .unwrap(),
            Value::Bool(true)
        );
    }

    #[test]
    fn host_builtins_cross_the_capability_boundary() {
        let path = Value::string(None, b"ignored".to_vec());
        let error = dispatch(&DeniedHost, Builtin::IoExists, &[path], None).unwrap_err();
        assert_eq!(error.message, "host access denied");
    }
}
