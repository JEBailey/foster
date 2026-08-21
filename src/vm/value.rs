use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::fmt;
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use crate::error::FosterError;

use super::Capture;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Unit,
    Bool(bool),
    Integer(i64),
    Float(f64),
    CodePoint(char),
    Byte(u8),
    RawBytes(Arc<Vec<u8>>),
    ByteBuffer(Vec<u8>),
    List(Vec<Value>),
    VmClosure {
        function: crate::hir::FunctionId,
        captures: Vec<Capture>,
    },
    Reference(PlaceHandle),
    Remote(RemoteValue),
    Future(FutureValue),
    Record {
        record: Option<crate::hir::RecordId>,
        name: String,
        fields: BTreeMap<String, Value>,
    },
    Variant {
        variant: Option<crate::hir::VariantTypeId>,
        type_name: String,
        alternative: String,
        payload: Vec<Value>,
    },
}

#[derive(Debug)]
pub struct Slot {
    storage: RefCell<SlotStorage>,
    generation: Cell<u64>,
}

#[derive(Debug)]
enum SlotStorage {
    Local(Value),
    Shared(Arc<SharedValue>),
}

#[derive(Debug)]
pub(crate) struct SharedValue {
    value: Mutex<WireValue>,
    gate: Arc<AccessGate>,
}

#[derive(Debug, Default)]
struct AccessGate {
    state: Mutex<AccessState>,
    available: Condvar,
}

#[derive(Debug, Default)]
struct AccessState {
    readers: usize,
    writer: bool,
}

pub(crate) struct AccessLease {
    gate: Arc<AccessGate>,
    write: bool,
}

impl Drop for AccessLease {
    fn drop(&mut self) {
        let mut state = self
            .gate
            .state
            .lock()
            .expect("shared access gate was poisoned");
        if self.write {
            state.writer = false;
        } else {
            state.readers -= 1;
        }
        self.gate.available.notify_all();
    }
}

impl SharedValue {
    fn acquire(&self, write: bool) -> Result<AccessLease, FosterError> {
        let mut state = self
            .gate
            .state
            .lock()
            .map_err(|_| FosterError::runtime("shared access gate was poisoned"))?;
        while state.writer || (write && state.readers > 0) {
            state = self
                .gate
                .available
                .wait(state)
                .map_err(|_| FosterError::runtime("shared access gate was poisoned"))?;
        }
        if write {
            state.writer = true;
        } else {
            state.readers += 1;
        }
        Ok(AccessLease {
            gate: self.gate.clone(),
            write,
        })
    }

    pub(crate) fn read_snapshot(&self) -> Result<(AccessLease, WireValue), FosterError> {
        let lease = self.acquire(false)?;
        let value = self
            .value
            .lock()
            .map_err(|_| FosterError::runtime("shared value lock was poisoned"))?
            .clone();
        Ok((lease, value))
    }

    pub(crate) fn write_snapshot(&self) -> Result<(AccessLease, WireValue), FosterError> {
        let lease = self.acquire(true)?;
        let value = self
            .value
            .lock()
            .map_err(|_| FosterError::runtime("shared value lock was poisoned"))?
            .clone();
        Ok((lease, value))
    }

    pub(crate) fn commit(&self, value: WireValue) -> Result<(), FosterError> {
        *self
            .value
            .lock()
            .map_err(|_| FosterError::runtime("shared value lock was poisoned"))? = value;
        Ok(())
    }
}

impl Slot {
    pub(crate) fn new(value: Value) -> Rc<Self> {
        Rc::new(Self {
            storage: RefCell::new(SlotStorage::Local(value)),
            generation: Cell::new(0),
        })
    }

    pub(crate) fn read(&self) -> Result<Value, FosterError> {
        let shared = match &*self.storage.borrow() {
            SlotStorage::Local(Value::Reference(reference)) => return reference.read(),
            SlotStorage::Local(value) => return Ok(value.clone()),
            SlotStorage::Shared(shared) => shared.clone(),
        };
        let value = shared.read_snapshot()?.1;
        Value::from_wire(value)
    }

