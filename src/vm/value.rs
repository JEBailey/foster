use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::fmt;
use std::rc::Rc;
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
    String(String),
    Symbol(String),
    List(Vec<Value>),
    VmClosure {
        function: crate::hir::FunctionId,
        captures: Vec<Capture>,
    },
    Reference(ReferenceValue),
    Remote(RemoteValue),
    Future(FutureValue),
    Record {
        name: String,
        fields: BTreeMap<String, Value>,
    },
    Variant {
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
}

#[derive(Debug, Clone)]
pub struct ReferenceValue {
    origin: Rc<Slot>,
    index: usize,
    generation: u64,
}

impl PartialEq for ReferenceValue {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.origin, &other.origin)
            && self.index == other.index
            && self.generation == other.generation
    }
}

impl ReferenceValue {
    pub(crate) fn indexed(origin: Rc<Slot>, index: usize) -> Result<Self, FosterError> {
        let exists = matches!(origin.read()?, Value::List(values) if index < values.len());
        if !exists {
            return Err(FosterError::runtime("reference index is out of bounds"));
        }
        let generation = origin.generation.get();
        Ok(Self {
            origin,
            index,
            generation,
        })
    }

    fn ensure_valid(&self) -> Result<(), FosterError> {
        if self.origin.generation.get() == self.generation {
            Ok(())
        } else {
            Err(FosterError::runtime(
                "reference was invalidated by structural mutation",
            ))
        }
    }

    pub(crate) fn read(&self) -> Result<Value, FosterError> {
        self.ensure_valid()?;
        let origin = self.origin.read()?;
        let Value::List(values) = origin else {
            return Err(FosterError::runtime("reference origin is no longer a List"));
        };
        values
            .get(self.index)
            .cloned()
            .ok_or_else(|| FosterError::runtime("referenced list element no longer exists"))
    }

    pub(crate) fn write(&self, value: Value) -> Result<(), FosterError> {
        self.ensure_valid()?;
        let mut origin = self.origin.read()?;
        let Value::List(values) = &mut origin else {
            return Err(FosterError::runtime("reference origin is no longer a List"));
        };
        *values
            .get_mut(self.index)
            .ok_or_else(|| FosterError::runtime("referenced list element no longer exists"))? =
            value;
        self.origin.write(origin)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum WireValue {
    Unit,
    Bool(bool),
    Integer(i64),
    Float(f64),
    String(String),
    Symbol(String),
    List(Vec<WireValue>),
    Record {
        name: String,
        fields: BTreeMap<String, WireValue>,
    },
    Variant {
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
    pub(crate) fn into_wire(self) -> Result<WireValue, FosterError> {
        Ok(match self {
            Self::Unit => WireValue::Unit,
            Self::Bool(value) => WireValue::Bool(value),
            Self::Integer(value) => WireValue::Integer(value),
            Self::Float(value) => WireValue::Float(value),
            Self::String(value) => WireValue::String(value),
            Self::Symbol(value) => WireValue::Symbol(value),
            Self::List(values) => WireValue::List(
                values
                    .into_iter()
                    .map(Self::into_wire)
                    .collect::<Result<_, _>>()?,
            ),
            Self::Record { name, fields } => WireValue::Record {
                name,
                fields: fields
                    .into_iter()
                    .map(|(name, value)| Ok((name, value.into_wire()?)))
                    .collect::<Result<_, FosterError>>()?,
            },
            Self::Variant {
                type_name,
                alternative,
                payload,
            } => WireValue::Variant {
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
            WireValue::String(value) => Self::String(value),
            WireValue::Symbol(value) => Self::Symbol(value),
            WireValue::List(values) => Self::List(
                values
                    .into_iter()
                    .map(Self::from_wire)
                    .collect::<Result<_, _>>()?,
            ),
            WireValue::Record { name, fields } => Self::Record {
                name,
                fields: fields
                    .into_iter()
                    .map(|(name, value)| Ok((name, Self::from_wire(value)?)))
                    .collect::<Result<_, FosterError>>()?,
            },
            WireValue::Variant {
                type_name,
                alternative,
                payload,
            } => Self::Variant {
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
            Self::String(value) => write!(formatter, "{value}"),
            Self::Symbol(value) => write!(formatter, ":{value}"),
            Self::List(values) => {
                write!(formatter, "[")?;
                for (index, value) in values.iter().enumerate() {
                    if index > 0 {
                        write!(formatter, ", ")?;
                    }
                    match value {
                        Self::String(value) => write!(formatter, "{value:?}")?,
                        value => write!(formatter, "{value}")?,
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
            Self::Record { name, fields } => {
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
