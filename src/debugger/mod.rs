//! Source debugger for checked Foster bytecode, speaking DAP over an authenticated loopback socket.
mod program;
use crate::{compiler::Compilation, error::RuntimeError, hir::FunctionId, vm};
use program::DebugProgram;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::Path;
use std::sync::{Arc, Mutex, mpsc};
use vm::debug::{Frame, Observer};

struct Output {
    stream: TcpStream,
    sequence: u64,
}
impl Output {
    fn send(&mut self, mut message: Value) {
        self.sequence += 1;
        message["seq"] = json!(self.sequence);
        let bytes = serde_json::to_vec(&message).unwrap();
        let _ = write!(self.stream, "Content-Length: {}\r\n\r\n", bytes.len());
        let _ = self.stream.write_all(&bytes);
        let _ = self.stream.flush();
    }
    fn event(&mut self, name: &str, body: Value) {
        self.send(json!({"type":"event","event":name,"body":body}));
    }
    fn response(&mut self, request: &Value, body: Value) {
        self.send(json!({"type":"response","request_seq":request["seq"],"command":request["command"],"success":true,"body":body}));
    }
    fn error(&mut self, request: &Value, message: &str) {
        self.send(json!({"type":"response","request_seq":request["seq"],"command":request["command"],"success":false,"message":message}));
    }
}

#[derive(Clone, Copy)]
enum Mode {
    Continue,
    In,
    Over(usize),
    Out(usize),
}
struct State {
    requests: mpsc::Receiver<Value>,
    output: Output,
    breakpoints: HashMap<std::path::PathBuf, HashSet<usize>>,
    frames: Vec<Frame>,
    frame_base: usize,
    mode: Mode,
    previous: Option<(FunctionId, usize, usize, usize)>,
    breakpoint_previous: HashMap<usize, (FunctionId, usize, usize)>,
    pause: bool,
    terminated: bool,
    launched: bool,
    configured: bool,
    reason: &'static str,
}
struct Session {
    functions: HashMap<FunctionId, program::Function>,
    state: Mutex<State>,
}

/// The caller connects to the extension's private loopback listener before compiling.
/// Program stdout/stderr remain ordinary process streams, separate from the DAP socket.
pub fn connect(port: u16, token: &str) -> std::io::Result<TcpStream> {
    let mut stream = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))?;
    stream.set_nodelay(true)?;
    writeln!(stream, "{token}")?;
    Ok(stream)
}

pub fn run(
    compilation: &Compilation,
    arguments: &crate::entry::CommandArguments,
    stream: TcpStream,
) -> Result<i32, Box<dyn std::error::Error>> {
    let debug = DebugProgram::compile(compilation)?;
    let reader = stream.try_clone()?;
    let (sender, requests) = mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        while let Ok(Some(request)) = read_message(&mut reader) {
            if sender.send(request).is_err() {
                break;
            }
        }
    });
    let session = Arc::new(Session {
        functions: debug.functions,
        state: Mutex::new(State {
            requests,
            output: Output {
                stream,
                sequence: 0,
            },
            breakpoints: HashMap::new(),
            frames: Vec::new(),
            frame_base: 0,
            mode: Mode::Continue,
            previous: None,
            breakpoint_previous: HashMap::new(),
            pause: false,
            terminated: false,
            launched: false,
            configured: false,
            reason: "entry",
        }),
    });
    {
        let mut state = session.state.lock().unwrap();
        while !(state.launched && state.configured) && !state.terminated {
            let request = state.requests.recv()?;
            session.handle(&mut state, request, false);
        }
        if state.terminated {
            return Ok(0);
        }
    }
    let result = vm::Machine::new(&debug.program.into_verified()?)
        .with_debugger(session.clone())
        .run_main_with_arguments(arguments);
    let mut state = session.state.lock().unwrap();
    let exit_code = match result {
        Ok(value) => {
            if value != vm::Value::Unit {
                println!("{value}");
            }
            match vm::release_value(value) {
                Ok(()) => 0,
                Err(error) => {
                    eprintln!("{error}");
                    1
                }
            }
        }
        Err(error) => {
            if state.terminated {
                0
            } else {
                eprintln!("{error}");
                1
            }
        }
    };
    state.output.event("exited", json!({"exitCode":exit_code}));
    state.output.event("terminated", json!({}));
    let _ = state.output.stream.shutdown(std::net::Shutdown::Both);
    Ok(exit_code)
}

fn read_message(reader: &mut impl BufRead) -> std::io::Result<Option<Value>> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            length = value.trim().parse::<usize>().ok();
        }
    }
    let length = length
        .filter(|length| *length <= 16 * 1024 * 1024)
        .ok_or_else(|| std::io::Error::other("invalid DAP content length"))?;
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(std::io::Error::other)
}