    pub(crate) fn share(&self) -> Result<Arc<SharedValue>, FosterError> {
        if let SlotStorage::Shared(shared) = &*self.storage.borrow() {
            return Ok(shared.clone());
        }
        let value = self.argument().into_wire()?;
        let shared = Arc::new(SharedValue {
            value: Mutex::new(value),
            gate: Arc::new(AccessGate::default()),
        });
        *self.storage.borrow_mut() = SlotStorage::Shared(shared.clone());
        Ok(shared)
    }

    pub(crate) fn argument(&self) -> Value {
        match &*self.storage.borrow() {
            SlotStorage::Local(value) => value.clone(),
            SlotStorage::Shared(shared) => {
                Value::from_wire(shared.read_snapshot().expect("shared value read failed").1)
                    .expect("shared slots contain wire-compatible values")
            }
        }
    }

    pub(crate) fn write(&self, value: Value) -> Result<(), FosterError> {
        let target = match &*self.storage.borrow() {
            SlotStorage::Local(Value::Reference(reference)) => {
                return reference.write(value);
            }
            SlotStorage::Local(_) => None,
            SlotStorage::Shared(shared) => Some(shared.clone()),
        };
        if let Some(shared) = target {
            let (_lease, _) = shared.write_snapshot()?;
            shared.commit(value.into_wire()?)?;
        } else {
            *self.storage.borrow_mut() = SlotStorage::Local(value);
        }
        Ok(())
    }

    pub(crate) fn replace(&self, value: Value) -> Value {
        let shared = match &*self.storage.borrow() {
            SlotStorage::Local(_) => None,
            SlotStorage::Shared(shared) => Some(shared.clone()),
        };
        match shared {
            None => {
                let SlotStorage::Local(previous) = self.storage.replace(SlotStorage::Local(value))
                else {
                    unreachable!()
                };
                previous
            }
            Some(shared) => {
                let replacement = value
                    .into_wire()
                    .expect("move replacements are wire-compatible");
                let (_lease, previous) = shared.write_snapshot().expect("shared value read failed");
                shared
                    .commit(replacement)
                    .expect("shared value write failed");
                Value::from_wire(previous).expect("shared slots contain wire-compatible values")
            }
        }
    }

    pub(crate) fn reshape(
        &self,
        update: impl FnOnce(&mut Value) -> Result<(), FosterError>,
    ) -> Result<(), FosterError> {
        let place = match &*self.storage.borrow() {
            SlotStorage::Local(Value::Reference(place)) => Some(place.clone()),
            SlotStorage::Local(_) | SlotStorage::Shared(_) => None,
        };
        if let Some(place) = place {
            return place.reshape(update);
        }
        let shared = match &*self.storage.borrow() {
            SlotStorage::Local(_) => None,
            SlotStorage::Shared(shared) => Some(shared.clone()),
        };
        if let Some(shared) = shared {
            let (_lease, wire) = shared.write_snapshot()?;
            let mut value = Value::from_wire(wire)?;
            update(&mut value)?;
            shared.commit(value.into_wire()?)?;
        } else {
            let mut storage = self.storage.borrow_mut();
            let SlotStorage::Local(value) = &mut *storage else {
                unreachable!()
            };
            update(value)?;
        }
        self.generation.set(self.generation.get() + 1);
        Ok(())
    }

    pub(crate) fn shared(&self) -> Option<Arc<SharedValue>> {
        match &*self.storage.borrow() {
            SlotStorage::Shared(shared) => Some(shared.clone()),
            SlotStorage::Local(_) => None,
        }
    }

