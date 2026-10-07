use std::rc::Rc;
use std::sync::{Arc, Mutex};

use crate::error::{FosterError, RuntimeError};
use crate::handlers::BuiltinHandler;
use crate::hir::{CaptureMode, FunctionId};

use super::operations::{binary, constant_value, unary};
use super::patterns::matches as match_pattern;
use super::value::{
    AccessLease, FutureValue, PlaceHandle, RecordFields, RemoteArgument, RemoteMessage,
    RemoteOwner, RemoteValue, SharedValue, Slot, WorkerCompletion, next_future_id, next_remote_id,
};
use super::{Capture, Instruction, Program, Register, Value};

struct Frame<'program> {
    function_id: FunctionId,
    /// Resolved once when the frame is created; the dispatch loop never hashes the function ID.
    function: &'program super::BytecodeFunction,
    registers: Vec<RegisterCell>,
    debug_live: Option<Vec<bool>>,
    instruction: usize,
    return_destination: Option<Register>,
    shared_commit: Option<SharedCommit>,
    argument_leases: Vec<AccessLease>,
}

impl Drop for Frame<'_> {
    fn drop(&mut self) {
        // Register numbers follow allocation order. Tear the frame down in the
        // opposite order so borrower wrappers and later temporaries release
        // before the storage they can reference.
        for register in self.registers.iter_mut().rev() {
            register.detach();
        }
        self.shared_commit.take();
        self.argument_leases.clear();
        let mut registers = std::mem::take(&mut self.registers);
        registers.clear();
        // Never retain values or places in the pool. Cleanup may reenter the VM,
        // so borrow the pool only after all user cleanup has finished.
        let _ = REGISTER_POOL.try_with(|pool| pool.borrow_mut().put(registers));
    }
}

struct FrameStack<'program>(Vec<Frame<'program>>);

impl<'program> std::ops::Deref for FrameStack<'program> {
    type Target = Vec<Frame<'program>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for FrameStack<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Drop for FrameStack<'_> {
    fn drop(&mut self) {
        // Runtime failures leave the active call stack intact. Pop explicitly
        // so invocation frames unwind from callee to caller.
        while self.0.pop().is_some() {}
    }
}

enum RegisterCell {
    Inline(Value),
    /// A place whose writes remain observable by its owner.
    Place(Rc<Slot>),
    /// A borrowed place. Reads observe the owner, while assignment detaches the local.
    Borrowed(Rc<Slot>),
}

#[derive(Default)]
struct RegisterPool {
    buffers: Vec<Vec<RegisterCell>>,
    capacity: usize,
}

impl RegisterPool {
    // Bound retained memory per host thread, including coroutine executions.
    const LIMIT: usize = 4096;

    fn take(&mut self, length: usize) -> Vec<RegisterCell> {
        let candidate = self
            .buffers
            .iter()
            .enumerate()
            .filter(|(_, buffer)| buffer.capacity() >= length)
            .min_by_key(|(_, buffer)| buffer.capacity())
            .map(|(index, _)| index);
        let mut buffer = if let Some(index) = candidate {
            let buffer = self.buffers.swap_remove(index);
            self.capacity -= buffer.capacity();
            buffer
        } else {
            Vec::with_capacity(length)
        };
        buffer.resize_with(length, || RegisterCell::Inline(Value::Unit));
        buffer
    }

    fn put(&mut self, buffer: Vec<RegisterCell>) {
        debug_assert!(buffer.is_empty());
        if buffer.capacity() > 0
            && self.buffers.len() < 32
            && buffer.capacity() <= Self::LIMIT - self.capacity
        {
            self.capacity += buffer.capacity();
            self.buffers.push(buffer);
        }
    }
}

#[derive(Default)]
struct ConstantCache {
    text: std::collections::HashMap<u16, Value>,
}

impl ConstantCache {
    fn load(&mut self, program: &Program, index: u16) -> Value {
        let constant = &program.metadata.constants[index as usize];
        let materialize = || {
            constant_value(
                constant,
                program.metadata.string_record,
                program.metadata.symbol_record,
            )
        };
        match constant {
            super::Constant::String(_) | super::Constant::Symbol(_) => {
                // Literal records have no user cleanup. Value's copy-on-write
                // fields preserve independent bindings while sharing bytes.
                self.text.entry(index).or_insert_with(materialize).clone()
            }
            _ => materialize(),
        }
    }
}

impl RegisterCell {
    fn read(&self) -> Result<Value, RuntimeError> {
        match self {
            Self::Inline(Value::Reference(place)) => place.read(),
            Self::Inline(value) => Ok(value.clone()),
            Self::Place(slot) | Self::Borrowed(slot) => slot.read(),
        }
    }

    /// Copies the value held by a register without reading through a reference handle.
    fn bind(&self) -> Value {
        match self {
            Self::Inline(value) => value.clone(),
            Self::Place(slot) | Self::Borrowed(slot) => slot.argument(),
        }
    }

    fn reference(&self) -> Option<PlaceHandle> {
        match self {
            Self::Inline(Value::Reference(reference)) => Some(reference.clone()),
            Self::Inline(_) => None,
            Self::Place(slot) | Self::Borrowed(slot) => slot.reference(),
        }
    }

    fn write(&mut self, value: Value) -> Result<(), RuntimeError> {
        match self {
            Self::Inline(Value::Reference(place)) => place.write(value)?,
            Self::Inline(current) => *current = value,
            Self::Place(slot) => slot.write(value)?,
            Self::Borrowed(_) => *self = Self::Inline(value),
        }
        Ok(())
    }

    fn take(&mut self) -> Value {
        match self {
            Self::Inline(value) => std::mem::replace(value, Value::Unit),
            Self::Place(slot) | Self::Borrowed(slot) => slot.replace(Value::Unit),
        }
    }

    fn replace(&mut self, value: Value) -> Value {
        match self {
            Self::Inline(current) => std::mem::replace(current, value),
            Self::Place(slot) | Self::Borrowed(slot) => slot.replace(value),
        }
    }

    fn reshape(
        &mut self,
        update: impl FnOnce(&mut Value) -> Result<(), RuntimeError>,
    ) -> Result<(), RuntimeError> {
        match self {
            Self::Inline(Value::Reference(place)) => place.reshape(update),
            Self::Inline(value) => update(value),
            Self::Place(slot) => slot.reshape(update),
            Self::Borrowed(slot) => {
                let mut value = slot.read()?;
                update(&mut value)?;
                *self = Self::Inline(value);
                Ok(())
            }
        }
    }

    fn share(&mut self) -> Result<Arc<SharedValue>, RuntimeError> {
        self.promote().share()
    }

    fn promote(&mut self) -> Rc<Slot> {
        match self {
            Self::Place(slot) | Self::Borrowed(slot) => slot.clone(),
            Self::Inline(value) => {
                let slot = Slot::new(std::mem::replace(value, Value::Unit));
                *self = Self::Place(slot.clone());
                slot
            }
        }
    }

    fn detach(&mut self) {
        match self {
            Self::Inline(value) => *value = Value::Unit,
            Self::Place(slot) | Self::Borrowed(slot) => {
                if Rc::strong_count(slot) == 1 && slot.shared().is_none() {
                    slot.replace(Value::Unit);
                }
                *self = Self::Inline(Value::Unit);
            }
        }
    }
}

struct SharedCommit {
    shared: Arc<SharedValue>,
    receiver: Rc<Slot>,
    _lease: AccessLease,
}

/// An execution context for an immutable verified executable.
/// ```compile_fail
/// let raw = foster_vm::Program::default();
/// let machine = foster_vm::Machine::new(&raw);
/// ```
pub struct Machine {
    interrupt: Option<fn() -> bool>,
    debugger: Option<Arc<dyn super::debug::Observer>>,
    cancellation: Option<Arc<crate::remote::Control>>,
    program: super::VerifiedProgram,
    host: Arc<super::host::HostContext>,
}

thread_local! {
    static CLEANUP_FAILURE: std::cell::RefCell<Option<RuntimeError>> = const { std::cell::RefCell::new(None) };
    static REGISTER_POOL: std::cell::RefCell<RegisterPool> = std::cell::RefCell::new(RegisterPool::default());
}

/// Releases a value returned to the embedding application and reports a failing deinit.
pub fn release_value(value: Value) -> Result<(), FosterError> {
    drop(value);
    CLEANUP_FAILURE
        .with(|failure| failure.borrow_mut().take())
        .map_or(Ok(()), |error| Err(error.into()))
}

#[derive(Clone)]
pub(super) struct Cleanup {
    program: super::VerifiedProgram,
    host: Arc<super::host::HostContext>,
    function: FunctionId,
    record: Option<crate::hir::RecordId>,
    variant: Option<(crate::hir::VariantTypeId, Arc<str>, Arc<str>)>,
    proxy: Option<Arc<super::value::WireOwned>>,
}

impl std::fmt::Debug for Cleanup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Cleanup").field(&self.function).finish()
    }
}

