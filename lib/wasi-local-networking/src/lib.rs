#![allow(unused_variables)]
use bytes::{Bytes, BytesMut};
use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr};
use std::sync::Mutex;
use std::time::Duration;
#[allow(unused_imports, dead_code)]
use tracing::{debug, error, info, trace, warn};
#[allow(unused_imports)]
use wasmer_vnet::{
    io_err_into_net_error, IpCidr, IpRoute, NetworkError, Result, SocketHttpRequest, SocketReceive,
    SocketReceiveFrom, SocketStatus, StreamSecurity, TimeType, VirtualConnectedSocket,
    VirtualConnectionlessSocket, VirtualIcmpSocket, VirtualNetworking, VirtualRawSocket,
    VirtualSocket, VirtualTcpListener, VirtualTcpSocket, VirtualUdpSocket, VirtualWebSocket,
};

#[derive(Debug, Default)]
pub struct LocalNetworking {}

#[async_trait::async_trait]
#[allow(unused_variables)]
impl VirtualNetworking for LocalNetworking {
    fn listen_tcp(
        &self,
        addr: SocketAddr,
        only_v6: bool,
        reuse_port: bool,
        reuse_addr: bool,
    ) -> Result<Box<dyn VirtualTcpListener + Sync>> {
        let listener = std::net::TcpListener::bind(addr)
            .map(|sock| {
                Box::new(LocalTcpListener {
                    stream: LocalTcpListenerMode::Blocking(sock),
                    timeout: None,
                    backlog: Mutex::new(Vec::new()),
                    nonblocking: false,
                })
            })
            .map_err(io_err_into_net_error)?;
        Ok(listener)
    }

    async fn listen_tcp_async(
        &self,
        addr: SocketAddr,
        only_v6: bool,
        reuse_port: bool,
        reuse_addr: bool,
    ) -> Result<Box<dyn VirtualTcpListener + Sync>> {
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .map(|sock| {
                Box::new(LocalTcpListener {
                    stream: LocalTcpListenerMode::Async(sock),
                    timeout: None,
                    backlog: Mutex::new(Vec::new()),
                    nonblocking: false,
                })
            })
            .map_err(io_err_into_net_error)?;
        Ok(listener)
    }

    fn bind_udp(
        &self,
        addr: SocketAddr,
        _reuse_port: bool,
        _reuse_addr: bool,
    ) -> Result<Box<dyn VirtualUdpSocket + Sync>> {
        let socket = std::net::UdpSocket::bind(addr)
            .map_err(io_err_into_net_error)?;
        Ok(Box::new(LocalUdpSocket {
            socket: LocalUdpSocketMode::Blocking(socket),
            addr,
            nonblocking: false
        }))
    }

    async fn bind_udp_async(
        &self,
        addr: SocketAddr,
        _reuse_port: bool,
        _reuse_addr: bool,
    ) -> Result<Box<dyn VirtualUdpSocket + Sync>> {
        let socket = tokio::net::UdpSocket::bind(addr)
            .await
            .map_err(io_err_into_net_error)?;
        Ok(Box::new(LocalUdpSocket {
            socket: LocalUdpSocketMode::Async(socket),
            addr,
            nonblocking: false
        }))
    }

    fn connect_tcp(
        &self,
        _addr: SocketAddr,
        peer: SocketAddr,
        timeout: Option<Duration>,
    ) -> Result<Box<dyn VirtualTcpSocket + Sync>> {
        let stream = if let Some(timeout) = timeout {
            std::net::TcpStream::connect_timeout(&peer, timeout)
        } else {
            std::net::TcpStream::connect(peer)
        }
        .map_err(io_err_into_net_error)?;
        let peer = stream.peer_addr().map_err(io_err_into_net_error)?;
        Ok(Box::new(LocalTcpStream {
            stream: LocalTcpStreamMode::Blocking(stream),
            addr: peer,
            connect_timeout: None,
            read_timeout: None,
            write_timeout: None,
            linger_timeout: None,
            nonblocking: false,
        }))
    }

    async fn connect_tcp_async(
        &self,
        _addr: SocketAddr,
        peer: SocketAddr,
        timeout: Option<Duration>,
    ) -> Result<Box<dyn VirtualTcpSocket + Sync>> {
        let stream = if let Some(timeout) = timeout {
            match tokio::time::timeout(timeout, tokio::net::TcpStream::connect(&peer))
                .await
            {
                Ok(a) => a,
                Err(err) => {
                    Err(Into::<std::io::Error>::into(std::io::ErrorKind::TimedOut))
                }
            }
        } else {
            tokio::net::TcpStream::connect(peer).await
        }
        .map_err(io_err_into_net_error)?;
        let peer = stream.peer_addr().map_err(io_err_into_net_error)?;
        Ok(Box::new(LocalTcpStream {
            stream: LocalTcpStreamMode::Async(stream),
            addr: peer,
            connect_timeout: None,
            read_timeout: None,
            write_timeout: None,
            linger_timeout: None,
            nonblocking: false,
        }))
    }

    fn resolve(
        &self,
        host: &str,
        port: Option<u16>,
        dns_server: Option<IpAddr>,
    ) -> Result<Vec<IpAddr>> {
        use std::net::ToSocketAddrs;
        Ok(if let Some(port) = port {
            let host = format!("{}:{}", host, port);
            host.to_socket_addrs()
                .map(|a| a.map(|a| a.ip()).collect::<Vec<_>>())
                .map_err(io_err_into_net_error)?
        } else {
            host.to_socket_addrs()
                .map(|a| a.map(|a| a.ip()).collect::<Vec<_>>())
                .map_err(io_err_into_net_error)?
        })
    }

