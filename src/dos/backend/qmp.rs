//! Minimal QMP client: line framing with hard limits, command builders, transport enum.
//! Only the verbs bilterm needs are exposed (`send-key`, `pmemsave`, `quit`, `stop`/`cont`).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

use super::{BackendError, ControlEndpoint, Result};

/// One QMP line may never exceed this many bytes (greeting and events are far smaller).
pub const MAX_LINE: usize = 64 * 1024;
/// Async events skipped while waiting for one reply.
const MAX_EVENTS_PER_REPLY: usize = 256;
const IO_TIMEOUT: Duration = Duration::from_secs(5);

/// Result of classifying one inbound line.
#[derive(Debug, PartialEq)]
pub enum Line {
    Greeting,
    Event,
    Return(Value),
    Error(String),
}

pub fn classify(line: &str) -> Result<Line> {
    let v: Value = serde_json::from_str(line.trim())
        .map_err(|e| BackendError::Qmp(format!("bad json: {e}")))?;
    let o = v
        .as_object()
        .ok_or_else(|| BackendError::Qmp("reply is not an object".into()))?;
    if o.contains_key("QMP") {
        Ok(Line::Greeting)
    } else if o.contains_key("event") {
        Ok(Line::Event)
    } else if let Some(r) = o.get("return") {
        Ok(Line::Return(r.clone()))
    } else if let Some(e) = o.get("error") {
        let desc = e.get("desc").and_then(Value::as_str).unwrap_or("unknown");
        Ok(Line::Error(desc.chars().take(200).collect()))
    } else {
        Err(BackendError::Qmp("unrecognised reply".into()))
    }
}

/// Reads one newline terminated line, refusing more than [`MAX_LINE`] bytes. `None` on EOF.
pub fn read_line_capped<R: BufRead>(r: &mut R) -> Result<Option<String>> {
    let mut buf = Vec::new();
    loop {
        let chunk = r.fill_buf()?;
        if chunk.is_empty() {
            return if buf.is_empty() {
                Ok(None)
            } else {
                Err(BackendError::Qmp("eof inside a line".into()))
            };
        }
        let (take, done) = match chunk.iter().position(|b| *b == b'\n') {
            Some(i) => (i + 1, true),
            None => (chunk.len(), false),
        };
        if buf.len() + take > MAX_LINE {
            return Err(BackendError::Qmp("reply line exceeds limit".into()));
        }
        buf.extend_from_slice(&chunk[..take]);
        r.consume(take);
        if done {
            return String::from_utf8(buf)
                .map(Some)
                .map_err(|_| BackendError::Qmp("reply is not utf-8".into()));
        }
    }
}

pub fn cmd_capabilities() -> Value {
    json!({"execute": "qmp_capabilities"})
}

pub fn cmd_send_key(qcodes: &[&str]) -> Value {
    let keys: Vec<Value> = qcodes
        .iter()
        .map(|q| json!({"type": "qcode", "data": q}))
        .collect();
    json!({"execute": "send-key", "arguments": {"keys": keys}})
}

pub fn cmd_pmemsave(addr: u64, size: usize, filename: &Path) -> Result<Value> {
    let f = filename
        .to_str()
        .ok_or_else(|| BackendError::Invalid("non-utf8 dump path".into()))?;
    Ok(json!({"execute": "pmemsave", "arguments": {"val": addr, "size": size, "filename": f}}))
}

pub fn cmd_simple(name: &str) -> Value {
    json!({"execute": name})
}

pub enum Conn {
    Tcp(TcpStream),
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixStream),
}

impl Conn {
    pub fn connect(ep: &ControlEndpoint) -> std::io::Result<Self> {
        match ep {
            ControlEndpoint::Tcp { port } => {
                let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, *port));
                let s = TcpStream::connect_timeout(&addr, IO_TIMEOUT)?;
                s.set_read_timeout(Some(IO_TIMEOUT))?;
                s.set_write_timeout(Some(IO_TIMEOUT))?;
                Ok(Self::Tcp(s))
            }
            #[cfg(unix)]
            ControlEndpoint::Unix { path } => {
                let s = std::os::unix::net::UnixStream::connect(path)?;
                s.set_read_timeout(Some(IO_TIMEOUT))?;
                s.set_write_timeout(Some(IO_TIMEOUT))?;
                Ok(Self::Unix(s))
            }
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "no usable control endpoint",
            )),
        }
    }

    fn try_clone(&self) -> std::io::Result<Self> {
        Ok(match self {
            Self::Tcp(s) => Self::Tcp(s.try_clone()?),
            #[cfg(unix)]
            Self::Unix(s) => Self::Unix(s.try_clone()?),
        })
    }
}

impl Read for Conn {
    fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Tcp(s) => s.read(b),
            #[cfg(unix)]
            Self::Unix(s) => s.read(b),
        }
    }
}

impl Write for Conn {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::Tcp(s) => s.write(b),
            #[cfg(unix)]
            Self::Unix(s) => s.write(b),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::Tcp(s) => s.flush(),
            #[cfg(unix)]
            Self::Unix(s) => s.flush(),
        }
    }
}

/// QMP session over any duplex pair (a socket in production, buffers in tests).
pub struct Qmp<R: Read, W: Write> {
    rd: BufReader<R>,
    wr: W,
}

impl Qmp<Conn, Conn> {
    pub fn open(conn: Conn) -> Result<Self> {
        let rd = conn.try_clone()?;
        Self::handshake(rd, conn)
    }
}

