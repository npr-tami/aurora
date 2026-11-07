//! Messages between the Launchpad and the mod's DLL. One JSON object per line, both ways.

use serde::{Deserialize, Serialize};

/// The Launchpad only asks whether the mod is there, and asks it to unload: everything else the mod
/// does on its own, from its own files.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Request {
    /// Are you there? Answered with `Pong`.
    Ping,
    /// Unload cleanly (closes the pipe, ends the thread).
    Bye,
}

/// One caught write, from a hardware watchpoint (the mod's god mode and credits catches, and
/// grbresearch-dll's `watch.request`). Registers in the order of `REG_NAMES`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WriteHit {
    pub rip: u64,
    /// Module the instruction lives in, and its offset there, when known.
    pub module: String,
    pub offset: u64,
    pub thread: u32,
    /// 16 bytes before rip and 32 after, when readable.
    pub code: Vec<u8>,
    pub regs: [u64; 16],
    /// How many times this rip fired.
    pub count: u32,
    /// Return address on the stack at the catch (who called), when readable.
    #[serde(default)]
    pub ret: u64,
}

/// How the ESP looks.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct EspStyle {
    /// Farthest box, in the game's own metres.
    pub range_m: f32,
    /// Distance under each box.
    pub labels: bool,
    /// Thin black edge around lines and text.
    pub edge: bool,
    /// A skeleton instead of a box, where the bones can be read.
    pub bones: bool,
    /// Line width in pixels; 0 = grows with the screen.
    pub width: u8,
    /// Enemy, friendly, other NPC, player, target.
    pub colors: [[u8; 3]; 5],
    /// Drones.
    pub drone: [u8; 3],
}

impl EspStyle {
    pub const DEFAULT: EspStyle = EspStyle {
        range_m: 375.0,
        labels: true,
        edge: true,
        bones: false,
        width: 0,
        colors: [[235, 80, 70], [110, 210, 130], [231, 182, 92], [110, 170, 255], [255, 255, 255]],
        drone: [160, 90, 255],
    };
}

impl Default for EspStyle {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Register names in `WriteHit::regs` order, for research output.
pub const REG_NAMES: [&str; 16] = [
    "rax", "rbx", "rcx", "rdx", "rsi", "rdi", "rbp", "rsp", "r8", "r9", "r10", "r11", "r12", "r13", "r14", "r15",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModuleInfo {
    pub name: String,
    pub base: u64,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Response {
    Pong { pid: u32, proto: u32, dll_version: String },
    Ok,
    Err { msg: String },
}

impl Response {
    pub fn err(msg: impl Into<String>) -> Self {
        Response::Err { msg: msg.into() }
    }
}

/// Encode one message as a line (no newline inside JSON, so a line is a message).
pub fn encode<T: Serialize>(msg: &T) -> String {
    let mut s = serde_json::to_string(msg).expect("protocol types always serialise");
    s.push('\n');
    s
}

/// Decode one line. Trailing newline or `\r\n` is fine.
pub fn decode<'a, T: Deserialize<'a>>(line: &'a str) -> Result<T, String> {
    serde_json::from_str(line.trim_end()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_roundtrip() {
        for r in [Request::Ping, Request::Bye] {
            let line = encode(&r);
            assert!(line.ends_with('\n'));
            assert_eq!(line.matches('\n').count(), 1, "one line per message");
            let back: Request = decode(&line).unwrap();
            assert_eq!(back, r);
        }
        // An older launcher's other requests are refused, not misread.
        assert!(decode::<Request>(r#"{"t":"god_status"}"#).is_err());
    }

    #[test]
    fn response_roundtrip_and_errors() {
        let r = Response::Pong { pid: 7, proto: 1, dll_version: "0.1.0".into() };
        let back: Response = decode(&encode(&r)).unwrap();
        assert_eq!(back, r);
        let bad: Result<Response, _> = decode("{not json");
        assert!(bad.is_err());
        let crlf = encode(&Response::Ok).replace('\n', "\r\n");
        let back: Response = decode(&crlf).unwrap();
        assert_eq!(back, Response::Ok);
    }
}
