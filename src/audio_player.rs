use crate::{Error, Result};
use rodio::{OutputStream, OutputStreamHandle};

pub trait AsyncAudioPlayer {
    fn play_audio_file(&self, file: String) -> Result<()>;
    fn set_volume(&self, volume: f32);
    fn sleep_until_end(&self);
}

pub struct AudioPlayer {
    _stream: OutputStream,
    _stream_handle: OutputStreamHandle,
    sink: rodio::Sink,
}

impl AudioPlayer {
    pub fn try_new() -> Result<Self> {
        let (stream, stream_handle) = rodio::OutputStream::try_default().map_err(Error::Stream)?;
        let sink = rodio::Sink::try_new(&stream_handle).map_err(Error::Play)?;
        Ok(Self {
            _stream: stream,
            _stream_handle: stream_handle,
            sink,
        })
    }
}

impl AsyncAudioPlayer for AudioPlayer {
    fn play_audio_file(&self, file: String) -> Result<()> {
        let file = std::fs::File::open(file).map_err(Error::Fs)?;
        let source = rodio::Decoder::new(file).map_err(Error::Decorer)?;
        self.sink.append(source);
        Ok(())
    }

    fn set_volume(&self, volume: f32) {
        self.sink.set_volume(volume);
    }

    fn sleep_until_end(&self) {
        self.sink.sleep_until_end();
    }
}
