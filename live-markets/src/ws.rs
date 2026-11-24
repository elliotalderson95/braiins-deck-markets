//! The client's side of the WebSocket protocol (RFC 6455), for a connection that the widget opens as a
//! plain TLS socket. The Deck's own WebSocket call is built without TLS and cannot reach `wss://`, but
//! its TLS socket is verified against the usual roots, so the upgrade request and the frames are
//! written and read here: text messages out and in, answers to pings, the end of a connection.
//!
//! Everything is plain bytes in and out, so it is tested on the host. The glue moves the bytes.

/// The opcodes a client sends.
pub const TEXT: u8 = 0x1;
pub const PONG: u8 = 0xA;

const CONTINUATION: u8 = 0x0;
const BINARY: u8 = 0x2;
const CLOSE: u8 = 0x8;
const PING: u8 = 0x9;

/// The largest frame taken in. The server sends a few hundred bytes at a time; anything beyond this is
/// not what was asked for, and the connection is dropped rather than buffered.
const MAX_FRAME: usize = 1 << 20;

/// Base64 of `bytes`, with padding.
#[must_use]
pub fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0_u32, |acc, (i, b)| acc | (u32::from(*b) << (16 - 8 * i)));
        for k in 0..4 {
            if k <= chunk.len() {
                out.push(char::from(ALPHABET[usize::try_from((n >> (18 - 6 * k)) & 0x3F).unwrap_or(0)]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The next number of a xorshift sequence: bytes that need to differ from one use to the next, not to be secret.
pub fn next_random(state: &mut u64) -> u64 {
    let mut x = (*state).max(1);
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    x.wrapping_mul(0x2545_F491_4F6C_DD1D)
}

/// The `Sec-WebSocket-Key` for a connection: 16 random bytes in base64.
#[must_use]
pub fn key(state: &mut u64) -> String {
    let mut bytes = [0_u8; 16];
    for half in bytes.chunks_mut(8) {
        half.copy_from_slice(&next_random(state).to_le_bytes());
    }
    base64(&bytes)
}

/// The HTTP request that asks the server to turn the connection into a WebSocket, with any headers
/// the server wants besides (an `Origin`, for one).
#[must_use]
pub fn request(host: &str, path: &str, key: &str, headers: &[(&str, &str)]) -> String {
    let mut out = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\nUser-Agent: LiveMarkets/0.5\r\n"
    );
    for (name, value) in headers {
        out.push_str(name);
        out.push_str(": ");
        out.push_str(value);
        out.push_str("\r\n");
    }
    out.push_str("\r\n");
    out
}

/// A frame to send. A client masks everything it sends, with a key that changes from frame to frame.
#[must_use]
pub fn encode(opcode: u8, payload: &[u8], mask: [u8; 4]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 14);
    out.push(0x80 | opcode);
    let len = payload.len();
    if let Ok(short) = u8::try_from(len)
        && short < 126
    {
        out.push(0x80 | short);
    } else if let Ok(medium) = u16::try_from(len) {
        out.push(0x80 | 0x7E);
        out.extend_from_slice(&medium.to_be_bytes());
    } else {
        out.push(0x80 | 0x7F);
        out.extend_from_slice(&(len as u64).to_be_bytes());
    }
    out.extend_from_slice(&mask);
    out.extend(payload.iter().enumerate().map(|(i, byte)| byte ^ mask[i % 4]));
    out
}

/// What the server's bytes say.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// The server accepted the upgrade: from here on the bytes are frames.
    Open,
    /// The server did not accept the upgrade, or sent what cannot be read.
    Refused,
    /// A message of text, whole.
    Text(String),
    /// A ping, to answer with a pong that carries the same bytes.
    Ping(Vec<u8>),
    /// The server ends the connection.
    Close,
}

/// Turns the bytes of a connection, as they come in pieces, into events.
#[derive(Debug, Default)]
pub struct Decoder {
    buf: Vec<u8>,
    /// The upgrade has been accepted.
    open: bool,
    /// The connection has ended or failed: nothing more is read.
    done: bool,
    /// The text of a message that comes in pieces, so far.
    partial: Vec<u8>,
    /// The pieces that follow are of a message that is not text, and are left out.
    skipping: bool,
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

impl Decoder {
    /// Takes the next bytes of the connection. Whatever they complete comes back, in order.
    pub fn push(&mut self, data: &[u8]) -> Vec<Event> {
        let mut events = Vec::new();
        if self.done {
            return events;
        }
        self.buf.extend_from_slice(data);
        if !self.open && !self.take_head(&mut events) {
            return events;
        }
        while !self.done {
            match self.take_frame() {
                Frame::More => break,
                Frame::Bad => {
                    self.done = true;
                    events.push(Event::Refused);
                }
                Frame::Is { fin, opcode, payload } => self.handle(fin, opcode, payload, &mut events),
            }
        }
        events
    }

    /// Reads the server's answer to the upgrade request. Whether the frames can be read now.
    fn take_head(&mut self, events: &mut Vec<Event>) -> bool {
        let Some(end) = find(&self.buf, b"\r\n\r\n") else {
            if self.buf.len() > 16_384 {
                self.done = true;
                events.push(Event::Refused);
            }
            return false;
        };
        let accepted = self.buf.starts_with(b"HTTP/1.1 101");
        self.buf.drain(..end + 4);
        if accepted {
            self.open = true;
            events.push(Event::Open);
        } else {
            self.done = true;
            events.push(Event::Refused);
        }
        accepted
    }

    fn take_frame(&mut self) -> Frame {
        let buf = &self.buf;
        if buf.len() < 2 {
            return Frame::More;
        }
        let (fin, opcode) = (buf[0] & 0x80 != 0, buf[0] & 0x0F);
        let masked = buf[1] & 0x80 != 0;
        let (len, mut at) = match buf[1] & 0x7F {
            126 => {
                let Some(bytes) = buf.get(2..4) else { return Frame::More };
                (usize::from(u16::from_be_bytes([bytes[0], bytes[1]])), 4)
            }
            127 => {
                let Some(bytes) = buf.get(2..10) else { return Frame::More };
                let mut eight = [0_u8; 8];
                eight.copy_from_slice(bytes);
                match usize::try_from(u64::from_be_bytes(eight)) {
                    Ok(len) if len <= MAX_FRAME => (len, 10),
                    _ => return Frame::Bad,
                }
            }
            short => (usize::from(short), 2),
        };
        if len > MAX_FRAME {
            return Frame::Bad;
        }
        let mask = if masked {
            let Some(bytes) = buf.get(at..at + 4) else { return Frame::More };
            at += 4;
            Some([bytes[0], bytes[1], bytes[2], bytes[3]])
        } else {
            None
        };
        let Some(body) = buf.get(at..at + len) else { return Frame::More };
        let mut payload = body.to_vec();
        if let Some(mask) = mask {
            for (i, byte) in payload.iter_mut().enumerate() {
                *byte ^= mask[i % 4];
            }
        }
        self.buf.drain(..at + len);
        Frame::Is { fin, opcode, payload }
    }

    fn handle(&mut self, fin: bool, opcode: u8, payload: Vec<u8>, events: &mut Vec<Event>) {
        match opcode {
            TEXT | CONTINUATION if opcode == TEXT || !self.skipping => {
                if opcode == TEXT {
                    self.partial.clear();
                    self.skipping = false;
                }
                self.partial.extend_from_slice(&payload);
                if self.partial.len() > MAX_FRAME {
                    self.done = true;
                    events.push(Event::Refused);
                } else if fin {
                    events.push(Event::Text(String::from_utf8_lossy(&std::mem::take(&mut self.partial)).into_owned()));
                }
            }
            BINARY | CONTINUATION => self.skipping = !fin,
            PING => events.push(Event::Ping(payload)),
            CLOSE => {
                self.done = true;
                events.push(Event::Close);
            }
            _ => {}
        }
    }
}

enum Frame {
    /// The frame is not all here yet.
    More,
    /// It cannot be read.
    Bad,
    Is { fin: bool, opcode: u8, payload: Vec<u8> },
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEAD: &[u8] = b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: x\r\n\r\n";

    /// A frame as the server writes it: not masked.
    fn server(fin: bool, opcode: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = vec![if fin { 0x80 } else { 0 } | opcode];
        match payload.len() {
            n if n < 126 => out.push(u8::try_from(n).expect("BUG: short")),
            n if n <= 0xFFFF => {
                out.push(126);
                out.extend_from_slice(&u16::try_from(n).expect("BUG: medium").to_be_bytes());
            }
            n => {
                out.push(127);
                out.extend_from_slice(&(n as u64).to_be_bytes());
            }
        }
        out.extend_from_slice(payload);
        out
    }

    fn open() -> Decoder {
        let mut d = Decoder::default();
        assert_eq!(d.push(HEAD), [Event::Open]);
        d
    }

    #[test]
    fn base64_follows_the_standard_with_padding() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(&[0xFF, 0xFE, 0xFD, 0xFC]), "//79/A==");
    }

    #[test]
    fn a_key_is_sixteen_bytes_of_base64_and_changes_each_time() {
        let mut state = 12_345;
        let (a, b) = (key(&mut state), key(&mut state));
        assert_eq!((a.len(), b.len()), (24, 24));
        assert!(a.ends_with("==") && b.ends_with("=="));
        assert_ne!(a, b);
        let mut again = 12_345;
        assert_eq!(key(&mut again), a, "the same seed gives the same key");
        let mut zero = 0;
        assert_ne!(next_random(&mut zero), 0, "a zero seed does not stick");
    }

    #[test]
    fn the_upgrade_request_names_the_host_and_the_key() {
        let text = request("api.hyperliquid.xyz", "/ws", "dGhlIHNhbXBsZSBub25jZQ==", &[("Origin", "https://example.test")]);
        assert!(text.starts_with("GET /ws HTTP/1.1\r\nHost: api.hyperliquid.xyz\r\n"));
        assert!(text.contains("Upgrade: websocket\r\n") && text.contains("Connection: Upgrade\r\n"));
        assert!(text.contains("Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n"));
        assert!(text.contains("Sec-WebSocket-Version: 13\r\n"));
        assert!(text.contains("\r\nOrigin: https://example.test\r\n"));
        assert!(text.ends_with("\r\n\r\n"));
        assert!(!request("h", "/", "k", &[]).contains("Origin"));
    }

    #[test]
    fn a_short_frame_is_masked_byte_by_byte() {
        let frame = encode(TEXT, b"hi", [1, 2, 3, 4]);
        assert_eq!(frame, [0x81, 0x82, 1, 2, 3, 4, b'h' ^ 1, b'i' ^ 2]);
    }

    #[test]
    fn longer_frames_say_their_length_in_two_or_eight_bytes() {
        let medium = encode(TEXT, &[b'a'; 200], [0, 0, 0, 0]);
        assert_eq!(&medium[..4], &[0x81, 0xFE, 0x00, 0xC8]);
        assert_eq!(medium.len(), 4 + 4 + 200);
        let long = encode(TEXT, &vec![b'a'; 70_000], [0, 0, 0, 0]);
        assert_eq!(&long[..10], &[0x81, 0xFF, 0, 0, 0, 0, 0, 1, 0x11, 0x70]);
        let pong = encode(PONG, b"x", [9, 9, 9, 9]);
        assert_eq!(pong[0], 0x8A);
    }

    #[test]
    fn a_frame_that_a_server_would_unmask_reads_back() {
        // What this side sends is what a server reads: unmask it and the text is there.
        let mask = [0xA1, 0xB2, 0xC3, 0xD4];
        let sent = encode(TEXT, b"hello hyperliquid", mask);
        let payload = &sent[6..];
        let plain: Vec<u8> = payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]).collect();
        assert_eq!(plain, b"hello hyperliquid");
    }

    #[test]
    fn the_upgrade_answer_opens_the_connection_even_in_pieces() {
        let mut d = Decoder::default();
        assert!(d.push(&HEAD[..20]).is_empty());
        assert!(d.push(&HEAD[20..HEAD.len() - 2]).is_empty());
        assert_eq!(d.push(&HEAD[HEAD.len() - 2..]), [Event::Open]);
    }

    #[test]
    fn frames_that_come_with_the_answer_are_read_at_once() {
        let mut bytes = HEAD.to_vec();
        bytes.extend(server(true, TEXT, b"one"));
        bytes.extend(server(true, TEXT, b"two"));
        let mut d = Decoder::default();
        assert_eq!(d.push(&bytes), [Event::Open, Event::Text("one".into()), Event::Text("two".into())]);
    }

    #[test]
    fn an_answer_that_is_not_an_upgrade_is_refused() {
        let mut d = Decoder::default();
        assert_eq!(d.push(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n"), [Event::Refused]);
        assert!(d.push(&server(true, TEXT, b"late")).is_empty(), "nothing is read after that");
    }

    #[test]
    fn a_frame_in_pieces_is_one_message() {
        let mut d = open();
        let frame = server(true, TEXT, br#"{"channel":"pong"}"#);
        for piece in frame.chunks(5).take(frame.chunks(5).count() - 1) {
            assert!(d.push(piece).is_empty());
        }
        let last = frame.chunks(5).last().expect("BUG: a frame has pieces");
        assert_eq!(d.push(last), [Event::Text(r#"{"channel":"pong"}"#.into())]);
    }

    #[test]
    fn long_frames_are_read_with_their_extended_lengths() {
        let mut d = open();
        let medium = "m".repeat(300);
        assert_eq!(d.push(&server(true, TEXT, medium.as_bytes())), [Event::Text(medium)]);
        let long = "l".repeat(70_000);
        assert_eq!(d.push(&server(true, TEXT, long.as_bytes())), [Event::Text(long)]);
    }

    #[test]
    fn a_message_in_fragments_comes_whole() {
        let mut d = open();
        assert!(d.push(&server(false, TEXT, b"hel")).is_empty());
        assert!(d.push(&server(false, CONTINUATION, b"lo ")).is_empty());
        assert_eq!(d.push(&server(true, CONTINUATION, b"there")), [Event::Text("hello there".into())]);
    }

    #[test]
    fn a_ping_between_fragments_is_answered_and_the_message_survives() {
        let mut d = open();
        let mut bytes = server(false, TEXT, b"ab");
        bytes.extend(server(true, PING, b"?"));
        bytes.extend(server(true, CONTINUATION, b"cd"));
        assert_eq!(d.push(&bytes), [Event::Ping(b"?".to_vec()), Event::Text("abcd".into())]);
    }

    #[test]
    fn binary_messages_and_pongs_are_left_out() {
        let mut d = open();
        let mut bytes = server(false, BINARY, b"\x00\x01");
        bytes.extend(server(true, CONTINUATION, b"\x02"));
        bytes.extend(server(true, 0xA, b""));
        bytes.extend(server(true, TEXT, b"after"));
        assert_eq!(d.push(&bytes), [Event::Text("after".into())]);
    }

    #[test]
    fn a_close_ends_the_reading() {
        let mut d = open();
        let mut bytes = server(true, CLOSE, &[0x03, 0xE8]);
        bytes.extend(server(true, TEXT, b"never"));
        assert_eq!(d.push(&bytes), [Event::Close]);
        assert!(d.push(&server(true, TEXT, b"never")).is_empty());
    }

    #[test]
    fn a_frame_far_too_big_is_refused_and_a_masked_one_is_still_read() {
        let mut d = open();
        let mut huge = vec![0x81, 127];
        huge.extend_from_slice(&(u64::try_from(MAX_FRAME).expect("BUG: fits") + 1).to_be_bytes());
        assert_eq!(d.push(&huge), [Event::Refused]);

        let mut d = open();
        let masked = encode(TEXT, b"masked", [5, 6, 7, 8]);
        assert_eq!(d.push(&masked), [Event::Text("masked".into())]);
    }
}
