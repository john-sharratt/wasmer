use tokio::sync::mpsc;
use wasmer_vnet::net_error_into_io_err;

use super::*;
use std::{
    io::{Read, Seek},
    sync::{RwLockReadGuard, RwLockWriteGuard}, future::Future, pin::Pin, task::Poll,
};

pub(crate) struct InodeValFilePollGuard<'a> {
    pub(crate) guard: RwLockReadGuard<'a, Kind>,
}

impl<'a> std::fmt::Debug
for InodeValFilePollGuard<'a>
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.guard.deref() {
            Kind::File { .. } => write!(f, "guard-file"),
            Kind::EventNotifications { .. } => write!(f, "guard-notifications"),
            Kind::Socket { socket } => {
                let socket = socket.inner.read().unwrap();
                match socket.kind {
                    InodeSocketKind::TcpListener(..) => write!(f, "guard-tcp-listener"),
                    InodeSocketKind::TcpStream(..) => write!(f, "guard-tcp-stream"),
                    InodeSocketKind::UdpSocket(..) => write!(f, "guard-udp-socket"),
                    InodeSocketKind::Raw(..) => write!(f, "guard-raw-socket"),
                    InodeSocketKind::HttpRequest(..) => write!(f, "guard-http-request"),
                    InodeSocketKind::WebSocket(..) => write!(f, "guard-web-socket"),
                    _ => write!(f, "guard-socket")
                }                
            },
            _ => write!(f, "guard-unknown"),
        }
    }
}

impl<'a> InodeValFilePollGuard<'a> {
    pub fn bytes_available_read(&self) -> wasmer_vfs::Result<Option<usize>> {
        match self.guard.deref() {
            Kind::File { handle, .. } => {
                if let Some(handle) = handle {
                    handle.bytes_available_read()
                } else {
                    Ok(None)
                }
            },
            Kind::EventNotifications { counter, .. } => {
                Ok(
                    Some(counter.load(std::sync::atomic::Ordering::Acquire) as usize)
                )
            },
            Kind::Socket { socket } => {
                socket.peek()
                    .map(|a| Some(a))
                    .map_err(fs_error_from_wasi_err)
            }
            _ => Ok(None)
        }
    }

    pub fn bytes_available_write(&self) -> wasmer_vfs::Result<Option<usize>> {
        match self.guard.deref() {
            Kind::File { handle, .. } => {
                if let Some(handle) = handle {
                    handle.bytes_available_write()
                } else {
                    Ok(None)
                }
            },
            Kind::EventNotifications { wakers, .. } => {
                let wakers = wakers.lock().unwrap();
                Ok(
                    Some(wakers.len())
                )
            }
            Kind::Socket { socket } => {
                if socket.can_write() {
                    Ok(Some(4096))
                } else {
                    Ok(Some(0))
                }
            }
            _ => Ok(None)
        }
    }

    pub fn is_open(&self) -> bool{
        match self.guard.deref() {
            Kind::File { handle, .. } => {
                if let Some(handle) = handle {
                    handle.is_open()
                } else {
                    false
                }
            },
            Kind::EventNotifications { .. } |
            Kind::Socket { .. } => {
                true
            }
            _ => false
        }
    }

    pub async fn wait_read(&self) -> wasmer_vfs::Result<()> {
        match self.guard.deref() {
            Kind::File { handle, .. } => {
                if let Some(handle) = handle {
                    handle.wait_read().await
                } else {
                    None
                }
            },
            Kind::EventNotifications { counter, wakers, .. } => {
                let counter = counter.clone();
                let (tx, rx) = tokio::sync::mpsc::channel(1);
                {
                    let mut wakers = wakers.lock().unwrap();
                    wakers.push_back(tx);
                }
                rx.recv().await;
                Some(Box::pin(async move {
                    let _ = rx.recv().await;
                    
                }))
            },
            Kind::Socket { socket } => {
                socket.poll_read()
            }
            _ => None
        }
    }