    async fn resolve_async(
        &self,
        host: &str,
        port: Option<u16>,
        dns_server: Option<IpAddr>,
    ) -> Result<Vec<IpAddr>> {
        tokio::net::lookup_host(host)
            .await
            .map(|a| a.map(|a| a.ip()).collect::<Vec<_>>())
            .map_err(io_err_into_net_error)
    }
}

#[derive(Debug)]
enum LocalTcpListenerMode {
    Blocking(std::net::TcpListener),
    Async(tokio::net::TcpListener),
    Uninitialized
}

impl LocalTcpListenerMode
{
    fn as_blocking_mut(&mut self) -> std::io::Result<&mut std::net::TcpListener> {
        match self {
            Self::Blocking(a) => Ok(a),
            Self::Async(_) => {
                let mut listener = Self::Uninitialized;
                std::mem::swap(self, &mut listener);
                listener = match listener {
                    Self::Async(a) => Self::Blocking(a.into_std()?),
                    a => unreachable!(),
                };
                std::mem::swap(self, &mut listener);
                match self {
                    Self::Blocking(a) => Ok(a),
                    _ => unreachable!()
                }
            },
            Self::Uninitialized => unreachable!()
        }
    }

    fn as_async_mut(&mut self) -> std::io::Result<&mut tokio::net::TcpListener> {
        match self {
            Self::Async(a) => Ok(a),
            Self::Blocking(_) => {
                let mut listener = Self::Uninitialized;
                std::mem::swap(self, &mut listener);
                listener = match listener {
                    Self::Blocking(a) => Self::Async(tokio::net::TcpListener::from_std(a)?),
                    a => unreachable!(),
                };
                std::mem::swap(self, &mut listener);
                match self {
                    Self::Async(a) => Ok(a),
                    _ => unreachable!()
                }
            },
            Self::Uninitialized => unreachable!()
        }
    }
}

#[derive(Debug)]
pub struct LocalTcpListener {
    stream: LocalTcpListenerMode,
    timeout: Option<Duration>,
    backlog: Mutex<Vec<(Box<LocalTcpStream>, SocketAddr)>>,
    nonblocking: bool,
}

#[async_trait::async_trait]
impl VirtualTcpListener for LocalTcpListener {
    fn accept(&mut self) -> Result<(Box<dyn VirtualTcpSocket + Sync>, SocketAddr)> {
        {
            let mut backlog = self.backlog.lock().unwrap();
            if let Some((sock, addr)) = backlog.pop() {
                return Ok((sock, addr));
            }
        }
        
        let stream = self.stream.as_blocking_mut().map_err(io_err_into_net_error)?;
        let (sock, addr) = stream
            .accept()
            .map(|(sock, addr)| {
                (
                    Box::new(LocalTcpStream {
                        stream: LocalTcpStreamMode::Blocking(sock),
                        addr,
                        connect_timeout: None,
                        read_timeout: None,
                        write_timeout: None,
                        linger_timeout: None,
                        nonblocking: false,
                    }),
                    addr,
                )
            })
            .map_err(io_err_into_net_error)?;
        Ok((sock, addr))
    }

    async fn accept_async(&mut self) -> Result<(Box<dyn VirtualTcpSocket + Sync>, SocketAddr)> {
        {
            let mut backlog = self.backlog.lock().unwrap();
            if let Some((sock, addr)) = backlog.pop() {
                return Ok((sock, addr));
            }
        }
        let stream = self.stream.as_async_mut().map_err(io_err_into_net_error)?;
        let (sock, addr) = stream
            .accept()
            .await
            .map(|(sock, addr)| {
                (
                    Box::new(LocalTcpStream {
                        stream: LocalTcpStreamMode::Async(sock),
                        addr,
                        connect_timeout: None,
                        read_timeout: None,
                        write_timeout: None,
                        linger_timeout: None,
                        nonblocking: false,
                    }),
                    addr,
                )
            })
            .map_err(io_err_into_net_error)?;
        Ok((sock, addr))
    }

    fn peek(&mut self) -> Result<usize> {
        {
            let backlog = self.backlog.lock().unwrap();
            if backlog.is_empty() == false {
                return Ok(backlog.len());
            }
        }

        let mut ret_err = None;
        let stream = self.stream.as_blocking_mut().map_err(io_err_into_net_error)?;
        stream.set_nonblocking(true).map_err(io_err_into_net_error)?;
        loop {
            match stream.accept()
            {
                Ok((sock, addr)) => {
                    let mut backlog = self.backlog.lock().unwrap();
                    backlog.push((
                        Box::new(LocalTcpStream {
                            stream: LocalTcpStreamMode::Blocking(sock),
                            addr,
                            connect_timeout: None,
                            read_timeout: None,
                            write_timeout: None,
                            linger_timeout: None,
                            nonblocking: false,
                        }),
                        addr,
                    ));
                },
                Err(err) if err.kind() == std::io::ErrorKind::TimedOut ||
                                err.kind() == std::io::ErrorKind::WouldBlock => {
                    break;
                },
                Err(err) => {
                    ret_err = Some(io_err_into_net_error(err));
                    break;
                }
            }
        }
        let _ = stream.set_nonblocking(self.nonblocking);
        if let Some(err) = ret_err {
            return Err(err);
        }

        let backlog = self.backlog.lock().unwrap();
        Ok(backlog.len())
    }

