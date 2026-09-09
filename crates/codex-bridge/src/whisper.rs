use std::net::SocketAddr;
use std::time::Duration;

use reqwest::blocking::multipart::{Form, Part};
use reqwest::blocking::Client;
use serde_json::Value;

use codex_bridge::{BackendFailure, RealtimeAudioChunk};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(45);

pub(crate) fn is_ready(listen: SocketAddr) -> bool {
    Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .and_then(|client| client.get(format!("http://{listen}/")).send())
        .is_ok_and(|response| response.status().is_success())
}

pub(crate) fn transcribe(
    listen: SocketAddr,
    audio: &RealtimeAudioChunk,
) -> Result<String, BackendFailure> {
    let wav = pcm16_wav(audio)?;
    let part = Part::bytes(wav)
        .file_name("recording.wav")
        .mime_str("audio/wav")
        .map_err(|error| failure("audio_transcription_failed", error))?;
    let response = Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|error| failure("audio_transcription_failed", error))?
        .post(format!("http://{listen}/inference"))
        .multipart(
            Form::new()
                .part("file", part)
                .text("response_format", "json")
                .text("temperature", "0.0"),
        )
        .send()
        .map_err(|error| failure("audio_transcription_unavailable", error))?;
    let status = response.status();
    let body = response
        .text()
        .map_err(|error| failure("audio_transcription_failed", error))?;
    if !status.is_success() {
        return Err(BackendFailure {
            code: "audio_transcription_failed",
            message: format!("whisper.cpp returned HTTP {status}: {}", truncate(&body)),
        });
    }
    let value: Value = serde_json::from_str(&body).map_err(|error| BackendFailure {
        code: "audio_transcription_failed",
        message: format!("whisper.cpp returned invalid JSON: {error}"),
    })?;
    value
        .get("text")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| BackendFailure {
            code: "audio_transcription_empty",
            message: "whisper.cpp did not detect speech".to_owned(),
        })
}

fn pcm16_wav(audio: &RealtimeAudioChunk) -> Result<Vec<u8>, BackendFailure> {
    let source = audio.data.as_bytes();
    let decoded = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, source)
        .map_err(|error| failure("invalid_audio", error))?;
    let samples = decoded
        .chunks_exact(2)
        .map(|bytes| i16::from_le_bytes([bytes[0], bytes[1]]))
        .collect::<Vec<_>>();
    let mono = if audio.num_channels == 1 {
        samples
    } else {
        samples
            .chunks_exact(usize::from(audio.num_channels))
            .map(|frame| {
                let total = frame.iter().map(|sample| i32::from(*sample)).sum::<i32>();
                (total / i32::from(audio.num_channels)) as i16
            })
            .collect()
    };
    let samples = resample_linear(&mono, audio.sample_rate, 16_000);
    let data_len = u32::try_from(samples.len().saturating_mul(2)).map_err(|_| BackendFailure {
        code: "invalid_audio",
        message: "audio is too large for a WAV container".to_owned(),
    })?;
    let mut wav = Vec::with_capacity(44 + data_len as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&16_000_u32.to_le_bytes());
    wav.extend_from_slice(&(16_000_u32 * 2).to_le_bytes());
    wav.extend_from_slice(&2_u16.to_le_bytes());
    wav.extend_from_slice(&16_u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        wav.extend_from_slice(&sample.to_le_bytes());
    }
    Ok(wav)
}

fn resample_linear(input: &[i16], from: u32, to: u32) -> Vec<i16> {
    if input.is_empty() || from == 0 || to == 0 {
        return Vec::new();
    }
    if from == to {
        return input.to_vec();
    }
    let output_len = ((input.len() as u64 * u64::from(to)) / u64::from(from)) as usize;
    (0..output_len)
        .map(|index| {
            let numerator = index as u64 * u64::from(from);
            let left = (numerator / u64::from(to)) as usize;
            let remainder = (numerator % u64::from(to)) as f32 / to as f32;
            let right = (left + 1).min(input.len() - 1);
            let value = input[left] as f32 + (input[right] as f32 - input[left] as f32) * remainder;
            value.round().clamp(i16::MIN as f32, i16::MAX as f32) as i16
        })
        .collect()
}

fn truncate(value: &str) -> String {
    value.chars().take(256).collect()
}

fn failure(code: &'static str, error: impl std::fmt::Display) -> BackendFailure {
    BackendFailure {
        code,
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn creates_16khz_mono_pcm_wav() {
        let input = [0_i16, 1000, -1000, 2000, -2000, 0];
        let audio = RealtimeAudioChunk {
            data: base64::engine::general_purpose::STANDARD.encode(
                input
                    .iter()
                    .flat_map(|sample| sample.to_le_bytes())
                    .collect::<Vec<_>>(),
            ),
            sample_rate: 24_000,
            num_channels: 1,
            samples_per_channel: input.len() as u32,
        };
        let wav = pcm16_wav(&audio).unwrap();
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 16_000);
        assert_eq!(u16::from_le_bytes(wav[22..24].try_into().unwrap()), 1);
        assert_eq!(wav.len(), 52);
    }

    #[test]
    fn sends_wav_to_whisper_inference_endpoint() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = vec![0; 16 * 1024];
            let count = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..count]);
            assert!(request.starts_with("POST /inference HTTP/1.1"));
            assert!(request.contains("name=\"file\"; filename=\"recording.wav\""));
            assert!(request.contains("Content-Type: audio/wav"));
            let body = r#"{"text":" local transcript "}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });
        let audio = RealtimeAudioChunk {
            data: base64::engine::general_purpose::STANDARD.encode([0_u8; 320]),
            sample_rate: 16_000,
            num_channels: 1,
            samples_per_channel: 160,
        };
        assert_eq!(transcribe(address, &audio).unwrap(), "local transcript");
        server.join().unwrap();
    }
}