impl Session {
    fn handle(&self, state: &mut State, request: Value, stopped: bool) -> bool {
        let args = &request["arguments"];
        let mut resume = false;
        match request["command"].as_str().unwrap_or("") {
            "initialize" => {
                if args["linesStartAt1"] == false
                    || args["columnsStartAt1"] == false
                    || args["pathFormat"]
                        .as_str()
                        .is_some_and(|format| format != "path")
                {
                    state.output.error(
                        &request,
                        "Foster requires one-based lines/columns and native file paths.",
                    );
                    return false;
                }
                state.output.response(&request,json!({"supportsConfigurationDoneRequest":true,"supportsTerminateRequest":true,"supportsEvaluateForHovers":true,"supportsDelayedStackTraceLoading":true}));
                state.output.event("initialized", json!({}));
            }
            "launch" if !state.launched => {
                state.launched = true;
                if args["stopOnEntry"].as_bool().unwrap_or(false) {
                    state.mode = Mode::In;
                }
                state.output.response(&request, json!({}));
            }
            "configurationDone" => {
                state.configured = true;
                state.output.response(&request, json!({}));
            }
            "setBreakpoints" => {
                let path =
                    program::normalize(Path::new(args["source"]["path"].as_str().unwrap_or("")));
                let mut available = self
                    .functions
                    .values()
                    .filter(|function| function.path == path)
                    .flat_map(|function| &function.lines)
                    .copied()
                    .filter(|line| *line > 0)
                    .collect::<Vec<_>>();
                available.sort_unstable();
                available.dedup();
                let mut accepted = HashSet::new();
                let breakpoints = args["breakpoints"].as_array().into_iter().flatten().map(|breakpoint| {
                    if ["condition","hitCondition","logMessage"].iter().any(|key| breakpoint.get(*key).is_some()) {return json!({"verified":false,"message":"Conditional breakpoints and logpoints are not supported."})}
                    let requested = breakpoint["line"].as_u64().unwrap_or(0) as usize;
                    match available.iter().copied().find(|line| *line >= requested && requested > 0) {
                        Some(line) => {accepted.insert(line);json!({"verified":true,"line":line,"source":{"path":path}})},
                        None => json!({"verified":false,"message":"No executable source location in this file."}),
                    }
                }).collect::<Vec<_>>();
                state.breakpoints.insert(path, accepted);
                state
                    .output
                    .response(&request, json!({"breakpoints":breakpoints}));
            }
            "setExceptionBreakpoints" => state.output.response(&request, json!({"breakpoints":[]})),
            "threads" => state
                .output
                .response(&request, json!({"threads":[{"id":1,"name":"Foster main"}]})),
            "stackTrace" if stopped => {
                let start = args["startFrame"].as_u64().unwrap_or(0) as usize;
                let levels = args["levels"]
                    .as_u64()
                    .filter(|n| *n > 0)
                    .unwrap_or(u64::MAX) as usize;
                let frames = state.frames.iter().enumerate().rev().enumerate().skip(start).take(levels).map(|(_, (index,frame))| {
                    let function = self.functions.get(&frame.function);
                    let line = function.and_then(|f| f.lines.get(frame.instruction)).copied().filter(|line| *line > 0).unwrap_or(1);
                    json!({"id":state.frame_base+index,"name":function.map(|f|f.name.as_str()).unwrap_or(&frame.name),"source":function.map(|f|json!({"path":f.path})),"line":line,"column":1})
                }).collect::<Vec<_>>();
                state.output.response(
                    &request,
                    json!({"stackFrames":frames,"totalFrames":state.frames.len()}),
                );
            }
            "scopes" if stopped => {
                let id = args["frameId"].as_u64().unwrap_or(0) as usize;
                if id
                    .checked_sub(state.frame_base)
                    .is_some_and(|index| index < state.frames.len())
                {
                    state.output.response(&request,json!({"scopes":[{"name":"Locals","presentationHint":"locals","variablesReference":id,"expensive":false}]}));
                } else {
                    state.output.error(&request, "Unknown or expired frame.");
                }
            }
            "variables" if stopped => {
                let id = args["variablesReference"].as_u64().unwrap_or(0) as usize;
                match self.variables(state, id) {
                    Some(variables) => state
                        .output
                        .response(&request, json!({"variables":variables})),
                    None => state
                        .output
                        .error(&request, "Unknown or expired variables reference."),
                }
            }
            "evaluate" if stopped => {
                let id = args["frameId"]
                    .as_u64()
                    .map(|id| id as usize)
                    .unwrap_or(state.frame_base + state.frames.len().saturating_sub(1));
                let expression = args["expression"].as_str().unwrap_or("").trim();
                if let Some(variable) = self
                    .variables(state, id)
                    .and_then(|variables| variables.into_iter().find(|v| v["name"] == expression))
                {
                    state.output.response(
                        &request,
                        json!({"result":variable["value"],"variablesReference":0}),
                    );
                } else {
                    state.output.error(&request,"Only visible local and parameter names can be inspected; expression evaluation is not supported.");
                }
            }
            command @ ("continue" | "next" | "stepIn" | "stepOut") if stopped => {
                state.mode = match command {
                    "next" => Mode::Over(state.frames.len()),
                    "stepOut" => Mode::Out(state.frames.len()),
                    "stepIn" => Mode::In,
                    _ => Mode::Continue,
                };
                state
                    .output
                    .response(&request, json!({"allThreadsContinued":true}));
                state.output.event(
                    "continued",
                    json!({"threadId":1,"allThreadsContinued":true}),
                );
                state.frames.clear();
                resume = true;
            }
            "pause" => {
                state.pause = true;
                state.output.response(&request, json!({}));
            }
            "disconnect" | "terminate" => {
                state.terminated = true;
                state.output.response(&request, json!({}));
                resume = true;
            }
            _ => state.output.error(
                &request,
                "Request is unsupported or requires a paused program.",
            ),
        }
        resume
    }

