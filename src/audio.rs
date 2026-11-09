//! Plays mp3 bytes through the default output device.

use std::io::Cursor;

#[derive(Default)]
pub struct Player {
    // The stream must stay alive while sounds play; created on first use.
    out: Option<(rodio::OutputStream, rodio::OutputStreamHandle)>,
    sink: Option<rodio::Sink>,
}

impl Player {
    pub fn play(&mut self, bytes: Vec<u8>) -> Result<(), String> {
        if self.out.is_none() {
            self.out = Some(rodio::OutputStream::try_default().map_err(|e| e.to_string())?);
        }
        let (_, handle) = self.out.as_ref().unwrap();
        if let Some(old) = self.sink.take() {
            old.stop();
        }
        let sink = rodio::Sink::try_new(handle).map_err(|e| e.to_string())?;
        let source = rodio::Decoder::new(Cursor::new(bytes)).map_err(|e| e.to_string())?;
        sink.append(source);
        self.sink = Some(sink);
        Ok(())
    }
}
