use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::fmt;
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

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
    RawByteBuffer(Vec<u8>),
    RawList(Vec<Value>),
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
        fields: RecordFields,
    },
    Variant {
        variant: Option<crate::hir::VariantTypeId>,
        type_name: Arc<str>,
        alternative: Arc<str>,
        payload: Vec<Value>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct RecordFields {
    layout: Arc<RecordLayout>,
    values: Vec<Value>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct RecordLayout {
    names: Vec<String>,
    indices: BTreeMap<String, usize>,
}

impl RecordLayout {
    pub(crate) fn new(names: Vec<String>) -> Self {
        let indices = names
            .iter()
            .enumerate()
            .map(|(index, name)| (name.clone(), index))
            .collect();
        Self { names, indices }
    }

    pub(crate) fn names(&self) -> &[String] {
        &self.names
    }
}

impl RecordFields {
    pub(crate) fn new(layout: Arc<RecordLayout>, values: Vec<Value>) -> Result<Self, FosterError> {
        if layout.names.len() != values.len() {
            return Err(FosterError::runtime(
                "record layout does not match its values",
            ));
        }
        Ok(Self { layout, values })
    }

    pub(crate) fn from_pairs(fields: impl IntoIterator<Item = (String, Value)>) -> Self {
        let (names, values) = fields.into_iter().unzip();
        Self {
            layout: Arc::new(RecordLayout::new(names)),
            values,
        }
    }

    pub fn get(&self, name: &str) -> Option<&Value> {
        self.index(name).and_then(|index| self.values.get(index))
    }

    pub(crate) fn get_mut(&mut self, name: &str) -> Option<&mut Value> {
        let index = self.index(name)?;
        self.values.get_mut(index)
    }

    pub(crate) fn contains_key(&self, name: &str) -> bool {
        self.index(name).is_some()
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&String, &Value)> {
        self.layout.names.iter().zip(&self.values)
    }

    pub(crate) fn into_pairs(self) -> impl Iterator<Item = (String, Value)> {
        self.layout.names.clone().into_iter().zip(self.values)
    }

    #[cfg(test)]
    pub(crate) fn shares_layout_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.layout, &other.layout)
    }

    fn index(&self, name: &str) -> Option<usize> {
        self.layout.indices.get(name).copied()
    }
}

impl std::ops::Index<&str> for RecordFields {
    type Output = Value;

    fn index(&self, name: &str) -> &Self::Output {
        self.get(name)
            .unwrap_or_else(|| panic!("record has no field `{name}`"))
    }
}

impl<'a> IntoIterator for &'a RecordFields {
    type Item = (&'a String, &'a Value);
    type IntoIter = std::iter::Zip<std::slice::Iter<'a, String>, std::slice::Iter<'a, Value>>;

    fn into_iter(self) -> Self::IntoIter {
        self.layout.names.iter().zip(&self.values)
    }
}

fn value_layout() -> Arc<RecordLayout> {
    static VALUE_LAYOUT: OnceLock<Arc<RecordLayout>> = OnceLock::new();
    VALUE_LAYOUT
        .get_or_init(|| Arc::new(RecordLayout::new(vec!["value".to_owned()])))
        .clone()
}