impl Cleanup {
    pub(super) fn is_c_resource(&self) -> bool {
        self.record
            .is_some_and(|record| self.program.metadata.is_c_resource(record))
    }
    pub(super) fn with_proxy(mut self, proxy: Arc<super::value::WireOwned>) -> Self {
        self.proxy = Some(proxy);
        self
    }

    pub(super) fn into_owned(mut self) -> Option<Self> {
        if let Some(proxy) = self.proxy.take() {
            proxy.cleanup.lock().unwrap().take()
        } else {
            Some(self)
        }
    }

    pub(super) fn run(mut self, values: Vec<Value>) {
        if let Some(proxy) = self.proxy.take() {
            if Arc::strong_count(&proxy) == 1 {
                let cleanup = proxy.cleanup.lock().unwrap().take();
                if let Some(cleanup) = cleanup {
                    cleanup.run(values);
                }
            }
            return;
        }
        let receiver = if let Some((variant, type_name, alternative)) = self.variant {
            Value::Variant {
                variant: Some(variant),
                type_name,
                alternative,
                payload: values.into(),
            }
        } else {
            let record = self.record.expect("record cleanup identity");
            let metadata = &self.program.metadata.records[&record];
            Value::Record {
                record: Some(record),
                name: metadata.name.clone(),
                fields: RecordFields::new(metadata.layout().clone(), values)
                    .expect("cleanup retains the record layout"),
            }
        };
        let machine = Machine {
            program: self.program,
            host: self.host,
            cancellation: None,
            interrupt: None,
            debugger: None,
        };
        let original = CLEANUP_FAILURE.with(|failure| failure.borrow_mut().take());
        let result = machine.execute(
            self.function,
            Vec::new(),
            Vec::new(),
            Some(Slot::new(receiver)),
        );
        let error = result.err();
        CLEANUP_FAILURE.with(|failure| {
            let nested = failure.borrow_mut().take();
            *failure.borrow_mut() = original.or(error).or(nested);
        });
    }
}

impl Machine {
    fn remote_variant(
        &self,
        parent: Option<crate::hir::VariantTypeId>,
        alternative: &str,
        value: Value,
    ) -> Result<Value, RuntimeError> {
        let metadata = self
            .program
            .metadata
            .variants
            .values()
            .find(|variant| {
                Some(variant.parent) == parent && variant.alternative.as_ref() == alternative
            })
            .ok_or_else(|| RuntimeError::runtime("missing remote outcome metadata"))?;
        Ok(Value::Variant {
            variant: Some(metadata.parent),
            type_name: metadata.type_name.clone(),
            alternative: metadata.alternative.clone(),
            payload: if alternative == "Shutdown" {
                vec![]
            } else {
                vec![value]
            }
            .into(),
        })
    }

    pub fn new(program: &super::VerifiedProgram) -> Self {
        let host = super::host::HostContext::current()
            .unwrap_or_else(|_| super::host::HostContext::new("."));
        Self::with_host_context(program, host)
    }

    pub fn with_host_context(
        program: &super::VerifiedProgram,
        host: impl Into<Arc<super::host::HostContext>>,
    ) -> Self {
        Self {
            program: program.clone(),
            host: host.into(),
            cancellation: None,
            interrupt: None,
            debugger: None,
        }
    }

    /// Cooperatively interrupts this execution when the embedding application cancels it.
    pub fn with_cancellation_probe(mut self, probe: fn() -> bool) -> Self {
        self.interrupt = Some(probe);
        self
    }

    pub fn with_debugger(mut self, debugger: Arc<dyn super::debug::Observer>) -> Self {
        self.debugger = Some(debugger);
        self
    }

    pub fn host_context(&self) -> &super::host::HostContext {
        &self.host
    }

    pub fn run_main(&self) -> Result<Value, FosterError> {
        self.run_main_with_arguments(&crate::entry::CommandArguments::default())
    }

    pub fn run_main_with_arguments(
        &self,
        arguments: &crate::entry::CommandArguments,
    ) -> Result<Value, FosterError> {
        self.run_main_runtime(arguments).map_err(Into::into)
    }

    fn run_main_runtime(
        &self,
        arguments: &crate::entry::CommandArguments,
    ) -> Result<Value, RuntimeError> {
        let main = self
            .program
            .metadata
            .main
            .ok_or_else(|| RuntimeError::runtime("bytecode has no `main` function"))?;
        let arguments = self
            .program
            .metadata
            .main_arguments
            .then(|| Value::command_arguments(self.program.metadata.string_record, arguments))
            .into_iter()
            .collect();
        self.execute(main, Vec::new(), arguments, None)
            .map(|result| result.0)
    }

    /// Executes a compiled zero-argument function in a fresh VM frame.
    pub fn run_function(&self, function: FunctionId) -> Result<Value, FosterError> {
        self.run_function_runtime(function).map_err(Into::into)
    }

    fn run_function_runtime(&self, function: FunctionId) -> Result<Value, RuntimeError> {
        let definition = self
            .program
            .functions
            .get(&function)
            .ok_or_else(|| RuntimeError::runtime("bytecode references an unknown function"))?;
        if definition.parameter_count() != 0 || definition.captures != 0 {
            return Err(RuntimeError::runtime(format!(
                "VM entry function `{}` must have no parameters or captures",
                definition.name
            )));
        }
        self.execute(function, Vec::new(), Vec::new(), None)
            .map(|result| result.0)
    }

    fn execute(
        &self,
        entry: FunctionId,
        captures: Vec<Capture>,
        arguments: Vec<Value>,
        receiver: Option<Rc<Slot>>,
    ) -> Result<(Value, Option<Value>), RuntimeError> {
        self.execute_with_leases(entry, captures, arguments, receiver, Vec::new())
    }

    fn execute_with_leases(
        &self,
        entry: FunctionId,
        captures: Vec<Capture>,
        arguments: Vec<Value>,
        receiver: Option<Rc<Slot>>,
        argument_leases: Vec<AccessLease>,
    ) -> Result<(Value, Option<Value>), RuntimeError> {
        let result =
            self.execute_frames(entry, captures, arguments, receiver, argument_leases, None);
        let cleanup_error = CLEANUP_FAILURE.with(|failure| failure.borrow_mut().take());
        match (result, cleanup_error) {
            (Err(error), _) | (_, Some(error)) => Err(error),
            (Ok(value), None) => Ok(value),
        }
    }

