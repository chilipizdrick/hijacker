use std::sync::mpsc;

use rodio::{PlayError, StreamError, decoder::DecoderError};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Play(PlayError),
    Stream(StreamError),
    Decorer(DecoderError),
    Fs(std::io::Error),
    Pipewire(pipewire::Error),
    Recv(mpsc::RecvError),
    IO(std::io::Error),
    Send,
    ApplicationNodeNotFound,
    ApplicationOutputPortNotFound,
    UnexpectedPwResponse,
    BinaryNameUnset,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ERROR: {:?}", self)
    }
}
