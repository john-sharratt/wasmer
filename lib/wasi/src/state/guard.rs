use tokio::sync::mpsc;
use wasmer_vnet::net_error_into_io_err;

use super::*;
use std::{
    io::{Read, Seek},
    sync::{RwLockReadGuard, RwLockWriteGuard}, future::Future, pin::Pin, task::Poll,
};

pub(crate) enum InodeValFilePollGuardMode<'a> {
    Guard(RwLockReadGuard<'a, Kind>),
    EventNotifications {
        immediate: bool,
        waker: Mutex<mpsc::Receiver<()>>,
        counter: Arc<AtomicU64>,
        wakers: Arc<Mutex<VecDeque<tokio::sync::mpsc::Sender<()>>>>
    },
    Socket(InodeSocket)
}

pub(crate) struct InodeValFilePollGuard<'a> {
    pub(crate) fd: u32,
    pub(crate) mode: InodeValFilePollGuardMode<'a>,
    pub(crate) subscriptions: HashMap<PollEventSet, WasiSubscription>,
}
impl<'a> InodeValFilePollGuard<'a> {
    pub(crate) fn new(fd: u32, guard: RwLockReadGuard<'a, Kind>, subscriptions: HashMap<PollEventSet, WasiSubscription>) -> Self {
        let mode = match guard.deref() {
            Kind::EventNotifications { counter, wakers, immediate, .. } => {
                let (tx, rx) = tokio::sync::mpsc::channel(1);
                let immediate = {
                    let mut wakers = wakers.lock().unwrap();
                    wakers.push_back(tx);
                    immediate.compare_exchange(true, false, Ordering::AcqRel, Ordering::Relaxed).is_ok()
                };
                InodeValFilePollGuardMode::EventNotifications {
                    immediate,
                    waker: Mutex::new(rx),
                    counter: counter.clone(),
                    wakers: wakers.clone(),
                }
            },
            Kind::Socket { socket } => InodeValFilePollGuardMode::Socket(socket.clone()),
            _ => InodeValFilePollGuardMode::Guard(guard),
        };
        Self {
            fd,
            mode,
            subscriptions
        }
    }
}

impl<'a> std::fmt::Debug
for InodeValFilePollGuard<'a>
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.mode {
            InodeValFilePollGuardMode::Guard(guard) => {
                match guard.deref() {
                    Kind::File { .. } => write!(f, "guard-file"),
                    _ => write!(f, "guard-unknown"),
                }
            },
            InodeValFilePollGuardMode::EventNotifications { .. } => write!(f, "guard-notifications"),
            InodeValFilePollGuardMode::Socket(socket) => {
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
            }
        }
    }
}

impl<'a> InodeValFilePollGuard<'a> {
    pub fn bytes_available_read(&self) -> wasmer_vfs::Result<Option<usize>> {
        match &self.mode {
            InodeValFilePollGuardMode::Guard(guard) => {
                match guard.deref() {
                    Kind::File { handle, .. } => {
                        if let Some(handle) = handle {
                            handle.bytes_available_read()
                        } else {
                            Ok(None)
                        }
                    },
                    _ => Ok(None)
                }
            },
            InodeValFilePollGuardMode::EventNotifications { counter, .. } => {
                Ok(
                    Some(counter.load(std::sync::atomic::Ordering::Acquire) as usize)
                )
            },
            InodeValFilePollGuardMode::Socket(socket) => {
                socket.peek()
                    .map(|a| Some(a))
                    .map_err(fs_error_from_wasi_err)
            }
        }
    }

    pub fn bytes_available_write(&self) -> wasmer_vfs::Result<Option<usize>> {
        match &self.mode {
            InodeValFilePollGuardMode::Guard(guard) => {
                match guard.deref() {
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
                    },
                    _ => Ok(None)
                }
            },
            InodeValFilePollGuardMode::EventNotifications { wakers, .. } => {
                let wakers = wakers.lock().unwrap();
                Ok(
                    Some(wakers.len())
                )
            },
            InodeValFilePollGuardMode::Socket(socket) => {
                if socket.can_write() {
                    Ok(Some(4096))
                } else {
                    Ok(Some(0))
                }
            }
        }
    }

    pub fn is_open(&self) -> bool{
        match &self.mode {
            InodeValFilePollGuardMode::Guard(guard) => {
                match guard.deref() {
                    Kind::File { handle, .. } => {
                        if let Some(handle) = handle {
                            handle.is_open()
                        } else {
                            false
                        }
                    },
                    _ => false
                }
            },
            InodeValFilePollGuardMode::EventNotifications { .. } |
            InodeValFilePollGuardMode::Socket(..) => {
                true
            }
        }
    }

    pub async fn wait(&'a self) -> __wasi_event_t {
        InodeValFilePollGuardJoin::new(self).await
    }
}