    async fn wait_accept(&mut self) -> Result<usize> {
        let (sock, addr) = self.stream
            .as_async_mut()
            .map_err(io_err_into_net_error)?
            .accept()
            .await
            .map_err(io_err_into_net_error)?;
        
        let mut backlog = self.backlog.lock().unwrap();
        backlog.push((
            Box::new(LocalTcpStream {
                stream: LocalTcpStreamMode::Async(sock),
                addr,
                connect_timeout: None,
                read_timeout: None,
                write_timeout: None,
                linger_timeout: None,
                nonblocking: false,
            }),
            addr,
        ));
        Ok(backlog.len())
    }

    /// Sets the accept timeout
    fn set_timeout(&mut self, timeout: Option<Duration>) -> Result<()> {
        self.timeout = timeout;
        Ok(())
    }

    /// Gets the accept timeout
    fn timeout(&self) -> Result<Option<Duration>> {
        Ok(self.timeout)
    }

    fn addr_local(&self) -> Result<SocketAddr> {
        match &self.stream {
            LocalTcpListenerMode::Blocking(a) => a.local_addr().map_err(io_err_into_net_error),
            LocalTcpListenerMode::Async(a) => a.local_addr().map_err(io_err_into_net_error),
            LocalTcpListenerMode::Uninitialized => unreachable!()
        }
    }

    fn set_ttl(&mut self, ttl: u8) -> Result<()> {
        match &mut self.stream {
            LocalTcpListenerMode::Blocking(a) => a.set_ttl(ttl as u32).map_err(io_err_into_net_error),
            LocalTcpListenerMode::Async(a) => a.set_ttl(ttl as u32).map_err(io_err_into_net_error),
            LocalTcpListenerMode::Uninitialized => unreachable!()
        }
    }

    fn ttl(&self) -> Result<u8> {
        match &self.stream {
            LocalTcpListenerMode::Blocking(a) => a.ttl().map(|ttl| ttl as u8).map_err(io_err_into_net_error),
            LocalTcpListenerMode::Async(a) => a.ttl().map(|ttl| ttl as u8).map_err(io_err_into_net_error),
            LocalTcpListenerMode::Uninitialized => unreachable!()
        }
    }

    fn set_nonblocking(&mut self, nonblocking: bool) -> Result<()> {
        self.nonblocking = nonblocking;
        self.stream
            .as_blocking_mut()
            .map_err(io_err_into_net_error)?
            .set_nonblocking(nonblocking)
            .map_err(io_err_into_net_error)?;
        Ok(())
    }

    fn nonblocking(&self) -> Result<bool> {
        Ok(self.nonblocking)
    }
}

#[derive(Debug)]
pub struct LocalTcpStream {
    stream: LocalTcpStreamMode,
    addr: SocketAddr,
    read_timeout: Option<Duration>,
    write_timeout: Option<Duration>,
    connect_timeout: Option<Duration>,
    linger_timeout: Option<Duration>,
    nonblocking: bool,
}

#[derive(Debug)]
enum LocalTcpStreamMode {
    Blocking(std::net::TcpStream),
    Async(tokio::net::TcpStream),
    Uninitialized
}

impl LocalTcpStreamMode
{
    fn as_blocking_mut(&mut self) -> std::io::Result<&mut std::net::TcpStream> {
        match self {
            Self::Blocking(a) => Ok(a),
            Self::Async(_) => {
                let mut listener = Self::Uninitialized;
                std::mem::swap(self, &mut listener);
                listener = match listener {
                    Self::Async(a) => Self::Blocking(a.into_std()?),
                    a => unreachable!(),
                };
                std::mem::swap(self, &mut listener);
                match self {
                    Self::Blocking(a) => Ok(a),
                    _ => unreachable!()
                }
            },
            Self::Uninitialized => unreachable!()
        }
    }

    fn as_async_mut(&mut self) -> std::io::Result<&mut tokio::net::TcpStream> {
        match self {
            Self::Async(a) => Ok(a),
            Self::Blocking(_) => {
                let mut listener = Self::Uninitialized;
                std::mem::swap(self, &mut listener);
                listener = match listener {
                    Self::Blocking(a) => Self::Async(tokio::net::TcpStream::from_std(a)?),
                    a => unreachable!(),
                };
                std::mem::swap(self, &mut listener);
                match self {
                    Self::Async(a) => Ok(a),
                    _ => unreachable!()
                }
            },
            Self::Uninitialized => unreachable!()
        }
    }
}

#[async_trait::async_trait]
impl VirtualTcpSocket for LocalTcpStream {
    fn set_opt_time(&mut self, ty: TimeType, timeout: Option<Duration>) -> Result<()> {
        match ty {
            TimeType::ReadTimeout => {
                self.read_timeout = timeout.clone();
                self
                    .stream
                    .as_blocking_mut()
                    .map_err(io_err_into_net_error)?
                    .set_read_timeout(timeout)
                    .map_err(io_err_into_net_error)
            },
            TimeType::WriteTimeout => {
                self.write_timeout = timeout.clone();
                self
                    .stream
                    .as_blocking_mut()
                    .map_err(io_err_into_net_error)?
                    .set_write_timeout(timeout)
                    .map_err(io_err_into_net_error)
            },
            TimeType::ConnectTimeout => {
                self.connect_timeout = timeout;
                Ok(())
            }
            #[cfg(feature = "wasix")]
            TimeType::Linger => {
                self.linger_timeout = timeout.clone();
                self
                    .stream
                    .as_blocking_mut()
                    .map_err(io_err_into_net_error)?
                    .set_linger(timeout)
                    .map_err(io_err_into_net_error)
            },
            _ => Err(NetworkError::InvalidInput),
        }
    }

