use std::rc::Rc;
use std::sync::{Arc, Mutex, OnceLock};

use crate::error::FosterError;
use crate::hir::{Builtin, CaptureMode, FunctionId};

use super::operations::{binary, constant_value, unary};
use super::patterns::matches as match_pattern;
use super::value::{
    AccessLease, FutureValue, PlaceHandle, RecordFields, RemoteArgument, RemoteMessage,
    RemoteValue, SharedValue, Slot, next_future_id, next_remote_id,
};
use super::{Capture, Instruction, Program, Register, Value};

struct Frame {
    function: FunctionId,
    registers: Vec<RegisterCell>,
    instruction: usize,
    return_destination: Option<Register>,
    shared_commit: Option<SharedCommit>,
    argument_leases: Vec<AccessLease>,
}

enum RegisterCell {
    Inline(Value),
    /// A place whose writes remain observable by its owner.
    Place(Rc<Slot>),
    /// A borrowed place. Reads observe the owner, while assignment detaches the local.
    Borrowed(Rc<Slot>),
}

impl RegisterCell {
    fn read(&self) -> Result<Value, FosterError> {
        match self {
            Self::Inline(Value::Reference(place)) => place.read(),
            Self::Inline(value) => Ok(value.clone()),
            Self::Place(slot) | Self::Borrowed(slot) => slot.read(),
        }
    }

    fn reference(&self) -> Option<PlaceHandle> {
        match self {
            Self::Inline(Value::Reference(reference)) => Some(reference.clone()),
            Self::Inline(_) => None,
            Self::Place(slot) | Self::Borrowed(slot) => slot.reference(),
        }
    }

