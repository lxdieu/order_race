//! Nhật ký FIX ghi trên thread riêng: thread gửi lệnh không bao giờ chờ đĩa.

use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::sync::mpsc::{self, Sender};
use std::sync::Mutex;
use std::thread::{self, JoinHandle};

use crate::clock;

pub struct Journal {
    tx: Mutex<Option<Sender<String>>>,
    writer: Mutex<Option<JoinHandle<io::Result<()>>>>,
    utc_offset: i32,
}

impl Journal {
    pub fn open(path: &Path, utc_offset: i32) -> io::Result<Journal> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        let (tx, rx) = mpsc::channel::<String>();
        let writer = thread::Builder::new().name("journal".into()).spawn(move || {
            let mut w = BufWriter::with_capacity(1 << 16, file);
            while let Ok(line) = rx.recv() {
                w.write_all(line.as_bytes())?;
                while let Ok(more) = rx.try_recv() {
                    w.write_all(more.as_bytes())?;
                }
                w.flush()?;
            }
            w.flush()?;
            w.get_ref().sync_all()
        })?;
        Ok(Journal { tx: Mutex::new(Some(tx)), writer: Mutex::new(Some(writer)), utc_offset })
    }

    pub fn line(&self, text: &str) {
        let line = format!("{} {text}\n", clock::local_time_us(clock::now_nanos(), self.utc_offset));
        if let Some(tx) = self.tx.lock().unwrap().as_ref() {
            let _ = tx.send(line);
        }
    }

    /// Ghi hết phần còn trong hàng đợi rồi đóng file.
    pub fn close(&self) -> io::Result<()> {
        drop(self.tx.lock().unwrap().take());
        match self.writer.lock().unwrap().take() {
            Some(h) => h.join().unwrap_or_else(|_| Err(io::Error::other("thread journal panic"))),
            None => Ok(()),
        }
    }
}

/// Ghi file và fsync trước khi trả về (kế hoạch gửi phải nằm trên đĩa trước giờ mở cửa).
pub fn write_synced(path: &Path, data: &[u8]) -> io::Result<()> {
    let mut f = File::create(path)?;
    f.write_all(data)?;
    f.sync_all()
}