    fn opt_time(&self, ty: TimeType) -> Result<Option<Duration>> {
        match ty {
            TimeType::ReadTimeout => Ok(self.read_timeout),
            TimeType::WriteTimeout => Ok(self.write_timeout),
            TimeType::ConnectTimeout => Ok(self.connect_timeout),
            TimeType::Linger => Ok(self.linger_timeout),
            _ => Err(NetworkError::InvalidInput),
        }
    }

    fn set_recv_buf_size(&mut self, size: usize) -> Result<()> {
        Ok(())
    }

    fn recv_buf_size(&self) -> Result<usize> {
        Err(NetworkError::Unsupported)
    }

    fn set_send_buf_size(&mut self, size: usize) -> Result<()> {
        Ok(())
    }

    fn send_buf_size(&self) -> Result<usize> {
        Err(NetworkError::Unsupported)
    }

    fn set_nodelay(&mut self, nodelay: bool) -> Result<()> {
        match &mut self.stream {
            LocalTcpStreamMode::Blocking(a) => a.set_nodelay(nodelay).map_err(io_err_into_net_error),
            LocalTcpStreamMode::Async(a) => a.set_nodelay(nodelay).map_err(io_err_into_net_error),
            LocalTcpStreamMode::Uninitialized => unreachable!()
        }
    }

    fn nodelay(&self) -> Result<bool> {
        match &self.stream {
            LocalTcpStreamMode::Blocking(a) => a.nodelay().map_err(io_err_into_net_error),
            LocalTcpStreamMode::Async(a) => a.nodelay().map_err(io_err_into_net_error),
            LocalTcpStreamMode::Uninitialized => unreachable!()
        }
    }

    fn addr_peer(&self) -> Result<SocketAddr> {
        Ok(self.addr)
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }

    async fn flush_async(&mut self) -> Result<()> {
        Ok(())
    }

    fn shutdown(&mut self, how: Shutdown) -> Result<()> {
        self.stream
            .as_blocking_mut()
            .map_err(io_err_into_net_error)?
            .shutdown(how)
            .map_err(io_err_into_net_error)
    }
}

#[async_trait::async_trait]
impl VirtualConnectedSocket for LocalTcpStream {
    fn set_linger(&mut self, linger: Option<Duration>) -> Result<()> {
        #[cfg(feature = "wasix")]
        self.stream
            .set_linger(linger)
            .map_err(io_err_into_net_error)?;
        Ok(())
    }

    #[cfg(feature = "wasix")]
    fn linger(&self) -> Result<Option<Duration>> {
        self.stream.linger().map_err(io_err_into_net_error)
    }

    #[cfg(not(feature = "wasix"))]
    fn linger(&self) -> Result<Option<Duration>> {
        Ok(None)
    }

    fn send(&mut self, data: Bytes) -> Result<usize> {
        use std::io::Write;
        self.stream
            .as_blocking_mut()
            .map_err(io_err_into_net_error)?
            .write_all(&data[..])
            .map(|_| data.len())
            .map_err(io_err_into_net_error)
    }

    async fn send_async(&mut self, data: Bytes) -> Result<usize> {
        use tokio::io::AsyncWriteExt;
        self.stream
            .as_async_mut()
            .map_err(io_err_into_net_error)?
            .write_all(&data[..])
            .await
            .map(|_| data.len())
            .map_err(io_err_into_net_error)
    }

    fn flush(&mut self) -> Result<()> {
        use std::io::Write;
        self.stream
            .as_blocking_mut()
            .map_err(io_err_into_net_error)?
            .flush()
            .map_err(io_err_into_net_error)
    }

    async fn flush_async(&mut self) -> Result<()> {
        use tokio::io::AsyncWriteExt;
        self.stream
            .as_async_mut()
            .map_err(io_err_into_net_error)?
            .flush()
            .await
            .map_err(io_err_into_net_error)
    }

    fn recv(&mut self) -> Result<SocketReceive> {
        use std::io::Read;
        let buf_size = 8192;
        let mut buf = BytesMut::with_capacity(buf_size);
        let read = self
            .stream
            .as_blocking_mut()
            .map_err(io_err_into_net_error)?
            .read(&mut buf[..])
            .map_err(io_err_into_net_error)?;
        let buf = Bytes::from(buf).slice(..read);
        Ok(SocketReceive {
            data: buf,
            truncated: read == buf_size,
        })
    }

    async fn recv_async(&mut self) -> Result<SocketReceive> {
        use tokio::io::AsyncReadExt;
        let buf_size = 8192;
        let mut buf = BytesMut::with_capacity(buf_size);
        let read = self
            .stream
            .as_async_mut()
            .map_err(io_err_into_net_error)?
            .read(&mut buf[..])
            .await
            .map_err(io_err_into_net_error)?;
        let buf = Bytes::from(buf).slice(..read);
        Ok(SocketReceive {
            data: buf,
            truncated: read == buf_size,
        })
    }

