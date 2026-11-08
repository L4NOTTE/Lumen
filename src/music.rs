//! Распознавание музыки (как Shazam): запись звука из системы или микрофона и поиск трека
//! через сервис AudD (нужен собственный ключ API).

use std::{
    fs,
    sync::{Arc, Mutex},
    time::Duration,
};

use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    SampleFormat,
};
use serde::Serialize;

#[derive(Serialize, Clone, Default)]
pub struct Song {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub date: String,
    pub link: String,
}

#[derive(Serialize, Clone, Default)]
pub struct MusicState {
    /// idle | listening | searching | found | notfound | error
    pub status: String,
    pub song: Option<Song>,
    pub msg: String,
}

impl MusicState {
    pub fn new(status: &str) -> Self {
        Self { status: status.into(), song: None, msg: String::new() }
    }
    pub fn error(msg: impl Into<String>) -> Self {
        Self { status: "error".into(), song: None, msg: msg.into() }
    }
}

/// Записывает `secs` секунд звука. `mic = false` — то, что играет в системе (WASAPI loopback).
/// Возвращает WAV: моно, 16 кГц, 16 бит.
pub fn record(mic: bool, secs: u64) -> Result<Vec<u8>, String> {
    let host = cpal::default_host();
    let (device, config) = if mic {
        let d = host.default_input_device().ok_or("Микрофон не найден")?;
        let c = d.default_input_config().map_err(|e| e.to_string())?;
        (d, c)
    } else {
        let d = host.default_output_device().ok_or("Устройство вывода звука не найдено")?;
        let c = d.default_output_config().map_err(|e| e.to_string())?;
        (d, c)
    };

    let channels = config.channels() as usize;
    let rate = config.sample_rate().0;
    let cfg: cpal::StreamConfig = config.clone().into();
    let buf: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
    let err_fn = |_e: cpal::StreamError| {};

    let b = buf.clone();
    let stream = match config.sample_format() {
        SampleFormat::F32 => device.build_input_stream(
            &cfg,
            move |d: &[f32], _| {
                if let Ok(mut g) = b.lock() {
                    g.extend_from_slice(d);
                }
            },
            err_fn,
            None,
        ),
        SampleFormat::I16 => device.build_input_stream(
            &cfg,
            move |d: &[i16], _| {
                if let Ok(mut g) = b.lock() {
                    g.extend(d.iter().map(|s| *s as f32 / 32768.0));
                }
            },
            err_fn,
            None,
        ),
        SampleFormat::U16 => device.build_input_stream(
            &cfg,
            move |d: &[u16], _| {
                if let Ok(mut g) = b.lock() {
                    g.extend(d.iter().map(|s| (*s as f32 - 32768.0) / 32768.0));
                }
            },
            err_fn,
            None,
        ),
        f => return Err(format!("Неподдерживаемый формат звука: {f:?}")),
    }
    .map_err(|e| format!("Не удалось начать запись: {e}"))?;

    stream.play().map_err(|e| format!("Не удалось начать запись: {e}"))?;
    std::thread::sleep(Duration::from_secs(secs));
    drop(stream);

    let data = buf.lock().map_err(|_| "Ошибка записи")?.clone();
    if channels == 0 || data.len() / channels < rate as usize {
        return Err(if mic {
            "Микрофон ничего не записал".into()
        } else {
            "Ничего не записалось: в системе не играет звук".into()
        });
    }

    // в моно
    let mono: Vec<f32> = data
        .chunks(channels)
        .map(|c| c.iter().sum::<f32>() / channels as f32)
        .collect();
    let peak = mono.iter().fold(0f32, |m, x| m.max(x.abs()));
    if peak < 0.003 {
        return Err("Звук не слышен — включите музыку погромче".into());
    }
    let gain = (0.9 / peak).min(8.0);

    // ресемплинг до 16 кГц усреднением
    let ratio = rate as f32 / 16000.0;
    let out_len = (mono.len() as f32 / ratio) as usize;
    let mut pcm: Vec<i16> = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let a = (i as f32 * ratio) as usize;
        let z = (((i + 1) as f32 * ratio) as usize).min(mono.len()).max(a + 1);
        let avg = mono[a..z].iter().sum::<f32>() / (z - a) as f32;
        pcm.push((avg * gain * 32767.0).clamp(-32768.0, 32767.0) as i16);
    }
    Ok(wav(&pcm, 16000))
}

fn wav(pcm: &[i16], rate: u32) -> Vec<u8> {
    let data_len = (pcm.len() * 2) as u32;
    let mut v = Vec::with_capacity(44 + pcm.len() * 2);
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&(36 + data_len).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes()); // PCM
    v.extend_from_slice(&1u16.to_le_bytes()); // моно
    v.extend_from_slice(&rate.to_le_bytes());
    v.extend_from_slice(&(rate * 2).to_le_bytes());
    v.extend_from_slice(&2u16.to_le_bytes());
    v.extend_from_slice(&16u16.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&data_len.to_le_bytes());
    for s in pcm {
        v.extend_from_slice(&s.to_le_bytes());
    }
    v
}

fn field(v: &serde_json::Value, k: &str) -> String {
    v[k].as_str().unwrap_or("").to_string()
}

/// Отправляет запись в AudD. Ok(None) — трек не найден.
pub fn recognize(token: &str, wav: &[u8]) -> Result<Option<Song>, String> {
    let path = std::env::temp_dir().join(format!("lumen-rec-{}.wav", std::process::id()));
    fs::write(&path, wav).map_err(|e| format!("Не удалось сохранить запись: {e}"))?;
    let out = crate::sys::hidden("curl.exe")
        .args(["-sS", "--max-time", "40", "-F"])
        .arg(format!("api_token={token}"))
        .args(["-F", "return=apple_music,spotify", "-F"])
        .arg(format!("file=@{}", path.display()))
        .arg("https://api.audd.io/")
        .output();
    let _ = fs::remove_file(&path);
    let out = out.map_err(|e| format!("Не удалось запустить curl: {e}"))?;
    if !out.status.success() {
        return Err("Нет связи с сервисом распознавания".into());
    }
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|_| "Неожиданный ответ сервиса".to_string())?;
    if v["status"] != "success" {
        let m = v["error"]["error_message"].as_str().unwrap_or("Ошибка сервиса распознавания");
        return Err(m.to_string());
    }
    let r = &v["result"];
    if r.is_null() {
        return Ok(None);
    }
    Ok(Some(Song {
        title: field(r, "title"),
        artist: field(r, "artist"),
        album: field(r, "album"),
        date: field(r, "release_date"),
        link: field(r, "song_link"),
    }))
}