    fn execute_frames(
        &self,
        entry: FunctionId,
        captures: Vec<Capture>,
        arguments: Vec<Value>,
        receiver: Option<Rc<Slot>>,
        argument_leases: Vec<AccessLease>,
        callback_slots: Option<&[Option<Rc<Slot>>]>,
    ) -> Result<(Value, Option<Value>), RuntimeError> {
        let mut constants = ConstantCache::default();
        let mut frames = FrameStack(vec![if let Some(receiver) = receiver.clone() {
            self.method_frame(entry, receiver, arguments, None)?
        } else {
            self.frame(entry, captures, arguments, None)?
        }]);
        if let Some(slots) = callback_slots {
            for (index, slot) in slots.iter().enumerate() {
                if let Some(slot) = slot {
                    frames.0[0].registers[index] = RegisterCell::Place(slot.clone());
                }
            }
        }
        frames[0].argument_leases = argument_leases;
        let mut interrupt_budget = 0u16;

        loop {
            if let Some(probe) = self.interrupt {
                if interrupt_budget == 0 {
                    if probe() {
                        return Err(RuntimeError::runtime("execution cancelled"));
                    }
                    interrupt_budget = 1024;
                }
                interrupt_budget -= 1;
            }
            if let Some(error) = self
                .cancellation
                .as_ref()
                .and_then(|control| control.error())
            {
                return Err(RuntimeError::runtime(error.to_string()));
            }
            if let Some(error) = CLEANUP_FAILURE.with(|failure| failure.borrow_mut().take()) {
                return Err(error);
            }
            if let Some(debugger) = &self.debugger {
                let top = frames.last().expect("the VM retains its entry frame");
                if debugger.should_stop(top.function_id, top.instruction, frames.len())? {
                    let snapshots = frames
                        .iter()
                        .map(|frame| super::debug::Frame {
                            function: frame.function_id,
                            name: frame.function.name.clone(),
                            instruction: if std::ptr::eq(frame, top) {
                                frame.instruction
                            } else {
                                frame.instruction.saturating_sub(1)
                            },
                            registers: debugger
                                .registers(frame.function_id)
                                .into_iter()
                                .filter_map(|index| {
                                    let cell = frame.registers.get(index)?;
                                    let value = if frame
                                        .debug_live
                                        .as_ref()
                                        .is_some_and(|live| !live[index])
                                    {
                                        "<unavailable>".into()
                                    } else {
                                        cell.read()
                                            .map(|value| super::debug::render(&value))
                                            .unwrap_or_else(|_| "<unavailable>".into())
                                    };
                                    Some((index, value))
                                })
                                .collect(),
                        })
                        .collect();
                    debugger.stopped(snapshots)?;
                }
            }
            let frame = frames.last_mut().expect("the VM retains its entry frame");
            let function = frame.function;
            let instruction = function
                .instructions
                .get(frame.instruction)
                .ok_or_else(|| {
                    RuntimeError::runtime(format!("VM function `{}` did not return", function.name))
                })?;
            frame.instruction += 1;

            match instruction {
                &Instruction::Drop { register } => {
                    drop_register(frame, register);
                }
                &Instruction::LoadConstant {
                    destination,
                    constant,
                } => write(frame, destination, constants.load(&self.program, constant))?,
                &Instruction::Move {
                    destination,
                    source,
                } => {
                    // SSA copy-on-write values can share their source register.
                    // VM containers detach during mutation; a self-copy must not
                    // write through a projected reference and invalidate loans.
                    if destination == source
                        && frame.registers[usize::from(destination.0)]
                            .reference()
                            .is_some()
                    {
                        continue;
                    }
                    let value = if frame.registers[usize::from(destination.0)]
                        .reference()
                        .is_some()
                    {
                        read(frame, source)?
                    } else {
                        bind(frame, source)
                    };
                    write(frame, destination, value)?;
                }
                &Instruction::Unary {
                    destination,
                    operator,
                    operand,
                } => write(frame, destination, unary(operator, &read(frame, operand)?)?)?,
                &Instruction::Binary {
                    destination,
                    operator,
                    left,
                    right,
                } => {
                    let left_value = read(frame, left)?;
                    let right_value = read(frame, right)?;
                    let value = binary(operator, &left_value, &right_value).map_err(|error| {
                        RuntimeError::runtime(format!(
                            "{} in `{}` with {left_value:?} and {right_value:?}",
                            error.message, function.name
                        ))
                    })?;
                    write(frame, destination, value)?;
                }
                Instruction::MakeList {
                    destination,
                    elements,
                    ..
                } => {
                    let values = elements
                        .iter()
                        .copied()
                        .map(|register| read(frame, register))
                        .collect::<Result<_, _>>()?;
                    write(frame, *destination, Value::list(values))?;
                }
                &Instruction::Index {
                    destination,
                    object,
                    index,
                } => {
                    let Value::Integer(index) = read(frame, index)? else {
                        return Err(RuntimeError::runtime("VM list index is not an integer"));
                    };
                    let index = usize::try_from(index)
                        .map_err(|_| RuntimeError::runtime("index is out of bounds"))?;
                    let object = read(frame, object)?;
                    let value = match object {
                        value if value.list_value().is_some() => {
                            value.list_value().unwrap().get(index).cloned()
                        }
                        value if value.byte_buffer_value().is_some() => value
                            .byte_buffer_value()
                            .unwrap()
                            .get(index)
                            .copied()
                            .map(Value::Byte),
                        value if value.byte_buffer_list_value().is_some() => {
                            value.byte_buffer_list_value().unwrap().get(index).cloned()
                        }
                        value if value.bytes_value().is_some() => value
                            .bytes_value()
                            .unwrap()
                            .get(index)
                            .copied()
                            .map(Value::Byte),
                        _ => return Err(RuntimeError::runtime("value does not support indexing")),
                    }
                    .ok_or_else(|| RuntimeError::runtime("index is out of bounds"))?;
                    write(frame, destination, value)?;
                }
                Instruction::MakeRecord {
                    destination,
                    record,
                    fields,
                    ..
                } => {
                    let values = fields
                        .iter()
                        .map(|(name, register)| {
                            read(frame, *register).map(|value| (name.clone(), value))
                        })
                        .collect::<Result<Vec<_>, RuntimeError>>()?;
                    let metadata = &self.program.metadata.records[record];
                    let fields = RecordFields::partial(metadata.layout().clone(), values)?;
                    if let Some(function) = self
                        .program
                        .metadata
                        .dispatch
                        .get(&(
                            crate::types::NominalTypeId::Record(*record),
                            crate::types::DEINIT_SLOT,
                        ))
                        .copied()
                    {
                        fields.set_cleanup(Cleanup {
                            program: self.program.clone(),
                            host: self.host.clone(),
                            function,
                            record: Some(*record),
                            variant: None,
                            proxy: None,
                        });
                    }
                    write(
                        frame,
                        *destination,
                        Value::Record {
                            record: Some(*record),
                            name: metadata.name.clone(),
                            fields,
                        },
                    )?;
                }
                Instruction::MakeVariant {
                    destination,
                    variant,
                    payload,
                    type_arguments,
                } => {
                    let metadata = &self.program.metadata.variants[variant];
                    let substitutions = metadata
                        .parameters
                        .iter()
                        .cloned()
                        .zip(type_arguments.iter().cloned())
                        .collect();
                    let payload: Vec<_> = payload
                        .iter()
                        .copied()
                        .zip(&metadata.payload)
                        .map(|(register, ty)| {
                            if matches!(
                                ty.substitute(&substitutions),
                                crate::codegen::types::ExecutableType::Reference(_)
                            ) {
                                Ok(bind(frame, register))
                            } else {
                                read(frame, register)
                            }
                        })
                        .collect::<Result<_, _>>()?;
                    let payload = super::value::VariantPayload::from(payload);
                    if let Some(function) = self
                        .program
                        .metadata
                        .dispatch
                        .get(&(
                            crate::types::NominalTypeId::Variant(metadata.parent),
                            crate::types::DEINIT_SLOT,
                        ))
                        .copied()
                    {
                        payload.set_cleanup(Cleanup {
                            program: self.program.clone(),
                            host: self.host.clone(),
                            function,
                            record: None,
                            variant: Some((
                                metadata.parent,
                                metadata.type_name.clone(),
                                metadata.alternative.clone(),
                            )),
                            proxy: None,
                        });
                    }
                    write(
                        frame,
                        *destination,
                        Value::Variant {
                            variant: Some(metadata.parent),
                            type_name: metadata.type_name.clone(),
                            alternative: metadata.alternative.clone(),
                            payload,
                        },
                    )?;
                }
                Instruction::LoadField {
                    destination,
                    object,
                    field,
                    by_reference,
                } => {
                    let value = read(frame, *object)?;
                    if *by_reference
                        && matches!(&value, Value::Record { fields, .. } if fields.contains_key(field))
                    {
                        let reference = PlaceHandle::field(place(frame, *object), field.clone())?;
                        write(frame, *destination, Value::Reference(reference))?;
                    } else {
                        let value = member(value, field, self.program.metadata.string_record)?;
                        write(frame, *destination, value)?;
                    }
                }
                Instruction::StoreField {
                    object,
                    field,
                    source,
                } => {
                    if frame.registers[usize::from(object.0)].reference().is_some() {
                        let target = PlaceHandle::field(place(frame, *object), field.clone())?;
                        target.write(read(frame, *source)?)?;
                        continue;
                    }
                    let mut value = read(frame, *object)?;
                    let Value::Record { fields, .. } = &mut value else {
                        return Err(RuntimeError::runtime("field assignment requires a record"));
                    };
                    *fields.get_mut(field).ok_or_else(|| {
                        RuntimeError::runtime(format!("record has no field `{field}`"))
                    })? = read(frame, *source)?;
                    write(frame, *object, value)?;
                }
                &Instruction::StoreIndex {
                    object,
                    index,
                    source,
                } => {
                    let Value::Integer(index) = read(frame, index)? else {
                        return Err(RuntimeError::runtime("list index is not an integer"));
                    };
                    let mut value = read(frame, object)?;
                    let index = usize::try_from(index)
                        .map_err(|_| RuntimeError::runtime("index is out of bounds"))?;
                    let source = read(frame, source)?;
                    if frame.registers[usize::from(object.0)].reference().is_some() {
                        let target = PlaceHandle::indexed(place(frame, object), index)?;
                        target.write(source)?;
                        continue;
                    }
                    match &mut value {
                        value if value.list_value().is_some() => {
                            let values = value.list_value_mut().unwrap();
                            *values
                                .get_mut(index)
                                .ok_or_else(|| RuntimeError::runtime("index is out of bounds"))? =
                                source;
                        }
                        receiver if receiver.byte_buffer_value().is_some() => {
                            let values = receiver.byte_buffer_value_mut().unwrap();
                            let Value::Byte(source) = source else {
                                return Err(RuntimeError::runtime(
                                    "byte-buffer elements require Byte values",
                                ));
                            };
                            *values
                                .get_mut(index)
                                .ok_or_else(|| RuntimeError::runtime("index is out of bounds"))? =
                                source;
                        }
                        receiver if receiver.byte_buffer_list_value().is_some() => {
                            let values = receiver.byte_buffer_list_value_mut().unwrap();
                            if !matches!(&source, Value::Byte(_)) {
                                return Err(RuntimeError::runtime(
                                    "byte-buffer elements require Byte values",
                                ));
                            }
                            *values
                                .get_mut(index)
                                .ok_or_else(|| RuntimeError::runtime("index is out of bounds"))? =
                                source;
                        }
                        _ => {
                            return Err(RuntimeError::runtime(
                                "indexed assignment requires mutable indexed storage",
                            ));
                        }
                    }
                    write(frame, object, value)?;
                }
                &Instruction::MakeReference {
                    destination,
                    object,
                    index,
                    ..
                } => {
                    let Value::Integer(index) = read(frame, index)? else {
                        return Err(RuntimeError::runtime("reference index must be Int"));
                    };
                    let index = usize::try_from(index)
                        .map_err(|_| RuntimeError::runtime("reference index is out of bounds"))?;
                    let reference = PlaceHandle::indexed(place(frame, object), index)?;
                    write(frame, destination, Value::Reference(reference))?;
                }
                &Instruction::MakeWholeReference {
                    destination,
                    object,
                    ..
                } => {
                    let reference = Slot::place(&place(frame, object));
                    write(frame, destination, Value::Reference(reference))?;
                }
                Instruction::MakeFieldReference {
                    destination,
                    object,
                    field,
                    ..
                } => {
                    let reference = PlaceHandle::field(place(frame, *object), field.clone())?;
                    write(frame, *destination, Value::Reference(reference))?;
                }
                &Instruction::MoveOut {
                    by_reference,
                    destination,
                    source,
                } => {
                    if let Some(live) = &mut frame.debug_live {
                        live[source.0 as usize] = false;
                    }
                    let value = if by_reference {
                        let source = place(frame, source);
                        let value = source.read()?;
                        source.write(Value::Unit)?;
                        value
                    } else {
                        frame.registers[source.0 as usize].replace(Value::Unit)
                    };
                    write(frame, destination, value)?;
                }
                &Instruction::Push {
                    destination,
                    object,
                    value,
                } => {
                    let value = read(frame, value)?;
                    frame.registers[object.0 as usize].reshape(|receiver| match receiver {
                        receiver if receiver.list_value().is_some() => {
                            let values = receiver.list_value_mut().unwrap();
                            values.push(value);
                            Ok(())
                        }
                        receiver if receiver.byte_buffer_value().is_some() => {
                            let values = receiver.byte_buffer_value_mut().unwrap();
                            let Value::Byte(value) = value else {
                                return Err(RuntimeError::runtime(
                                    "ByteBuffer.push requires a Byte",
                                ));
                            };
                            values.push(value);
                            Ok(())
                        }
                        _ => Err(RuntimeError::runtime(
                            "`push` requires a List or ByteBuffer",
                        )),
                    })?;
                    write(frame, destination, Value::Unit)?;
                }
                &Instruction::Append {
                    destination,
                    object,
                    value,
                } => {
                    let mut list = read(frame, object)?;
                    let Some(values) = list.list_value_mut() else {
                        return Err(RuntimeError::runtime(
                            "List.append requires a List receiver",
                        ));
                    };
                    values.push(read(frame, value)?);
                    write(frame, destination, list)?;
                }
                Instruction::Contains {
                    destination,
                    value,
                    candidates,
                } => {
                    let value = read(frame, *value)?;
                    let found = candidates
                        .iter()
                        .copied()
                        .map(|candidate| read(frame, candidate))
                        .collect::<Result<Vec<_>, _>>()?
                        .contains(&value);
                    write(frame, *destination, Value::Bool(found))?;
                }
                Instruction::Builtin {
                    destination,
                    builtin,
                    arguments,
                } => {
                    if *builtin == crate::intrinsics::Builtin::CCallbackNew {
                        let callback = take(frame, arguments[0]);
                        let signature = read(frame, arguments[1])?.string_text()?.to_owned();
                        let Value::Integer(mode) = read(frame, arguments[2])? else {
                            return Err(RuntimeError::runtime("invalid callback mode"));
                        };
                        let result = self.register_callback(callback, &signature, mode);
                        write(
                            frame,
                            *destination,
                            Value::bytes(crate::foreign::runtime::response(
                                result.map(|token| token.to_le_bytes().to_vec()),
                            )),
                        )?;
                        continue;
                    }
                    let value = match crate::handlers::handler(*builtin) {
                        Some(BuiltinHandler::Direct(handler)) => {
                            let arguments = arguments
                                .iter()
                                .copied()
                                .map(|argument| read(frame, argument))
                                .collect::<Result<Vec<_>, _>>()?;
                            handler(&arguments, self.program.metadata.string_record)?
                        }
                        Some(BuiltinHandler::ConsumeFirst(handler)) => {
                            let Some((receiver, arguments)) = arguments.split_first() else {
                                return Err(RuntimeError::runtime(
                                    "consuming builtin requires a receiver",
                                ));
                            };
                            let arguments = arguments
                                .iter()
                                .copied()
                                .map(|argument| read(frame, argument))
                                .collect::<Result<Vec<_>, _>>()?;
                            if let Some(live) = &mut frame.debug_live {
                                live[receiver.0 as usize] = false;
                            }
                            let receiver =
                                frame.registers[receiver.0 as usize].replace(Value::Unit);
                            handler(receiver, &arguments, self.program.metadata.string_record)?
                        }
                        None => {
                            let arguments = arguments
                                .iter()
                                .copied()
                                .map(|argument| read(frame, argument))
                                .collect::<Result<Vec<_>, _>>()?;
                            super::builtins::dispatch(
                                self.host.as_ref(),
                                *builtin,
                                &arguments,
                                self.program.metadata.string_record,
                            )?
                        }
                    };
                    write(frame, *destination, value)?;
                }
                Instruction::MatchPattern {
                    destination,
                    subject,
                    pattern,
                    bindings,
                } => {
                    let mut values = Vec::new();
                    let matched =
                        match_pattern(&self.program, pattern, &bind(frame, *subject), &mut values)?;
                    if matched {
                        for (register, value) in bindings.iter().copied().zip(values) {
                            write(frame, register, value)?;
                        }
                    }
                    write(frame, *destination, Value::Bool(matched))?;
                }
                &Instruction::Jump { target } => frame.instruction = target,
                &Instruction::JumpIfFalse { condition, target } => {
                    if read(frame, condition)? == Value::Bool(false) {
                        frame.instruction = target;
                    }
                }
                &Instruction::Assert { condition, message } => {
                    let Value::Bool(condition) = read(frame, condition)? else {
                        return Err(RuntimeError::runtime("VM assertion condition is not Bool"));
                    };
                    if !condition {
                        let message = message
                            .map(|message| {
                                read(frame, message).and_then(|value| {
                                    value.as_string().map(str::to_owned).ok_or_else(|| {
                                        RuntimeError::runtime("VM assertion message is not String")
                                    })
                                })
                            })
                            .transpose()?;
                        return Err(RuntimeError::runtime(match message {
                            Some(message) => format!("assertion failed: {message}"),
                            None => "assertion failed".to_owned(),
                        }));
                    }
                }
                Instruction::MakeClosure {
                    destination,
                    function,
                    captures,
                    ..
                } => {
                    let captures = capture(frame, captures)?;
                    write(
                        frame,
                        *destination,
                        Value::VmClosure {
                            function: *function,
                            captures,
                        },
                    )?;
                }
                Instruction::Call {
                    destination,
                    function,
                    arguments,
                    ..
                } => {
                    let next = self.call_frame(
                        *function,
                        Vec::new(),
                        frame,
                        arguments,
                        Some(*destination),
                    )?;
                    frames.push(next);
                }
                Instruction::CallMethod {
                    destination,
                    receiver,
                    function,
                    arguments,
                    ..
                } => {
                    let receiver = place(frame, *receiver);
                    if let Some(shared) = receiver.shared() {
                        let (lease, state) = shared.write_snapshot()?;
                        let local = Slot::new(Value::from_wire(state)?);
                        let mut method = self.method_call_frame(
                            *function,
                            local.clone(),
                            frame,
                            arguments,
                            Some(*destination),
                        )?;
                        method.shared_commit = Some(SharedCommit {
                            shared,
                            receiver: local,
                            _lease: lease,
                        });
                        frames.push(method);
                    } else {
                        let next = self.method_call_frame(
                            *function,
                            receiver,
                            frame,
                            arguments,
                            Some(*destination),
                        )?;
                        frames.push(next);
                    }
                }
                Instruction::CallContractMethod {
                    destination,
                    receiver,
                    slot,
                    name,
                    arguments,
                    ..
                } => {
                    let receiver = place(frame, *receiver);
                    let value = receiver.read()?;
                    if *slot == crate::types::CAN_COPY_SLOT || *slot == crate::types::COPY_SLOT {
                        let nominal = match &value {
                            Value::Record {
                                record: Some(record),
                                ..
                            } => Some(crate::types::NominalTypeId::Record(*record)),
                            Value::Variant {
                                variant: Some(variant),
                                ..
                            } => Some(crate::types::NominalTypeId::Variant(*variant)),
                            _ => None,
                        };
                        let implementation = nominal
                            .and_then(|nominal| {
                                self.program
                                    .metadata
                                    .dispatch
                                    .get(&(nominal, crate::types::COPY_SLOT))
                            })
                            .copied();
                        let trivial = matches!(
                            value,
                            Value::Unit
                                | Value::Bool(_)
                                | Value::Integer(_)
                                | Value::Float(_)
                                | Value::CodePoint(_)
                                | Value::Byte(_)
                        ) || value.symbol_bytes().is_some();
                        if *slot == crate::types::CAN_COPY_SLOT {
                            write(
                                frame,
                                *destination,
                                Value::Bool(trivial || implementation.is_some()),
                            )?;
                        } else if trivial {
                            write(frame, *destination, value)?;
                        } else {
                            let target = implementation.ok_or_else(|| {
                                RuntimeError::runtime("value does not implement Copy")
                            })?;
                            let next = self.method_call_frame(
                                target,
                                receiver,
                                frame,
                                &[],
                                Some(*destination),
                            )?;
                            frames.push(next);
                        }
                        continue;
                    }
                    if let Value::Record { fields, .. } = &value
                        && arguments.is_empty()
                        && let Some(field) = fields.get(name)
                        && value.string_bytes().is_none()
                    {
                        write(frame, *destination, field.clone())?;
                        continue;
                    }
                    let function = match &value {
                        Value::Record {
                            record: Some(record),
                            ..
                        } => self
                            .program
                            .metadata
                            .dispatch
                            .get(&(crate::types::NominalTypeId::Record(*record), *slot))
                            .copied(),
                        Value::Variant {
                            variant: Some(variant),
                            ..
                        } => self
                            .program
                            .metadata
                            .dispatch
                            .get(&(crate::types::NominalTypeId::Variant(*variant), *slot))
                            .copied(),
                        _ => None,
                    }
                    .or_else(|| {
                        let mut matches = self.program.metadata.dispatch.iter().filter_map(
                            |((_, candidate_slot), target)| {
                                if candidate_slot != slot {
                                    return None;
                                }
                                let matches = match self.program.functions[target]
                                    .parameters
                                    .first()
                                    .map(|p| &p.ty)
                                {
                                    Some(crate::codegen::types::ExecutableType::List(_)) => {
                                        value.list_value().is_some()
                                    }
                                    Some(crate::codegen::types::ExecutableType::Bytes) => {
                                        value.bytes_value().is_some()
                                    }
                                    Some(crate::codegen::types::ExecutableType::ByteBuffer) => {
                                        value.byte_buffer_value().is_some()
                                            || value.byte_buffer_list_value().is_some()
                                    }
                                    _ => false,
                                };
                                matches.then_some(*target)
                            },
                        );
                        let first = matches.next()?;
                        matches.all(|other| other == first).then_some(first)
                    });
                    if let Some(function) = function {
                        if let Some(shared) = receiver.shared() {
                            let (lease, state) = shared.write_snapshot()?;
                            let local = Slot::new(Value::from_wire(state)?);
                            let mut method = self.method_call_frame(
                                function,
                                local.clone(),
                                frame,
                                arguments,
                                Some(*destination),
                            )?;
                            method.shared_commit = Some(SharedCommit {
                                shared,
                                receiver: local,
                                _lease: lease,
                            });
                            frames.push(method);
                        } else {
                            let next = self.method_call_frame(
                                function,
                                receiver,
                                frame,
                                arguments,
                                Some(*destination),
                            )?;
                            frames.push(next);
                        }
                        continue;
                    }
                    if value.string_bytes().is_some()
                        || value.bytes_value().is_some()
                        || value.list_value().is_some()
                        || value.byte_buffer_value().is_some()
                        || value.byte_buffer_list_value().is_some()
                    {
                        if !arguments.is_empty() {
                            return Err(RuntimeError::runtime(format!(
                                "contract member `{name}` does not accept arguments"
                            )));
                        }
                        write(
                            frame,
                            *destination,
                            member(value, name, self.program.metadata.string_record)?,
                        )?;
                        continue;
                    }
                    if !matches!(
                        value,
                        Value::Record { .. }
                            | Value::Variant {
                                variant: Some(_),
                                ..
                            }
                    ) {
                        if !arguments.is_empty() {
                            return Err(RuntimeError::runtime(format!(
                                "contract member `{}` does not accept arguments",
                                name
                            )));
                        }
                        write(
                            frame,
                            *destination,
                            member(value, name, self.program.metadata.string_record)?,
                        )?;
                        continue;
                    }
                    return Err(RuntimeError::runtime(format!(
                        "value has no implementation of required method `{name}`"
                    )));
                }
                Instruction::CallValue {
                    destination,
                    callee,
                    arguments,
                } => {
                    let Value::VmClosure { function, captures } = read(frame, *callee)? else {
                        return Err(RuntimeError::runtime("VM dynamic call requires a closure"));
                    };
                    let next =
                        self.call_frame(function, captures, frame, arguments, Some(*destination))?;
                    frames.push(next);
                }
                Instruction::CallClosure {
                    destination,
                    function,
                    captures,
                    arguments,
                    ..
                } => {
                    let captures = capture(frame, captures)?;
                    let next =
                        self.call_frame(*function, captures, frame, arguments, Some(*destination))?;
                    frames.push(next);
                }
                &Instruction::SpawnRemote { destination, value } => {
                    let state = read(frame, value)?.into_wire()?;
                    let (sender, inbox) = may::sync::mpsc::channel::<RemoteMessage>();
                    let program = self.program.clone();
                    let host = self.host.clone();
                    let id = next_remote_id();
                    let control = Arc::new(crate::remote::Control::default());
                    let worker_control = control.clone();
                    let (finished, completion) = may::sync::mpsc::channel();
                    let _handle = may::go_with!(1024 * 1024, move || {
                        let _finished = WorkerCompletion(finished);
                        let machine = Machine {
                            program,
                            host,
                            cancellation: Some(worker_control.clone()),
                            interrupt: None,
                            debugger: None,
                        };
                        let mut state = state;
                        while let Ok(message) = inbox.recv() {
                            if worker_control.error().is_some() {
                                continue;
                            }
                            let result = (|| {
                                let receiver = Slot::new(
                                    Value::from_wire(state.clone())
                                        .map_err(|error| error.message)?,
                                );
                                let (arguments, leases) =
                                    materialize_remote_arguments(message.arguments)
                                        .map_err(|error| error.message)?;
                                let (value, updated) = machine
                                    .execute_with_leases(
                                        message.function,
                                        Vec::new(),
                                        arguments,
                                        Some(receiver),
                                        leases,
                                    )
                                    .map_err(|error| error.message)?;
                                state = updated
                                    .expect("remote methods retain self")
                                    .into_wire()
                                    .map_err(|error| error.message)?;
                                value.into_wire().map_err(|error| error.message)
                            })();
                            match result {
                                Err(error) => worker_control
                                    .terminate(crate::remote::RemoteError::Failed(error)),
                                Ok(value) => {
                                    if worker_control.complete(message.request) {
                                        let _ = message.response.send(Ok(value));
                                    }
                                }
                            }
                        }
                    });
                    write(
                        frame,
                        destination,
                        Value::Remote(RemoteValue {
                            id,
                            sender,
                            _owner: Arc::new(RemoteOwner {
                                control: control.clone(),
                                completion: Mutex::new(Some(completion)),
                            }),
                            control,
                        }),
                    )?;
                }
                &Instruction::SpawnRemoteBorrow {
                    destination,
                    source,
                } => {
                    let shared = frame.registers[source.0 as usize].share()?;
                    let (sender, inbox) = may::sync::mpsc::channel::<RemoteMessage>();
                    let program = self.program.clone();
                    let host = self.host.clone();
                    let id = next_remote_id();
                    let control = Arc::new(crate::remote::Control::default());
                    let worker_control = control.clone();
                    let (finished, completion) = may::sync::mpsc::channel();
                    let _handle = may::go_with!(1024 * 1024, move || {
                        let _finished = WorkerCompletion(finished);
                        let machine = Machine {
                            program,
                            host,
                            cancellation: Some(worker_control.clone()),
                            interrupt: None,
                            debugger: None,
                        };
                        while let Ok(message) = inbox.recv() {
                            if worker_control.error().is_some() {
                                continue;
                            }
                            let result = (|| {
                                let (_lease, state) =
                                    shared.read_snapshot().map_err(|error| error.message)?;
                                let receiver = Slot::new(
                                    Value::from_wire(state).map_err(|error| error.message)?,
                                );
                                let (arguments, leases) =
                                    materialize_remote_arguments(message.arguments)
                                        .map_err(|error| error.message)?;
                                let (value, _) = machine
                                    .execute_with_leases(
                                        message.function,
                                        Vec::new(),
                                        arguments,
                                        Some(receiver),
                                        leases,
                                    )
                                    .map_err(|error| error.message)?;
                                value.into_wire().map_err(|error| error.message)
                            })();
                            match result {
                                Err(error) => worker_control
                                    .terminate(crate::remote::RemoteError::Failed(error)),
                                Ok(value) => {
                                    if worker_control.complete(message.request) {
                                        let _ = message.response.send(Ok(value));
                                    }
                                }
                            }
                        }
                    });
                    write(
                        frame,
                        destination,
                        Value::Remote(RemoteValue {
                            id,
                            sender,
                            _owner: Arc::new(RemoteOwner {
                                control: control.clone(),
                                completion: Mutex::new(Some(completion)),
                            }),
                            control,
                        }),
                    )?;
                }
                Instruction::RemoteCall {
                    destination,
                    remote,
                    function,
                    arguments,
                } => {
                    let Value::Remote(remote) = read(frame, *remote)? else {
                        return Err(RuntimeError::runtime(
                            "remote call requires a remote object",
                        ));
                    };
                    let arguments = arguments
                        .iter()
                        .copied()
                        .map(|(mode, register)| match mode {
                            crate::ast::ParameterMode::Borrow => frame.registers
                                [register.0 as usize]
                                .share()
                                .map(RemoteArgument::Borrowed),
                            crate::ast::ParameterMode::Consume => frame.registers
                                [register.0 as usize]
                                .take()
                                .into_wire()
                                .map(RemoteArgument::Owned),
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    let (sender, receiver) = may::sync::mpsc::channel();
                    let cancelled = sender.clone();
                    let request = remote.control.register(move |error| {
                        let _ = cancelled.send(Err(error));
                    });
                    if let Some(request) = request
                        && remote
                            .sender
                            .send(RemoteMessage {
                                request,
                                function: *function,
                                arguments,
                                response: sender,
                            })
                            .is_err()
                    {
                        remote.control.terminate(crate::remote::RemoteError::Failed(
                            "remote object is closed".into(),
                        ));
                    }
                    write(
                        frame,
                        *destination,
                        Value::Future(FutureValue {
                            id: next_future_id(),
                            receiver: Arc::new(Mutex::new(Some(receiver))),
                        }),
                    )?;
                }
                &Instruction::Await {
                    destination,
                    future,
                } => {
                    let Value::Future(future) = read(frame, future)? else {
                        return Err(RuntimeError::runtime("`await` requires a Future"));
                    };
                    let receiver = future
                        .receiver
                        .lock()
                        .map_err(|_| RuntimeError::runtime("future lock was poisoned"))?
                        .take()
                        .ok_or_else(|| RuntimeError::runtime("future has already been awaited"))?;
                    let value = loop {
                        if let Some(error) = self
                            .cancellation
                            .as_ref()
                            .and_then(|control| control.error())
                        {
                            return Err(RuntimeError::runtime(error.to_string()));
                        }
                        match receiver.try_recv() {
                            Ok(value) => break value,
                            Err(std::sync::mpsc::TryRecvError::Empty) => {
                                may::coroutine::sleep(std::time::Duration::from_millis(1))
                            }
                            Err(_) => {
                                return Err(RuntimeError::runtime(
                                    "remote object terminated before replying",
                                ));
                            }
                        }
                    };
                    let outcome = match value {
                        Ok(value) => self.remote_variant(
                            self.program.metadata.remote_result,
                            "Ok",
                            Value::from_wire(value)?,
                        )?,
                        Err(error) => {
                            let error = match error {
                                crate::remote::RemoteError::Shutdown => self.remote_variant(
                                    self.program.metadata.remote_error,
                                    "Shutdown",
                                    Value::Unit,
                                )?,
                                crate::remote::RemoteError::Failed(error) => self.remote_variant(
                                    self.program.metadata.remote_error,
                                    "Failed",
                                    Value::string(
                                        self.program.metadata.string_record,
                                        error.into_bytes(),
                                    ),
                                )?,
                            };
                            self.remote_variant(
                                self.program.metadata.remote_result,
                                "Error",
                                error,
                            )?
                        }
                    };
                    write(frame, destination, outcome)?;
                }
                &Instruction::Return { source } => {
                    let value = if function.returns_reference {
                        bind(frame, source)
                    } else {
                        read(frame, source)?
                    };
                    let mut completed = frames.pop().expect("return has a frame");
                    if let Some(commit) = completed.shared_commit.take() {
                        commit.shared.commit(commit.receiver.read()?.into_wire()?)?;
                    }
                    let Some(caller) = frames.last_mut() else {
                        return Ok((
                            value,
                            receiver.as_ref().map(|slot| slot.read()).transpose()?,
                        ));
                    };
                    write(
                        caller,
                        completed
                            .return_destination
                            .expect("non-entry calls have a destination"),
                        value,
                    )?;
                }
            }
        }
    }

    fn register_callback(
        &self,
        callback: Value,
        signature: &str,
        mode: i64,
    ) -> Result<i64, String> {
        if may::coroutine::is_coroutine() {
            return Err("C callbacks cannot be registered in remote tasks".into());
        }
        if !(0..=2).contains(&mode) {
            return Err("invalid callback mode".into());
        }
        fn owned(value: &Value) -> bool {
            match value {
                Value::Reference(_) => false,
                Value::VmClosure { .. } => false,
                Value::Record { fields, .. } => fields.iter().all(|(_, value)| owned(value)),
                Value::Variant { payload, .. } => payload.iter().all(owned),
                Value::RawList(values) => values.iter().all(owned),
                _ => true,
            }
        }
        let Value::VmClosure { function, captures } = callback else {
            return Err("callback requires a closure".into());
        };
        if mode != 0
            && !captures
                .iter()
                .all(|capture| matches!(capture, Capture::Value(value) if owned(value)))
        {
            return Err("retained callbacks require owned data without borrowed references or nested callables".into());
        }
        if self.program.functions[&function].parameter_count() != 1 {
            return Err("callback adapter requires one packet argument".into());
        }
        let mut slots = Vec::new();
        let captures = captures
            .into_iter()
            .map(|capture| match capture {
                Capture::Value(value) => {
                    slots.push(Some(Slot::new(value)));
                    Capture::Value(Value::Unit)
                }
                capture => {
                    slots.push(None);
                    capture
                }
            })
            .collect::<Vec<_>>();
        let machine = Machine {
            program: self.program.clone(),
            host: self.host.clone(),
            cancellation: None,
            interrupt: None,
            debugger: None,
        };
        crate::foreign::runtime::callbacks::register(
            signature,
            mode == 2,
            Box::new(move |payload| {
                let arguments = vec![Value::bytes(payload.to_vec())];
                let previous = CLEANUP_FAILURE.with(|failure| failure.borrow_mut().take());
                let result = machine.execute_frames(
                    function,
                    captures.clone(),
                    arguments,
                    None,
                    vec![],
                    Some(&slots),
                );
                let cleanup = CLEANUP_FAILURE.with(|failure| failure.replace(previous));
                if let Some(error) = cleanup {
                    return Err(error.to_string());
                }
                result
                    .map_err(|error| error.to_string())?
                    .0
                    .bytes_value()
                    .map(<[u8]>::to_vec)
                    .ok_or_else(|| RuntimeError::runtime("callback response requires Bytes"))
                    .map_err(|error| error.to_string())
            }),
        )
    }

    fn frame<'program>(
        &'program self,
        function: FunctionId,
        captures: Vec<Capture>,
        arguments: impl IntoIterator<Item = Value, IntoIter: ExactSizeIterator>,
        return_destination: Option<Register>,
    ) -> Result<Frame<'program>, RuntimeError> {
        let arguments = arguments.into_iter();
        let bytecode = &self.program.functions[&function];
        if captures.len() != usize::from(bytecode.captures)
            || arguments.len() != usize::from(bytecode.parameter_count())
        {
            return Err(RuntimeError::runtime(format!(
                "VM call to `{}` has an invalid capture or parameter layout (expected {}/{}, received {}/{})",
                bytecode.name,
                bytecode.captures,
                bytecode.parameter_count(),
                captures.len(),
                arguments.len()
            )));
        }
        let mut registers =
            REGISTER_POOL.with(|pool| pool.borrow_mut().take(bytecode.registers as usize));
        for (index, capture) in captures.into_iter().enumerate() {
            registers[index] = match capture {
                Capture::Value(value) => RegisterCell::Inline(value),
                Capture::Place(place) => RegisterCell::Inline(Value::Reference(place)),
            };
        }
        let offset = usize::from(bytecode.captures);
        for (index, argument) in arguments.into_iter().enumerate() {
            registers[offset + index] = RegisterCell::Inline(argument);
        }
        Ok(Frame {
            function_id: function,
            function: bytecode,
            registers,
            debug_live: self.debugger.as_ref().map(|_| {
                (0..bytecode.registers)
                    .map(|register| register < bytecode.captures + bytecode.parameter_count())
                    .collect()
            }),
            instruction: 0,
            return_destination,
            shared_commit: None,
            argument_leases: Vec::new(),
        })
    }