    fn try_recv(&mut self) -> Result<Option<SocketReceive>> {
        use std::io::Read;
        let buf_size = 8192;
        let mut buf = BytesMut::with_capacity(buf_size);

        let stream = self.stream
            .as_blocking_mut()
            .map_err(io_err_into_net_error)?;
        stream.set_nonblocking(true).map_err(io_err_into_net_error)?;
        let read = stream.read(&mut buf[..]);
        let _ = stream.set_nonblocking(self.nonblocking);

        let read = match read {
            Ok(a) => Ok(a),
            Err(err) if err.kind() == std::io::ErrorKind::TimedOut ||
                               err.kind() == std::io::ErrorKind::WouldBlock => {
                return Ok(None);
            },
            Err(err) => Err(io_err_into_net_error(err))
        }?;
        

        let buf = Bytes::from(buf).slice(..read);
        Ok(Some(SocketReceive {
            data: buf,
            truncated: read == buf_size,
        }))
    }

    fn peek(&mut self) -> Result<SocketReceive> {
        let buf_size = 8192;
        let mut buf = BytesMut::with_capacity(buf_size);
        let read = self
            .stream
            .as_blocking_mut()
            .map_err(io_err_into_net_error)?
            .peek(&mut buf[..])
            .map_err(io_err_into_net_error)?;
        let buf = Bytes::from(buf).slice(..read);
        Ok(SocketReceive {
            data: buf,
            truncated: read == buf_size,
        })
    }
}

#[async_trait::async_trait]
impl VirtualSocket for LocalTcpStream {
    fn set_ttl(&mut self, ttl: u32) -> Result<()> {
        match &mut self.stream {
            LocalTcpStreamMode::Blocking(a) => a.set_ttl(ttl).map_err(io_err_into_net_error),
            LocalTcpStreamMode::Async(a) => a.set_ttl(ttl).map_err(io_err_into_net_error),
            LocalTcpStreamMode::Uninitialized => unreachable!()
        }
    }

    fn ttl(&self) -> Result<u32> {
        match &self.stream {
            LocalTcpStreamMode::Blocking(a) => a.ttl().map_err(io_err_into_net_error),
            LocalTcpStreamMode::Async(a) => a.ttl().map_err(io_err_into_net_error),
            LocalTcpStreamMode::Uninitialized => unreachable!()
        }
    }

    fn set_nonblocking(&mut self, nonblocking: bool) -> Result<()> {
        self.nonblocking = nonblocking;
        self.stream
            .as_blocking_mut()
            .map_err(io_err_into_net_error)?
            .set_nonblocking(nonblocking)
            .map_err(io_err_into_net_error)?;
        Ok(())
    }

    fn addr_local(&self) -> Result<SocketAddr> {
        match &self.stream {
            LocalTcpStreamMode::Blocking(a) => a.local_addr().map_err(io_err_into_net_error),
            LocalTcpStreamMode::Async(a) => a.local_addr().map_err(io_err_into_net_error),
            LocalTcpStreamMode::Uninitialized => unreachable!()
        }
    }

    fn status(&self) -> Result<SocketStatus> {
        Ok(SocketStatus::Opened)
    }

    async fn wait_read(&mut self) -> Result<()> {
        let stream = self.stream
            .as_async_mut()
            .map_err(io_err_into_net_error)?;
        let read = LocalTcpStreamReadReady { stream };
        read.await
    }

    async fn wait_write(&mut self) -> Result<()> {
        let stream = self.stream
            .as_async_mut()
            .map_err(io_err_into_net_error)?;
        let read = LocalTcpStreamWriteReady { stream };
        read.await
    }
}

struct LocalTcpStreamReadReady<'a> {
    stream: &'a mut tokio::net::TcpStream,
}
impl<'a> Future
for LocalTcpStreamReadReady<'a>
{
    type Output = Result<()>;

    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<Self::Output> {
        self.stream
            .poll_read_ready(cx)
            .map_err(io_err_into_net_error)
    }
}

struct LocalTcpStreamWriteReady<'a> {
    stream: &'a mut tokio::net::TcpStream,
}
impl<'a> Future
for LocalTcpStreamWriteReady<'a>
{
    type Output = Result<()>;

    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<Self::Output> {
        self.stream
            .poll_write_ready(cx)
            .map_err(io_err_into_net_error)
    }
}

#[derive(Debug)]
pub struct LocalUdpSocket {
    socket: LocalUdpSocketMode,
    #[allow(dead_code)]
    addr: SocketAddr,
    nonblocking: bool
}

#[derive(Debug)]
enum LocalUdpSocketMode {
    Blocking(std::net::UdpSocket),
    Async(tokio::net::UdpSocket),
    Uninitialized
}

impl LocalUdpSocketMode
{
    fn as_blocking_mut(&mut self) -> std::io::Result<&mut std::net::UdpSocket> {
        match self {
            Self::Blocking(a) => Ok(a),
            Self::Async(_) => {
                let mut listener = Self::Uninitialized;
                std::mem::swap(self, &mut listener);
                listener = match listener {
                    Self::Async(a) => Self::Blocking(a.into_std()?),
                    a => unreachable!(),
                };
                std::mem::swap(self, &mut listener);
                match self {
                    Self::Blocking(a) => Ok(a),
                    _ => unreachable!()
                }
            },
            Self::Uninitialized => unreachable!()
        }
    }

