//! The WebRTC endpoint's sockets, shared by every WebRTC session.
//!
//! One set of UDP sockets (one per host address, on `--webrtc-port`) serves
//! all of an environment's WebRTC viewers. Each viewer has its own `Rtc`
//! inside its own session task; this module only moves datagrams between the
//! sockets and those tasks.
//!
//! Incoming datagrams are routed by the sender's address. An address no
//! session has claimed yet (a browser's first STUN check, a NAT rebinding) goes
//! to every session; each asks its own `Rtc::accepts` (str0m matches STUN by
//! the ICE username, other traffic by the nominated address) and the one that
//! accepts claims the address. From then on that address goes to that session
//! alone. A session that is sent a datagram it doesn't accept lets the address
//! go, so the next one is offered to all again.
//!
//! The hub is made at startup and lives as long as the gateway (taken from
//! cha-streamer's `rtc_hub`, without its per-`?host=` hubs).

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, bail};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::warn;

/// Datagrams a session may have waiting before the newest are dropped (UDP
/// would drop them too; a session that far behind is stuck).
const INBOX: usize = 1024;

/// A datagram that arrived on the hub's socket number `socket`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Datagram {
    pub data: Vec<u8>,
    pub source: SocketAddr,
    pub socket: usize,
}

/// Who gets which datagram.
#[derive(Default)]
pub struct Router {
    /// The session that claimed each remote address.
    routes: Mutex<HashMap<SocketAddr, u64>>,
    peers: Mutex<HashMap<u64, mpsc::Sender<Datagram>>>,
}

impl Router {
    pub fn add(&self, id: u64) -> mpsc::Receiver<Datagram> {
        let (tx, rx) = mpsc::channel(INBOX);
        self.peers.lock().expect("peers lock").insert(id, tx);
        rx
    }

    /// Forgets a session and the addresses it claimed.
    pub fn remove(&self, id: u64) {
        self.peers.lock().expect("peers lock").remove(&id);
        self.routes
            .lock()
            .expect("routes lock")
            .retain(|_, owner| *owner != id);
    }

    /// Session `id` accepted a datagram from `source`: it's theirs.
    pub fn claim(&self, source: SocketAddr, id: u64) {
        self.routes.lock().expect("routes lock").insert(source, id);
    }

    /// Session `id` was sent a datagram from `source` that isn't its own.
    pub fn release(&self, source: SocketAddr, id: u64) {
        let mut routes = self.routes.lock().expect("routes lock");
        if routes.get(&source) == Some(&id) {
            routes.remove(&source);
        }
    }

    /// Hands a datagram to the session that claimed its sender, or to all.
    pub fn dispatch(&self, datagram: Datagram) {
        let owner = self
            .routes
            .lock()
            .expect("routes lock")
            .get(&datagram.source)
            .copied();
        let peers = self.peers.lock().expect("peers lock");
        match owner.and_then(|id| peers.get(&id)) {
            Some(tx) => {
                let _ = tx.try_send(datagram);
            }
            None => {
                for tx in peers.values() {
                    let _ = tx.try_send(datagram.clone());
                }
            }
        }
    }
}

/// The sockets and the task that reads them.
pub struct Hub {
    sockets: Arc<Vec<UdpSocket>>,
    pub locals: Vec<SocketAddr>,
    router: Arc<Router>,
    reader: JoinHandle<()>,
}

impl Hub {
    /// Binds `port` on each of `hosts` (any free port where it's taken).
    pub async fn bind(hosts: &[IpAddr], port: u16) -> Result<Arc<Self>> {
        let mut sockets = Vec::new();
        for host in hosts {
            let mut bound = None;
            // The hub of a session that just ended may still be letting go.
            for attempt in 0..5 {
                match UdpSocket::bind(SocketAddr::new(*host, port)).await {
                    Ok(socket) => {
                        bound = Some(socket);
                        break;
                    }
                    Err(_) if attempt < 4 => tokio::time::sleep(Duration::from_millis(40)).await,
                    Err(_) => {}
                }
            }
            let socket = match bound {
                Some(socket) => socket,
                None => match UdpSocket::bind(SocketAddr::new(*host, 0)).await {
                    Ok(socket) => socket,
                    Err(err) => {
                        warn!(%host, "no UDP socket: {err}");
                        continue;
                    }
                },
            };
            sockets.push(socket);
        }
        if sockets.is_empty() {
            bail!("no address to receive WebRTC on");
        }
        let locals = sockets
            .iter()
            .map(UdpSocket::local_addr)
            .collect::<std::io::Result<Vec<_>>>()?;
        let sockets = Arc::new(sockets);
        let router = Arc::new(Router::default());
        let reader = tokio::spawn(read_loop(Arc::clone(&sockets), Arc::clone(&router)));
        Ok(Arc::new(Self {
            sockets,
            locals,
            router,
            reader,
        }))
    }