    fn method_frame<'program>(
        &'program self,
        function: FunctionId,
        receiver: Rc<Slot>,
        arguments: impl IntoIterator<Item = Value, IntoIter: ExactSizeIterator>,
        return_destination: Option<Register>,
    ) -> Result<Frame<'program>, RuntimeError> {
        let arguments = arguments.into_iter();
        let mut frame = self.frame(
            function,
            Vec::new(),
            (0..arguments.len() + 1).map(|_| Value::Unit),
            return_destination,
        )?;
        let offset = usize::from(frame.function.captures);
        frame.registers[offset] = RegisterCell::Place(receiver);
        for (index, argument) in arguments.enumerate() {
            frame.registers[offset + index + 1] = RegisterCell::Inline(argument);
        }
        Ok(frame)
    }

    fn call_frame<'program>(
        &'program self,
        function: FunctionId,
        captures: Vec<Capture>,
        caller: &mut Frame<'program>,
        arguments: &[Register],
        return_destination: Option<Register>,
    ) -> Result<Frame<'program>, RuntimeError> {
        let mut frame = self.frame(
            function,
            captures,
            (0..arguments.len()).map(|_| Value::Unit),
            return_destination,
        )?;
        let bytecode = frame.function;
        let offset = usize::from(bytecode.captures);
        for (index, argument) in arguments.iter().copied().enumerate() {
            frame.registers[offset + index] = match bytecode.parameters[index].mode {
                crate::ast::ParameterMode::Consume => RegisterCell::Inline(take(caller, argument)),
                crate::ast::ParameterMode::Borrow if bytecode.parameters[index].mutable => {
                    RegisterCell::Place(place(caller, argument))
                }
                crate::ast::ParameterMode::Borrow => borrow_parameter(caller, argument),
            };
        }
        Ok(frame)
    }

    fn method_call_frame<'program>(
        &'program self,
        function: FunctionId,
        receiver: Rc<Slot>,
        caller: &mut Frame<'program>,
        arguments: &[Register],
        return_destination: Option<Register>,
    ) -> Result<Frame<'program>, RuntimeError> {
        let mut frame = self.method_frame(
            function,
            receiver,
            (0..arguments.len()).map(|_| Value::Unit),
            return_destination,
        )?;
        let bytecode = frame.function;
        let offset = usize::from(bytecode.captures);
        for (index, argument) in arguments.iter().copied().enumerate() {
            let parameter = index + 1;
            frame.registers[offset + parameter] = match bytecode.parameters[parameter].mode {
                crate::ast::ParameterMode::Consume => RegisterCell::Inline(take(caller, argument)),
                crate::ast::ParameterMode::Borrow if bytecode.parameters[parameter].mutable => {
                    RegisterCell::Place(place(caller, argument))
                }
                crate::ast::ParameterMode::Borrow => borrow_parameter(caller, argument),
            };
        }
        Ok(frame)
    }
}