impl<R: Read, W: Write> Qmp<R, W> {
    /// Reads the greeting and negotiates capabilities.
    pub fn handshake(rd: R, wr: W) -> Result<Self> {
        let mut q = Self {
            rd: BufReader::new(rd),
            wr,
        };
        match q.next_line()? {
            Line::Greeting => {}
            other => {
                return Err(BackendError::Qmp(format!(
                    "expected greeting, got {other:?}"
                )));
            }
        }
        q.execute(&cmd_capabilities())?;
        Ok(q)
    }

    fn next_line(&mut self) -> Result<Line> {
        let l = read_line_capped(&mut self.rd)?
            .ok_or_else(|| BackendError::Qmp("connection closed".into()))?;
        classify(&l)
    }

    /// Sends one command and returns its `return` value, skipping async events.
    pub fn execute(&mut self, cmd: &Value) -> Result<Value> {
        let mut s = cmd.to_string();
        s.push('\n');
        self.wr.write_all(s.as_bytes())?;
        self.wr.flush()?;
        for _ in 0..MAX_EVENTS_PER_REPLY {
            match self.next_line()? {
                Line::Return(v) => return Ok(v),
                Line::Error(e) => return Err(BackendError::Qmp(e)),
                Line::Event => continue,
                Line::Greeting => return Err(BackendError::Qmp("unexpected greeting".into())),
            }
        }
        Err(BackendError::Qmp("too many events before reply".into()))
    }

    pub fn send_key(&mut self, qcodes: &[&str]) -> Result<()> {
        self.execute(&cmd_send_key(qcodes)).map(|_| ())
    }

    pub fn pmemsave(&mut self, addr: u64, size: usize, filename: &Path) -> Result<()> {
        self.execute(&cmd_pmemsave(addr, size, filename)?)
            .map(|_| ())
    }

    pub fn quit(&mut self) -> Result<()> {
        // QEMU may close the socket right after replying; either outcome means it is going away.
        match self.execute(&cmd_simple("quit")) {
            Ok(_) | Err(BackendError::Qmp(_)) | Err(BackendError::Io(_)) => Ok(()),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn script(lines: &[&str]) -> Cursor<Vec<u8>> {
        Cursor::new(
            lines
                .iter()
                .map(|l| format!("{l}\n"))
                .collect::<String>()
                .into_bytes(),
        )
    }

    const GREET: &str = r#"{"QMP":{"version":{"qemu":{"major":10}},"capabilities":["oob"]}}"#;

    #[test]
    fn classify_lines() {
        assert_eq!(classify(GREET).unwrap(), Line::Greeting);
        assert_eq!(classify(r#"{"event":"STOP"}"#).unwrap(), Line::Event);
        assert_eq!(
            classify(r#"{"return":{}}"#).unwrap(),
            Line::Return(json!({}))
        );
        assert_eq!(
            classify(r#"{"error":{"class":"X","desc":"nope"}}"#).unwrap(),
            Line::Error("nope".into())
        );
        assert!(classify("not json").is_err());
        assert!(classify("[1]").is_err());
        assert!(classify(r#"{"x":1}"#).is_err());
    }

    #[test]
    fn handshake_skips_events_and_sends_commands() {
        let rd = script(&[
            GREET,
            r#"{"return":{}}"#,
            r#"{"event":"A"}"#,
            r#"{"event":"B"}"#,
            r#"{"return":{}}"#,
        ]);
        let mut sent = Vec::new();
        {
            let mut q = Qmp::handshake(rd, &mut sent).unwrap();
            q.send_key(&["shift", "a"]).unwrap();
        }
        let text = String::from_utf8(sent).unwrap();
        let cmds: Vec<Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(cmds[0], json!({"execute":"qmp_capabilities"}));
        assert_eq!(
            cmds[1],
            json!({"execute":"send-key","arguments":{"keys":[
                {"type":"qcode","data":"shift"},{"type":"qcode","data":"a"}]}})
        );
    }

    #[test]
    fn error_reply_and_bad_handshake() {
        let rd = script(&[GREET, r#"{"return":{}}"#, r#"{"error":{"desc":"bad key"}}"#]);
        let mut q = Qmp::handshake(rd, Vec::new()).unwrap();
        assert!(matches!(q.send_key(&["zz"]), Err(BackendError::Qmp(m)) if m == "bad key"));
        assert!(Qmp::handshake(script(&[r#"{"return":{}}"#]), Vec::new()).is_err());
        assert!(Qmp::handshake(script(&[]), Vec::new()).is_err());
    }

    #[test]
    fn oversize_line_and_event_flood_rejected() {
        let big = format!("{}\n", "a".repeat(MAX_LINE + 10));
        assert!(read_line_capped(&mut Cursor::new(big.into_bytes())).is_err());
        assert!(read_line_capped(&mut Cursor::new(b"abc".to_vec())).is_err());
        let mut lines = vec![GREET, r#"{"return":{}}"#];
        let ev = r#"{"event":"E"}"#;
        lines.extend(std::iter::repeat_n(ev, MAX_EVENTS_PER_REPLY + 1));
        let mut q = Qmp::handshake(script(&lines), Vec::new()).unwrap();
        assert!(q.send_key(&["a"]).is_err());
    }

    #[test]
    fn pmemsave_command_shape() {
        let c = cmd_pmemsave(0xB8000, 4000, Path::new("/run/x/vga.bin")).unwrap();
        assert_eq!(
            c,
            json!({"execute":"pmemsave","arguments":{"val":0xB8000,"size":4000,"filename":"/run/x/vga.bin"}})
        );
    }
}
