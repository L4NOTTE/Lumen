use std::sync::{Arc, atomic::{AtomicBool, Ordering}, mpsc::SyncSender};
use cpal::{traits::{HostTrait, DeviceTrait, StreamTrait}, SampleFormat};
pub enum Packet { Audio(u32, Vec<f32>), Error(String), End }
pub fn start(tx: SyncSender<Packet>, stop: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let result = (|| -> Result<(), String> {
            let device = cpal::default_host().default_output_device().ok_or("Нет устройства вывода звука")?;
            let config = device.default_output_config().map_err(|e|e.to_string())?;
            let channels = config.channels() as usize; let rate=config.sample_rate().0;
            let cfg: cpal::StreamConfig=config.clone().into();
            let out=tx.clone();let errors=tx.clone();
            let send=move |samples:Vec<f32>| {let mono=samples.chunks(channels).map(|c|c.iter().sum::<f32>()/c.len() as f32).collect();let _=out.try_send(Packet::Audio(rate,mono));};
            let error=move |e:cpal::StreamError|{let _=errors.try_send(Packet::Error(e.to_string()));};
            let stream=match config.sample_format() {
                SampleFormat::F32=>device.build_input_stream(&cfg,move|d:&[f32],_|send(d.to_vec()),error,None),
                SampleFormat::I16=>device.build_input_stream(&cfg,move|d:&[i16],_|send(d.iter().map(|x|*x as f32/32768.).collect()),error,None),
                SampleFormat::U16=>device.build_input_stream(&cfg,move|d:&[u16],_|send(d.iter().map(|x|(*x as f32-32768.)/32768.).collect()),error,None),
                _=>return Err("Неподдерживаемый формат WASAPI".into()),
            }.map_err(|e|e.to_string())?;
            stream.play().map_err(|e|e.to_string())?;
            let began=std::time::Instant::now();
            while !stop.load(Ordering::Relaxed) && began.elapsed().as_secs()<60 {std::thread::sleep(std::time::Duration::from_millis(20));}
            drop(stream);Ok(())
        })();
        if let Err(error)=result {let _=tx.try_send(Packet::Error(error));}
        let _=tx.try_send(Packet::End);
    });
}