fn drop_register(frame: &mut Frame<'_>, register: Register) {
    if let Some(live) = &mut frame.debug_live {
        live[register.0 as usize] = false;
    }
    frame.registers[register.0 as usize].detach();
}

fn materialize_remote_arguments(
    arguments: Vec<RemoteArgument>,
) -> Result<(Vec<Value>, Vec<AccessLease>), RuntimeError> {
    let mut values = Vec::with_capacity(arguments.len());
    let mut leases = Vec::new();
    for argument in arguments {
        match argument {
            RemoteArgument::Owned(value) => values.push(Value::from_wire(value)?),
            RemoteArgument::Borrowed(shared) => {
                let (lease, value) = shared.read_snapshot()?;
                values.push(Value::from_wire(value)?);
                leases.push(lease);
            }
        }
    }
    Ok((values, leases))
}

fn capture(
    frame: &mut Frame<'_>,
    captures: &[(CaptureMode, Register)],
) -> Result<Vec<Capture>, RuntimeError> {
    captures
        .iter()
        .copied()
        .map(|(mode, register)| {
            Ok(match mode {
                CaptureMode::Ref => Capture::Place(Slot::place(&place(frame, register))),
                CaptureMode::Move => Capture::Value(take(frame, register)),
                CaptureMode::Copy | CaptureMode::Pending => Capture::Value(read(frame, register)?),
            })
        })
        .collect()
}

