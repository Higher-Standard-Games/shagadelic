use std::io::{self, BufRead, Read, Write};

use serde::{Deserialize, Serialize};

const VERSION: u32 = 1;
const MAX_RECORD_BYTES: usize = 64 * 1024;

#[derive(Deserialize)]
struct Initialization {
    version: u32,
    lobby_id: String,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Command {
    Initialize(Initialization),
    Shutdown,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Event<'a> {
    Ready { version: u32, lobby_id: &'a str },
}

pub fn run(reader: &mut impl BufRead, writer: &mut impl Write) -> io::Result<()> {
    let mut record = Vec::new();
    let Some(command) = read_command(reader, &mut record)? else {
        return Ok(());
    };
    let Command::Initialize(init) = command else {
        return Err(invalid("expected initialization"));
    };
    if init.version != VERSION {
        return Err(invalid("unsupported control version"));
    }
    if init.lobby_id.trim().is_empty() {
        return Err(invalid("lobby ID is empty"));
    }

    serde_json::to_writer(
        &mut *writer,
        &Event::Ready {
            version: VERSION,
            lobby_id: &init.lobby_id,
        },
    )?;
    writer.write_all(b"\n")?;
    writer.flush()?;

    match read_command(reader, &mut record)? {
        Some(Command::Initialize(_)) => Err(invalid("lobby already initialized")),
        Some(Command::Shutdown) | None => Ok(()),
    }
}

fn read_command(reader: &mut impl BufRead, record: &mut Vec<u8>) -> io::Result<Option<Command>> {
    record.clear();
    let count = reader
        .take((MAX_RECORD_BYTES + 1) as u64)
        .read_until(b'\n', record)?;
    if count == 0 {
        return Ok(None);
    }
    if count > MAX_RECORD_BYTES {
        return Err(invalid("control record exceeds size limit"));
    }
    if record.last() != Some(&b'\n') {
        // A dead host may leave an incomplete command in the pipe.
        return Ok(None);
    }
    serde_json::from_slice(record)
        .map(Some)
        .map_err(|_| invalid("invalid control command"))
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