    pub async fn wait_write(&self) -> wasmer_vfs::Result<()> {
        match self.guard.deref() {
            Kind::File { handle, .. } => {
                if let Some(handle) = handle {
                    handle.poll_write()
                } else {
                    None
                }
            },
            Kind::EventNotifications { counter, wakers, .. } => {
                let counter = counter.clone();
                let (tx, rx) = tokio::sync::mpsc::channel(1);
                let mut wakers = wakers.lock().unwrap();
                wakers.push_back(tx);
                Some(Box::pin(async move {
                    let _ = rx.recv().await;
                    counter.load(std::sync::atomic::Ordering::Acquire) as usize
                }))
            },
            Kind::Socket { socket } => {
                socket.poll_write()
            }
            _ => None
        }
    }
}

struct InodeValFilePollGuardReadJoin<'a> {
    guard: &'a InodeValFilePollGuard<'a>,
    notifications: Option<mpsc::Receiver<()>>,
    socket: Option<Pin<Box<dyn Future<Output=wasmer_vnet::Result<()>> + 'static>>>
}
impl<'a> Future
for InodeValFilePollGuardReadJoin<'a>
{
    type Output = wasmer_vfs::Result<()>;

    fn poll(self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        match self.guard.guard.deref() {
            Kind::File { handle, .. } => {
                if let Some(handle) = handle {
                    handle.poll_read_ready(cx).map(|_| Ok(()))
                } else {
                    Poll::Ready(Err(FsError::WouldBlock))
                }
            },
            Kind::EventNotifications { counter, wakers, .. } => {
                if self.notifications.is_none() {
                    let (tx, rx) = tokio::sync::mpsc::channel(1);
                    let mut wakers = wakers.lock().unwrap();
                    wakers.push_back(tx);
                    self.notifications.replace(rx);
                }
                let notifications = Pin::new(self.notifications.as_mut().unwrap());
                notifications.poll_recv(cx).map(|_| Ok(()))
            },
            Kind::Socket { socket } => {
                if self.socket.is_none() {
                    let socket = socket.clone();
                    let work = socket.poll_read();
                    self.socket.replace(Box::pin(work));
                }
                let socket = self.socket.as_mut().unwrap().as_mut();
                socket.poll(cx).map_err(net_error_into_io_err).map_err(Into::<FsError>::into)
            }
            _ => Poll::Ready(Err(FsError::WouldBlock))
        }
    }
}

struct InodeValFilePollGuardWriteJoin<'a> {
    guard: &'a InodeValFilePollGuard<'a>,
    notifications: Option<mpsc::Receiver<()>>,
    socket: Option<Pin<Box<dyn Future<Output=wasmer_vnet::Result<()>> + 'static>>>
}
impl<'a> Future
for InodeValFilePollGuardWriteJoin<'a>
{
    type Output = wasmer_vfs::Result<()>;

    fn poll(self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        match self.guard.guard.deref() {
            Kind::File { handle, .. } => {
                if let Some(handle) = handle {
                    handle.poll_write_ready(cx).map(|_| Ok(()))
                } else {
                    Poll::Ready(Err(FsError::WouldBlock))
                }
            },
            Kind::EventNotifications { counter, wakers, .. } => {
                if self.notifications.is_none() {
                    let (tx, rx) = tokio::sync::mpsc::channel(1);
                    let mut wakers = wakers.lock().unwrap();
                    wakers.push_back(tx);
                    self.notifications.replace(rx);
                }
                let notifications = Pin::new(self.notifications.as_mut().unwrap());
                notifications.poll_recv(cx).map(|_| Ok(()))
            },
            Kind::Socket { socket } => {
                if self.socket.is_none() {
                    let socket = socket.clone();
                    let work = socket.poll_write();
                    self.socket.replace(Box::pin(work));
                }
                let socket = self.socket.as_mut().unwrap().as_mut();
                socket.poll(cx).map_err(net_error_into_io_err).map_err(Into::<FsError>::into)
            }
            _ => Poll::Ready(Err(FsError::WouldBlock))
        }
    }
}

#[derive(Debug)]
pub(crate) struct InodeValFileReadGuard<'a> {
    pub(crate) guard: RwLockReadGuard<'a, Kind>,
}