fn read(frame: &Frame<'_>, register: Register) -> Result<Value, RuntimeError> {
    frame.registers[register.0 as usize].read()
}

fn bind(frame: &Frame<'_>, register: Register) -> Value {
    frame.registers[register.0 as usize].bind()
}

fn write(frame: &mut Frame<'_>, register: Register, value: Value) -> Result<(), RuntimeError> {
    frame.registers[register.0 as usize].write(value)?;
    if let Some(live) = &mut frame.debug_live {
        live[register.0 as usize] = true;
    }
    Ok(())
}

fn place(frame: &mut Frame<'_>, register: Register) -> Rc<Slot> {
    frame.registers[register.0 as usize].promote()
}

fn take(frame: &mut Frame<'_>, register: Register) -> Value {
    if let Some(live) = &mut frame.debug_live {
        live[register.0 as usize] = false;
    }
    frame.registers[register.0 as usize].take()
}

fn borrow_parameter(frame: &mut Frame<'_>, register: Register) -> RegisterCell {
    if let Some(reference) = frame.registers[register.0 as usize].reference() {
        RegisterCell::Inline(Value::Reference(reference))
    } else {
        RegisterCell::Borrowed(place(frame, register))
    }
}

fn member(
    value: Value,
    field: &str,
    string_record: Option<crate::hir::RecordId>,
) -> Result<Value, RuntimeError> {
    if let Some(storage) = value.string_storage() {
        let text = std::str::from_utf8(&storage)
            .map_err(|_| RuntimeError::runtime("Foster String contains invalid UTF-8"))?;
        return match field {
            "empty?" => Ok(Value::Bool(storage.is_empty())),
            "whitespace?" => Ok(Value::Bool(text.chars().all(char::is_whitespace))),
            "bytes" | "value" => Ok(Value::bytes_shared(storage)),
            _ => Err(RuntimeError::runtime(format!(
                "value has no field `{field}`"
            ))),
        };
    }
    if let Some(values) = value.bytes_value() {
        return match field {
            "empty?" => Ok(Value::Bool(values.is_empty())),
            "length" => Ok(Value::Integer(values.len() as i64)),
            "head" => values
                .first()
                .copied()
                .map(Value::Byte)
                .ok_or_else(|| RuntimeError::runtime("cannot take `head` of empty Bytes")),
            "rest" => Ok(Value::bytes(values.get(1..).unwrap_or_default().to_vec())),
            _ => Err(RuntimeError::runtime(format!(
                "value has no field `{field}`"
            ))),
        };
    }
    if let Some(values) = value.byte_buffer_value() {
        return match field {
            "empty?" => Ok(Value::Bool(values.is_empty())),
            "length" => Ok(Value::Integer(values.len() as i64)),
            "capacity" => Ok(Value::Integer(values.capacity() as i64)),
            "value" => Ok(Value::RawByteBuffer(values.clone())),
            _ => Err(RuntimeError::runtime(format!(
                "value has no field `{field}`"
            ))),
        };
    }
    if let Some(values) = value.byte_buffer_list_value() {
        return match field {
            "empty?" => Ok(Value::Bool(values.is_empty())),
            "length" | "capacity" => Ok(Value::Integer(values.len() as i64)),
            "value" => Ok(Value::list(values.clone())),
            _ => Err(RuntimeError::runtime(format!(
                "value has no field `{field}`"
            ))),
        };
    }
    if let Some(values) = value.list_value() {
        return match field {
            "empty?" => Ok(Value::Bool(values.is_empty())),
            "length" => Ok(Value::Integer(values.len() as i64)),
            "head" => values
                .first()
                .cloned()
                .ok_or_else(|| RuntimeError::runtime("cannot take `head` of an empty list")),
            "rest" => Ok(Value::list(values.get(1..).unwrap_or_default().to_vec())),
            _ => Err(RuntimeError::runtime(format!(
                "value has no field `{field}`"
            ))),
        };
    }
    match (value, field) {
        (Value::Record { fields, .. }, field) => fields
            .get(field)
            .cloned()
            .ok_or_else(|| RuntimeError::runtime(format!("record has no field `{field}`"))),
        (Value::CodePoint(value), "whitespace?") => Ok(Value::Bool(value.is_whitespace())),
        (Value::CodePoint(value), "string") => {
            Ok(Value::string(string_record, value.to_string().into_bytes()))
        }
        (Value::Byte(value), "int") => Ok(Value::Integer(i64::from(value))),
        (value, field) => Err(RuntimeError::runtime(format!(
            "value {value:?} has no field `{field}`"
        ))),
    }
}

