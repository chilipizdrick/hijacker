use std::fmt::Debug;
use std::fs::File;
use std::path::Path;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::anyhow;
use pipewire::spa::param::audio::{AudioFormat, AudioInfoRaw, AudioInfoRawFlags, MAX_CHANNELS};
use pipewire::spa::sys::spa_audio_channel;
use symphonia::core::audio::{Channels, Position};
use symphonia::core::formats::TrackType;
use symphonia::core::formats::probe::Hint;
use symphonia::core::io::MediaSourceStream;

pub trait AudioSource: Send {
    fn audio_info(&self) -> &AudioInfoRaw;
    fn consumer(&mut self) -> &mut rtrb::Consumer<f32>;
}

#[derive(Debug)]
pub struct AudioFileStream {
    audio_info: AudioInfoRaw,
    consumer: rtrb::Consumer<f32>,
    _producer_thread_join_handle: JoinHandle<()>,
}

impl AudioFileStream {
    pub fn new(file_path: impl AsRef<Path>, ring_buffer_length: Duration) -> anyhow::Result<Self> {
        let audio_source_file = File::open(&file_path)?;
        let mss = MediaSourceStream::new(Box::new(audio_source_file), Default::default());

        let mut hint = Hint::new();
        if let Some(file_extension) = file_path.as_ref().extension() {
            hint.with_extension(&file_extension.to_string_lossy());
        }
        let format_ops = Default::default();
        let metadata_ops = Default::default();
        let mut format_reader =
            symphonia::default::get_probe().probe(&hint, mss, format_ops, metadata_ops)?;

        let track = format_reader
            .default_track(TrackType::Audio)
            .ok_or_else(|| anyhow!("file does not have a default audio track"))?;
        let codec_params = track
            .codec_params
            .as_ref()
            .ok_or_else(|| anyhow!("could not read codec parameters from audio track"))?;
        let audio_codec_params = codec_params
            .audio()
            .expect("the track is audio so codec params should also be audio");
        let mut decoder = symphonia::default::get_codecs()
            .make_audio_decoder(audio_codec_params, &Default::default())?;

        let mut audio_info = AudioInfoRaw::new();
        audio_info.set_format(AudioFormat::F32LE);

        let sample_rate = audio_codec_params
            .sample_rate
            .ok_or_else(|| anyhow!("missing sample rate in audio codec parameters"))?;
        audio_info.set_rate(sample_rate);

        let channels = audio_codec_params
            .channels
            .as_ref()
            .ok_or_else(|| anyhow!("missing number of channels in audio codec parameters"))?;
        audio_info.set_channels(channels.count() as u32);

        if let Channels::Positioned(position) = channels {
            let mut res = [0; MAX_CHANNELS];
            for (i, pos) in position.iter().enumerate() {
                res[i] = position_to_spa_audio_channel(pos);
            }
            audio_info.set_position(res);
        } else {
            audio_info.set_flags(AudioInfoRawFlags::UNPOSITIONED);
        }

        let ring_buffer_size =
            ring_buffer_length.as_millis() as usize * sample_rate as usize * channels.count();
        let (mut producer, consumer) = rtrb::RingBuffer::new(ring_buffer_size);

        let join_handle = thread::Builder::new()
            .name("audio-decoder".into())
            .spawn(move || {
                use symphonia::core::errors::Error;

                let mut sample_buf: Vec<f32> = Vec::new();

                while let Ok(Some(packet)) = format_reader.next_packet() {
                    let decoded = match decoder.decode(&packet) {
                        Ok(v) => v,
                        Err(Error::DecodeError(_) | Error::IoError(_)) => continue,
                        Err(_) => return,
                    };

                    let num_samples = decoded.frames() * decoded.spec().channels().count();
                    sample_buf.resize(num_samples, 0.0);
                    decoded.copy_to_slice_interleaved(&mut sample_buf);

                    let mut current_sample_idx = 0;
                    let samples_len = sample_buf.len();

                    while current_sample_idx < samples_len {
                        let available = producer.slots();
                        if available == 0 {
                            thread::sleep(Duration::from_millis(10));
                            continue;
                        }

                        let new_sample_idx = (current_sample_idx + available).min(samples_len);
                        producer
                            .push_entire_slice(&sample_buf[current_sample_idx..new_sample_idx])
                            .expect("should be enough space in the ring buffer");

                        current_sample_idx = new_sample_idx;
                        if current_sample_idx < samples_len {
                            thread::sleep(Duration::from_millis(10));
                        }
                    }
                }
            })
            .map_err(|err| anyhow!("could not spawn audio decoder thread: {err}"))?;

        Ok(Self {
            audio_info,
            consumer,
            _producer_thread_join_handle: join_handle,
        })
    }
}

impl AudioSource for AudioFileStream {
    fn audio_info(&self) -> &AudioInfoRaw {
        &self.audio_info
    }
    fn consumer(&mut self) -> &mut rtrb::Consumer<f32> {
        &mut self.consumer
    }
}

fn position_to_spa_audio_channel(position: Position) -> spa_audio_channel {
    use pipewire::spa::sys::*;
    match position {
        Position::FRONT_LEFT => SPA_AUDIO_CHANNEL_FL,
        Position::FRONT_RIGHT => SPA_AUDIO_CHANNEL_FR,
        Position::FRONT_CENTER => SPA_AUDIO_CHANNEL_FC,
        Position::LFE1 => SPA_AUDIO_CHANNEL_LFE,
        Position::REAR_LEFT => SPA_AUDIO_CHANNEL_RL,
        Position::REAR_RIGHT => SPA_AUDIO_CHANNEL_RR,
        Position::FRONT_LEFT_CENTER => SPA_AUDIO_CHANNEL_FLC,
        Position::FRONT_RIGHT_CENTER => SPA_AUDIO_CHANNEL_FRC,
        Position::REAR_CENTER => SPA_AUDIO_CHANNEL_RC,
        Position::SIDE_LEFT => SPA_AUDIO_CHANNEL_SL,
        Position::SIDE_RIGHT => SPA_AUDIO_CHANNEL_SR,
        Position::TOP_CENTER => SPA_AUDIO_CHANNEL_TC,
        Position::TOP_FRONT_LEFT => SPA_AUDIO_CHANNEL_TFL,
        Position::TOP_FRONT_CENTER => SPA_AUDIO_CHANNEL_TFC,
        Position::TOP_FRONT_RIGHT => SPA_AUDIO_CHANNEL_TFR,
        Position::TOP_REAR_LEFT => SPA_AUDIO_CHANNEL_TRL,
        Position::TOP_REAR_CENTER => SPA_AUDIO_CHANNEL_TRC,
        Position::TOP_REAR_RIGHT => SPA_AUDIO_CHANNEL_TRR,
        Position::LFE2 => SPA_AUDIO_CHANNEL_LFE2,
        Position::TOP_SIDE_LEFT => SPA_AUDIO_CHANNEL_TSL,
        Position::TOP_SIDE_RIGHT => SPA_AUDIO_CHANNEL_TSR,
        Position::BOTTOM_FRONT_CENTER => SPA_AUDIO_CHANNEL_BC,
        Position::BOTTOM_FRONT_LEFT => SPA_AUDIO_CHANNEL_BLC,
        Position::BOTTOM_FRONT_RIGHT => SPA_AUDIO_CHANNEL_BRC,
        Position::FRONT_LEFT_WIDE => SPA_AUDIO_CHANNEL_FLW,
        Position::FRONT_RIGHT_WIDE => SPA_AUDIO_CHANNEL_FRW,
        _ => 0,
    }
}
