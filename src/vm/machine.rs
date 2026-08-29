use std::rc::Rc;
use std::sync::{Arc, Mutex};

use crate::error::{FosterError, RuntimeError};
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

pub struct Machine {
    program: Arc<Program>,
    host: Arc<dyn super::builtins::HostServices>,
}

impl Machine {
    pub fn new(program: &Program) -> Self {
        Self {
            program: Arc::new(program.clone()),
            host: super::builtins::native_host(),
        }
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
            .main
            .ok_or_else(|| RuntimeError::runtime("bytecode has no `main` function"))?;
        let arguments = self
            .program
            .main_arguments
            .then(|| Value::command_arguments(self.program.string_record, arguments))
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
        if definition.parameters != 0 || definition.captures != 0 {
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
                    RuntimeError::runtime(format!("VM function `{}` did not return", function.name))
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
                } => write(frame, destination, bind(frame, source))?,
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
                } => {
                    let values = fields
                        .into_iter()
                        .map(|(_, register)| read(frame, register))
                        .collect::<Result<Vec<_>, RuntimeError>>()?;
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
                        return Err(RuntimeError::runtime("field assignment requires a record"));
                    };
                    *fields.get_mut(&field).ok_or_else(|| {
                        RuntimeError::runtime(format!("record has no field `{field}`"))
                    })? = read(frame, source)?;
                    write(frame, object, value)?;
                }
                Instruction::StoreIndex {
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
                        _ => {
                            return Err(RuntimeError::runtime(
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
                        return Err(RuntimeError::runtime("reference index must be Int"));
                    };
                    let index = usize::try_from(index)
                        .map_err(|_| RuntimeError::runtime("reference index is out of bounds"))?;
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
                Instruction::Append {
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
                            return Err(RuntimeError::runtime(
                                "ByteBuffer.freeze requires one ByteBuffer",
                            ));
                        };
                        let value = frame.registers[buffer.0 as usize].replace(Value::Unit);
                        let Value::RawByteBuffer(values) = value else {
                            return Err(RuntimeError::runtime(
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
                        super::builtins::dispatch(
                            self.host.as_ref(),
                            builtin,
                            &arguments,
                            self.program.string_record,
                        )?,
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
                Instruction::Assert { condition, message } => {
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
                    method,
                    arguments,
                } => {
                    let receiver = place(frame, receiver);
                    let value = receiver.read()?;
                    if let Value::Record { fields, .. } = &value
                        && arguments.is_empty()
                        && let Some(field) = fields.get(&method.name)
                        && value.string_bytes().is_none()
                    {
                        write(frame, destination, field.clone())?;
                        continue;
                    }
                    if value.string_bytes().is_some() {
                        if !arguments.is_empty() {
                            return Err(RuntimeError::runtime(format!(
                                "contract member `{}` does not accept arguments",
                                method.name
                            )));
                        }
                        write(
                            frame,
                            destination,
                            member(value, &method.name, self.program.string_record)?,
                        )?;
                        continue;
                    }
                    if value.bytes_value().is_some() {
                        if !arguments.is_empty() {
                            return Err(RuntimeError::runtime(format!(
                                "contract member `{}` does not accept arguments",
                                method.name
                            )));
                        }
                        write(
                            frame,
                            destination,
                            member(value, &method.name, self.program.string_record)?,
                        )?;
                        continue;
                    }
                    if value.list_value().is_some() {
                        if !arguments.is_empty() {
                            return Err(RuntimeError::runtime(format!(
                                "contract member `{}` does not accept arguments",
                                method.name
                            )));
                        }
                        write(
                            frame,
                            destination,
                            member(value, &method.name, self.program.string_record)?,
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
                                method.name
                            )));
                        }
                        write(
                            frame,
                            destination,
                            member(value, &method.name, self.program.string_record)?,
                        )?;
                        continue;
                    }
                    let function = match &value {
                        Value::Record {
                            record: Some(record),
                            ..
                        } => self
                            .program
                            .methods
                            .get(&(*record, method.clone()))
                            .copied(),
                        Value::Variant {
                            variant: Some(variant),
                            ..
                        } => self
                            .program
                            .variant_methods
                            .get(&(*variant, method.clone()))
                            .copied(),
                        _ => None,
                    };
                    let function = function.ok_or_else(|| {
                        RuntimeError::runtime(format!(
                            "value has no implementation of required method `{}`",
                            method.name
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
                        return Err(RuntimeError::runtime("VM dynamic call requires a closure"));
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
                    let host = self.host.clone();
                    let id = next_remote_id();
                    let _handle = may::go_with!(1024 * 1024, move || {
                        let machine = Machine { program, host };
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
                    let host = self.host.clone();
                    let id = next_remote_id();
                    let _handle = may::go_with!(1024 * 1024, move || {
                        let machine = Machine { program, host };
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
                        return Err(RuntimeError::runtime(
                            "remote call requires a remote object",
                        ));
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
                        .map_err(|_| RuntimeError::runtime("remote object is closed"))?;
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
                        return Err(RuntimeError::runtime("`await` requires a Future"));
                    };
                    let receiver = future
                        .receiver
                        .lock()
                        .map_err(|_| RuntimeError::runtime("future lock was poisoned"))?
                        .take()
                        .ok_or_else(|| RuntimeError::runtime("future has already been awaited"))?;
                    let value = receiver
                        .recv()
                        .map_err(|_| {
                            RuntimeError::runtime("remote object terminated before replying")
                        })?
                        .map_err(RuntimeError::runtime)?;
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
    ) -> Result<Frame, RuntimeError> {
        let bytecode = &self.program.functions[&function];
        if captures.len() != usize::from(bytecode.captures)
            || arguments.len() != usize::from(bytecode.parameters)
        {
            return Err(RuntimeError::runtime(format!(
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
    ) -> Result<Frame, RuntimeError> {
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
    ) -> Result<Frame, RuntimeError> {
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
    ) -> Result<Frame, RuntimeError> {
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
    frame: &mut Frame,
    captures: Vec<(CaptureMode, Register)>,
) -> Result<Vec<Capture>, RuntimeError> {
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

fn read(frame: &Frame, register: Register) -> Result<Value, RuntimeError> {
    frame.registers[register.0 as usize].read()
}

fn bind(frame: &Frame, register: Register) -> Value {
    frame.registers[register.0 as usize].bind()
}

fn write(frame: &mut Frame, register: Register, value: Value) -> Result<(), RuntimeError> {
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
) -> Result<Value, RuntimeError> {
    if let Some(bytes) = value.string_bytes() {
        let text = std::str::from_utf8(bytes)
            .map_err(|_| RuntimeError::runtime("Foster String contains invalid UTF-8"))?;
        return match field {
            "empty?" => Ok(Value::Bool(bytes.is_empty())),
            "length" => Ok(Value::Integer(text.chars().count() as i64)),
            "head" => text
                .chars()
                .next()
                .map(Value::CodePoint)
                .ok_or_else(|| RuntimeError::runtime("cannot take `head` of an empty string")),
            "rest" => {
                let offset = text.chars().next().map_or(0, char::len_utf8);
                Ok(Value::string(string_record, bytes[offset..].to_vec()))
            }
            "whitespace?" => Ok(Value::Bool(text.chars().all(char::is_whitespace))),
            "utf8" | "value" => Ok(Value::bytes(bytes.to_vec())),
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

fn transform_raw_byte_buffer(
    frame: &mut Frame,
    builtin: Builtin,
    arguments: &[Register],
) -> Result<Option<Value>, RuntimeError> {
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
        return Err(RuntimeError::runtime(
            "raw byte-buffer transform requires storage",
        ));
    };
    let Value::RawByteBuffer(mut values) = frame.registers[buffer.0 as usize].replace(Value::Unit)
    else {
        return Err(RuntimeError::runtime(
            "raw byte-buffer transform requires RawByteBuffer",
        ));
    };
    match (builtin, &arguments[1..]) {
        (Builtin::ByteBufferPush, [value]) => {
            let Value::Byte(value) = read(frame, *value)? else {
                return Err(RuntimeError::runtime("ByteBuffer.push requires a Byte"));
            };
            values.push(value);
        }
        (Builtin::ByteBufferExtend, [value]) => {
            let value = read(frame, *value)?;
            let Some(value) = value.bytes_value() else {
                return Err(RuntimeError::runtime("ByteBuffer.extend requires Bytes"));
            };
            values.extend_from_slice(value);
        }
        (Builtin::ByteBufferClear, []) => values.clear(),
        (Builtin::ByteBufferTruncate, [length]) => {
            let Value::Integer(length) = read(frame, *length)? else {
                return Err(RuntimeError::runtime("ByteBuffer.truncate requires Int"));
            };
            values.truncate(
                usize::try_from(length)
                    .map_err(|_| RuntimeError::runtime("truncate length cannot be negative"))?,
            );
        }
        (Builtin::ByteBufferReserve, [additional]) => {
            let Value::Integer(additional) = read(frame, *additional)? else {
                return Err(RuntimeError::runtime("ByteBuffer.reserve requires Int"));
            };
            values.reserve(
                usize::try_from(additional)
                    .map_err(|_| RuntimeError::runtime("reserve amount cannot be negative"))?,
            );
        }
        _ => return Err(RuntimeError::runtime("invalid raw byte-buffer arguments")),
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