    fn as_async_mut(&mut self) -> std::io::Result<&mut tokio::net::UdpSocket> {
        match self {
            Self::Async(a) => Ok(a),
            Self::Blocking(_) => {
                let mut listener = Self::Uninitialized;
                std::mem::swap(self, &mut listener);
                listener = match listener {
                    Self::Blocking(a) => Self::Async(tokio::net::UdpSocket::from_std(a)?),
                    a => unreachable!(),
                };
                std::mem::swap(self, &mut listener);
                match self {
                    Self::Async(a) => Ok(a),
                    _ => unreachable!()
                }
            },
            Self::Uninitialized => unreachable!()
        }
    }
}

#[async_trait::async_trait]
impl VirtualUdpSocket for LocalUdpSocket {
    fn connect(&mut self, addr: SocketAddr) -> Result<()> {
        self.socket
            .as_blocking_mut()
            .map_err(io_err_into_net_error)?
            .connect(addr)
            .map_err(io_err_into_net_error)
    }

    async fn connect_async(&mut self, addr: SocketAddr) -> Result<()> {
        self.socket
            .as_async_mut()
            .map_err(io_err_into_net_error)?
            .connect(addr)
            .await
            .map_err(io_err_into_net_error)
    }

    fn set_broadcast(&mut self, broadcast: bool) -> Result<()> {
        match &mut self.socket {
            LocalUdpSocketMode::Blocking(a) => a.set_broadcast(broadcast).map_err(io_err_into_net_error),
            LocalUdpSocketMode::Async(a) => a.set_broadcast(broadcast).map_err(io_err_into_net_error),
            LocalUdpSocketMode::Uninitialized => unreachable!()
        }
    }

    fn broadcast(&self) -> Result<bool> {
        match &self.socket {
            LocalUdpSocketMode::Blocking(a) => a.broadcast().map_err(io_err_into_net_error),
            LocalUdpSocketMode::Async(a) => a.broadcast().map_err(io_err_into_net_error),
            LocalUdpSocketMode::Uninitialized => unreachable!()
        }
    }

    fn set_multicast_loop_v4(&mut self, val: bool) -> Result<()> {
        match &mut self.socket {
            LocalUdpSocketMode::Blocking(a) => a.set_multicast_loop_v4(val).map_err(io_err_into_net_error),
            LocalUdpSocketMode::Async(a) => a.set_multicast_loop_v4(val).map_err(io_err_into_net_error),
            LocalUdpSocketMode::Uninitialized => unreachable!()
        }
    }

    fn multicast_loop_v4(&self) -> Result<bool> {
        match &self.socket {
            LocalUdpSocketMode::Blocking(a) => a.multicast_loop_v4().map_err(io_err_into_net_error),
            LocalUdpSocketMode::Async(a) => a.multicast_loop_v4().map_err(io_err_into_net_error),
            LocalUdpSocketMode::Uninitialized => unreachable!()
        }
    }

    fn set_multicast_loop_v6(&mut self, val: bool) -> Result<()> {
        match &mut self.socket {
            LocalUdpSocketMode::Blocking(a) => a.set_multicast_loop_v6(val).map_err(io_err_into_net_error),
            LocalUdpSocketMode::Async(a) => a.set_multicast_loop_v6(val).map_err(io_err_into_net_error),
            LocalUdpSocketMode::Uninitialized => unreachable!()
        }
    }

    fn multicast_loop_v6(&self) -> Result<bool> {
        match &self.socket {
            LocalUdpSocketMode::Blocking(a) => a.multicast_loop_v6().map_err(io_err_into_net_error),
            LocalUdpSocketMode::Async(a) => a.multicast_loop_v6().map_err(io_err_into_net_error),
            LocalUdpSocketMode::Uninitialized => unreachable!()
        }
    }

    fn set_multicast_ttl_v4(&mut self, ttl: u32) -> Result<()> {
        match &mut self.socket {
            LocalUdpSocketMode::Blocking(a) => a.set_multicast_ttl_v4(ttl).map_err(io_err_into_net_error),
            LocalUdpSocketMode::Async(a) => a.set_multicast_ttl_v4(ttl).map_err(io_err_into_net_error),
            LocalUdpSocketMode::Uninitialized => unreachable!()
        }
    }

    fn multicast_ttl_v4(&self) -> Result<u32> {
        match &self.socket {
            LocalUdpSocketMode::Blocking(a) => a.multicast_ttl_v4().map_err(io_err_into_net_error),
            LocalUdpSocketMode::Async(a) => a.multicast_ttl_v4().map_err(io_err_into_net_error),
            LocalUdpSocketMode::Uninitialized => unreachable!()
        }
    }

    fn join_multicast_v4(&mut self, multiaddr: Ipv4Addr, iface: Ipv4Addr) -> Result<()> {
        match &mut self.socket {
            LocalUdpSocketMode::Blocking(a) => a.join_multicast_v4(&multiaddr, &iface).map_err(io_err_into_net_error),
            LocalUdpSocketMode::Async(a) => a.join_multicast_v4(multiaddr, iface).map_err(io_err_into_net_error),
            LocalUdpSocketMode::Uninitialized => unreachable!()
        }
    }

    fn leave_multicast_v4(&mut self, multiaddr: Ipv4Addr, iface: Ipv4Addr) -> Result<()> {
        match &mut self.socket {
            LocalUdpSocketMode::Blocking(a) => a.leave_multicast_v4(&multiaddr, &iface).map_err(io_err_into_net_error),
            LocalUdpSocketMode::Async(a) => a.leave_multicast_v4(multiaddr, iface).map_err(io_err_into_net_error),
            LocalUdpSocketMode::Uninitialized => unreachable!()
        }
    }

