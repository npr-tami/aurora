//! Client side of the pipe to the DLL.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::time::{Duration, Instant};

use aurora_core::PIPE_NAME;
use aurora_core::proto::{self, Request, Response};

pub struct Pipe {
    reader: BufReader<File>,
    writer: File,
}

impl Pipe {
    /// Connect, retrying for up to `wait` (the DLL needs a moment after injection).
    pub fn connect(wait: Duration) -> Result<Pipe, String> {
        let start = Instant::now();
        loop {
            match OpenOptions::new().read(true).write(true).open(PIPE_NAME) {
                Ok(file) => {
                    let writer = file.try_clone().map_err(|e| e.to_string())?;
                    return Ok(Pipe {
                        reader: BufReader::new(file),
                        writer,
                    });
                }
                Err(e) if start.elapsed() < wait => {
                    let _ = e;
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => return Err(format!("pipe not open: {e}")),
            }
        }
    }

    pub fn call(&mut self, req: &Request) -> Result<Response, String> {
        self.writer
            .write_all(proto::encode(req).as_bytes())
            .map_err(|e| format!("send: {e}"))?;
        let mut line = String::new();
        match self.reader.read_line(&mut line) {
            Ok(0) => Err("DLL closed the pipe".into()),
            Ok(_) => proto::decode(&line),
            Err(e) => Err(format!("receive: {e}")),
        }
    }
}
