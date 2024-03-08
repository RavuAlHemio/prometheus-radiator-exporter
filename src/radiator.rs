use std::fmt;
use std::io;
use std::sync::OnceLock;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::Mutex;

use crate::config::{CONFIG, RadiatorConfig};


pub(crate) static RADIATOR_SOCKET: OnceLock<Mutex<BufReader<TcpStream>>> = OnceLock::new();


#[derive(Debug)]
pub(crate) enum Error {
    Io(io::Error),
    InvalidCredentials,
    UnexpectedLoginResponse { response: Vec<u8> },
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {}", e),
            Self::InvalidCredentials => write!(f, "invalid credentials"),
            Self::UnexpectedLoginResponse { .. } => write!(f, "unexpected login response"),
        }
    }
}
impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::InvalidCredentials => None,
            Self::UnexpectedLoginResponse { .. } => None,
        }
    }
}
impl From<io::Error> for Error {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}


pub(crate) async fn connect_to_radiator(config: &RadiatorConfig) -> Result<BufReader<TcpStream>, Error> {
    // connect
    let connection = TcpStream::connect((config.target, config.mgmt_port)).await?;
    let mut buffered_connection = BufReader::new(connection);

    // switch to binary mode and log in
    let login_string = format!("BINARY\r\nLOGIN {} {}\0", config.username, config.password);
    let login_bytes = login_string.as_bytes(); // UTF-8
    buffered_connection.write_all(&login_bytes).await?;
    buffered_connection.flush().await?;

    // read login response
    let mut buf = Vec::new();
    buffered_connection.read_until(b'\0', &mut buf).await?;
    if buf == b"LOGGEDIN\0" {
        Ok(buffered_connection)
    } else if buf == b"BADLOGIN\0" {
        Err(Error::InvalidCredentials)
    } else {
        Err(Error::UnexpectedLoginResponse { response: buf })
    }
}

async fn write_command(connection: &mut BufReader<TcpStream>, command: &[u8]) -> Result<(), Error> {
    // no NUL byte in command
    assert!(command.iter().all(|b| *b != 0x00), "no NUL byte in command");

    connection.write_all(command).await?;
    connection.write_u8(b'\0').await?;
    connection.flush().await?;

    Ok(())
}

pub(crate) async fn communicate(command: &[u8]) -> Result<Vec<u8>, Error> {
    let mut connection_guard = RADIATOR_SOCKET
        .get().expect("RADIATOR_SOCKET not set?!")
        .lock().await;

    // try sending
    if let Err(_) = write_command(&mut *connection_guard, command).await {
        // that failed; try making a new connection
        let config_guard = CONFIG
            .get().expect("CONFIG not set?!");

        // if this fails as well, fail the whole call
        let new_conn = connect_to_radiator(&config_guard.radiator).await?;

        // store for later
        *connection_guard = new_conn;

        // try sending again (give up if it fails)
        write_command(&mut *connection_guard, command).await?;
    }

    let mut buf = Vec::new();
    loop {
        // try receiving a response
        buf.clear();
        connection_guard.read_until(b'\0', &mut buf).await?;
        assert_eq!(buf.last(), Some(&b'\0'));
        buf.pop();

        // get the next response if we receive a log message
        if !buf.starts_with(b"LOG ") {
            break;
        }
    }
    Ok(buf)
}