    fn join_multicast_v6(&mut self, multiaddr: Ipv6Addr, iface: u32) -> Result<()> {
        match &mut self.socket {
            LocalUdpSocketMode::Blocking(a) => a.join_multicast_v6(&multiaddr, iface).map_err(io_err_into_net_error),
            LocalUdpSocketMode::Async(a) => a.join_multicast_v6(&multiaddr, iface).map_err(io_err_into_net_error),
            LocalUdpSocketMode::Uninitialized => unreachable!()
        }
    }

    fn leave_multicast_v6(&mut self, multiaddr: Ipv6Addr, iface: u32) -> Result<()> {
        match &mut self.socket {
            LocalUdpSocketMode::Blocking(a) => a.leave_multicast_v6(&multiaddr, iface).map_err(io_err_into_net_error),
            LocalUdpSocketMode::Async(a) => a.leave_multicast_v6(&multiaddr, iface).map_err(io_err_into_net_error),
            LocalUdpSocketMode::Uninitialized => unreachable!()
        }
    }

    fn addr_peer(&self) -> Result<Option<SocketAddr>> {
        match &self.socket {
            LocalUdpSocketMode::Blocking(a) => a.peer_addr().map(Some).map_err(io_err_into_net_error),
            LocalUdpSocketMode::Async(a) => a.peer_addr().map(Some).map_err(io_err_into_net_error),
            LocalUdpSocketMode::Uninitialized => unreachable!()
        }
    }
}

#[async_trait::async_trait]
impl VirtualConnectedSocket for LocalUdpSocket {
    fn set_linger(&mut self, linger: Option<Duration>) -> Result<()> {
        Err(NetworkError::Unsupported)
    }

    fn linger(&self) -> Result<Option<Duration>> {
        Err(NetworkError::Unsupported)
    }

    fn send(&mut self, data: Bytes) -> Result<usize> {
        self.socket
            .as_blocking_mut()
            .map_err(io_err_into_net_error)?
            .send(&data[..])
            .map_err(io_err_into_net_error)
    }

    async fn send_async(&mut self, data: Bytes) -> Result<usize> {
        self.socket
            .as_async_mut()
            .map_err(io_err_into_net_error)?
            .send(&data[..])
            .await
            .map_err(io_err_into_net_error)
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }

    async fn flush_async(&mut self) -> Result<()> {
        Ok(())
    }

    fn recv(&mut self) -> Result<SocketReceive> {
        let buf_size = 8192;
        let mut buf = BytesMut::with_capacity(buf_size);
        let read = self.socket
            .as_blocking_mut()
            .map_err(io_err_into_net_error)?
            .recv(&mut buf[..])
            .map_err(io_err_into_net_error)?;
        let buf = Bytes::from(buf).slice(..read);
        Ok(SocketReceive {
            data: buf,
            truncated: read == buf_size,
        })
    }

    async fn recv_async(&mut self) -> Result<SocketReceive> {
        let buf_size = 8192;
        let mut buf = BytesMut::with_capacity(buf_size);
        let read = self.socket
            .as_async_mut()
            .map_err(io_err_into_net_error)?
            .recv(&mut buf[..])
            .await
            .map_err(io_err_into_net_error)?;
        let buf = Bytes::from(buf).slice(..read);
        Ok(SocketReceive {
            data: buf,
            truncated: read == buf_size,
        })
    }

    fn try_recv(&mut self) -> Result<Option<SocketReceive>> {
        let buf_size = 8192;
        let mut buf = BytesMut::with_capacity(buf_size);

        let socket = self.socket.as_blocking_mut().map_err(io_err_into_net_error)?;
        socket.set_nonblocking(true).map_err(io_err_into_net_error)?;
        let read = socket.recv(&mut buf[..]);
        let _ = socket.set_nonblocking(self.nonblocking);

        let read = match read {
            Ok(a) => Ok(a),
            Err(err) if err.kind() == std::io::ErrorKind::TimedOut ||
                               err.kind() == std::io::ErrorKind::WouldBlock => {
                return Ok(None);
            },
            Err(err) => Err(io_err_into_net_error(err))
        }?;

        let buf = Bytes::from(buf).slice(..read);
        Ok(Some(SocketReceive {
            data: buf,
            truncated: read == buf_size,
        }))
    }

    fn peek(&mut self) -> Result<SocketReceive> {
        let buf_size = 8192;
        let mut buf = BytesMut::with_capacity(buf_size);
        let read = self.socket
            .as_blocking_mut()
            .map_err(io_err_into_net_error)?
            .peek(&mut buf[..])
            .map_err(io_err_into_net_error)?;
        let buf = Bytes::from(buf).slice(..read);
        Ok(SocketReceive {
            data: buf,
            truncated: read == buf_size,
        })
    }
}

#[async_trait::async_trait]
impl VirtualConnectionlessSocket for LocalUdpSocket {
    fn send_to(&mut self, data: Bytes, addr: SocketAddr) -> Result<usize> {
        self.socket
            .as_blocking_mut()
            .map_err(io_err_into_net_error)?
            .send_to(&data[..], addr)
            .map_err(io_err_into_net_error)
    }

    async fn send_to_async(&mut self, data: Bytes, addr: SocketAddr) -> Result<usize> {
        self.socket
            .as_async_mut()
            .map_err(io_err_into_net_error)?
            .send_to(&data[..], addr)
            .await
            .map_err(io_err_into_net_error)
    }