    /// A session's place on the hub; leaving when dropped.
    pub fn join(self: &Arc<Self>, id: u64) -> Peer {
        Peer {
            inbox: self.router.add(id),
            hub: Arc::clone(self),
            id,
        }
    }

    pub fn send(
        &self,
        source: SocketAddr,
        destination: SocketAddr,
        contents: &[u8],
    ) -> std::io::Result<()> {
        // From the candidate str0m chose (a public address, announced on the
        // first socket, goes out of the first).
        let socket = self.locals.iter().position(|l| *l == source).unwrap_or(0);
        match self.sockets[socket].try_send_to(contents, destination) {
            Err(err) if err.kind() != std::io::ErrorKind::WouldBlock => Err(err),
            _ => Ok(()),
        }
    }
}

impl Drop for Hub {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

async fn read_loop(sockets: Arc<Vec<UdpSocket>>, router: Arc<Router>) {
    let mut buf = vec![0u8; 2048];
    loop {
        match recv_any(&sockets, &mut buf).await {
            Ok((n, source, socket)) => router.dispatch(Datagram {
                data: buf[..n].to_vec(),
                source,
                socket,
            }),
            // A socket error (ICMP unreachable on some platforms) isn't the
            // end of the endpoint.
            Err(err) => {
                warn!("WebRTC socket: {err}");
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    }
}

/// The next datagram on any of `sockets`: its length, sender and socket.
async fn recv_any(
    sockets: &[UdpSocket],
    buf: &mut [u8],
) -> std::io::Result<(usize, SocketAddr, usize)> {
    std::future::poll_fn(|cx| {
        for (i, socket) in sockets.iter().enumerate() {
            let mut read = tokio::io::ReadBuf::new(buf);
            if let std::task::Poll::Ready(result) = socket.poll_recv_from(cx, &mut read) {
                return std::task::Poll::Ready(result.map(|from| (read.filled().len(), from, i)));
            }
        }
        std::task::Poll::Pending
    })
    .await
}

/// One session's end of the hub.
pub struct Peer {
    pub hub: Arc<Hub>,
    pub id: u64,
    pub inbox: mpsc::Receiver<Datagram>,
}

impl Peer {
    pub fn claim(&self, source: SocketAddr) {
        self.hub.router.claim(source, self.id);
    }

    pub fn release(&self, source: SocketAddr) {
        self.hub.router.release(source, self.id);
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        self.hub.router.remove(self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    fn datagram(source: &str, tag: u8) -> Datagram {
        Datagram {
            data: vec![tag],
            source: addr(source),
            socket: 0,
        }
    }

    #[test]
    fn unclaimed_senders_go_to_everyone_and_claimed_ones_to_their_owner() {
        let router = Router::default();
        let mut a = router.add(1);
        let mut b = router.add(2);
        router.dispatch(datagram("192.0.2.1:5000", 1));
        assert_eq!(a.try_recv().unwrap().data, [1]);
        assert_eq!(b.try_recv().unwrap().data, [1]);

        router.claim(addr("192.0.2.1:5000"), 2);
        router.dispatch(datagram("192.0.2.1:5000", 2));
        assert!(a.try_recv().is_err());
        assert_eq!(b.try_recv().unwrap().data, [2]);

        // Released (the owner didn't accept it): offered to all again.
        router.release(addr("192.0.2.1:5000"), 1); // not the owner: no effect
        router.dispatch(datagram("192.0.2.1:5000", 3));
        assert!(a.try_recv().is_err());
        assert!(b.try_recv().is_ok());
        router.release(addr("192.0.2.1:5000"), 2);
        router.dispatch(datagram("192.0.2.1:5000", 4));
        assert!(a.try_recv().is_ok() && b.try_recv().is_ok());

        // A session that leaves takes its addresses with it.
        router.claim(addr("192.0.2.1:5000"), 2);
        router.remove(2);
        router.dispatch(datagram("192.0.2.1:5000", 5));
        assert_eq!(a.try_recv().unwrap().data, [5]);
    }
}