    fn write(&mut self, value: Value) -> Result<(), FosterError> {
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
        update: impl FnOnce(&mut Value) -> Result<(), FosterError>,
    ) -> Result<(), FosterError> {
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

    fn share(&mut self) -> Result<Arc<SharedValue>, FosterError> {
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

pub struct Machine {
    program: Arc<Program>,
}

impl Machine {
    pub fn new(program: &Program) -> Self {
        Self {
            program: Arc::new(program.clone()),
        }
    }

    pub fn run_main(&self) -> Result<Value, FosterError> {
        let main = self
            .program
            .main
            .ok_or_else(|| FosterError::runtime("bytecode has no `main` function"))?;
        self.execute(main, Vec::new(), Vec::new(), None)
            .map(|result| result.0)
    }

    /// Executes a compiled zero-argument function in a fresh VM frame.
    pub fn run_function(&self, function: FunctionId) -> Result<Value, FosterError> {
        let definition = self
            .program
            .functions
            .get(&function)
            .ok_or_else(|| FosterError::runtime("bytecode references an unknown function"))?;
        if definition.parameters != 0 || definition.captures != 0 {
            return Err(FosterError::runtime(format!(
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
    ) -> Result<(Value, Option<Value>), FosterError> {
        self.execute_with_leases(entry, captures, arguments, receiver, Vec::new())
    }

    fn execute_with_leases(
        &self,
        entry: FunctionId,
        captures: Vec<Capture>,
        arguments: Vec<Value>,
        receiver: Option<Rc<Slot>>,
        argument_leases: Vec<AccessLease>,
    ) -> Result<(Value, Option<Value>), FosterError> {
        let mut frames = vec![if let Some(receiver) = receiver.clone() {
            self.method_frame(entry, receiver, arguments, None)?
        } else {
            self.frame(entry, captures, arguments, None)?
        }];
        frames[0].argument_leases = argument_leases;

        loop {
            let frame = frames.last_mut().expect("the VM retains its entry frame");
            let function = &self.program.functions[&frame.function];
            let instruction = function
                .instructions
                .get(frame.instruction)
                .cloned()
                .ok_or_else(|| {
                    FosterError::runtime(format!("VM function `{}` did not return", function.name))
                })?;
            frame.instruction += 1;

            match instruction {
                Instruction::Drop { register } => {
                    drop_register(frame, register);
                }
                Instruction::LoadConstant {
                    destination,
                    constant,
                } => write(
                    frame,
                    destination,
                    constant_value(
                        &self.program.constants[constant as usize],
                        self.program.string_record,
                        self.program.symbol_record,
                    ),
                )?,
                Instruction::Move {
                    destination,
                    source,
                } => write(frame, destination, read(frame, source)?)?,
                Instruction::Unary {
                    destination,
                    operator,
                    operand,
                } => write(frame, destination, unary(operator, &read(frame, operand)?)?)?,
                Instruction::Binary {
                    destination,
                    operator,
                    left,
                    right,
                } => {
                    let left_value = read(frame, left)?;
                    let right_value = read(frame, right)?;
                    let value = binary(operator, &left_value, &right_value).map_err(|error| {
                        FosterError::runtime(format!(
                            "{} in `{}` with {left_value:?} and {right_value:?}",
                            error.message, function.name
                        ))
                    })?;
                    write(frame, destination, value)?;
                }
                Instruction::MakeList {
                    destination,
                    elements,
                } => {
                    let values = elements
                        .into_iter()
                        .map(|register| read(frame, register))
                        .collect::<Result<_, _>>()?;
                    write(frame, destination, Value::list(values))?;
                }
                Instruction::Index {
                    destination,
                    object,
                    index,
                } => {
                    let Value::Integer(index) = read(frame, index)? else {
                        return Err(FosterError::runtime("VM list index is not an integer"));
                    };
                    let index = usize::try_from(index)
                        .map_err(|_| FosterError::runtime("index is out of bounds"))?;
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
                        value if value.bytes_value().is_some() => value
                            .bytes_value()
                            .unwrap()
                            .get(index)
                            .copied()
                            .map(Value::Byte),
                        _ => return Err(FosterError::runtime("value does not support indexing")),
                    }
                    .ok_or_else(|| FosterError::runtime("index is out of bounds"))?;
                    write(frame, destination, value)?;
                }
                Instruction::MakeRecord {
                    destination,
                    record,
                    fields,
                } => {
                    let values = fields
                        .into_iter()
                        .map(|(_, register)| read(frame, register))
                        .collect::<Result<Vec<_>, FosterError>>()?;
                    let fields =
                        RecordFields::new(self.program.record_layouts[&record].clone(), values)?;
                    write(
                        frame,
                        destination,
                        Value::Record {
                            record: Some(record),
                            name: self.program.records[&record].clone(),
                            fields,
                        },
                    )?;
                }
                Instruction::MakeVariant {
                    destination,
                    variant,
                    payload,
                } => {
                    let (variant_type, type_name, alternative) = &self.program.variants[&variant];
                    let payload = payload
                        .into_iter()
                        .map(|register| read(frame, register))
                        .collect::<Result<_, _>>()?;
                    write(
                        frame,
                        destination,
                        Value::Variant {
                            variant: Some(*variant_type),
                            type_name: type_name.clone(),
                            alternative: alternative.clone(),
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
                    let value = read(frame, object)?;
                    if by_reference
                        && matches!(&value, Value::Record { fields, .. } if fields.contains_key(&field))
                    {
                        let reference = PlaceHandle::field(place(frame, object), field)?;
                        write(frame, destination, Value::Reference(reference))?;
                    } else {
                        let value = member(value, &field, self.program.string_record)?;
                        write(frame, destination, value)?;
                    }
                }
                Instruction::StoreField {
                    object,
                    field,
                    source,
                } => {
                    let mut value = read(frame, object)?;
                    let Value::Record { fields, .. } = &mut value else {
                        return Err(FosterError::runtime("field assignment requires a record"));
                    };
                    *fields.get_mut(&field).ok_or_else(|| {
                        FosterError::runtime(format!("record has no field `{field}`"))
                    })? = read(frame, source)?;
                    write(frame, object, value)?;
                }
                Instruction::StoreIndex {
                    object,
                    index,
                    source,
                } => {
                    let Value::Integer(index) = read(frame, index)? else {
                        return Err(FosterError::runtime("list index is not an integer"));
                    };
                    let mut value = read(frame, object)?;
                    let index = usize::try_from(index)
                        .map_err(|_| FosterError::runtime("index is out of bounds"))?;
                    let source = read(frame, source)?;
                    match &mut value {
                        value if value.list_value().is_some() => {
                            let values = value.list_value_mut().unwrap();
                            *values
                                .get_mut(index)
                                .ok_or_else(|| FosterError::runtime("index is out of bounds"))? =
                                source;
                        }
                        receiver if receiver.byte_buffer_value().is_some() => {
                            let values = receiver.byte_buffer_value_mut().unwrap();
                            let Value::Byte(source) = source else {
                                return Err(FosterError::runtime(
                                    "byte-buffer elements require Byte values",
                                ));
                            };
                            *values
                                .get_mut(index)
                                .ok_or_else(|| FosterError::runtime("index is out of bounds"))? =
                                source;
                        }
                        _ => {
                            return Err(FosterError::runtime(
                                "indexed assignment requires mutable indexed storage",
                            ));
                        }
                    }
                    write(frame, object, value)?;
                }
                Instruction::MakeReference {
                    destination,
                    object,
                    index,
                } => {
                    let Value::Integer(index) = read(frame, index)? else {
                        return Err(FosterError::runtime("reference index must be Int"));
                    };
                    let index = usize::try_from(index)
                        .map_err(|_| FosterError::runtime("reference index is out of bounds"))?;
                    let reference = PlaceHandle::indexed(place(frame, object), index)?;
                    write(frame, destination, Value::Reference(reference))?;
                }
                Instruction::MakeFieldReference {
                    destination,
                    object,
                    field,
                } => {
                    let reference = PlaceHandle::field(place(frame, object), field)?;
                    write(frame, destination, Value::Reference(reference))?;
                }
                Instruction::MoveOut {
                    destination,
                    source,
                } => {
                    let value = frame.registers[source.0 as usize].replace(Value::Unit);
                    write(frame, destination, value)?;
                }
                Instruction::Push {
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
                                return Err(FosterError::runtime(
                                    "ByteBuffer.push requires a Byte",
                                ));
                            };
                            values.push(value);
                            Ok(())
                        }
                        _ => Err(FosterError::runtime("`push` requires a List or ByteBuffer")),
                    })?;
                    write(frame, destination, Value::Unit)?;
                }
                Instruction::Append {
                    destination,
                    object,
                    value,
                } => {
                    let mut list = read(frame, object)?;
                    let Some(values) = list.list_value_mut() else {
                        return Err(FosterError::runtime("`append` requires a List"));
                    };
                    values.push(read(frame, value)?);
                    write(frame, destination, list)?;
                }
                Instruction::Contains {
                    destination,
                    value,
                    candidates,
                } => {
                    let value = read(frame, value)?;
                    let found = candidates
                        .into_iter()
                        .map(|candidate| read(frame, candidate))
                        .collect::<Result<Vec<_>, _>>()?
                        .contains(&value);
                    write(frame, destination, Value::Bool(found))?;
                }
                Instruction::Builtin {
                    destination,
                    builtin,
                    arguments,
                } => {
                    if builtin == Builtin::ByteBufferFreeze {
                        let [buffer] = arguments.as_slice() else {
                            return Err(FosterError::runtime(
                                "ByteBuffer.freeze requires one ByteBuffer",
                            ));
                        };
                        let value = frame.registers[buffer.0 as usize].replace(Value::Unit);
                        let Value::RawByteBuffer(values) = value else {
                            return Err(FosterError::runtime(
                                "ByteBuffer.freeze requires a ByteBuffer",
                            ));
                        };
                        write(frame, destination, Value::bytes(values))?;
                        continue;
                    }
                    if let Some(value) = transform_raw_byte_buffer(frame, builtin, &arguments)? {
                        write(frame, destination, value)?;
                        continue;
                    }
                    let arguments = arguments
                        .into_iter()
                        .map(|argument| read(frame, argument))
                        .collect::<Result<Vec<_>, _>>()?;
                    write(
                        frame,
                        destination,
                        call_builtin(builtin, &arguments, self.program.string_record)?,
                    )?;
                }
                Instruction::MatchPattern {
                    destination,
                    subject,
                    pattern,
                    bindings,
                } => {
                    let mut values = Vec::new();
                    let matched =
                        match_pattern(&self.program, &pattern, &read(frame, subject)?, &mut values);
                    if matched {
                        for (register, value) in bindings.into_iter().zip(values) {
                            write(frame, register, value)?;
                        }
                    }
                    write(frame, destination, Value::Bool(matched))?;
                }
                Instruction::Jump { target } => frame.instruction = target,
                Instruction::JumpIfFalse { condition, target } => {
                    if read(frame, condition)? == Value::Bool(false) {
                        frame.instruction = target;
                    }
                }
                Instruction::MakeClosure {
                    destination,
                    function,
                    captures,
                } => {
                    let captures = capture(frame, captures)?;
                    write(frame, destination, Value::VmClosure { function, captures })?;
                }
                Instruction::Call {
                    destination,
                    function,
                    arguments,
                } => {
                    let next = self.call_frame(
                        function,
                        Vec::new(),
                        frame,
                        &arguments,
                        Some(destination),
                    )?;
                    frames.push(next);
                }
                Instruction::CallMethod {
                    destination,
                    receiver,
                    function,
                    arguments,
                } => {
                    let receiver = place(frame, receiver);
                    if let Some(shared) = receiver.shared() {
                        let (lease, state) = shared.write_snapshot()?;
                        let local = Slot::new(Value::from_wire(state)?);
                        let mut method = self.method_call_frame(
                            function,
                            local.clone(),
                            frame,
                            &arguments,
                            Some(destination),
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
                            &arguments,
                            Some(destination),
                        )?;
                        frames.push(next);
                    }
                }
                Instruction::CallContractMethod {
                    destination,
                    receiver,
                    name,
                    arguments,
                } => {
                    let receiver = place(frame, receiver);
                    let value = receiver.read()?;
                    if let Value::Record { fields, .. } = &value
                        && arguments.is_empty()
                        && let Some(field) = fields.get(&name)
                        && value.string_bytes().is_none()
                    {
                        write(frame, destination, field.clone())?;
                        continue;
                    }
                    if value.string_bytes().is_some() {
                        if !arguments.is_empty() {
                            return Err(FosterError::runtime(format!(
                                "contract member `{name}` does not accept arguments"
                            )));
                        }
                        write(
                            frame,
                            destination,
                            member(value, &name, self.program.string_record)?,
                        )?;
                        continue;
                    }
                    if value.bytes_value().is_some() {
                        if !arguments.is_empty() {
                            return Err(FosterError::runtime(format!(
                                "contract member `{name}` does not accept arguments"
                            )));
                        }
                        write(
                            frame,
                            destination,
                            member(value, &name, self.program.string_record)?,
                        )?;
                        continue;
                    }
                    if value.list_value().is_some() {
                        if !arguments.is_empty() {
                            return Err(FosterError::runtime(format!(
                                "contract member `{name}` does not accept arguments"
                            )));
                        }
                        write(
                            frame,
                            destination,
                            member(value, &name, self.program.string_record)?,
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
                            return Err(FosterError::runtime(format!(
                                "contract member `{name}` does not accept arguments"
                            )));
                        }
                        write(
                            frame,
                            destination,
                            member(value, &name, self.program.string_record)?,
                        )?;
                        continue;
                    }
                    let function = match &value {
                        Value::Record {
                            record: Some(record),
                            ..
                        } => self.program.methods.get(&(*record, name.clone())).copied(),
                        Value::Variant {
                            variant: Some(variant),
                            ..
                        } => self
                            .program
                            .variant_methods
                            .get(&(*variant, name.clone()))
                            .copied(),
                        _ => None,
                    };
                    let function = function.ok_or_else(|| {
                        FosterError::runtime(format!(
                            "value has no implementation of required method `{name}`"
                        ))
                    })?;
                    if let Some(shared) = receiver.shared() {
                        let (lease, state) = shared.write_snapshot()?;
                        let local = Slot::new(Value::from_wire(state)?);
                        let mut method = self.method_call_frame(
                            function,
                            local.clone(),
                            frame,
                            &arguments,
                            Some(destination),
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
                            &arguments,
                            Some(destination),
                        )?;
                        frames.push(next);
                    }
                }
                Instruction::CallValue {
                    destination,
                    callee,
                    arguments,
                } => {
                    let Value::VmClosure { function, captures } = read(frame, callee)? else {
                        return Err(FosterError::runtime("VM dynamic call requires a closure"));
                    };
                    let next =
                        self.call_frame(function, captures, frame, &arguments, Some(destination))?;
                    frames.push(next);
                }
                Instruction::CallClosure {
                    destination,
                    function,
                    captures,
                    arguments,
                } => {
                    let captures = capture(frame, captures)?;
                    let next =
                        self.call_frame(function, captures, frame, &arguments, Some(destination))?;
                    frames.push(next);
                }
                Instruction::SpawnRemote { destination, value } => {
                    let state = read(frame, value)?.into_wire()?;
                    let (sender, inbox) = may::sync::mpsc::channel::<RemoteMessage>();
                    let program = self.program.clone();
                    let id = next_remote_id();
                    let _handle = may::go_with!(1024 * 1024, move || {
                        let machine = Machine { program };
                        let mut state = state;
                        while let Ok(message) = inbox.recv() {
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
                            let _ = message.response.send(result);
                        }
                    });
                    write(
                        frame,
                        destination,
                        Value::Remote(RemoteValue { id, sender }),
                    )?;
                }
                Instruction::SpawnRemoteBorrow {
                    destination,
                    source,
                } => {
                    let shared = frame.registers[source.0 as usize].share()?;
                    let (sender, inbox) = may::sync::mpsc::channel::<RemoteMessage>();
                    let program = self.program.clone();
                    let id = next_remote_id();
                    let _handle = may::go_with!(1024 * 1024, move || {
                        let machine = Machine { program };
                        while let Ok(message) = inbox.recv() {
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
                            let _ = message.response.send(result);
                        }
                    });
                    write(
                        frame,
                        destination,
                        Value::Remote(RemoteValue { id, sender }),
                    )?;
                }
                Instruction::RemoteCall {
                    destination,
                    remote,
                    function,
                    arguments,
                } => {
                    let Value::Remote(remote) = read(frame, remote)? else {
                        return Err(FosterError::runtime("remote call requires a remote object"));
                    };
                    let arguments = arguments
                        .into_iter()
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
                    remote
                        .sender
                        .send(RemoteMessage {
                            function,
                            arguments,
                            response: sender,
                        })
                        .map_err(|_| FosterError::runtime("remote object is closed"))?;
                    write(
                        frame,
                        destination,
                        Value::Future(FutureValue {
                            id: next_future_id(),
                            receiver: Arc::new(Mutex::new(Some(receiver))),
                        }),
                    )?;
                }
                Instruction::Await {
                    destination,
                    future,
                } => {
                    let Value::Future(future) = read(frame, future)? else {
                        return Err(FosterError::runtime("`await` requires a Future"));
                    };
                    let receiver = future
                        .receiver
                        .lock()
                        .map_err(|_| FosterError::runtime("future lock was poisoned"))?
                        .take()
                        .ok_or_else(|| FosterError::runtime("future has already been awaited"))?;
                    let value = receiver
                        .recv()
                        .map_err(|_| {
                            FosterError::runtime("remote object terminated before replying")
                        })?
                        .map_err(FosterError::runtime)?;
                    write(frame, destination, Value::from_wire(value)?)?;
                }
                Instruction::Return { source } => {
                    let value = read(frame, source)?;
                    let completed = frames.pop().expect("return has a frame");
                    if let Some(commit) = completed.shared_commit {
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

    fn frame(
        &self,
        function: FunctionId,
        captures: Vec<Capture>,
        arguments: Vec<Value>,
        return_destination: Option<Register>,
    ) -> Result<Frame, FosterError> {
        let bytecode = &self.program.functions[&function];
        if captures.len() != usize::from(bytecode.captures)
            || arguments.len() != usize::from(bytecode.parameters)
        {
            return Err(FosterError::runtime(format!(
                "VM call to `{}` has an invalid capture or parameter layout (expected {}/{}, received {}/{})",
                bytecode.name,
                bytecode.captures,
                bytecode.parameters,
                captures.len(),
                arguments.len()
            )));
        }
        let mut registers = (0..bytecode.registers)
            .map(|_| RegisterCell::Inline(Value::Unit))
            .collect::<Vec<_>>();
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
            function,
            registers,
            instruction: 0,
            return_destination,
            shared_commit: None,
            argument_leases: Vec::new(),
        })
    }

    fn method_frame(
        &self,
        function: FunctionId,
        receiver: Rc<Slot>,
        arguments: Vec<Value>,
        return_destination: Option<Register>,
    ) -> Result<Frame, FosterError> {
        let mut frame = self.frame(
            function,
            Vec::new(),
            std::iter::once(Value::Unit).chain(arguments).collect(),
            return_destination,
        )?;
        let offset = usize::from(self.program.functions[&function].captures);
        frame.registers[offset] = RegisterCell::Place(receiver);
        Ok(frame)
    }

    fn call_frame(
        &self,
        function: FunctionId,
        captures: Vec<Capture>,
        caller: &mut Frame,
        arguments: &[Register],
        return_destination: Option<Register>,
    ) -> Result<Frame, FosterError> {
        let bytecode = &self.program.functions[&function];
        let mut frame = self.frame(
            function,
            captures,
            vec![Value::Unit; arguments.len()],
            return_destination,
        )?;
        let offset = usize::from(bytecode.captures);
        for (index, argument) in arguments.iter().copied().enumerate() {
            frame.registers[offset + index] = match bytecode.parameter_modes[index] {
                crate::ast::ParameterMode::Consume => RegisterCell::Inline(take(caller, argument)),
                crate::ast::ParameterMode::Borrow if bytecode.mutable_parameters[index] => {
                    RegisterCell::Place(place(caller, argument))
                }
                crate::ast::ParameterMode::Borrow => borrow_parameter(caller, argument),
            };
        }
        Ok(frame)
    }

    fn method_call_frame(
        &self,
        function: FunctionId,
        receiver: Rc<Slot>,
        caller: &mut Frame,
        arguments: &[Register],
        return_destination: Option<Register>,
    ) -> Result<Frame, FosterError> {
        let bytecode = &self.program.functions[&function];
        let mut frame = self.method_frame(
            function,
            receiver,
            vec![Value::Unit; arguments.len()],
            return_destination,
        )?;
        let offset = usize::from(bytecode.captures);
        for (index, argument) in arguments.iter().copied().enumerate() {
            let parameter = index + 1;
            frame.registers[offset + parameter] = match bytecode.parameter_modes[parameter] {
                crate::ast::ParameterMode::Consume => RegisterCell::Inline(take(caller, argument)),
                crate::ast::ParameterMode::Borrow if bytecode.mutable_parameters[parameter] => {
                    RegisterCell::Place(place(caller, argument))
                }
                crate::ast::ParameterMode::Borrow => borrow_parameter(caller, argument),
            };
        }
        Ok(frame)
    }
}

fn drop_register(frame: &mut Frame, register: Register) {
    frame.registers[register.0 as usize].detach();
}

fn materialize_remote_arguments(
    arguments: Vec<RemoteArgument>,
) -> Result<(Vec<Value>, Vec<AccessLease>), FosterError> {
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
    frame: &mut Frame,
    captures: Vec<(CaptureMode, Register)>,
) -> Result<Vec<Capture>, FosterError> {
    captures
        .into_iter()
        .map(|(mode, register)| {
            Ok(match mode {
                CaptureMode::Ref => Capture::Place(Slot::place(&place(frame, register))),
                CaptureMode::Move => Capture::Value(take(frame, register)),
                CaptureMode::Copy | CaptureMode::Pending => Capture::Value(read(frame, register)?),
            })
        })
        .collect()
}

fn read(frame: &Frame, register: Register) -> Result<Value, FosterError> {
    frame.registers[register.0 as usize].read()
}

fn write(frame: &mut Frame, register: Register, value: Value) -> Result<(), FosterError> {
    frame.registers[register.0 as usize].write(value)
}

fn place(frame: &mut Frame, register: Register) -> Rc<Slot> {
    frame.registers[register.0 as usize].promote()
}

fn take(frame: &mut Frame, register: Register) -> Value {
    frame.registers[register.0 as usize].take()
}

fn borrow_parameter(frame: &mut Frame, register: Register) -> RegisterCell {
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
) -> Result<Value, FosterError> {
    if let Some(bytes) = value.string_bytes() {
        let text = std::str::from_utf8(bytes)
            .map_err(|_| FosterError::runtime("Foster String contains invalid UTF-8"))?;
        return match field {
            "empty?" => Ok(Value::Bool(bytes.is_empty())),
            "length" => Ok(Value::Integer(text.chars().count() as i64)),
            "head" => text
                .chars()
                .next()
                .map(Value::CodePoint)
                .ok_or_else(|| FosterError::runtime("cannot take `head` of an empty string")),
            "rest" => {
                let offset = text.chars().next().map_or(0, char::len_utf8);
                Ok(Value::string(string_record, bytes[offset..].to_vec()))
            }
            "whitespace?" => Ok(Value::Bool(text.chars().all(char::is_whitespace))),
            "utf8" | "value" => Ok(Value::bytes(bytes.to_vec())),
            _ => Err(FosterError::runtime(format!(
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
                .ok_or_else(|| FosterError::runtime("cannot take `head` of empty Bytes")),
            "rest" => Ok(Value::bytes(values.get(1..).unwrap_or_default().to_vec())),
            _ => Err(FosterError::runtime(format!(
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
            _ => Err(FosterError::runtime(format!(
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
                .ok_or_else(|| FosterError::runtime("cannot take `head` of an empty list")),
            "rest" => Ok(Value::list(values.get(1..).unwrap_or_default().to_vec())),
            _ => Err(FosterError::runtime(format!(
                "value has no field `{field}`"
            ))),
        };
    }
    match (value, field) {
        (Value::Record { fields, .. }, field) => fields
            .get(field)
            .cloned()
            .ok_or_else(|| FosterError::runtime(format!("record has no field `{field}`"))),
        (Value::CodePoint(value), "whitespace?") => Ok(Value::Bool(value.is_whitespace())),
        (Value::CodePoint(value), "string") => {
            Ok(Value::string(string_record, value.to_string().into_bytes()))
        }
        (Value::Byte(value), "int") => Ok(Value::Integer(i64::from(value))),
        (value, field) => Err(FosterError::runtime(format!(
            "value {value:?} has no field `{field}`"
        ))),
    }
}

fn call_builtin(
    builtin: Builtin,
    arguments: &[Value],
    string_record: Option<crate::hir::RecordId>,
) -> Result<Value, FosterError> {
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
            .ok_or_else(|| FosterError::runtime("invalid Unicode scalar value")),
        (Builtin::ParseFloat, [value]) if value.string_bytes().is_some() => value
            .string_text()?
            .parse::<f64>()
            .map(Value::Float)
            .map_err(|_| FosterError::runtime("invalid Float text")),
        (Builtin::ByteValid, [Value::Integer(value)]) => {
            Ok(Value::Bool(u8::try_from(*value).is_ok()))
        }
        (Builtin::ByteUnchecked, [Value::Integer(value)]) => u8::try_from(*value)
            .map(Value::Byte)
            .map_err(|_| FosterError::runtime("Byte is outside 0..255")),
        (Builtin::BytesEmpty, []) => Ok(Value::bytes(Vec::new())),
        (Builtin::BytesFromList, [value]) if value.list_value().is_some() => {
            let values = value.list_value().unwrap();
            let bytes = values
                .iter()
                .map(|value| match value {
                    Value::Byte(value) => Ok(*value),
                    _ => Err(FosterError::runtime("Bytes.from requires List<Byte>")),
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
                .map_err(|_| FosterError::runtime("byte slice start is out of bounds"))?;
            let end = usize::try_from(*end)
                .map_err(|_| FosterError::runtime("byte slice end is out of bounds"))?;
            let slice = values
                .get(start..end)
                .ok_or_else(|| FosterError::runtime("byte slice is out of bounds"))?;
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
                .map_err(|_| FosterError::runtime("Bytes are not valid UTF-8"))
        }
        (Builtin::ByteBufferEmpty, []) => Ok(Value::RawByteBuffer(Vec::new())),
        (Builtin::ByteBufferWithCapacity, [Value::Integer(capacity)]) => {
            let capacity = usize::try_from(*capacity)
                .map_err(|_| FosterError::runtime("ByteBuffer capacity cannot be negative"))?;
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
                .map_err(|_| FosterError::runtime("truncate length cannot be negative"))?;
            let mut buffer = buffer.clone();
            buffer.truncate(length);
            Ok(Value::RawByteBuffer(buffer))
        }
        (
            Builtin::ByteBufferReserve,
            [Value::RawByteBuffer(buffer), Value::Integer(additional)],
        ) => {
            let additional = usize::try_from(*additional)
                .map_err(|_| FosterError::runtime("reserve amount cannot be negative"))?;
            let mut buffer = buffer.clone();
            buffer.reserve(additional);
            Ok(Value::RawByteBuffer(buffer))
        }
        (
            Builtin::ByteBufferFreeze | Builtin::ByteBufferSnapshot,
            [Value::RawByteBuffer(value)],
        ) => Ok(Value::bytes(value.clone())),
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
        (Builtin::IoParent, [path]) if path.string_bytes().is_some() => optional_path_component(
            std::path::Path::new(path.string_text()?)
                .parent()
                .map(std::path::Path::to_path_buf),
            string_record,
        ),
        (Builtin::IoFileName, [path]) if path.string_bytes().is_some() => optional_os_component(
            std::path::Path::new(path.string_text()?).file_name(),
            string_record,
        ),
        (Builtin::IoExtension, [path]) if path.string_bytes().is_some() => optional_os_component(
            std::path::Path::new(path.string_text()?).extension(),
            string_record,
        ),
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
        (Builtin::TcpSetTimeout, [Value::Integer(connection), Value::Integer(milliseconds)]) => {
            Ok(tcp_result(
                "set_timeout",
                super::host::set_timeout(*connection, *milliseconds).map(|()| Value::Unit),
                string_record,
            ))
        }
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
        _ => Err(FosterError::runtime("invalid builtin arguments")),
    }
}

fn transform_raw_byte_buffer(
    frame: &mut Frame,
    builtin: Builtin,
    arguments: &[Register],
) -> Result<Option<Value>, FosterError> {
    if !matches!(
        builtin,
        Builtin::ByteBufferPush
            | Builtin::ByteBufferExtend
            | Builtin::ByteBufferClear
            | Builtin::ByteBufferTruncate
            | Builtin::ByteBufferReserve
    ) {
        return Ok(None);
    }
    let Some(buffer) = arguments.first() else {
        return Err(FosterError::runtime(
            "raw byte-buffer transform requires storage",
        ));
    };
    let Value::RawByteBuffer(mut values) = frame.registers[buffer.0 as usize].replace(Value::Unit)
    else {
        return Err(FosterError::runtime(
            "raw byte-buffer transform requires RawByteBuffer",
        ));
    };
    match (builtin, &arguments[1..]) {
        (Builtin::ByteBufferPush, [value]) => {
            let Value::Byte(value) = read(frame, *value)? else {
                return Err(FosterError::runtime("ByteBuffer.push requires a Byte"));
            };
            values.push(value);
        }
        (Builtin::ByteBufferExtend, [value]) => {
            let value = read(frame, *value)?;
            let Some(value) = value.bytes_value() else {
                return Err(FosterError::runtime("ByteBuffer.extend requires Bytes"));
            };
            values.extend_from_slice(value);
        }
        (Builtin::ByteBufferClear, []) => values.clear(),
        (Builtin::ByteBufferTruncate, [length]) => {
            let Value::Integer(length) = read(frame, *length)? else {
                return Err(FosterError::runtime("ByteBuffer.truncate requires Int"));
            };
            values.truncate(
                usize::try_from(length)
                    .map_err(|_| FosterError::runtime("truncate length cannot be negative"))?,
            );
        }
        (Builtin::ByteBufferReserve, [additional]) => {
            let Value::Integer(additional) = read(frame, *additional)? else {
                return Err(FosterError::runtime("ByteBuffer.reserve requires Int"));
            };
            values.reserve(
                usize::try_from(additional)
                    .map_err(|_| FosterError::runtime("reserve amount cannot be negative"))?,
            );
        }
        _ => return Err(FosterError::runtime("invalid raw byte-buffer arguments")),
    }
    Ok(Some(Value::RawByteBuffer(values)))
}

#[cfg(test)]
mod register_storage_tests {
    use super::*;

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
) -> Result<Value, FosterError> {
    path.into_os_string()
        .into_string()
        .map(|value| Value::string(string_record, value.into_bytes()))
        .map_err(|_| FosterError::runtime("path is not valid UTF-8"))
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
) -> Result<Value, FosterError> {
    match path {
        Some(path) => path_value(path, string_record),
        None => Ok(Value::string(string_record, Vec::new())),
    }
}

fn optional_os_component(
    value: Option<&std::ffi::OsStr>,
    string_record: Option<crate::hir::RecordId>,
) -> Result<Value, FosterError> {
    match value {
        Some(value) => value
            .to_str()
            .map(|value| Value::string(string_record, value.as_bytes().to_vec()))
            .ok_or_else(|| FosterError::runtime("path component is not valid UTF-8")),
        None => Ok(Value::string(string_record, Vec::new())),
    }
}