    fn try_recv_from(&mut self) -> Result<Option<SocketReceiveFrom>> {
        let buf_size = 8192;
        let mut buf = BytesMut::with_capacity(buf_size);

        let socket = self.socket.as_blocking_mut().map_err(io_err_into_net_error)?;
        socket.set_nonblocking(true).map_err(io_err_into_net_error)?;
        let read = socket.recv_from(&mut buf[..]);
        let _ = socket.set_nonblocking(self.nonblocking);

        let (read, peer) = match read {
            Ok((a, b)) => Ok((a, b)),
            Err(err) if err.kind() == std::io::ErrorKind::TimedOut ||
                               err.kind() == std::io::ErrorKind::WouldBlock => {
                return Ok(None);
            },
            Err(err) => Err(io_err_into_net_error(err))
        }?;
        
        let buf = Bytes::from(buf).slice(..read);
        Ok(Some(SocketReceiveFrom {
            data: buf,
            truncated: read == buf_size,
            addr: peer,
        }))
    }

    fn recv_from(&mut self) -> Result<SocketReceiveFrom> {
        let buf_size = 8192;
        let mut buf = BytesMut::with_capacity(buf_size);
        let (read, peer) = self
            .socket
            .as_blocking_mut()
            .map_err(io_err_into_net_error)?
            .recv_from(&mut buf[..])
            .map_err(io_err_into_net_error)?;
        let buf = Bytes::from(buf).slice(..read);
        Ok(SocketReceiveFrom {
            data: buf,
            truncated: read == buf_size,
            addr: peer,
        })
    }

    async fn recv_from_async(&mut self) -> Result<SocketReceiveFrom> {
        let buf_size = 8192;
        let mut buf = BytesMut::with_capacity(buf_size);
        let (read, peer) = self
            .socket
            .as_async_mut()
            .map_err(io_err_into_net_error)?
            .recv_from(&mut buf[..])
            .await
            .map_err(io_err_into_net_error)?;
        let buf = Bytes::from(buf).slice(..read);
        Ok(SocketReceiveFrom {
            data: buf,
            truncated: read == buf_size,
            addr: peer,
        })
    }

    fn peek_from(&mut self) -> Result<SocketReceiveFrom> {
        let buf_size = 8192;
        let mut buf = BytesMut::with_capacity(buf_size);
        let (read, peer) = self
            .socket
            .as_blocking_mut()
            .map_err(io_err_into_net_error)?
            .peek_from(&mut buf[..])
            .map_err(io_err_into_net_error)?;
        let buf = Bytes::from(buf).slice(..read);
        Ok(SocketReceiveFrom {
            data: buf,
            truncated: read == buf_size,
            addr: peer,
        })
    }
}

#[async_trait::async_trait]
impl VirtualSocket for LocalUdpSocket {
    fn set_ttl(&mut self, ttl: u32) -> Result<()> {
        match &mut self.socket {
            LocalUdpSocketMode::Blocking(a) => a.set_ttl(ttl).map_err(io_err_into_net_error),
            LocalUdpSocketMode::Async(a) => a.set_ttl(ttl).map_err(io_err_into_net_error),
            LocalUdpSocketMode::Uninitialized => unreachable!()
        }
    }

    fn set_nonblocking(&mut self, nonblocking: bool) -> Result<()> {
        self.nonblocking = nonblocking;
        self.socket
            .as_blocking_mut()
            .map_err(io_err_into_net_error)?
            .set_nonblocking(nonblocking)
            .map_err(io_err_into_net_error)?;
        Ok(())
    }

    fn ttl(&self) -> Result<u32> {
        match &self.socket {
            LocalUdpSocketMode::Blocking(a) => a.ttl().map_err(io_err_into_net_error),
            LocalUdpSocketMode::Async(a) => a.ttl().map_err(io_err_into_net_error),
            LocalUdpSocketMode::Uninitialized => unreachable!()
        }
    }

    fn addr_local(&self) -> Result<SocketAddr> {
        match &self.socket {
            LocalUdpSocketMode::Blocking(a) => a.local_addr().map_err(io_err_into_net_error),
            LocalUdpSocketMode::Async(a) => a.local_addr().map_err(io_err_into_net_error),
            LocalUdpSocketMode::Uninitialized => unreachable!()
        }
    }

    fn status(&self) -> Result<SocketStatus> {
        Ok(SocketStatus::Opened)
    }

    async fn wait_read(&mut self) -> Result<()> {
        let socket = self.socket
            .as_async_mut()
            .map_err(io_err_into_net_error)?;
        let read = LocalUdpSocketReadReady { socket };
        read.await
    }

    async fn wait_write(&mut self) -> Result<()> {
        let socket = self.socket
            .as_async_mut()
            .map_err(io_err_into_net_error)?;
        let read = LocalUdpSocketWriteReady { socket };
        read.await
    }
}

struct LocalUdpSocketReadReady<'a> {
    socket: &'a mut tokio::net::UdpSocket,
}
impl<'a> Future
for LocalUdpSocketReadReady<'a>
{
    type Output = Result<()>;

    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<Self::Output> {
        self.socket
            .poll_recv_ready(cx)
            .map_err(io_err_into_net_error)
    }
}

struct LocalUdpSocketWriteReady<'a> {
    socket: &'a mut tokio::net::UdpSocket,
}
impl<'a> Future
for LocalUdpSocketWriteReady<'a>
{
    type Output = Result<()>;

    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<Self::Output> {
        self.socket
            .poll_send_ready(cx)
            .map_err(io_err_into_net_error)
    }
}
