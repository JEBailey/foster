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

pub(crate) fn handler(builtin: crate::intrinsics::Builtin) -> Option<BuiltinHandler> {
    match builtin {
        crate::intrinsics::Builtin::CCallbackNew => builtin_handler!(ConsumeFirst, CCallbackNew),
        crate::intrinsics::Builtin::CCallbackRelease => builtin_handler!(Direct, CCallbackRelease),
        crate::intrinsics::Builtin::CCallbackPoll => builtin_handler!(Direct, CCallbackPoll),
        crate::intrinsics::Builtin::CCallbackError => builtin_handler!(Direct, CCallbackError),
        crate::intrinsics::Builtin::ProcessReserve => builtin_handler!(Direct, ProcessReserve),
        crate::intrinsics::Builtin::ProcessExchange => builtin_handler!(Direct, ProcessExchange),
        crate::intrinsics::Builtin::ProcessRelease => builtin_handler!(Direct, ProcessRelease),
        crate::intrinsics::Builtin::ProcessWait => builtin_handler!(Direct, ProcessWait),
        crate::intrinsics::Builtin::CExchange => builtin_handler!(Direct, CExchange),
        crate::intrinsics::Builtin::CClose => builtin_handler!(Direct, CClose),
        crate::intrinsics::Builtin::CRelease => builtin_handler!(Direct, CRelease),
        crate::intrinsics::Builtin::CEncodeInt => builtin_handler!(Direct, CEncodeInt),
        crate::intrinsics::Builtin::CEncodeFloat => builtin_handler!(Direct, CEncodeFloat),
        crate::intrinsics::Builtin::CDecodeInt => builtin_handler!(Direct, CDecodeInt),
        crate::intrinsics::Builtin::CDecodeFloat => builtin_handler!(Direct, CDecodeFloat),
        crate::intrinsics::Builtin::Print => builtin_handler!(Direct, Print),
        crate::intrinsics::Builtin::Println => builtin_handler!(Direct, Println),
        crate::intrinsics::Builtin::FromCodePoint => builtin_handler!(Direct, FromCodePoint),
        crate::intrinsics::Builtin::ParseFloat => builtin_handler!(Direct, ParseFloat),
        crate::intrinsics::Builtin::FormatFloat => builtin_handler!(Direct, FormatFloat),
        crate::intrinsics::Builtin::FloatBits => builtin_handler!(Direct, FloatBits),
        crate::intrinsics::Builtin::FloatSqrt => builtin_handler!(Direct, FloatSqrt),
        crate::intrinsics::Builtin::FloatSin => builtin_handler!(Direct, FloatSin),
        crate::intrinsics::Builtin::FloatCos => builtin_handler!(Direct, FloatCos),
        crate::intrinsics::Builtin::FloatFromInt => builtin_handler!(Direct, FloatFromInt),
        crate::intrinsics::Builtin::IntFromFloat => builtin_handler!(Direct, IntFromFloat),
        crate::intrinsics::Builtin::FloatFloor => builtin_handler!(Direct, FloatFloor),
        crate::intrinsics::Builtin::FloatCeil => builtin_handler!(Direct, FloatCeil),
        crate::intrinsics::Builtin::FloatRound => builtin_handler!(Direct, FloatRound),
        crate::intrinsics::Builtin::FloatTruncate => builtin_handler!(Direct, FloatTruncate),
        crate::intrinsics::Builtin::FloatAtan2 => builtin_handler!(Direct, FloatAtan2),
        crate::intrinsics::Builtin::Float32Bits => builtin_handler!(Direct, Float32Bits),
        crate::intrinsics::Builtin::ByteValid => builtin_handler!(Direct, ByteValid),
        crate::intrinsics::Builtin::ByteUnchecked => builtin_handler!(Direct, ByteUnchecked),
        crate::intrinsics::Builtin::BytesEmpty => builtin_handler!(Direct, BytesEmpty),
        crate::intrinsics::Builtin::BytesFromList => builtin_handler!(Direct, BytesFromList),
        crate::intrinsics::Builtin::BytesConcat => builtin_handler!(Direct, BytesConcat),
        crate::intrinsics::Builtin::BytesSlice => builtin_handler!(Direct, BytesSlice),
        crate::intrinsics::Builtin::BytesToList => builtin_handler!(Direct, BytesToList),
        crate::intrinsics::Builtin::BytesHex => builtin_handler!(Direct, BytesHex),
        crate::intrinsics::Builtin::BytesFromHex => builtin_handler!(Direct, BytesFromHex),
        crate::intrinsics::Builtin::StringBytes => builtin_handler!(Direct, StringBytes),
        crate::intrinsics::Builtin::BytesUtf8Valid => builtin_handler!(Direct, BytesUtf8Valid),
        crate::intrinsics::Builtin::ByteBufferEmpty => builtin_handler!(Direct, ByteBufferEmpty),
        crate::intrinsics::Builtin::ByteBufferWithCapacity => {
            builtin_handler!(Direct, ByteBufferWithCapacity)
        }
        crate::intrinsics::Builtin::ByteBufferPush => {
            builtin_handler!(TransformFirst, ByteBufferPush)
        }
        crate::intrinsics::Builtin::ByteBufferExtend => {
            builtin_handler!(TransformFirst, ByteBufferExtend)
        }
        crate::intrinsics::Builtin::ByteBufferClear => {
            builtin_handler!(TransformFirst, ByteBufferClear)
        }
        crate::intrinsics::Builtin::ByteBufferTruncate => {
            builtin_handler!(TransformFirst, ByteBufferTruncate)
        }
        crate::intrinsics::Builtin::ByteBufferReserve => {
            builtin_handler!(TransformFirst, ByteBufferReserve)
        }
        crate::intrinsics::Builtin::ByteBufferFreeze => {
            builtin_handler!(ConsumeFirst, ByteBufferFreeze)
        }
        crate::intrinsics::Builtin::ByteBufferSnapshot => {
            builtin_handler!(Direct, ByteBufferSnapshot)
        }
        crate::intrinsics::Builtin::IoReadText => builtin_handler!(Host, IoReadText),
        crate::intrinsics::Builtin::IoWriteText => builtin_handler!(Host, IoWriteText),
        crate::intrinsics::Builtin::IoReadBytes => builtin_handler!(Host, IoReadBytes),
        crate::intrinsics::Builtin::IoStdinRead => builtin_handler!(Host, IoStdinRead),
        crate::intrinsics::Builtin::IoWriteBytes => builtin_handler!(Host, IoWriteBytes),
        crate::intrinsics::Builtin::IoListDirectory => builtin_handler!(Host, IoListDirectory),
        crate::intrinsics::Builtin::IoExists => builtin_handler!(Host, IoExists),
        crate::intrinsics::Builtin::IoIsFile => builtin_handler!(Host, IoIsFile),
        crate::intrinsics::Builtin::IoIsDirectory => builtin_handler!(Host, IoIsDirectory),
        crate::intrinsics::Builtin::IoCreateDirectory => builtin_handler!(Host, IoCreateDirectory),
        crate::intrinsics::Builtin::IoCreateDirectoryAll => {
            builtin_handler!(Host, IoCreateDirectoryAll)
        }
        crate::intrinsics::Builtin::IoRemoveFile => builtin_handler!(Host, IoRemoveFile),
        crate::intrinsics::Builtin::IoRemoveDirectory => builtin_handler!(Host, IoRemoveDirectory),
        crate::intrinsics::Builtin::IoRename => builtin_handler!(Host, IoRename),
        crate::intrinsics::Builtin::IoCopyFile => builtin_handler!(Host, IoCopyFile),
        crate::intrinsics::Builtin::IoJoin => builtin_handler!(Host, IoJoin),
        crate::intrinsics::Builtin::IoParent => builtin_handler!(Host, IoParent),
        crate::intrinsics::Builtin::IoFileName => builtin_handler!(Host, IoFileName),
        crate::intrinsics::Builtin::IoExtension => builtin_handler!(Host, IoExtension),
        crate::intrinsics::Builtin::IoCanonicalize => builtin_handler!(Host, IoCanonicalize),
        crate::intrinsics::Builtin::IoCurrentDirectory => {
            builtin_handler!(Host, IoCurrentDirectory)
        }
        crate::intrinsics::Builtin::TcpListen => builtin_handler!(Host, TcpListen),
        crate::intrinsics::Builtin::TcpConnect => builtin_handler!(Host, TcpConnect),
        crate::intrinsics::Builtin::TcpAccept => builtin_handler!(Host, TcpAccept),
        crate::intrinsics::Builtin::TcpRead => builtin_handler!(Host, TcpRead),
        crate::intrinsics::Builtin::TcpWrite => builtin_handler!(Host, TcpWrite),
        crate::intrinsics::Builtin::TcpReadBytes => builtin_handler!(Host, TcpReadBytes),
        crate::intrinsics::Builtin::TcpWriteBytes => builtin_handler!(Host, TcpWriteBytes),
        crate::intrinsics::Builtin::TcpSetTimeout => builtin_handler!(Host, TcpSetTimeout),
        crate::intrinsics::Builtin::TcpCloseListener => builtin_handler!(Host, TcpCloseListener),
        crate::intrinsics::Builtin::TcpCloseConnection => {
            builtin_handler!(Host, TcpCloseConnection)
        }
        crate::intrinsics::Builtin::IoReadRange => builtin_handler!(Host, IoReadRange),
        crate::intrinsics::Builtin::IoAppendBytes => builtin_handler!(Host, IoAppendBytes),
        crate::intrinsics::Builtin::IoFileLength => builtin_handler!(Host, IoFileLength),
        crate::intrinsics::Builtin::TimeWallNow => builtin_handler!(Host, TimeWallNow),
        crate::intrinsics::Builtin::TimeMonotonicNow => builtin_handler!(Host, TimeMonotonicNow),
        crate::intrinsics::Builtin::RandomBytes => builtin_handler!(Host, RandomBytes),
        crate::intrinsics::Builtin::TcpWaitReadable => builtin_handler!(Host, TcpWaitReadable),
        crate::intrinsics::Builtin::TcpWaitWritable => builtin_handler!(Host, TcpWaitWritable),
        crate::intrinsics::Builtin::TcpWaitAccept => builtin_handler!(Host, TcpWaitAccept),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intrinsics::{BUILTINS, BuiltinExecution};
    #[test]
    fn all_handlers_match_execution_contracts() {
        for descriptor in BUILTINS {
            assert_eq!(
                handler(descriptor.builtin).is_some(),
                descriptor.execution != BuiltinExecution::Host,
                "handler registration for {:?}",
                descriptor.builtin
            );
            assert!(matches!(
                (descriptor.execution, handler(descriptor.builtin)),
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
