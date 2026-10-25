//! PipeWire capture through its standard recorder. Raw float samples stay in
//! memory; cancellation kills and reaps only this owned child process.
use crate::{Core, Fault, Listener, Outcome, Recognition, SPEECH_RATE};
use std::{
    io::Read,
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    thread::JoinHandle,
};

pub(crate) struct Capture {
    core: Arc<Core>,
    child: Arc<Mutex<Child>>,
    worker: Option<JoinHandle<()>>,
}
impl Capture {
    pub(crate) fn start(
        recognition: Recognition,
        pause: bool,
        listener: Arc<dyn Listener>,
    ) -> Result<Self, Fault> {
        if recognition != Recognition::Upload {
            return Err(Fault::LanguageUnavailable);
        }
        let mut child = Command::new("pw-record")
            .args([
                "--raw",
                "--format",
                "f32",
                "--rate",
                "16000",
                "--channels",
                "1",
                "--media-role",
                "Communication",
                "--properties",
                "application.name=Zephium",
                "-",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| Fault::NoMicrophone)?;
        let stdout = child.stdout.take().ok_or(Fault::NoMicrophone)?;
        let child = Arc::new(Mutex::new(child));
        let core = Arc::new(Core::new(f64::from(SPEECH_RATE), true, pause, listener));
        let worker_core = core.clone();

        let worker = match std::thread::Builder::new()
            .name("zephium-pipewire-capture".into())
            .spawn(move || {
                let mut stdout = stdout;
                let mut block = [0u8; 640 * 4];
                while stdout.read_exact(&mut block).is_ok() {
                    let mono: Vec<f32> = block
                        .chunks_exact(4)
                        .map(|bytes| f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
                        .map(|sample| {
                            if sample.is_finite() {
                                sample.clamp(-1.0, 1.0)
                            } else {
                                0.0
                            }
                        })
                        .collect();
                    worker_core.push(&mono);
                }
                let stopped = worker_core.state.lock().map_or(true, |state| state.stopped);
                if !stopped {
                    worker_core.listener.ended(crate::Ending::NothingHeard);
                }
            }) {
            Ok(worker) => worker,
            Err(_) => {
                if let Ok(mut child) = child.lock() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
                return Err(Fault::NoMicrophone);
            }
        };
        Ok(Self {
            core,
            child,
            worker: Some(worker),
        })
    }
    fn release(&mut self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
    pub(crate) fn finish(mut self) -> Result<Outcome, Fault> {
        self.release();
        let (take, heard) = self.core.stop();
        Ok(Core::upload(take, heard))
    }
    pub(crate) fn cancel(mut self) {
        self.core.stop();
        self.release();
    }
}
impl Drop for Capture {
    fn drop(&mut self) {
        self.core.stop();
        self.release();
    }
}
