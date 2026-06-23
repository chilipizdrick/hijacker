use pipewire::{
    self as pw,
    spa::{param::audio::AudioFormat, sys::spa_audio_channel},
};
use symphonia::core::{audio::Channels, sample::SampleFormat};

pub fn sample_format_to_audio_format(sample_format: SampleFormat) -> AudioFormat {
    match sample_format {
        SampleFormat::U8 => AudioFormat::U8,
        SampleFormat::U16 => AudioFormat::U16,
        SampleFormat::U24 => AudioFormat::U24,
        SampleFormat::U32 => AudioFormat::U32,
        SampleFormat::S8 => AudioFormat::S8,
        SampleFormat::S16 => AudioFormat::S16,
        SampleFormat::S24 => AudioFormat::S24,
        SampleFormat::S32 => AudioFormat::S32,
        SampleFormat::F32 => AudioFormat::F32LE,
        SampleFormat::F64 => AudioFormat::F64LE,
    }
}

pub fn channels_to_spa_audio_channel(chan: Channels) -> spa_audio_channel {
    use pw::spa::sys::*;
    match chan {
        Channels::FRONT_LEFT => SPA_AUDIO_CHANNEL_FL,
        Channels::FRONT_RIGHT => SPA_AUDIO_CHANNEL_FR,
        Channels::FRONT_CENTRE => SPA_AUDIO_CHANNEL_FC,
        Channels::LFE1 => SPA_AUDIO_CHANNEL_LFE,
        Channels::REAR_LEFT => SPA_AUDIO_CHANNEL_RL,
        Channels::REAR_RIGHT => SPA_AUDIO_CHANNEL_RR,
        Channels::FRONT_LEFT_CENTRE => SPA_AUDIO_CHANNEL_FLC,
        Channels::FRONT_RIGHT_CENTRE => SPA_AUDIO_CHANNEL_FRC,
        Channels::REAR_CENTRE => SPA_AUDIO_CHANNEL_RC,
        Channels::SIDE_LEFT => SPA_AUDIO_CHANNEL_SL,
        Channels::SIDE_RIGHT => SPA_AUDIO_CHANNEL_SR,
        Channels::TOP_CENTRE => SPA_AUDIO_CHANNEL_TC,
        Channels::TOP_FRONT_LEFT => SPA_AUDIO_CHANNEL_TFL,
        Channels::TOP_FRONT_CENTRE => SPA_AUDIO_CHANNEL_TFC,
        Channels::TOP_FRONT_RIGHT => SPA_AUDIO_CHANNEL_TFR,
        Channels::TOP_REAR_LEFT => SPA_AUDIO_CHANNEL_TRL,
        Channels::TOP_REAR_CENTRE => SPA_AUDIO_CHANNEL_TRC,
        Channels::TOP_REAR_RIGHT => SPA_AUDIO_CHANNEL_TRR,
        Channels::REAR_LEFT_CENTRE => SPA_AUDIO_CHANNEL_RLC,
        Channels::REAR_RIGHT_CENTRE => SPA_AUDIO_CHANNEL_RRC,
        Channels::FRONT_LEFT_WIDE => SPA_AUDIO_CHANNEL_FLW,
        Channels::FRONT_RIGHT_WIDE => SPA_AUDIO_CHANNEL_FRW,
        Channels::FRONT_LEFT_HIGH => SPA_AUDIO_CHANNEL_FLH,
        Channels::FRONT_CENTRE_HIGH => SPA_AUDIO_CHANNEL_FCH,
        Channels::FRONT_RIGHT_HIGH => SPA_AUDIO_CHANNEL_FRH,
        Channels::LFE2 => SPA_AUDIO_CHANNEL_LFE2,
        _ => 0,
    }
}