fn value_fields(value: Value) -> RecordFields {
    RecordFields {
        layout: value_layout(),
        values: vec![value],
    }
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

    pub(crate) fn reference(&self) -> Option<PlaceHandle> {
        match &*self.storage.borrow() {
            SlotStorage::Local(Value::Reference(reference)) => Some(reference.clone()),
            SlotStorage::Local(_) | SlotStorage::Shared(_) => None,
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
    Index { index: usize, generation: u64 },
    Field { name: String },
}

#[derive(Debug, Clone)]
pub struct PlaceHandle {
    origin: Weak<Slot>,
    projections: Vec<PlaceProjection>,
}

impl PartialEq for PlaceHandle {
    fn eq(&self, other: &Self) -> bool {
        Weak::ptr_eq(&self.origin, &other.origin) && self.projections == other.projections
    }
}

impl PlaceHandle {
    fn whole(origin: &Rc<Slot>) -> Self {
        Self {
            origin: Rc::downgrade(origin),
            projections: Vec::new(),
        }
    }

    pub(crate) fn indexed(origin: Rc<Slot>, index: usize) -> Result<Self, FosterError> {
        let mut place = Slot::place(&origin);
        let value = place.read()?;
        let exists = match value {
            Value::RawByteBuffer(values) => index < values.len(),
            value => {
                value
                    .list_value()
                    .is_some_and(|values| index < values.len())
                    || value
                        .byte_buffer_value()
                        .is_some_and(|values| index < values.len())
                    || value
                        .bytes_value()
                        .is_some_and(|values| index < values.len())
            }
        };
        if !exists {
            return Err(FosterError::runtime("reference index is out of bounds"));
        }
        let root = place.origin()?;
        let generation = root.generation.get();
        place
            .projections
            .push(PlaceProjection::Index { index, generation });
        Ok(place)
    }

    pub(crate) fn field(origin: Rc<Slot>, name: String) -> Result<Self, FosterError> {
        let mut place = Slot::place(&origin);
        let value = place.read()?;
        let Value::Record { fields, .. } = value else {
            return Err(FosterError::runtime("field projection requires a record"));
        };
        if !fields.contains_key(&name) {
            return Err(FosterError::runtime(format!(
                "record has no field `{name}`"
            )));
        }
        place.projections.push(PlaceProjection::Field { name });
        Ok(place)
    }

    fn origin(&self) -> Result<Rc<Slot>, FosterError> {
        self.origin
            .upgrade()
            .ok_or_else(|| FosterError::runtime("borrowed place has expired"))
    }

    pub(crate) fn read(&self) -> Result<Value, FosterError> {
        let origin = self.origin()?;
        let mut value = origin.read()?;
        for projection in &self.projections {
            value = project_value(value, projection, &origin)?;
        }
        Ok(value)
    }

    pub(crate) fn write(&self, value: Value) -> Result<(), FosterError> {
        let origin = self.origin()?;
        if self.projections.is_empty() {
            return origin.write(value);
        }
        let mut current = origin.read()?;
        replace_projected(&mut current, &self.projections, &origin, value)?;
        origin.write(current)
    }

    pub(crate) fn reshape(
        &self,
        update: impl FnOnce(&mut Value) -> Result<(), FosterError>,
    ) -> Result<(), FosterError> {
        let origin = self.origin()?;
        if self.projections.is_empty() {
            return origin.reshape(update);
        }
        let mut current = origin.read()?;
        update_projected(&mut current, &self.projections, &origin, update)?;
        origin.write(current)?;
        origin.generation.set(origin.generation.get() + 1);
        Ok(())
    }
}

fn project_value(
    value: Value,
    projection: &PlaceProjection,
    origin: &Slot,
) -> Result<Value, FosterError> {
    match projection {
        PlaceProjection::Field { name } => {
            let Value::Record { fields, .. } = value else {
                return Err(FosterError::runtime("field projection requires a record"));
            };
            fields
                .get(name)
                .cloned()
                .ok_or_else(|| FosterError::runtime(format!("record has no field `{name}`")))
        }
        PlaceProjection::Index { index, generation } => {
            ensure_generation(origin, *generation)?;
            indexed_value(value, *index)
        }
    }
}

fn indexed_value(value: Value, index: usize) -> Result<Value, FosterError> {
    match value {
        Value::RawByteBuffer(values) => values.get(index).copied().map(Value::Byte),
        value if value.list_value().is_some() => value.list_value().unwrap().get(index).cloned(),
        value if value.byte_buffer_value().is_some() => value
            .byte_buffer_value()
            .unwrap()
            .get(index)
            .copied()
            .map(Value::Byte),
        value => value
            .bytes_value()
            .and_then(|values| values.get(index).copied())
            .map(Value::Byte),
    }
    .ok_or_else(|| FosterError::runtime("referenced indexed value no longer exists"))
}

fn replace_projected(
    root: &mut Value,
    projections: &[PlaceProjection],
    origin: &Slot,
    value: Value,
) -> Result<(), FosterError> {
    update_projected(root, projections, origin, |target| {
        *target = value;
        Ok(())
    })
}

fn update_projected(
    current: &mut Value,
    projections: &[PlaceProjection],
    origin: &Slot,
    update: impl FnOnce(&mut Value) -> Result<(), FosterError>,
) -> Result<(), FosterError> {
    let Some((projection, remaining)) = projections.split_first() else {
        return update(current);
    };
    match projection {
        PlaceProjection::Field { name } => {
            let Value::Record { fields, .. } = current else {
                return Err(FosterError::runtime("field projection requires a record"));
            };
            let field = fields
                .get_mut(name)
                .ok_or_else(|| FosterError::runtime(format!("record has no field `{name}`")))?;
            update_projected(field, remaining, origin, update)
        }
        PlaceProjection::Index { index, generation } => {
            ensure_generation(origin, *generation)?;
            if let Some(values) = current.list_value_mut() {
                let item = values.get_mut(*index).ok_or_else(|| {
                    FosterError::runtime("referenced list element no longer exists")
                })?;
                return update_projected(item, remaining, origin, update);
            }
            if let Some(values) = current.byte_buffer_value_mut() {
                if !remaining.is_empty() {
                    return Err(FosterError::runtime("byte projection cannot be nested"));
                }
                let byte = values
                    .get_mut(*index)
                    .ok_or_else(|| FosterError::runtime("referenced byte no longer exists"))?;
                let mut value = Value::Byte(*byte);
                update(&mut value)?;
                let Value::Byte(updated) = value else {
                    return Err(FosterError::runtime(
                        "byte-buffer elements require Byte values",
                    ));
                };
                *byte = updated;
                return Ok(());
            }
            Err(FosterError::runtime(
                "reference origin is not mutable indexed storage",
            ))
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
    RawByteBuffer(Vec<u8>),
    RawList(Vec<WireValue>),
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

    /// Returns the elements when this value is a Foster `List` type.
    pub fn as_list(&self) -> Option<&[Value]> {
        self.list_value().map(Vec::as_slice)
    }

    /// Returns the buffered bytes when this value is a Foster `ByteBuffer` type.
    pub fn as_byte_buffer(&self) -> Option<&[u8]> {
        self.byte_buffer_value().map(Vec::as_slice)
    }

    pub(crate) fn string(record: Option<crate::hir::RecordId>, value: impl Into<Vec<u8>>) -> Self {
        Self::Record {
            record,
            name: "String".into(),
            fields: value_fields(Self::bytes(value)),
        }
    }

    pub(crate) fn bytes(value: impl Into<Vec<u8>>) -> Self {
        Self::Record {
            record: None,
            name: "Bytes".into(),
            fields: value_fields(Self::RawBytes(Arc::new(value.into()))),
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

    pub(crate) fn byte_buffer_value(&self) -> Option<&Vec<u8>> {
        let Self::Record { name, fields, .. } = self else {
            return None;
        };
        if name != "ByteBuffer" {
            return None;
        }
        match fields.get("value") {
            Some(Self::RawByteBuffer(value)) => Some(value),
            _ => None,
        }
    }

    pub(crate) fn byte_buffer_value_mut(&mut self) -> Option<&mut Vec<u8>> {
        let Self::Record { name, fields, .. } = self else {
            return None;
        };
        if name != "ByteBuffer" {
            return None;
        }
        match fields.get_mut("value") {
            Some(Self::RawByteBuffer(value)) => Some(value),
            _ => None,
        }
    }

    pub(crate) fn list(values: Vec<Value>) -> Self {
        Self::Record {
            record: None,
            name: "List".into(),
            fields: value_fields(Self::RawList(values)),
        }
    }

    pub(crate) fn list_value(&self) -> Option<&Vec<Value>> {
        let Self::Record { name, fields, .. } = self else {
            return None;
        };
        if name != "List" {
            return None;
        }
        match fields.get("value") {
            Some(Self::RawList(values)) => Some(values),
            _ => None,
        }
    }

    pub(crate) fn list_value_mut(&mut self) -> Option<&mut Vec<Value>> {
        let Self::Record { name, fields, .. } = self else {
            return None;
        };
        if name != "List" {
            return None;
        }
        match fields.get_mut("value") {
            Some(Self::RawList(values)) => Some(values),
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
            fields: value_fields(Self::string(string_record, value)),
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
            Self::RawByteBuffer(value) => WireValue::RawByteBuffer(value),
            Self::RawList(values) => WireValue::RawList(
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
                    .into_pairs()
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
                type_name: type_name.to_string(),
                alternative: alternative.to_string(),
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
            WireValue::RawByteBuffer(value) => Self::RawByteBuffer(value),
            WireValue::RawList(values) => Self::RawList(
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
                fields: RecordFields::from_pairs(
                    fields
                        .into_iter()
                        .map(|(name, value)| Ok((name, Self::from_wire(value)?)))
                        .collect::<Result<Vec<_>, FosterError>>()?,
                ),
            },
            WireValue::Variant {
                variant,
                type_name,
                alternative,
                payload,
            } => Self::Variant {
                variant,
                type_name: Arc::from(type_name),
                alternative: Arc::from(alternative),
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
            value if value.byte_buffer_value().is_some() => {
                write!(
                    formatter,
                    "ByteBuffer(len={})",
                    value.byte_buffer_value().unwrap().len()
                )
            }
            Self::RawByteBuffer(value) => write!(formatter, "RawByteBuffer(len={})", value.len()),
            value if value.symbol_bytes().is_some() => write!(
                formatter,
                ":{}",
                String::from_utf8_lossy(value.symbol_bytes().unwrap())
            ),
            Self::RawList(values) => {
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
                if let Some(values) = self.list_value() {
                    write!(formatter, "[")?;
                    for (index, value) in values.iter().enumerate() {
                        if index > 0 {
                            write!(formatter, ", ")?;
                        }
                        write!(formatter, "{value}")?;
                    }
                    return write!(formatter, "]");
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
        let origin = Slot::new(Value::list(vec![Value::Integer(42)]));
        let projected = PlaceHandle::indexed(origin.clone(), 0).unwrap();
        let wrapper = Slot::new(Value::Reference(projected.clone()));

        let flattened = Slot::place(&wrapper);
        drop(wrapper);

        assert_eq!(flattened.read().unwrap(), Value::Integer(42));
        assert_eq!(flattened, projected);
    }

    #[test]
    fn nested_projections_share_one_weak_root_without_retaining_wrappers() {
        let origin = Slot::new(Value::Record {
            record: None,
            name: "Outer".into(),
            fields: RecordFields::from_pairs([(
                "inner".into(),
                Value::Record {
                    record: None,
                    name: "Inner".into(),
                    fields: RecordFields::from_pairs([("value".into(), Value::Integer(1))]),
                },
            )]),
        });
        let inner = PlaceHandle::field(origin.clone(), "inner".into()).unwrap();
        let wrapper = Slot::new(Value::Reference(inner));
        let value = PlaceHandle::field(wrapper.clone(), "value".into()).unwrap();

        drop(wrapper);
        value.write(Value::Integer(42)).unwrap();
        assert_eq!(value.read().unwrap(), Value::Integer(42));

        drop(origin);
        assert!(value.read().unwrap_err().message.contains("expired"));
    }

    #[test]
    fn record_values_share_their_indexed_layout() {
        let layout = Arc::new(RecordLayout::new(vec!["left".into(), "right".into()]));
        let first =
            RecordFields::new(layout.clone(), vec![Value::Integer(1), Value::Integer(2)]).unwrap();
        let second = RecordFields::new(layout, vec![Value::Integer(3), Value::Integer(4)]).unwrap();
        assert!(first.shares_layout_with(&second));
        assert_eq!(first.get("right"), Some(&Value::Integer(2)));
    }

    #[test]
    fn nested_reshape_invalidates_an_older_index_projection() {
        let origin = Slot::new(Value::Record {
            record: None,
            name: "Outer".into(),
            fields: RecordFields::from_pairs([(
                "items".into(),
                Value::list(vec![Value::Integer(1)]),
            )]),
        });
        let items = PlaceHandle::field(origin.clone(), "items".into()).unwrap();
        let wrapper = Slot::new(Value::Reference(items.clone()));
        let first = PlaceHandle::indexed(wrapper, 0).unwrap();

        items
            .reshape(|value| {
                value.list_value_mut().unwrap().push(Value::Integer(2));
                Ok(())
            })
            .unwrap();

        assert!(first.read().unwrap_err().message.contains("invalidated"));
    }
}
