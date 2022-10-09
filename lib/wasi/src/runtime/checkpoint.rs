use std::{sync::{Mutex, atomic::{AtomicBool, Ordering}, Arc}, pin::Pin, task::{Context, Poll}, ops::DerefMut};

use tokio::sync::mpsc;
use tracing::*;

#[allow(dead_code)]
#[derive(Debug)]
pub struct WasmCheckpoint {
    rx: Mutex<Option<mpsc::Receiver<()>>>,
    triggered: AtomicBool,
}

#[allow(dead_code)]
impl WasmCheckpoint {
    pub fn new() -> (mpsc::Sender<()>, Arc<WasmCheckpoint>) {
        let (tx, rx) = mpsc::channel(1);
        let cp = WasmCheckpoint {
            rx: Mutex::new(Some(rx)),
            triggered: AtomicBool::new(false)
        };
        (tx, Arc::new(cp))
    }

    pub async fn wait(&self) -> bool {
        if self.triggered.load(Ordering::Acquire) {
            return true;
        }
        let mut rx = {
            let mut rx = self.rx.lock().unwrap();
            match rx.take() {
                Some(a) => a,
                None => {
                    trace!("wasm checkpoint aborted(1)");
                    return false;
                }
            }
        };
        let ret = rx.recv().await.is_some();
        if ret == true {
            trace!("wasm checkpoint triggered");
            self.triggered.store(true, Ordering::Release);
        } else {
            trace!("wasm checkpoint aborted(2)");
        }
        ret
    }

    pub fn poll(self: Pin<&Self>, cx: &mut Context<'_>) -> Poll::<()> {
        if self.triggered.load(Ordering::Acquire) {
            return Poll::Ready(());
        }
        let mut rx = self.rx.lock().unwrap();
        let rx = match rx.deref_mut() {
            Some(a) => a,
            None => {
                trace!("wasm checkpoint aborted");
                self.triggered.store(true, Ordering::Release);
                return Poll::Ready(());
            }
        };
        let mut rx = Pin::new(rx);
        match rx.poll_recv(cx) {
            Poll::Ready(_) => {
                trace!("wasm checkpoint triggered");
                self.triggered.store(true, Ordering::Release);
                Poll::Ready(())
            },
            Poll::Pending => Poll::Pending
        }
    }
}

impl From<mpsc::Receiver<()>>
for WasmCheckpoint
{
    fn from(rx: mpsc::Receiver<()>) -> Self {
        WasmCheckpoint {
            rx: Mutex::new(Some(rx)),
            triggered: AtomicBool::new(false)
        }
    }
}