#[cfg(test)]
mod register_storage_tests {
    use super::*;

    #[test]
    fn text_constants_share_storage_without_sharing_mutation() {
        let mut program = Program::default();
        program.metadata.constants = vec![
            super::super::Constant::String("original".into()),
            super::super::Constant::Symbol("tag".into()),
        ];
        let mut cache = ConstantCache::default();
        let mut first = cache.load(&program, 0);
        let second = cache.load(&program, 0);
        assert!(Arc::ptr_eq(
            &first.string_storage().unwrap(),
            &second.string_storage().unwrap()
        ));
        let Value::Record { fields, .. } = &mut first else {
            unreachable!()
        };
        *fields.get_mut("value").unwrap() = Value::bytes(b"changed".to_vec());
        assert_eq!(first.string_text().unwrap(), "changed");
        assert_eq!(second.string_text().unwrap(), "original");
        assert_eq!(cache.load(&program, 0).string_text().unwrap(), "original");
        let symbol = cache.load(&program, 1);
        let other = cache.load(&program, 1);
        let (Value::Record { fields: a, .. }, Value::Record { fields: b, .. }) = (symbol, other)
        else {
            unreachable!()
        };
        assert!(a.shares_values_with(&b));

        let mut other_program = Program::default();
        other_program.metadata.constants = vec![super::super::Constant::String("different".into())];
        assert_eq!(
            ConstantCache::default()
                .load(&other_program, 0)
                .string_text()
                .unwrap(),
            "different"
        );
    }

