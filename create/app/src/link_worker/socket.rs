//! The datagram operations used by a link worker.

use super::{local_network_off, timed_out, WAKE};
use std::io;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::time::Duration;

pub(super) trait LinkSocket {
    fn local_addr(&self) -> io::Result<SocketAddr>;
    fn send_to(&self, bytes: &[u8], to: SocketAddr) -> io::Result<usize>;
    fn recv_from_within(&self, buf: &mut [u8], wait: Duration) -> io::Result<(usize, SocketAddr)>;
    /// Read queued datagrams until the inbox is empty or the caller has seen enough.
    fn drain_queued(&self, buf: &mut [u8], receive: impl FnMut(&[u8], SocketAddr) -> bool) -> io::Result<()>;
    fn wake_line(&self) -> io::Result<WakeLine>;
}

fn drain(buf: &mut [u8], mut read: impl FnMut(&mut [u8]) -> io::Result<(usize, SocketAddr)>,
    mut receive: impl FnMut(&[u8], SocketAddr) -> bool) -> io::Result<()> {
    loop {
        match read(buf) {
            Ok((n, from)) => {
                if !receive(&buf[..n], from) { return Ok(()) }
            }
            Err(e) if timed_out(&e) || local_network_off(&e) => return Ok(()),
            Err(e) if matches!(e.kind(), io::ErrorKind::Interrupted | io::ErrorKind::ConnectionReset) => continue,
            Err(e) => return Err(e),
        }
    }
}

impl LinkSocket for UdpSocket {
    fn local_addr(&self) -> io::Result<SocketAddr> { self.local_addr() }

    fn send_to(&self, bytes: &[u8], to: SocketAddr) -> io::Result<usize> { self.send_to(bytes, to) }

    fn recv_from_within(&self, buf: &mut [u8], wait: Duration) -> io::Result<(usize, SocketAddr)> {
        self.set_read_timeout(Some(wait))?;
        self.recv_from(buf)
    }

    fn drain_queued(&self, buf: &mut [u8], receive: impl FnMut(&[u8], SocketAddr) -> bool) -> io::Result<()> {
        self.set_nonblocking(true)?;
        let drained = drain(buf, |buf| self.recv_from(buf), receive);
        self.set_nonblocking(false)?;
        drained
    }

    fn wake_line(&self) -> io::Result<WakeLine> {
        let mut to = self.local_addr()?;
        if to.ip().is_unspecified() {
            to.set_ip(Ipv4Addr::LOCALHOST.into());
        }
        let socket = UdpSocket::bind((to.ip(), 0))?;
        socket.set_nonblocking(true)?;
        Ok(WakeLine { from: socket.local_addr()?, socket: WakeSocket::Udp(socket), to })
    }
}

enum WakeSocket {
    Udp(UdpSocket),
    #[cfg(test)]
    Mem(mem::MemSocket),
}

pub(super) struct WakeLine {
    socket: WakeSocket,
    pub(super) from: SocketAddr,
    to: SocketAddr,
}

impl WakeLine {
    pub(super) fn wake(&self) -> io::Result<usize> {
        match &self.socket {
            WakeSocket::Udp(socket) => socket.send_to(WAKE, self.to),
            #[cfg(test)]
            WakeSocket::Mem(socket) => socket.send_to(WAKE, self.to),
        }
    }
}

#[cfg(test)]
pub(super) mod mem {
    use super::*;
    use std::collections::{BTreeMap, VecDeque};
    use std::sync::{Arc, Condvar, Mutex, Weak};

    #[derive(Default)]
    struct Queue {
        packets: VecDeque<(SocketAddr, Vec<u8>)>,
        reading: bool,
        fault: Option<io::ErrorKind>,
        closed: bool,
    }

    #[derive(Default)]
    struct Inbox {
        queue: Mutex<Queue>,
        ready: Condvar,
    }

    #[derive(Default)]
    struct Routes {
        sockets: BTreeMap<SocketAddr, Weak<Inbox>>,
        address: Option<Ipv4Addr>,
        watch_punch: Option<std::sync::mpsc::Sender<SocketAddr>>,
        closed: bool,
    }

    #[derive(Default)]
    pub struct MemNet {
        routes: Arc<Mutex<Routes>>,
    }

    impl MemNet {
        pub fn observed(address: Ipv4Addr) -> (Self, std::sync::mpsc::Receiver<SocketAddr>) {
            let net = Self::default();
            let (send, recv) = std::sync::mpsc::channel();
            {
                let mut routes = net.routes.lock().unwrap();
                routes.address = Some(address);
                routes.watch_punch = Some(send);
            }
            (net, recv)
        }

        pub fn fail_receive(&self, addr: SocketAddr, error: io::ErrorKind) {
            let inbox = self.routes.lock().unwrap().sockets[&addr].upgrade().unwrap();
            inbox.queue.lock().unwrap().fault = Some(error);
            inbox.ready.notify_all();
        }

