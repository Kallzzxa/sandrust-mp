//! The SA-MP server query protocol (the same one samp.exe's server browser
//! uses): one UDP packet, opcode `i` (info), answered with the server's
//! hostname, gamemode, map, player counts and password flag. Ping is the
//! round-trip time of that exchange.
//!
//! Request:  `SAMP` + IPv4 (4 bytes) + port u16 LE + `i` (0x69).
//! Response: `SAMP` + IPv4 (4) + port u16 LE + `i` + passworded u8
//!           + players u16 LE + maxplayers u16 LE
//!           + hostname_len u32 LE + hostname
//!           + gamemode_len u32 LE + gamemode
//!           + mapname_len u32 LE + mapname.

use std::io;
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::time::{Duration, Instant};

/// How long one server gets to answer before it counts as offline.
pub const QUERY_TIMEOUT: Duration = Duration::from_millis(1500);

/// What the browser shows for one server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerInfo {
    pub passworded: bool,
    pub players: u16,
    pub max_players: u16,
    pub hostname: String,
    pub gamemode: String,
    pub mapname: String,
    /// Round-trip time of the query, in milliseconds.
    pub ping_ms: u64,
}

/// Build the `i` (info) request for `addr` (the server's UDP address).
pub fn build_info_request(addr: SocketAddr) -> Vec<u8> {
    let mut out = Vec::with_capacity(11);
    out.extend_from_slice(b"SAMP");
    match addr.ip() {
        std::net::IpAddr::V4(v4) => out.extend_from_slice(&v4.octets()),
        std::net::IpAddr::V6(_) => out.extend_from_slice(&[0, 0, 0, 0]),
    }
    out.extend_from_slice(&addr.port().to_le_bytes());
    out.push(b'i');
    out
}

/// Parse an `i` response. `ping_ms` is measured by the caller.
pub fn parse_info_response(buf: &[u8], ping_ms: u64) -> Option<ServerInfo> {
    let mut r = Reader { buf, pos: 0 };
    if r.take(4)? != b"SAMP" {
        return None;
    }
    let _ip = r.take(4)?;
    let _port = r.take_u16_le()?;
    if r.take(1)? != b"i" {
        return None;
    }
    let passworded = r.take_u8()? != 0;
    let players = r.take_u16_le()?;
    let max_players = r.take_u16_le()?;
    let hostname = r.take_len_string()?;
    let gamemode = r.take_len_string()?;
    let mapname = r.take_len_string()?;
    Some(ServerInfo { passworded, players, max_players, hostname, gamemode, mapname, ping_ms })
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let s = self.buf.get(self.pos..end)?;
        self.pos = end;
        Some(s)
    }

    fn take_u8(&mut self) -> Option<u8> {
        Some(*self.take(1)?.first()?)
    }

    fn take_u16_le(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }

    fn take_u32_le(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn take_len_string(&mut self) -> Option<String> {
        let len = self.take_u32_le()? as usize;
        let bytes = self.take(len)?;
        Some(String::from_utf8_lossy(bytes).into_owned())
    }
}

/// Query one server: resolve `host:port`, send the `i` packet, wait for the
/// answer. `Ok(None)` means the server did not answer in time (offline).
pub fn query_server(host: &str, port: u16) -> io::Result<Option<ServerInfo>> {
    let addr = format!("{host}:{port}")
        .to_socket_addrs()?
        .find(|a| a.is_ipv4())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "could not resolve host"))?;
    let sock = UdpSocket::bind("0.0.0.0:0")?;
    sock.set_read_timeout(Some(QUERY_TIMEOUT))?;
    sock.connect(addr)?;
    let req = build_info_request(addr);
    let start = Instant::now();
    sock.send(&req)?;
    let mut buf = [0u8; 2048];
    match sock.recv(&mut buf) {
        Ok(n) => Ok(parse_info_response(&buf[..n], start.elapsed().as_millis() as u64)),
        Err(e) if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut => Ok(None),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_response() -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(b"SAMP");
        v.extend_from_slice(&[127, 0, 0, 1]);
        v.extend_from_slice(&7777u16.to_le_bytes());
        v.push(b'i');
        v.push(0); // not passworded
        v.extend_from_slice(&7u16.to_le_bytes());
        v.extend_from_slice(&100u16.to_le_bytes());
        for s in ["MC-RP | Kota Baru", "Atlantic RP", "San Andreas"] {
            v.extend_from_slice(&(s.len() as u32).to_le_bytes());
            v.extend_from_slice(s.as_bytes());
        }
        v
    }

    #[test]
    fn info_request_layout() {
        let addr: SocketAddr = "127.0.0.1:7777".parse().unwrap();
        // "SAMP" + 127.0.0.1 + port 7777 little-endian (0x61, 0x1E) + 'i'.
        assert_eq!(build_info_request(addr), b"SAMP\x7f\x00\x00\x01\x61\x1ei".to_vec());
    }

    #[test]
    fn info_response_parses() {
        let info = parse_info_response(&sample_response(), 42).unwrap();
        assert_eq!(
            info,
            ServerInfo {
                passworded: false,
                players: 7,
                max_players: 100,
                hostname: "MC-RP | Kota Baru".into(),
                gamemode: "Atlantic RP".into(),
                mapname: "San Andreas".into(),
                ping_ms: 42,
            }
        );
    }

    #[test]
    fn info_response_rejects_garbage() {
        assert!(parse_info_response(b"", 0).is_none());
        assert!(parse_info_response(b"SAMP", 0).is_none());
        let mut bad = sample_response();
        bad[10] = b'x'; // wrong opcode
        assert!(parse_info_response(&bad, 0).is_none());
        let mut trunc = sample_response();
        trunc.truncate(20); // cut inside the hostname
        assert!(parse_info_response(&trunc, 0).is_none());
    }
}