    #[test]
    fn frame_recycling_detaches_surviving_places_and_resets_registers() {
        let compiled = foster_compiler::compile("func main() -> Int { 42 }").unwrap();
        let program = foster_compiler::vm::compile(&compiled).unwrap();
        let machine = Machine::new(&program.clone().into_verified().unwrap());
        let entry = program.metadata.main.unwrap();
        REGISTER_POOL.with(|pool| *pool.borrow_mut() = RegisterPool::default());
        let mut frame = machine.frame(entry, vec![], vec![], None).unwrap();
        let allocation = frame.registers.as_ptr();
        let survivor = Slot::new(Value::Integer(42));
        frame.registers[0] = RegisterCell::Place(survivor.clone());
        drop(frame);
        let mut next = machine.frame(entry, vec![], vec![], None).unwrap();
        assert_eq!(next.registers.as_ptr(), allocation);
        assert!(
            next.registers
                .iter()
                .all(|cell| matches!(cell, RegisterCell::Inline(Value::Unit)))
        );
        next.registers[0].write(Value::Integer(7)).unwrap();
        assert_eq!(survivor.read().unwrap(), Value::Integer(42));
    }

    #[test]
    fn register_pool_bounds_retained_capacity() {
        let mut pool = RegisterPool::default();
        pool.put(Vec::with_capacity(RegisterPool::LIMIT));
        pool.put(Vec::with_capacity(1));
        pool.put(Vec::with_capacity(RegisterPool::LIMIT + 1));
        assert_eq!(pool.capacity, RegisterPool::LIMIT);
        assert_eq!(pool.buffers.len(), 1);
        let buffer = pool.take(4);
        assert_eq!(buffer.len(), 4);
        assert_eq!(pool.capacity, 0);
    }

    #[test]
    fn embedding_cancellation_stops_an_infinite_vm_loop() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static POLLS: AtomicUsize = AtomicUsize::new(0);
        fn cancelled() -> bool {
            POLLS.fetch_add(1, Ordering::Relaxed) > 0
        }
        let compilation = foster_compiler::compile("func main() -> Int { loop {}\n0 }").unwrap();
        let program = foster_compiler::vm::compile(&compilation).unwrap();
        let error = Machine::new(&program.into_verified().unwrap())
            .with_cancellation_probe(cancelled)
            .run_main()
            .unwrap_err();
        assert_eq!(error.message, "execution cancelled");
        assert_eq!(POLLS.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn owner_cancellation_stops_running_vm_code_and_runs_cleanup() {
        let compilation = foster_compiler::compile(
            r#"
import core.drop.Drop
import core.result.Result
import std.fs
type Held = & Drop & {}
impl Held { func deinit(self: Self) -> () { fs::write_text("released", "yes")
() } }
func main() -> Int {
    let held = Held {}
    assert(fs::write_text("started", "yes").success?())
    loop {}
    0
}
"#,
        )
        .unwrap();
        for optimize in [false, true] {
            let directory = std::env::temp_dir().join(format!(
                "foster-vm-shutdown-{}-{}",
                std::process::id(),
                next_remote_id()
            ));
            std::fs::create_dir(&directory).unwrap();
            let program = foster_compiler::vm::compile_with_options(
                &compilation,
                foster_compiler::vm::CompileOptions { optimize },
            )
            .unwrap();
            let control = Arc::new(crate::remote::Control::default());
            let owner = crate::remote::Owner(control.clone());
            let path = directory.clone();
            let (send, receive) = std::sync::mpsc::channel();
            let worker = std::thread::spawn(move || {
                let mut machine = Machine::with_host_context(
                    &program.into_verified().unwrap(),
                    super::super::HostContext::new(path),
                );
                machine.cancellation = Some(control);
                send.send(
                    machine
                        .run_main()
                        .map(|_| ())
                        .map_err(|error| error.message),
                )
                .unwrap();
            });
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !directory.join("started").exists() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "VM never started executing"
                );
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            drop(owner);
            let error = receive
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("VM failed to stop")
                .unwrap_err();
            assert!(error.contains("owner shut down"), "{error}");
            worker.join().unwrap();
            assert!(
                directory.join("released").exists(),
                "cancellation skipped deinit"
            );
            std::fs::remove_dir_all(directory).unwrap();
        }
    }

    #[test]
    fn ordinary_registers_remain_inline_until_their_place_is_observed() {
        let mut cell = RegisterCell::Inline(Value::Integer(42));
        assert!(matches!(cell, RegisterCell::Inline(_)));
        assert_eq!(cell.read().unwrap(), Value::Integer(42));

        let slot = cell.promote();
        assert!(matches!(cell, RegisterCell::Place(_)));
        assert_eq!(slot.read().unwrap(), Value::Integer(42));
    }

    #[test]
    fn consuming_an_inline_register_transfers_its_value() {
        let mut cell = RegisterCell::Inline(Value::RawList(vec![Value::Integer(42)]));
        assert_eq!(cell.take(), Value::RawList(vec![Value::Integer(42)]));
        assert_eq!(cell.read().unwrap(), Value::Unit);
    }

    #[test]
    fn assigning_to_a_read_only_borrow_detaches_the_parameter() {
        let origin = Slot::new(Value::Integer(1));
        let mut cell = RegisterCell::Borrowed(origin.clone());
        cell.write(Value::Integer(2)).unwrap();
        assert_eq!(cell.read().unwrap(), Value::Integer(2));
        assert_eq!(origin.read().unwrap(), Value::Integer(1));
    }
}
