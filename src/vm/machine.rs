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
                    constant_value(&self.program.constants[constant as usize]),
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
                    let Value::List(values) = read(frame, object)? else {
                        return Err(FosterError::runtime("VM indexing requires a list"));
                    };
                    let index = usize::try_from(index)
                        .map_err(|_| FosterError::runtime("list index is out of bounds"))?;
                    let value = values
                        .get(index)
                        .cloned()
                        .ok_or_else(|| FosterError::runtime("list index is out of bounds"))?;
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
                    let (type_name, alternative) = &self.program.variants[&variant];
                    let payload = payload
                        .into_iter()
                        .map(|register| read(frame, register))
                        .collect::<Result<_, _>>()?;
                    write(
                        frame,
                        destination,
                        Value::Variant {
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
                    let value = member(read(frame, object)?, &field)?;
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
                    let Value::List(values) = &mut value else {
                        return Err(FosterError::runtime("indexed assignment requires a list"));
                    };
                    let index = usize::try_from(index)
                        .map_err(|_| FosterError::runtime("list index is out of bounds"))?;
                    *values
                        .get_mut(index)
                        .ok_or_else(|| FosterError::runtime("list index is out of bounds"))? =
                        read(frame, source)?;
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
                        _ => Err(FosterError::runtime("`push` requires a List")),
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
                    let arguments = arguments
                        .into_iter()
                        .map(|argument| read(frame, argument))
                        .collect::<Result<Vec<_>, _>>()?;
                    write(frame, destination, call_builtin(builtin, &arguments)?)?;
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
                    let arguments = values(frame, arguments)?;
                    frames.push(self.frame(function, Vec::new(), arguments, Some(destination))?);
                }
                Instruction::CallMethod {
                    destination,
                    receiver,
                    function,
                    arguments,
                } => {
                    let arguments = values(frame, arguments)?;
                    let receiver = frame.registers[receiver.0 as usize].clone();
                    if let Some(shared) = receiver.shared() {
                        let (lease, state) = shared.write_snapshot()?;
                        let local = Slot::new(Value::from_wire(state)?);
                        let mut method = self.method_frame(
                            function,
                            local.clone(),
                            arguments,
                            Some(destination),
                        )?;
                        method.shared_commit = Some(SharedCommit {
                            shared,
                            receiver: local,
                            _lease: lease,
                        });
                        frames.push(method);
                    } else {
                        frames.push(self.method_frame(
                            function,
                            receiver,
                            arguments,
                            Some(destination),
                        )?);
                    }
                }
                Instruction::CallContractMethod {
                    destination,
                    receiver,
                    name,
                    arguments,
                } => {
                    let arguments = values(frame, arguments)?;
                    let receiver = frame.registers[receiver.0 as usize].clone();
                    let value = receiver.read()?;
                    if let Value::Record { fields, .. } = &value
                        && arguments.is_empty()
                        && let Some(field) = fields.get(&name)
                    {
                        write(frame, destination, field.clone())?;
                        continue;
                    }
                    if !matches!(value, Value::Record { .. }) {
                        if !arguments.is_empty() {
                            return Err(FosterError::runtime(format!(
                                "contract member `{name}` does not accept arguments"
                            )));
                        }
                        write(frame, destination, member(value, &name)?)?;
                        continue;
                    }
                    let Value::Record {
                        record: Some(record),
                        ..
                    } = value
                    else {
                        return Err(FosterError::runtime(format!(
                            "record cannot dispatch required method `{name}`"
                        )));
                    };
                    let function = self
                        .program
                        .methods
                        .get(&(record, name.clone()))
                        .copied()
                        .ok_or_else(|| {
                            FosterError::runtime(format!(
                                "record has no implementation of required method `{name}`"
                            ))
                        })?;
                    if let Some(shared) = receiver.shared() {
                        let (lease, state) = shared.write_snapshot()?;
                        let local = Slot::new(Value::from_wire(state)?);
                        let mut method = self.method_frame(
                            function,
                            local.clone(),
                            arguments,
                            Some(destination),
                        )?;
                        method.shared_commit = Some(SharedCommit {
                            shared,
                            receiver: local,
                            _lease: lease,
                        });
                        frames.push(method);
                    } else {
                        frames.push(self.method_frame(
                            function,
                            receiver,
                            arguments,
                            Some(destination),
                        )?);
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
                    let arguments = values(frame, arguments)?;
                    frames.push(self.frame(function, captures, arguments, Some(destination))?);
                }
                Instruction::CallClosure {
                    destination,
                    function,
                    captures,
                    arguments,
                } => {
                    let arguments = values(frame, arguments)?;
                    let captures = capture(frame, captures)?;
                    frames.push(self.frame(function, captures, arguments, Some(destination))?);
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

fn values(frame: &Frame, registers: Vec<Register>) -> Result<Vec<Value>, FosterError> {
    Ok(registers
        .into_iter()
        .map(|register| frame.registers[register.0 as usize].argument())
        .collect())
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

fn member(value: Value, field: &str) -> Result<Value, FosterError> {
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
        (Value::String(value), "empty?") => Ok(Value::Bool(value.is_empty())),
        (Value::String(value), "length") => Ok(Value::Integer(value.chars().count() as i64)),
        (Value::String(value), "head") => value
            .chars()
            .next()
            .map(Value::CodePoint)
            .ok_or_else(|| FosterError::runtime("cannot take `head` of an empty string")),
        (Value::String(value), "rest") => Ok(Value::String(value.chars().skip(1).collect())),
        (Value::String(value), "whitespace?") => {
            Ok(Value::Bool(value.chars().all(char::is_whitespace)))
        }
        (Value::CodePoint(value), "whitespace?") => Ok(Value::Bool(value.is_whitespace())),
        (Value::CodePoint(value), "string") => Ok(Value::String(value.to_string())),
        (_, field) => Err(FosterError::runtime(format!(
            "value has no field `{field}`"
        ))),
    }
}

fn call_builtin(builtin: Builtin, arguments: &[Value]) -> Result<Value, FosterError> {
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
        (Builtin::ParseFloat, [Value::String(value)]) => value
            .parse::<f64>()
            .map(Value::Float)
            .map_err(|_| FosterError::runtime("invalid Float text")),
        (Builtin::IoReadText, [Value::String(path)]) => Ok(io_result(
            "read_text",
            path,
            std::fs::read_to_string(path).map(Value::String),
        )),
        (Builtin::IoWriteText, [Value::String(path), Value::String(text)]) => Ok(io_result(
            "write_text",
            path,
            std::fs::write(path, text).map(|()| Value::Unit),
        )),
        (Builtin::IoListDirectory, [Value::String(path)]) => {
            let entries = std::fs::read_dir(path).and_then(|entries| {
                let mut names = Vec::new();
                for entry in entries {
                    let name = entry?.file_name().into_string().map_err(|_| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "directory entry name is not valid UTF-8",
                        )
                    })?;
                    names.push(Value::String(name));
                }
                names.sort_by_key(|name| name.to_string());
                Ok(Value::List(names))
            });
            Ok(io_result("list_directory", path, entries))
        }
        (Builtin::IoExists, [Value::String(path)]) => {
            Ok(Value::Bool(std::path::Path::new(path).exists()))
        }
        (Builtin::IoIsFile, [Value::String(path)]) => {
            Ok(Value::Bool(std::path::Path::new(path).is_file()))
        }
        (Builtin::IoIsDirectory, [Value::String(path)]) => {
            Ok(Value::Bool(std::path::Path::new(path).is_dir()))
        }
        (Builtin::IoJoin, [Value::String(left), Value::String(right)]) => {
            path_value(std::path::Path::new(left).join(right))
        }
        (Builtin::IoParent, [Value::String(path)]) => optional_path_component(
            std::path::Path::new(path)
                .parent()
                .map(std::path::Path::to_path_buf),
        ),
        (Builtin::IoFileName, [Value::String(path)]) => {
            optional_os_component(std::path::Path::new(path).file_name())
        }
        (Builtin::IoExtension, [Value::String(path)]) => {
            optional_os_component(std::path::Path::new(path).extension())
        }
        (Builtin::IoCanonicalize, [Value::String(path)]) => Ok(io_result(
            "canonicalize",
            path,
            std::fs::canonicalize(path).and_then(path_value_io),
        )),
        (Builtin::IoCurrentDirectory, []) => Ok(io_result(
            "current_directory",
            "",
            std::env::current_dir().and_then(path_value_io),
        )),
        (Builtin::TcpListen, [Value::String(address), Value::Integer(port)]) => Ok(tcp_result(
            "listen",
            super::host::listen(address, *port).map(Value::Integer),
        )),
        (Builtin::TcpConnect, [Value::String(address), Value::Integer(port)]) => Ok(tcp_result(
            "connect",
            super::host::connect(address, *port).map(Value::Integer),
        )),
        (Builtin::TcpAccept, [Value::Integer(listener)]) => Ok(tcp_result(
            "accept",
            super::host::accept(*listener).map(Value::Integer),
        )),
        (Builtin::TcpRead, [Value::Integer(connection), Value::Integer(maximum)]) => {
            Ok(tcp_result(
                "read",
                super::host::read(*connection, *maximum).map(Value::String),
            ))
        }
        (Builtin::TcpWrite, [Value::Integer(connection), Value::String(text)]) => Ok(tcp_result(
            "write",
            super::host::write(*connection, text).map(|()| Value::Unit),
        )),
        (Builtin::TcpSetTimeout, [Value::Integer(connection), Value::Integer(milliseconds)]) => {
            Ok(tcp_result(
                "set_timeout",
                super::host::set_timeout(*connection, *milliseconds).map(|()| Value::Unit),
            ))
        }
        (Builtin::TcpCloseListener, [Value::Integer(listener)]) => Ok(tcp_result(
            "close_listener",
            super::host::close_listener(*listener).map(|()| Value::Unit),
        )),
        (Builtin::TcpCloseConnection, [Value::Integer(connection)]) => Ok(tcp_result(
            "close_connection",
            super::host::close_connection(*connection).map(|()| Value::Unit),
        )),
        _ => Err(FosterError::runtime("invalid builtin arguments")),
    }
}