impl<'a> InodeValFileReadGuard<'a> {
    pub fn into_poll_guard(self) -> InodeValFilePollGuard::<'a> {
        InodeValFilePollGuard {
            guard: self.guard
        }
    }
}

impl<'a> Deref for InodeValFileReadGuard<'a> {
    type Target = Option<Box<dyn VirtualFile + Send + Sync + 'static>>;
    fn deref(&self) -> &Self::Target {
        if let Kind::File { handle, .. } = self.guard.deref() {
            return handle;
        }
        unreachable!()
    }
}

#[derive(Debug)]
pub struct InodeValFileWriteGuard<'a> {
    pub(crate) guard: RwLockWriteGuard<'a, Kind>,
}

impl<'a> Deref for InodeValFileWriteGuard<'a> {
    type Target = Option<Box<dyn VirtualFile + Send + Sync + 'static>>;
    fn deref(&self) -> &Self::Target {
        if let Kind::File { handle, .. } = self.guard.deref() {
            return handle;
        }
        unreachable!()
    }
}

impl<'a> DerefMut for InodeValFileWriteGuard<'a> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        if let Kind::File { handle, .. } = self.guard.deref_mut() {
            return handle;
        }
        unreachable!()
    }
}

#[derive(Debug)]
pub(crate) struct WasiStateFileGuard {
    inodes: Arc<RwLock<WasiInodes>>,
    inode: generational_arena::Index,
}

impl WasiStateFileGuard {
    pub fn new(state: &WasiState, fd: __wasi_fd_t) -> Result<Option<Self>, FsError> {
        let inodes = state.inodes.read().unwrap();
        let fd_map = state.fs.fd_map.read().unwrap();
        if let Some(fd) = fd_map.get(&fd) {
            let guard = inodes.arena[fd.inode].read();
            if let Kind::File { .. } = guard.deref() {
                Ok(Some(Self {
                    inodes: state.inodes.clone(),
                    inode: fd.inode,
                }))
            } else {
                // Our public API should ensure that this is not possible
                Err(FsError::NotAFile)
            }
        } else {
            Ok(None)
        }
    }

    pub fn lock_read<'a>(
        &self,
        inodes: &'a RwLockReadGuard<WasiInodes>,
    ) -> InodeValFileReadGuard<'a> {
        let guard = inodes.arena[self.inode].read();
        if let Kind::File { .. } = guard.deref() {
            InodeValFileReadGuard { guard }
        } else {
            // Our public API should ensure that this is not possible
            unreachable!("Non-file found in standard device location")
        }
    }

    pub fn lock_write<'a>(
        &self,
        inodes: &'a RwLockReadGuard<WasiInodes>,
    ) -> InodeValFileWriteGuard<'a> {
        let guard = inodes.arena[self.inode].write();
        if let Kind::File { .. } = guard.deref() {
            InodeValFileWriteGuard { guard }
        } else {
            // Our public API should ensure that this is not possible
            unreachable!("Non-file found in standard device location")
        }
    }
}

impl VirtualFile for WasiStateFileGuard {
    fn last_accessed(&self) -> u64 {
        let inodes = self.inodes.read().unwrap();
        let guard = self.lock_read(&inodes);
        if let Some(file) = guard.deref() {
            file.last_accessed()
        } else {
            0
        }
    }

    fn last_modified(&self) -> u64 {
        let inodes = self.inodes.read().unwrap();
        let guard = self.lock_read(&inodes);
        if let Some(file) = guard.deref() {
            file.last_modified()
        } else {
            0
        }
    }

    fn created_time(&self) -> u64 {
        let inodes = self.inodes.read().unwrap();
        let guard = self.lock_read(&inodes);
        if let Some(file) = guard.deref() {
            file.created_time()
        } else {
            0
        }
    }

    fn size(&self) -> u64 {
        let inodes = self.inodes.read().unwrap();
        let guard = self.lock_read(&inodes);
        if let Some(file) = guard.deref() {
            file.size()
        } else {
            0
        }
    }

    fn set_len(&mut self, new_size: u64) -> Result<(), FsError> {
        let inodes = self.inodes.read().unwrap();
        let mut guard = self.lock_write(&inodes);
        if let Some(file) = guard.deref_mut() {
            file.set_len(new_size)
        } else {
            Err(FsError::IOError)
        }
    }