    fn variables(&self, state: &State, id: usize) -> Option<Vec<Value>> {
        let frame = state.frames.get(id.checked_sub(state.frame_base)?)?;
        let Some(function) = self.functions.get(&frame.function) else {
            return Some(Vec::new());
        };
        let offset = function
            .spans
            .get(frame.instruction)
            .map_or(0, |span| span.start);
        let mut variables=function.locals.iter().filter(|local| local.parameter || local.scope.contains(&offset)).filter_map(|local| {
            let value=frame.registers.get(&local.register)?;
            Some(json!({"name":local.name,"value":value,"variablesReference":0,"evaluateName":local.name,"presentationHint":{"attributes":["readOnly"]}}))
        }).collect::<Vec<_>>();
        variables.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        Some(variables)
    }
}

impl Observer for Session {
    fn registers(&self, function: FunctionId) -> Vec<usize> {
        self.functions
            .get(&function)
            .map_or_else(Vec::new, |function| {
                function.locals.iter().map(|local| local.register).collect()
            })
    }
    fn should_stop(
        &self,
        function: FunctionId,
        instruction: usize,
        depth: usize,
    ) -> Result<bool, RuntimeError> {
        let mut state = self.state.lock().unwrap();
        loop {
            match state.requests.try_recv() {
                Ok(request) => {
                    self.handle(&mut state, request, false);
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    state.terminated = true;
                    break;
                }
            }
        }
        if state.terminated {
            return Err(RuntimeError::runtime("debug session terminated"));
        }
        let Some(definition) = self.functions.get(&function) else {
            return Ok(false);
        };
        let line = definition.lines.get(instruction).copied().unwrap_or(0);
        if line == 0 {
            return Ok(false);
        }
        let changed =
            state
                .previous
                .is_none_or(|(old_function, old_instruction, old_depth, old_line)| {
                    old_function != function
                        || old_depth != depth
                        || old_line != line
                        || instruction <= old_instruction
                });
        state.previous = Some((function, instruction, depth, line));
        if !changed {
            return Ok(false);
        }
        state
            .breakpoint_previous
            .retain(|old_depth, _| *old_depth <= depth);
        let breakpoint_changed = state
            .breakpoint_previous
            .insert(depth, (function, instruction, line))
            .is_none_or(|(old_function, old_instruction, old_line)| {
                old_function != function || old_line != line || instruction <= old_instruction
            });
        let breakpoint = breakpoint_changed
            && state
                .breakpoints
                .get(&definition.path)
                .is_some_and(|lines| lines.contains(&line));
        let step = match state.mode {
            Mode::Continue => false,
            Mode::In => true,
            Mode::Over(old) => depth <= old,
            Mode::Out(old) => depth < old,
        };
        if breakpoint || step || state.pause {
            state.reason = if state.pause {
                "pause"
            } else if breakpoint {
                "breakpoint"
            } else if state.frame_base == 0 {
                "entry"
            } else {
                "step"
            };
            state.pause = false;
            return Ok(true);
        }
        Ok(false)
    }
    fn stopped(&self, frames: Vec<Frame>) -> Result<(), RuntimeError> {
        let mut state = self.state.lock().unwrap();
        state.frame_base += state.frames.len().max(frames.len()) + 1;
        state.frames = frames;
        let reason = state.reason;
        state.output.event(
            "stopped",
            json!({"reason":reason,"threadId":1,"allThreadsStopped":true}),
        );
        loop {
            let request = state
                .requests
                .recv()
                .map_err(|_| RuntimeError::runtime("debug client disconnected"))?;
            if self.handle(&mut state, request, true) {
                break;
            }
        }
        if state.terminated {
            Err(RuntimeError::runtime("debug session terminated"))
        } else {
            Ok(())
        }
    }
}
