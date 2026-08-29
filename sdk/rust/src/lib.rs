//! Rust client SDK for Ditto's JSON Lines Unix-socket protocol.

use std::path::Path;

pub use ditto_protocol::*;
use thiserror::Error;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    net::{
        UnixStream,
        unix::{OwnedReadHalf, OwnedWriteHalf},
    },
};

#[derive(Debug)]
pub struct Client {
    reader: Lines<BufReader<OwnedReadHalf>>,
    writer: OwnedWriteHalf,
}

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("Ditto socket I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("Ditto protocol serialization failed: {0}")]
    Protocol(#[from] serde_json::Error),
}

impl Client {
    /// Connects to a running Ditto daemon over its Unix socket.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::Io`] when the socket cannot be reached.
    pub async fn connect(path: impl AsRef<Path>) -> Result<Self, ClientError> {
        let stream = UnixStream::connect(path).await?;
        let (reader, writer) = stream.into_split();
        Ok(Self {
            reader: BufReader::new(reader).lines(),
            writer,
        })
    }

    /// Writes one JSON Lines command to the daemon.
    ///
    /// # Errors
    ///
    /// Returns an I/O or serialization error when the command cannot be sent.
    pub async fn send(&mut self, command: &ClientCommand) -> Result<(), ClientError> {
        let message = serde_json::to_vec(command)?;
        self.writer.write_all(&message).await?;
        self.writer.write_all(b"\n").await?;
        Ok(())
    }

    /// Reads and decodes the next server message.
    ///
    /// # Errors
    ///
    /// Returns an I/O or protocol error when the next line cannot be decoded.
    pub async fn next(&mut self) -> Result<Option<ServerMessage>, ClientError> {
        self.reader
            .next_line()
            .await?
            .map(|line| serde_json::from_str(&line).map_err(ClientError::from))
            .transpose()
    }
}