fn io_result(operation: &str, path: &str, result: Result<Value, std::io::Error>) -> Value {
    match result {
        Ok(value) => result_ok(value),
        Err(error) => result_error(Value::Record {
            record: None,
            name: "IoError".into(),
            fields: BTreeMap::from([
                ("operation".into(), Value::String(operation.into())),
                ("path".into(), Value::String(path.into())),
                ("message".into(), Value::String(error.to_string())),
            ]),
        }),
    }
}

fn tcp_result(operation: &str, result: Result<Value, String>) -> Value {
    match result {
        Ok(value) => result_ok(value),
        Err(message) => result_error(Value::Record {
            record: None,
            name: "NetworkError".into(),
            fields: BTreeMap::from([
                ("operation".into(), Value::String(operation.into())),
                ("message".into(), Value::String(message)),
            ]),
        }),
    }
}

fn result_ok(value: Value) -> Value {
    Value::Variant {
        type_name: "Result".into(),
        alternative: "Ok".into(),
        payload: vec![value],
    }
}

fn result_error(error: Value) -> Value {
    Value::Variant {
        type_name: "Result".into(),
        alternative: "Error".into(),
        payload: vec![error],
    }
}

fn path_value(path: std::path::PathBuf) -> Result<Value, FosterError> {
    path.into_os_string()
        .into_string()
        .map(Value::String)
        .map_err(|_| FosterError::runtime("path is not valid UTF-8"))
}

fn path_value_io(path: std::path::PathBuf) -> Result<Value, std::io::Error> {
    path.into_os_string()
        .into_string()
        .map(Value::String)
        .map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "path is not valid UTF-8")
        })
}

fn optional_path_component(path: Option<std::path::PathBuf>) -> Result<Value, FosterError> {
    match path {
        Some(path) => path_value(path),
        None => Ok(Value::String(String::new())),
    }
}

fn optional_os_component(value: Option<&std::ffi::OsStr>) -> Result<Value, FosterError> {
    match value {
        Some(value) => value
            .to_str()
            .map(|value| Value::String(value.into()))
            .ok_or_else(|| FosterError::runtime("path component is not valid UTF-8")),
        None => Ok(Value::String(String::new())),
    }
}
