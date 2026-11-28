use super::{NotLinked, Seat};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PeerUndelivered {
    pub peer: Seat,
    pub topic: u8,
    pub count: usize,
}

/// All calls return immediately. The peer key is supplied by the authenticated computer link.
pub trait Relay {
    fn peers(&self) -> Vec<Seat>;
    fn send(&mut self, peer: Seat, topic: u8, body: Vec<u8>) -> Result<(), NotLinked>;
    /// Buffered transports replace an unsent message on this topic. An immediate relay can send directly.
    fn send_latest(&mut self, peer: Seat, topic: u8, body: Vec<u8>) -> Result<(), NotLinked> {
        self.send(peer, topic, body)
    }
    fn take(&mut self, topic: u8) -> Vec<(Seat, Vec<u8>)>;
    fn take_dropped(&mut self, _topic: u8) -> Vec<PeerUndelivered> { Vec::new() }
}