struct InodeValFilePollGuardJoin<'a> {
    mode: &'a InodeValFilePollGuardMode<'a>,
    subscriptions: HashMap<PollEventSet, WasiSubscription>,
}
impl<'a> InodeValFilePollGuardJoin<'a> {
    fn new(guard: &'a InodeValFilePollGuard<'a>) -> Self {
        Self {
            mode: &guard.mode,
            subscriptions: guard.subscriptions.clone(),
        }
    }
}
impl<'a> Future
for InodeValFilePollGuardJoin<'a>
{
    type Output = __wasi_event_t;

    fn poll(self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        for (set, s) in self.subscriptions.iter() {
            for in_event in iterate_poll_events(*set) {
                match in_event {
                    PollEvent::PollIn => {
                        let poll_result = match &self.mode {
                            InodeValFilePollGuardMode::Guard(guard) => {
                                match guard.deref().deref() {
                                    Kind::File { handle, .. } => {
                                        if let Some(handle) = handle {
                                            handle.poll_read_ready(cx)
                                        } else {
                                            Poll::Ready(Err(FsError::WouldBlock))
                                        }
                                    },
                                    _ => Poll::Ready(Err(FsError::WouldBlock))
                                }
                            },
                            InodeValFilePollGuardMode::EventNotifications { waker, counter, immediate, .. } => {
                                if *immediate {
                                    let cnt = counter.load(Ordering::Acquire);
                                    Poll::Ready(Ok(cnt as usize))
                                } else {
                                    let counter = counter.clone();
                                    let mut waker = waker.lock().unwrap();
                                    let mut notifications = Pin::new(waker.deref_mut());
                                    notifications.poll_recv(cx).map(|_| {
                                        let cnt = counter.load(Ordering::Acquire);
                                        Ok(cnt as usize)
                                    })
                                }
                            },
                            InodeValFilePollGuardMode::Socket(socket) => {
                                socket.poll_read_ready(cx)
                                    .map_err(net_error_into_io_err)
                                    .map_err(Into::<FsError>::into)
                            }
                        };
                        if let Poll::Ready(bytes_available) = poll_result {
                            return Poll::Ready(__wasi_event_t {
                                userdata: s.user_data,
                                error: bytes_available.clone().map(|_| __WASI_ESUCCESS).unwrap_or_else(fs_error_into_wasi_err),
                                type_: s.event_type.raw_tag(),
                                u: {
                                    __wasi_event_u {
                                        fd_readwrite: __wasi_event_fd_readwrite_t {
                                            nbytes: bytes_available.unwrap_or_default() as u64,
                                            flags: 0,
                                        },
                                    }
                                },
                            });
                        }
                    },
                    PollEvent::PollOut => {
                        let poll_result = match self.mode {
                            InodeValFilePollGuardMode::Guard(guard) => {
                                match guard.deref().deref() {
                                    Kind::File { handle, .. } => {
                                        if let Some(handle) = handle {
                                            handle.poll_write_ready(cx)
                                        } else {
                                            Poll::Ready(Err(FsError::WouldBlock))
                                        }
                                    },
                                    _ => Poll::Ready(Err(FsError::WouldBlock))
                                }
                            },
                            InodeValFilePollGuardMode::EventNotifications { waker, counter, immediate, .. } => {
                                if *immediate {
                                    let cnt = counter.load(Ordering::Acquire);
                                    Poll::Ready(Ok(cnt as usize))
                                } else {
                                    let counter = counter.clone();
                                    let mut waker = waker.lock().unwrap();
                                    let mut notifications = Pin::new(waker.deref_mut());
                                    notifications.poll_recv(cx).map(|_| {
                                        let cnt = counter.load(Ordering::Acquire);
                                        Ok(cnt as usize)
                                    })
                                }
                            },
                            InodeValFilePollGuardMode::Socket(socket) => {
                                socket.poll_write_ready(cx)
                                    .map_err(net_error_into_io_err)
                                    .map_err(Into::<FsError>::into)
                            }
                        };
                        if let Poll::Ready(bytes_available) = poll_result {
                            return Poll::Ready(__wasi_event_t {
                                userdata: s.user_data,
                                error: bytes_available.clone().map(|_| __WASI_ESUCCESS).unwrap_or_else(fs_error_into_wasi_err),
                                type_: s.event_type.raw_tag(),
                                u: {
                                    __wasi_event_u {
                                        fd_readwrite: __wasi_event_fd_readwrite_t {
                                            nbytes: bytes_available.unwrap_or_default() as u64,
                                            flags: 0,
                                        },
                                    }
                                },
                            });
                        }
                    },
                    PollEvent::PollError |
                    PollEvent::PollHangUp |
                    PollEvent::PollInvalid => {
                        let poll_result = match self.mode {
                            InodeValFilePollGuardMode::Guard(guard) => {
                                match guard.deref().deref() {
                                    Kind::File { handle, .. } => {
                                        if let Some(handle) = handle {
                                            handle.poll_close_ready(cx).map(|_| Ok(()))
                                        } else {
                                            Poll::Ready(Err(FsError::WouldBlock))
                                        }
                                    },
                                    _ => Poll::Ready(Err(FsError::WouldBlock))
                                }
                            },
                            InodeValFilePollGuardMode::EventNotifications { .. } => {
                                Poll::Ready(Err(FsError::WouldBlock))
                            },
                            InodeValFilePollGuardMode::Socket(..) => {
                                Poll::Ready(Err(FsError::WouldBlock))
                            }
                        };
                        if let Poll::Ready(bytes_available) = poll_result {
                            return Poll::Ready(__wasi_event_t {
                                userdata: s.user_data,
                                error: bytes_available.clone().map(|_| __WASI_ESUCCESS).unwrap_or_else(fs_error_into_wasi_err),
                                type_: s.event_type.raw_tag(),
                                u: {
                                    __wasi_event_u {
                                        fd_readwrite: __wasi_event_fd_readwrite_t {
                                            nbytes: 0,
                                            flags: if let PollEvent::PollHangUp = in_event {
                                                __WASI_EVENT_FD_READWRITE_HANGUP
                                            } else { 0 },
                                        },
                                    }
                                },
                            });
                        }
                    }
                }
            }
        }
        Poll::Pending
    }
}

#[derive(Debug)]
pub(crate) struct InodeValFileReadGuard<'a> {
    pub(crate) guard: RwLockReadGuard<'a, Kind>,
}

impl<'a> InodeValFileReadGuard<'a> {
    pub fn into_poll_guard(self, fd: u32, subscriptions: HashMap<PollEventSet, WasiSubscription>) -> InodeValFilePollGuard::<'a> {
        InodeValFilePollGuard::new(fd, self.guard, subscriptions)
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