    fn unlink(&mut self) -> Result<(), FsError> {
        let inodes = self.inodes.read().unwrap();
        let mut guard = self.lock_write(&inodes);
        if let Some(file) = guard.deref_mut() {
            file.unlink()
        } else {
            Err(FsError::IOError)
        }
    }

    fn sync_to_disk(&self) -> Result<(), FsError> {
        let inodes = self.inodes.read().unwrap();
        let guard = self.lock_read(&inodes);
        if let Some(file) = guard.deref() {
            file.sync_to_disk()
        } else {
            Err(FsError::IOError)
        }
    }

    fn bytes_available(&self) -> Result<usize, FsError> {
        let inodes = self.inodes.read().unwrap();
        let guard = self.lock_read(&inodes);
        if let Some(file) = guard.deref() {
            file.bytes_available()
        } else {
            Err(FsError::IOError)
        }
    }

    fn bytes_available_read(&self) -> Result<Option<usize>, FsError> {
        let inodes = self.inodes.read().unwrap();
        let guard = self.lock_read(&inodes);
        if let Some(file) = guard.deref() {
            file.bytes_available_read()
        } else {
            Err(FsError::IOError)
        }
    }

    fn bytes_available_write(&self) -> Result<Option<usize>, FsError> {
        let inodes = self.inodes.read().unwrap();
        let guard = self.lock_read(&inodes);
        if let Some(file) = guard.deref() {
            file.bytes_available_write()
        } else {
            Err(FsError::IOError)
        }
    }

    fn is_open(&self) -> bool {
        let inodes = self.inodes.read().unwrap();
        let guard = self.lock_read(&inodes);
        if let Some(file) = guard.deref() {
            file.is_open()
        } else {
            false
        }
    }

    fn get_fd(&self) -> Option<wasmer_vfs::FileDescriptor> {
        let inodes = self.inodes.read().unwrap();
        let guard = self.lock_read(&inodes);
        if let Some(file) = guard.deref() {
            file.get_fd()
        } else {
            None
        }
    }
}

impl Write for WasiStateFileGuard {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let inodes = self.inodes.read().unwrap();
        let mut guard = self.lock_write(&inodes);
        if let Some(file) = guard.deref_mut() {
            file.write(buf)
        } else {
            Err(std::io::ErrorKind::Unsupported.into())
        }
    }

    fn write_vectored(&mut self, bufs: &[std::io::IoSlice<'_>]) -> std::io::Result<usize> {
        let inodes = self.inodes.read().unwrap();
        let mut guard = self.lock_write(&inodes);
        if let Some(file) = guard.deref_mut() {
            file.write_vectored(bufs)
        } else {
            Err(std::io::ErrorKind::Unsupported.into())
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let inodes = self.inodes.read().unwrap();
        let mut guard = self.lock_write(&inodes);
        if let Some(file) = guard.deref_mut() {
            file.flush()
        } else {
            Err(std::io::ErrorKind::Unsupported.into())
        }
    }
}

impl Read for WasiStateFileGuard {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let inodes = self.inodes.read().unwrap();
        let mut guard = self.lock_write(&inodes);
        if let Some(file) = guard.deref_mut() {
            file.read(buf)
        } else {
            Err(std::io::ErrorKind::Unsupported.into())
        }
    }

    fn read_vectored(&mut self, bufs: &mut [std::io::IoSliceMut<'_>]) -> std::io::Result<usize> {
        let inodes = self.inodes.read().unwrap();
        let mut guard = self.lock_write(&inodes);
        if let Some(file) = guard.deref_mut() {
            file.read_vectored(bufs)
        } else {
            Err(std::io::ErrorKind::Unsupported.into())
        }
    }
}

impl Seek for WasiStateFileGuard {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        let inodes = self.inodes.read().unwrap();
        let mut guard = self.lock_write(&inodes);
        if let Some(file) = guard.deref_mut() {
            file.seek(pos)
        } else {
            Err(std::io::ErrorKind::Unsupported.into())
        }
    }
}