    /// Creates a non-owning handle to the place represented by this slot.
    /// Reference wrapper slots are flattened to their original place.
    pub(crate) fn place(slot: &Rc<Self>) -> PlaceHandle {
        match &*slot.storage.borrow() {
            SlotStorage::Local(Value::Reference(place)) => place.clone(),
            SlotStorage::Local(_) | SlotStorage::Shared(_) => PlaceHandle::whole(slot),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PlaceProjection {
    Whole,
    Index { index: usize, generation: u64 },
}

#[derive(Debug, Clone)]
pub struct PlaceHandle {
    origin: Weak<Slot>,
    projection: PlaceProjection,
}

impl PartialEq for PlaceHandle {
    fn eq(&self, other: &Self) -> bool {
        Weak::ptr_eq(&self.origin, &other.origin) && self.projection == other.projection
    }
}

impl PlaceHandle {
    fn whole(origin: &Rc<Slot>) -> Self {
        Self {
            origin: Rc::downgrade(origin),
            projection: PlaceProjection::Whole,
        }
    }

    pub(crate) fn indexed(origin: Rc<Slot>, index: usize) -> Result<Self, FosterError> {
        let value = origin.read()?;
        let exists = match value {
            Value::List(values) => index < values.len(),
            Value::ByteBuffer(values) => index < values.len(),
            value => value
                .bytes_value()
                .is_some_and(|values| index < values.len()),
        };
        if !exists {
            return Err(FosterError::runtime("reference index is out of bounds"));
        }
        let generation = origin.generation.get();
        Ok(Self {
            origin: Rc::downgrade(&origin),
            projection: PlaceProjection::Index { index, generation },
        })
    }

    fn origin(&self) -> Result<Rc<Slot>, FosterError> {
        self.origin
            .upgrade()
            .ok_or_else(|| FosterError::runtime("borrowed place has expired"))
    }

    pub(crate) fn read(&self) -> Result<Value, FosterError> {
        let origin = self.origin()?;
        match self.projection {
            PlaceProjection::Whole => origin.read(),
            PlaceProjection::Index { index, generation } => {
                ensure_generation(&origin, generation)?;
                let value = origin.read()?;
                match value {
                    Value::List(values) => values.get(index).cloned(),
                    Value::ByteBuffer(values) => values.get(index).copied().map(Value::Byte),
                    value => value
                        .bytes_value()
                        .and_then(|values| values.get(index).copied())
                        .map(Value::Byte),
                }
                .ok_or_else(|| FosterError::runtime("referenced indexed value no longer exists"))
            }
        }
    }

    pub(crate) fn write(&self, value: Value) -> Result<(), FosterError> {
        let origin = self.origin()?;
        match self.projection {
            PlaceProjection::Whole => origin.write(value),
            PlaceProjection::Index { index, generation } => {
                ensure_generation(&origin, generation)?;
                let mut current = origin.read()?;
                match &mut current {
                    Value::List(values) => {
                        *values.get_mut(index).ok_or_else(|| {
                            FosterError::runtime("referenced list element no longer exists")
                        })? = value;
                    }
                    Value::ByteBuffer(values) => {
                        let Value::Byte(value) = value else {
                            return Err(FosterError::runtime(
                                "byte-buffer elements require Byte values",
                            ));
                        };
                        *values.get_mut(index).ok_or_else(|| {
                            FosterError::runtime("referenced byte no longer exists")
                        })? = value;
                    }
                    _ => {
                        return Err(FosterError::runtime(
                            "reference origin is not mutable indexed storage",
                        ));
                    }
                }
                origin.write(current)
            }
        }
    }

    fn reshape(
        &self,
        update: impl FnOnce(&mut Value) -> Result<(), FosterError>,
    ) -> Result<(), FosterError> {
        let origin = self.origin()?;
        match self.projection {
            PlaceProjection::Whole => origin.reshape(update),
            PlaceProjection::Index { index, generation } => {
                ensure_generation(&origin, generation)?;
                let mut current = origin.read()?;
                match &mut current {
                    Value::List(values) => {
                        let value = values.get_mut(index).ok_or_else(|| {
                            FosterError::runtime("referenced list element no longer exists")
                        })?;
                        update(value)?;
                    }
                    Value::ByteBuffer(values) => {
                        let byte = values.get_mut(index).ok_or_else(|| {
                            FosterError::runtime("referenced byte no longer exists")
                        })?;
                        let mut value = Value::Byte(*byte);
                        update(&mut value)?;
                        let Value::Byte(updated) = value else {
                            return Err(FosterError::runtime(
                                "byte-buffer elements require Byte values",
                            ));
                        };
                        *byte = updated;
                    }
                    _ => {
                        return Err(FosterError::runtime(
                            "reference origin is not mutable indexed storage",
                        ));
                    }
                }
                origin.write(current)
            }
        }
    }
}

fn ensure_generation(origin: &Slot, generation: u64) -> Result<(), FosterError> {
    if origin.generation.get() == generation {
        Ok(())
    } else {
        Err(FosterError::runtime(
            "reference was invalidated by structural mutation",
        ))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum WireValue {
    Unit,
    Bool(bool),
    Integer(i64),
    Float(f64),
    CodePoint(char),
    Byte(u8),
    RawBytes(Arc<Vec<u8>>),
    ByteBuffer(Vec<u8>),
    List(Vec<WireValue>),
    Record {
        record: Option<crate::hir::RecordId>,
        name: String,
        fields: BTreeMap<String, WireValue>,
    },
    Variant {
        variant: Option<crate::hir::VariantTypeId>,
        type_name: String,
        alternative: String,
        payload: Vec<WireValue>,
    },
    Remote(RemoteValue),
}

pub(crate) type WireResult = Result<WireValue, String>;
pub(crate) type FutureReceiver = may::sync::mpsc::Receiver<WireResult>;

pub(crate) enum RemoteArgument {
    Owned(WireValue),
    Borrowed(Arc<SharedValue>),
}

pub(crate) struct RemoteMessage {
    pub(crate) function: crate::hir::FunctionId,
    pub(crate) arguments: Vec<RemoteArgument>,
    pub(crate) response: may::sync::mpsc::Sender<WireResult>,
}

#[derive(Clone)]
pub struct RemoteValue {
    pub(crate) id: u64,
    pub(crate) sender: may::sync::mpsc::Sender<RemoteMessage>,
}

impl fmt::Debug for RemoteValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Remote({})", self.id)
    }
}

impl PartialEq for RemoteValue {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

#[derive(Clone)]
pub struct FutureValue {
    pub(crate) id: u64,
    pub(crate) receiver: Arc<Mutex<Option<FutureReceiver>>>,
}

impl fmt::Debug for FutureValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Future({})", self.id)
    }
}

impl PartialEq for FutureValue {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

pub(crate) static NEXT_REMOTE_ID: AtomicU64 = AtomicU64::new(1);
pub(crate) static NEXT_FUTURE_ID: AtomicU64 = AtomicU64::new(1);

impl Value {
    /// Returns the UTF-8 text when this value is a Foster `String` record.
    pub fn as_string(&self) -> Option<&str> {
        std::str::from_utf8(self.string_bytes()?).ok()
    }

    /// Returns the identifier text when this value is a Foster `Symbol` type.
    pub fn as_symbol(&self) -> Option<&str> {
        std::str::from_utf8(self.symbol_bytes()?).ok()
    }

    /// Returns the contents when this value is a Foster `Bytes` type.
    pub fn as_bytes(&self) -> Option<&[u8]> {
        self.bytes_value()
    }

    pub(crate) fn string(record: Option<crate::hir::RecordId>, value: impl Into<Vec<u8>>) -> Self {
        Self::Record {
            record,
            name: "String".into(),
            fields: BTreeMap::from([("value".into(), Self::bytes(value))]),
        }
    }

    pub(crate) fn bytes(value: impl Into<Vec<u8>>) -> Self {
        Self::Record {
            record: None,
            name: "Bytes".into(),
            fields: BTreeMap::from([("value".into(), Self::RawBytes(Arc::new(value.into())))]),
        }
    }

    pub(crate) fn bytes_value(&self) -> Option<&[u8]> {
        let Self::Record { name, fields, .. } = self else {
            return None;
        };
        if name != "Bytes" {
            return None;
        }
        match fields.get("value") {
            Some(Self::RawBytes(value)) => Some(value.as_slice()),
            _ => None,
        }
    }

    pub(crate) fn string_bytes(&self) -> Option<&[u8]> {
        let Self::Record { name, fields, .. } = self else {
            return None;
        };
        if name != "String" {
            return None;
        }
        fields.get("value")?.bytes_value()
    }

    pub(crate) fn symbol(
        symbol_record: Option<crate::hir::RecordId>,
        string_record: Option<crate::hir::RecordId>,
        value: impl Into<Vec<u8>>,
    ) -> Self {
        Self::Record {
            record: symbol_record,
            name: "Symbol".into(),
            fields: BTreeMap::from([("value".into(), Self::string(string_record, value))]),
        }
    }

    pub(crate) fn symbol_bytes(&self) -> Option<&[u8]> {
        let Self::Record { name, fields, .. } = self else {
            return None;
        };
        (name == "Symbol")
            .then(|| fields.get("value")?.string_bytes())
            .flatten()
    }

    pub(crate) fn string_text(&self) -> Result<&str, FosterError> {
        self.as_string()
            .ok_or_else(|| FosterError::runtime("expected a valid Foster String value"))
    }

    pub(crate) fn into_wire(self) -> Result<WireValue, FosterError> {
        Ok(match self {
            Self::Unit => WireValue::Unit,
            Self::Bool(value) => WireValue::Bool(value),
            Self::Integer(value) => WireValue::Integer(value),
            Self::Float(value) => WireValue::Float(value),
            Self::CodePoint(value) => WireValue::CodePoint(value),
            Self::Byte(value) => WireValue::Byte(value),
            Self::RawBytes(value) => WireValue::RawBytes(value),
            Self::ByteBuffer(value) => WireValue::ByteBuffer(value),
            Self::List(values) => WireValue::List(
                values
                    .into_iter()
                    .map(Self::into_wire)
                    .collect::<Result<_, _>>()?,
            ),
            Self::Record {
                record,
                name,
                fields,
            } => WireValue::Record {
                record,
                name,
                fields: fields
                    .into_iter()
                    .map(|(name, value)| Ok((name, value.into_wire()?)))
                    .collect::<Result<_, FosterError>>()?,
            },
            Self::Variant {
                variant,
                type_name,
                alternative,
                payload,
            } => WireValue::Variant {
                variant,
                type_name,
                alternative,
                payload: payload
                    .into_iter()
                    .map(Self::into_wire)
                    .collect::<Result<_, _>>()?,
            },
            Self::Remote(remote) => WireValue::Remote(remote),
            _ => {
                return Err(FosterError::runtime(
                    "value cannot cross a remote-object boundary",
                ));
            }
        })
    }

    pub(crate) fn from_wire(value: WireValue) -> Result<Self, FosterError> {
        Ok(match value {
            WireValue::Unit => Self::Unit,
            WireValue::Bool(value) => Self::Bool(value),
            WireValue::Integer(value) => Self::Integer(value),
            WireValue::Float(value) => Self::Float(value),
            WireValue::CodePoint(value) => Self::CodePoint(value),
            WireValue::Byte(value) => Self::Byte(value),
            WireValue::RawBytes(value) => Self::RawBytes(value),
            WireValue::ByteBuffer(value) => Self::ByteBuffer(value),
            WireValue::List(values) => Self::List(
                values
                    .into_iter()
                    .map(Self::from_wire)
                    .collect::<Result<_, _>>()?,
            ),
            WireValue::Record {
                record,
                name,
                fields,
            } => Self::Record {
                record,
                name,
                fields: fields
                    .into_iter()
                    .map(|(name, value)| Ok((name, Self::from_wire(value)?)))
                    .collect::<Result<_, FosterError>>()?,
            },
            WireValue::Variant {
                variant,
                type_name,
                alternative,
                payload,
            } => Self::Variant {
                variant,
                type_name,
                alternative,
                payload: payload
                    .into_iter()
                    .map(Self::from_wire)
                    .collect::<Result<_, _>>()?,
            },
            WireValue::Remote(remote) => Self::Remote(remote),
        })
    }
}

impl fmt::Display for Value {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unit => write!(formatter, "()"),
            Self::Bool(value) => write!(formatter, "{value}"),
            Self::Integer(value) => write!(formatter, "{value}"),
            Self::Float(value) => write!(formatter, "{value}"),
            Self::CodePoint(value) => write!(formatter, "{value}"),
            Self::Byte(value) => write!(formatter, "{value}"),
            Self::RawBytes(value) => {
                write!(formatter, "0x")?;
                for byte in value.iter() {
                    write!(formatter, "{byte:02x}")?;
                }
                Ok(())
            }
            Self::ByteBuffer(value) => write!(formatter, "ByteBuffer(len={})", value.len()),
            value if value.symbol_bytes().is_some() => write!(
                formatter,
                ":{}",
                String::from_utf8_lossy(value.symbol_bytes().unwrap())
            ),
            Self::List(values) => {
                write!(formatter, "[")?;
                for (index, value) in values.iter().enumerate() {
                    if index > 0 {
                        write!(formatter, ", ")?;
                    }
                    if let Ok(value) = value.string_text() {
                        write!(formatter, "{value:?}")?;
                    } else {
                        match value {
                            Self::CodePoint(value) => write!(formatter, "'{value}'")?,
                            value => write!(formatter, "{value}")?,
                        }
                    }
                }
                write!(formatter, "]")
            }
            Self::VmClosure { .. } => write!(formatter, "<closure>"),
            Self::Reference(reference) => {
                write!(formatter, "{}", reference.read().map_err(|_| fmt::Error)?)
            }
            Self::Remote(remote) => write!(formatter, "<remote {}>", remote.id),
            Self::Future(future) => write!(formatter, "<future {}>", future.id),
            Self::Record { name, fields, .. } => {
                if let Ok(value) = self.string_text() {
                    return write!(formatter, "{value}");
                }
                write!(formatter, "{name} {{")?;
                for (index, (field, value)) in fields.iter().enumerate() {
                    if index > 0 {
                        write!(formatter, ", ")?;
                    }
                    write!(formatter, "{field}: {value}")?;
                }
                write!(formatter, "}}")
            }
            Self::Variant {
                type_name,
                alternative,
                payload,
                ..
            } => {
                write!(formatter, "{type_name}.{alternative}")?;
                if payload.is_empty() {
                    return Ok(());
                }
                write!(formatter, "(")?;
                for (index, value) in payload.iter().enumerate() {
                    if index > 0 {
                        write!(formatter, ", ")?;
                    }
                    write!(formatter, "{value}")?;
                }
                write!(formatter, ")")
            }
        }
    }
}

pub(crate) fn next_remote_id() -> u64 {
    NEXT_REMOTE_ID.fetch_add(1, Ordering::Relaxed)
}

pub(crate) fn next_future_id() -> u64 {
    NEXT_FUTURE_ID.fetch_add(1, Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::Capture;
    use la_arena::{Idx, RawIdx};

    #[test]
    fn expired_place_handles_fail_safely() {
        let origin = Slot::new(Value::Integer(42));
        let place = Slot::place(&origin);
        drop(origin);

        let error = place.read().unwrap_err();
        assert!(error.message.contains("borrowed place has expired"));
    }

    #[test]
    fn reference_captures_do_not_retain_their_origin_cycle() {
        let origin = Slot::new(Value::Unit);
        let weak_origin = Rc::downgrade(&origin);
        let place = Slot::place(&origin);
        let function = Idx::from_raw(RawIdx::from_u32(0));
        origin
            .write(Value::VmClosure {
                function,
                captures: vec![Capture::Place(place)],
            })
            .unwrap();

        drop(origin);
        assert!(weak_origin.upgrade().is_none());
    }

    #[test]
    fn capturing_a_reference_flattens_its_wrapper_slot() {
        let origin = Slot::new(Value::List(vec![Value::Integer(42)]));
        let projected = PlaceHandle::indexed(origin.clone(), 0).unwrap();
        let wrapper = Slot::new(Value::Reference(projected.clone()));

        let flattened = Slot::place(&wrapper);
        drop(wrapper);

        assert_eq!(flattened.read().unwrap(), Value::Integer(42));
        assert_eq!(flattened, projected);
    }
}
