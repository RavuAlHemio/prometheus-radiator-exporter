use std::fmt;
use std::io;
use std::sync::OnceLock;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::Mutex;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tracing::{error, warn};

use crate::config::{CONFIG, RadiatorConfig};


pub(crate) static SOCKET_STATE: OnceLock<Mutex<SocketState>> = OnceLock::new();


#[derive(Debug)]
pub struct SocketState {
    pub socket_writer: Option<OwnedWriteHalf>,
    pub new_socket_sender: UnboundedSender<BufReader<OwnedReadHalf>>,
    pub message_receiver: UnboundedReceiver<Vec<u8>>,
}


async fn message_processor(
    mut new_socket_receiver: UnboundedReceiver<BufReader<OwnedReadHalf>>,
    message_sender: UnboundedSender<Vec<u8>>,
) {
    loop {
        // obtain a socket
        // if no new socket will ever come, break out
        let Some(mut socket) = new_socket_receiver.recv().await else { break };

        let mut buf = Vec::new();
        loop {
            // read out a packet
            buf.clear();
            if let Err(e) = socket.read_until(b'\0', &mut buf).await {
                error!("error reading from Radiator management socket: {}", e);

                // break out, waiting for a new socket
                break;
            }
            if buf.len() == 0 {
                // EOF
                warn!("end-of-file encountered while reading from Radiator management socket");

                // again, wait for a new socket
                break;
            }
            assert!(buf.last() == Some(&b'\0'));
            buf.pop();

            // if it starts with "LOG ", ignore it
            // otherwise, pass it on
            if !buf.starts_with(b"LOG ") {
                message_sender.send(buf.clone())
                    .expect("sending received message failed");
            }
        }
    }
}


pub fn start_message_processor() -> SocketState {
    let (new_socket_sender, new_socket_receiver) = mpsc::unbounded_channel();
    let (message_sender, message_receiver) = mpsc::unbounded_channel();

    tokio::spawn(async move {
        message_processor(new_socket_receiver, message_sender).await
    });
    SocketState {
        socket_writer: None,
        new_socket_sender,
        message_receiver,
    }
}


#[derive(Debug)]
pub(crate) enum Error {
    Io(io::Error),
    InvalidCredentials,
    UnexpectedLoginResponse { response: Vec<u8> },
    ReaderGone,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {}", e),
            Self::InvalidCredentials => write!(f, "invalid credentials"),
            Self::UnexpectedLoginResponse { .. } => write!(f, "unexpected login response"),
            Self::ReaderGone => write!(f, "the reader has disappeared"),
        }
    }
}
impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::InvalidCredentials => None,
            Self::UnexpectedLoginResponse { .. } => None,
            Self::ReaderGone => None,
        }
    }
}
impl From<io::Error> for Error {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}


pub(crate) async fn connect_to_radiator(config: &RadiatorConfig, state: &mut SocketState) -> Result<(), Error> {
    // connect
    let connection = TcpStream::connect((config.target, config.mgmt_port)).await?;
    let (read_half, mut write_half) = connection.into_split();
    let mut buffered_reader = BufReader::new(read_half);

    // switch to binary mode and log in
    let login_string = format!("BINARY\r\nLOGIN {} {}\0", config.username, config.password);
    let login_bytes = login_string.as_bytes(); // UTF-8
    write_half.write_all(&login_bytes).await?;
    write_half.flush().await?;

    // read login response
    let mut buf = Vec::new();
    buffered_reader.read_until(b'\0', &mut buf).await?;
    if buf == b"LOGGEDIN\0" {
        // store writing socket
        state.socket_writer = Some(write_half);

        // send fresh reading socket to reading task
        state.new_socket_sender.send(buffered_reader)
            .expect("sending new socket failed");

        Ok(())
    } else if buf == b"BADLOGIN\0" {
        Err(Error::InvalidCredentials)
    } else {
        Err(Error::UnexpectedLoginResponse { response: buf })
    }
}

async fn write_command(writer: &mut OwnedWriteHalf, command: &[u8]) -> Result<(), Error> {
    // no NUL byte in command
    assert!(command.iter().all(|b| *b != 0x00), "no NUL byte in command");

    let mut terminated_command = Vec::with_capacity(command.len() + 1);
    terminated_command.extend_from_slice(command);
    terminated_command.push(b'\0');

    writer.write_all(&terminated_command).await?;
    writer.flush().await?;

    Ok(())
}

pub(crate) async fn communicate(command: &[u8]) -> Result<Vec<u8>, Error> {
    let mut state_guard = SOCKET_STATE
        .get().expect("SOCKET_STATE not set?!")
        .lock().await;
    let writer = state_guard.socket_writer
        .as_mut().expect("SOCKET_STATE.socket_writer not set?!");

    // try sending
    if let Err(_) = write_command(writer, command).await {
        // that failed; try making a new connection
        let config_guard = CONFIG
            .get().expect("CONFIG not set?!");

        // if this fails as well, fail the whole call
        connect_to_radiator(&config_guard.radiator, &mut *state_guard).await?;

        // try sending again (give up if it fails)
        let new_writer = state_guard.socket_writer
            .as_mut().expect("SOCKET_STATE.socket_writer not set?!");
        write_command(new_writer, command).await?;
    }

    // receive a response
    state_guard.message_receiver
        .recv().await
        .ok_or(Error::ReaderGone)
}