        pub fn bind(&self, port: u16) -> io::Result<MemSocket> {
            MemSocket::bind(&self.routes, port)
        }

        pub fn binder(&self) -> impl Fn(u16) -> io::Result<MemSocket> + 'static {
            let routes = self.routes.clone();
            move |port| MemSocket::bind(&routes, port)
        }

        /// After a send, the worker has handled it and sent any reply once it waits to read again.
        pub fn wait_for_reader(&self, addr: SocketAddr) {
            let inbox = self.routes.lock().unwrap().sockets[&addr].upgrade().unwrap();
            let queue = inbox.queue.lock().unwrap();
            drop(inbox.ready.wait_while(queue, |q| !q.reading || !q.packets.is_empty() || q.fault.is_some()).unwrap());
        }

        /// Queued packets still arrive. Subsequent empty reads complete as logical timeouts.
        pub fn close(&self) {
            let mut routes = self.routes.lock().unwrap();
            routes.closed = true;
            for inbox in routes.sockets.values().filter_map(Weak::upgrade) {
                inbox.queue.lock().unwrap().closed = true;
                inbox.ready.notify_all();
            }
            routes.sockets.clear();
        }
    }

    impl Drop for MemNet {
        fn drop(&mut self) { self.close(); }
    }

    pub struct MemSocket {
        routes: Arc<Mutex<Routes>>,
        addr: SocketAddr,
        timed: bool,
        inbox: Arc<Inbox>,
    }

    impl MemSocket {
        fn bind(routes: &Arc<Mutex<Routes>>, port: u16) -> io::Result<Self> {
            let mut table = routes.lock().unwrap();
            if table.closed { return Err(io::ErrorKind::NotConnected.into()); }
            let timed = table.address.is_some();
            let ip = table.address.unwrap_or(Ipv4Addr::LOCALHOST);
            let sockets = &mut table.sockets;
            let ports = if port == 0 { 1..=u16::MAX } else { port..=port };
            let addr = ports.map(|p| SocketAddr::from((ip, p)))
                .find(|addr| sockets.get(addr).and_then(Weak::upgrade).is_none())
                .ok_or(io::ErrorKind::AddrInUse)?;
            let inbox = Arc::new(Inbox::default());
            sockets.insert(addr, Arc::downgrade(&inbox));
            Ok(Self { routes: routes.clone(), addr, timed, inbox })
        }

        fn pop(queue: &mut Queue, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
            if let Some(error) = queue.fault.take() { return Err(error.into()); }
            let (from, bytes) = queue.packets.pop_front().ok_or(io::ErrorKind::WouldBlock)?;
            let n = buf.len().min(bytes.len());
            buf[..n].copy_from_slice(&bytes[..n]);
            Ok((n, from))
        }

        pub fn try_recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
            Self::pop(&mut self.inbox.queue.lock().unwrap(), buf)
        }
    }

    impl LinkSocket for MemSocket {
        fn local_addr(&self) -> io::Result<SocketAddr> { Ok(self.addr) }

        fn send_to(&self, bytes: &[u8], to: SocketAddr) -> io::Result<usize> {
            let routes = self.routes.lock().unwrap();
            if bytes.first() == Some(&makepad_network::p2p::punch::KIND) {
                if let Some(watch) = &routes.watch_punch { let _ = watch.send(self.addr); }
            }
            if let Some(inbox) = routes.sockets.get(&to).and_then(Weak::upgrade) {
                inbox.queue.lock().unwrap().packets.push_back((self.addr, bytes.to_vec()));
                inbox.ready.notify_all();
            }
            Ok(bytes.len())
        }

        fn recv_from_within(&self, buf: &mut [u8], wait: Duration) -> io::Result<(usize, SocketAddr)> {
            let mut queue = self.inbox.queue.lock().unwrap();
            if queue.packets.is_empty() && !queue.closed && queue.fault.is_none() {
                queue.reading = true;
                self.inbox.ready.notify_all();
                let empty = |q: &mut Queue| q.packets.is_empty() && !q.closed && q.fault.is_none();
                queue = if self.timed { self.inbox.ready.wait_timeout_while(queue, wait, empty).unwrap().0 }
                    else { self.inbox.ready.wait_while(queue, empty).unwrap() };
                queue.reading = false;
            }
            Self::pop(&mut queue, buf)
        }

        fn drain_queued(&self, buf: &mut [u8], receive: impl FnMut(&[u8], SocketAddr) -> bool) -> io::Result<()> {
            drain(buf, |buf| self.try_recv_from(buf), receive)
        }

        fn wake_line(&self) -> io::Result<WakeLine> {
            let socket = Self::bind(&self.routes, 0)?;
            Ok(WakeLine { from: socket.addr, socket: WakeSocket::Mem(socket), to: self.addr })
        }
    }
}
