use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use crate::error::FosterError;
use crate::hir::{Builtin, CaptureMode, FunctionId};

use super::operations::{binary, constant_value, unary};
use super::patterns::matches as match_pattern;
use super::value::{
    AccessLease, FutureValue, PlaceHandle, RemoteArgument, RemoteMessage, RemoteValue, SharedValue,
    Slot, next_future_id, next_remote_id,
};
use super::{Capture, Instruction, Program, Register, Value};

struct Frame {
    function: FunctionId,
    registers: Vec<Rc<Slot>>,
    instruction: usize,
    return_destination: Option<Register>,
    shared_commit: Option<SharedCommit>,
    argument_leases: Vec<AccessLease>,
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
                } => write(
                    frame,
                    destination,
                    binary(operator, &read(frame, left)?, &read(frame, right)?)?,
                )?,
                Instruction::MakeList {
                    destination,
                    elements,
                } => {
                    let values = elements
                        .into_iter()
                        .map(|register| read(frame, register))
                        .collect::<Result<_, _>>()?;
                    write(frame, destination, Value::List(values))?;
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
                        Value::List(values) => values.get(index).cloned(),
                        Value::ByteBuffer(values) => values.get(index).copied().map(Value::Byte),
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
                    let fields = fields
                        .into_iter()
                        .map(|(name, register)| Ok((name, read(frame, register)?)))
                        .collect::<Result<BTreeMap<_, _>, FosterError>>()?;
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
                } => {
                    let value = member(read(frame, object)?, &field, self.program.string_record)?;
                    write(frame, destination, value)?;
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
                        Value::List(values) => {
                            *values
                                .get_mut(index)
                                .ok_or_else(|| FosterError::runtime("index is out of bounds"))? =
                                source;
                        }
                        Value::ByteBuffer(values) => {
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
                    let reference =
                        PlaceHandle::indexed(frame.registers[object.0 as usize].clone(), index)?;
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
                        Value::List(values) => {
                            values.push(value);
                            Ok(())
                        }
                        Value::ByteBuffer(values) => {
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
                    let Value::List(mut values) = read(frame, object)? else {
                        return Err(FosterError::runtime("`append` requires a List"));
                    };
                    values.push(read(frame, value)?);
                    write(frame, destination, Value::List(values))?;
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
                        let Value::ByteBuffer(values) = value else {
                            return Err(FosterError::runtime(
                                "ByteBuffer.freeze requires a ByteBuffer",
                            ));
                        };
                        write(frame, destination, Value::bytes(values))?;
                        continue;
                    }
                    if mutate_byte_buffer(frame, builtin, &arguments)? {
                        write(frame, destination, Value::Unit)?;
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
                    write(
                        frame,
                        destination,
                        Value::VmClosure {
                            function,
                            captures: capture(frame, captures)?,
                        },
                    )?;
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
                    let receiver = frame.registers[receiver.0 as usize].clone();
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
                    let receiver = frame.registers[receiver.0 as usize].clone();
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
                                .argument()
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
            .map(|_| Slot::new(Value::Unit))
            .collect::<Vec<_>>();
        for (index, capture) in captures.into_iter().enumerate() {
            registers[index] = match capture {
                Capture::Value(value) => Slot::new(value),
                Capture::Place(place) => Slot::new(Value::Reference(place)),
            };
        }
        let offset = usize::from(bytecode.captures);
        for (index, argument) in arguments.into_iter().enumerate() {
            registers[offset + index].write(argument)?;
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
        frame.registers[offset] = receiver;
        Ok(frame)
    }

    fn call_frame(
        &self,
        function: FunctionId,
        captures: Vec<Capture>,
        caller: &Frame,
        arguments: &[Register],
        return_destination: Option<Register>,
    ) -> Result<Frame, FosterError> {
        let values = arguments
            .iter()
            .map(|argument| caller.registers[argument.0 as usize].argument())
            .collect();
        let mut frame = self.frame(function, captures, values, return_destination)?;
        let bytecode = &self.program.functions[&function];
        let offset = usize::from(bytecode.captures);
        for (index, (argument, mutable)) in arguments
            .iter()
            .zip(&bytecode.mutable_parameters)
            .enumerate()
        {
            if *mutable {
                frame.registers[offset + index] = caller.registers[argument.0 as usize].clone();
            }
        }
        Ok(frame)
    }

    fn method_call_frame(
        &self,
        function: FunctionId,
        receiver: Rc<Slot>,
        caller: &Frame,
        arguments: &[Register],
        return_destination: Option<Register>,
    ) -> Result<Frame, FosterError> {
        let values = arguments
            .iter()
            .map(|argument| caller.registers[argument.0 as usize].argument())
            .collect::<Vec<_>>();
        let mut frame = self.method_frame(function, receiver, values, return_destination)?;
        let bytecode = &self.program.functions[&function];
        let offset = usize::from(bytecode.captures);
        for (index, (argument, mutable)) in arguments
            .iter()
            .zip(bytecode.mutable_parameters.iter().skip(1))
            .enumerate()
        {
            if *mutable {
                frame.registers[offset + index + 1] = caller.registers[argument.0 as usize].clone();
            }
        }
        Ok(frame)
    }
}

fn drop_register(frame: &mut Frame, register: Register) {
    let slot = &mut frame.registers[register.0 as usize];
    if Rc::strong_count(slot) == 1 && slot.shared().is_none() {
        slot.replace(Value::Unit);
    } else {
        // Detach from observable storage. Writing Unit through a captured or
        // remotely shared slot would change the value seen by another owner.
        *slot = Slot::new(Value::Unit);
    }
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
    frame: &Frame,
    captures: Vec<(CaptureMode, Register)>,
) -> Result<Vec<Capture>, FosterError> {
    captures
        .into_iter()
        .map(|(mode, register)| {
            Ok(match mode {
                CaptureMode::Ref => {
                    Capture::Place(Slot::place(&frame.registers[register.0 as usize]))
                }
                CaptureMode::Move => {
                    Capture::Value(frame.registers[register.0 as usize].replace(Value::Unit))
                }
                CaptureMode::Copy | CaptureMode::Pending => Capture::Value(read(frame, register)?),
            })
        })
        .collect()
}

fn read(frame: &Frame, register: Register) -> Result<Value, FosterError> {
    frame.registers[register.0 as usize].read()
}

fn write(frame: &Frame, register: Register, value: Value) -> Result<(), FosterError> {
    frame.registers[register.0 as usize].write(value)
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
    match (value, field) {
        (Value::Record { fields, .. }, field) => fields
            .get(field)
            .cloned()
            .ok_or_else(|| FosterError::runtime(format!("record has no field `{field}`"))),
        (Value::List(values), "empty?") => Ok(Value::Bool(values.is_empty())),
        (Value::List(values), "length") => Ok(Value::Integer(values.len() as i64)),
        (Value::List(values), "head") => values
            .first()
            .cloned()
            .ok_or_else(|| FosterError::runtime("cannot take `head` of an empty list")),
        (Value::List(values), "rest") => {
            Ok(Value::List(values.get(1..).unwrap_or_default().to_vec()))
        }
        (Value::CodePoint(value), "whitespace?") => Ok(Value::Bool(value.is_whitespace())),
        (Value::CodePoint(value), "string") => {
            Ok(Value::string(string_record, value.to_string().into_bytes()))
        }
        (Value::Byte(value), "int") => Ok(Value::Integer(i64::from(value))),
        (Value::ByteBuffer(values), "empty?") => Ok(Value::Bool(values.is_empty())),
        (Value::ByteBuffer(values), "length") => Ok(Value::Integer(values.len() as i64)),
        (Value::ByteBuffer(values), "capacity") => Ok(Value::Integer(values.capacity() as i64)),
        (_, field) => Err(FosterError::runtime(format!(
            "value has no field `{field}`"
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
        (Builtin::BytesFromList, [Value::List(values)]) => {
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
        (Builtin::BytesToList, [values]) if values.bytes_value().is_some() => Ok(Value::List(
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
                    fields: BTreeMap::from([
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
        (Builtin::ByteBufferEmpty, []) => Ok(Value::ByteBuffer(Vec::new())),
        (Builtin::ByteBufferWithCapacity, [Value::Integer(capacity)]) => {
            let capacity = usize::try_from(*capacity)
                .map_err(|_| FosterError::runtime("ByteBuffer capacity cannot be negative"))?;
            Ok(Value::ByteBuffer(Vec::with_capacity(capacity)))
        }
        (Builtin::ByteBufferFreeze | Builtin::ByteBufferSnapshot, [Value::ByteBuffer(values)]) => {
            Ok(Value::bytes(values.clone()))
        }
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
                Ok(Value::List(names))
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

fn mutate_byte_buffer(
    frame: &Frame,
    builtin: Builtin,
    arguments: &[Register],
) -> Result<bool, FosterError> {
    let mutation = match (builtin, arguments) {
        (Builtin::ByteBufferPush, [buffer, value]) => {
            let Value::Byte(value) = read(frame, *value)? else {
                return Err(FosterError::runtime("ByteBuffer.push requires a Byte"));
            };
            Some((*buffer, ByteBufferMutation::Push(value)))
        }
        (Builtin::ByteBufferExtend, [buffer, values]) => {
            let values = read(frame, *values)?;
            let Some(values) = values.bytes_value() else {
                return Err(FosterError::runtime("ByteBuffer.extend requires Bytes"));
            };
            Some((
                *buffer,
                ByteBufferMutation::Extend(Arc::new(values.to_vec())),
            ))
        }
        (Builtin::ByteBufferClear, [buffer]) => Some((*buffer, ByteBufferMutation::Clear)),
        (Builtin::ByteBufferTruncate, [buffer, length]) => {
            let Value::Integer(length) = read(frame, *length)? else {
                return Err(FosterError::runtime("ByteBuffer.truncate requires Int"));
            };
            let length = usize::try_from(length)
                .map_err(|_| FosterError::runtime("truncate length cannot be negative"))?;
            Some((*buffer, ByteBufferMutation::Truncate(length)))
        }
        (Builtin::ByteBufferReserve, [buffer, additional]) => {
            let Value::Integer(additional) = read(frame, *additional)? else {
                return Err(FosterError::runtime("ByteBuffer.reserve requires Int"));
            };
            let additional = usize::try_from(additional)
                .map_err(|_| FosterError::runtime("reserve amount cannot be negative"))?;
            Some((*buffer, ByteBufferMutation::Reserve(additional)))
        }
        _ => None,
    };
    let Some((buffer, mutation)) = mutation else {
        return Ok(false);
    };
    frame.registers[buffer.0 as usize].reshape(|value| {
        let Value::ByteBuffer(values) = value else {
            return Err(FosterError::runtime(
                "byte-buffer mutation requires ByteBuffer",
            ));
        };
        match mutation {
            ByteBufferMutation::Push(value) => values.push(value),
            ByteBufferMutation::Extend(value) => values.extend_from_slice(&value),
            ByteBufferMutation::Clear => values.clear(),
            ByteBufferMutation::Truncate(length) => values.truncate(length),
            ByteBufferMutation::Reserve(additional) => values.reserve(additional),
        }
        Ok(())
    })?;
    Ok(true)
}

enum ByteBufferMutation {
    Push(u8),
    Extend(Arc<Vec<u8>>),
    Clear,
    Truncate(usize),
    Reserve(usize),
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
            fields: BTreeMap::from([
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
            fields: BTreeMap::from([
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
    Value::Variant {
        variant: None,
        type_name: "Result".into(),
        alternative: "Ok".into(),
        payload: vec![value],
    }
}

fn result_error(error: Value) -> Value {
    Value::Variant {
        variant: None,
        type_name: "Result".into(),
        alternative: "Error".into(),
        payload: vec![error],
    }
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
