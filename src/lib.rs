//! Scan one flatbed page from a Brother network scanner over Brother's proprietary
//! protocol on TCP 54921 (the one brscan3/brscan4 use), with no driver installed.
//!
//! Stdlib only. Gray scans come back as binary PGM (decoded from the scanner's PackBits
//! stream), color scans as the JPEG the scanner produces. Set `BROTHER_NETSCAN_DEBUG=1`
//! to log the exchange to stderr.
//!
//! ```no_run
//! let opts = brother_netscan::ScanOptions::default(); // 300 dpi gray
//! let (image, ext) = brother_netscan::scan("192.0.2.10", opts)?;
//! std::fs::write(format!("page.{ext}"), image)?;
//! # Ok::<(), brother_netscan::ScanError>(())
//! ```

use std::fmt;
use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::str::FromStr;
use std::time::Duration;

/// TCP port of the scan service.
pub const PORT: u16 = 54921;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const REPLY_TIMEOUT: Duration = Duration::from_secs(10);
/// Longest silence allowed while the page is scanned (lamp warm-up, head travel).
const DATA_TIMEOUT: Duration = Duration::from_secs(60);
/// How long to wait for the rest of a reply that arrived in pieces.
const PIECE_TIMEOUT: Duration = Duration::from_millis(500);
/// Upper bounds for values the scanner reports (well above any flatbed).
const MAX_DPI: u32 = 9600;
const MAX_PX: u32 = 100_000;

/// Scan resolution. What the scanner does with each value is its own decision; on a
/// DCP-1610W: 200/300/400/600 are native, 800 is refused (it offers 600), and 1200 is
/// scanned optically at 600x1200, then widened to 1200x1200 by doubling each pixel
/// (gray only; color at 1200 is refused). Anything the device does not do as asked fails with
/// [`ScanError::UnsupportedResolution`]; nothing falls back silently.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Dpi {
    /// 200 dpi.
    D200,
    /// 300 dpi, the default: good for OCR (Tesseract).
    #[default]
    D300,
    /// 400 dpi.
    D400,
    /// 600 dpi.
    D600,
    /// 800 dpi.
    D800,
    /// 1200 dpi.
    D1200,
}

impl Dpi {
    /// Every selectable value, in order.
    pub const ALL: [Dpi; 6] = [
        Dpi::D200,
        Dpi::D300,
        Dpi::D400,
        Dpi::D600,
        Dpi::D800,
        Dpi::D1200,
    ];

    /// Dots per inch as a number.
    pub fn value(self) -> u32 {
        match self {
            Dpi::D200 => 200,
            Dpi::D300 => 300,
            Dpi::D400 => 400,
            Dpi::D600 => 600,
            Dpi::D800 => 800,
            Dpi::D1200 => 1200,
        }
    }
}

impl fmt::Display for Dpi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.value())
    }
}

impl FromStr for Dpi {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        Dpi::ALL
            .into_iter()
            .find(|d| s.trim().parse() == Ok(d.value()))
            .ok_or_else(|| {
                format!("unsupported dpi {s:?}: choose one of 200, 300, 400, 600, 800, 1200")
            })
    }
}

/// Color mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// 8-bit gray, returned as PGM. The default.
    #[default]
    Gray,
    /// 24-bit color, returned as the scanner's JPEG.
    Color,
}

/// What to scan. The area is always the whole flatbed (209 × 291 mm on a DCP-1610W).
#[derive(Clone, Copy, Debug, Default)]
pub struct ScanOptions {
    /// Resolution.
    pub dpi: Dpi,
    /// Gray or color.
    pub mode: Mode,
}

/// Result of a streamed scan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScanInfo {
    /// File extension of the bytes written: `"pgm"` or `"jpg"`.
    pub ext: &'static str,
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Resolution the scanner actually used (x, y), before any widening.
    pub scanned_dpi: (u32, u32),
}

/// Why a scan failed.
#[derive(Debug)]
pub enum ScanError {
    /// Network trouble: refused, unreachable, timed out, connection dropped.
    Io(io::Error),
    /// The scanner answered `-NG ...` instead of `+OK 200` (`-NG 401`: another scan holds it).
    Busy(String),
    /// The scanner sent this status byte where image data should start.
    DeviceStatus(u8),
    /// The scanner would not scan at the requested resolution; `offered` is what it proposed.
    UnsupportedResolution {
        /// Requested dpi.
        requested: u32,
        /// (x, y) dpi the scanner offered instead.
        offered: (u32, u32),
    },
    /// The scanner sent something this crate does not understand.
    Protocol(String),
}

impl fmt::Display for ScanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScanError::Io(e) => write!(f, "{e}"),
            ScanError::Busy(g) => write!(
                f,
                "scanner busy ({g}): another computer is scanning or a scan is still finishing; retry in a minute"
            ),
            ScanError::DeviceStatus(c) => {
                let hint = match c {
                    0xc2 => "nothing to scan / no document",
                    _ => "device error or not ready, check the printer's display",
                };
                write!(
                    f,
                    "scanner returned status byte 0x{c:02x} instead of image data: {hint}"
                )
            }
            ScanError::UnsupportedResolution {
                requested,
                offered: (x, y),
            } => write!(
                f,
                "scanner does not support {requested} dpi in this mode (it offered {x}x{y}); choose another resolution"
            ),
            ScanError::Protocol(m) => write!(f, "unexpected reply from scanner: {m}"),
        }
    }
}

impl std::error::Error for ScanError {}

impl From<io::Error> for ScanError {
    fn from(e: io::Error) -> Self {
        ScanError::Io(e)
    }
}

impl From<ScanError> for io::Error {
    fn from(e: ScanError) -> Self {
        match e {
            ScanError::Io(e) => e,
            ScanError::Busy(_) => io::Error::new(io::ErrorKind::ResourceBusy, e),
            other => io::Error::other(other),
        }
    }
}

fn debug() -> bool {
    std::env::var_os("BROTHER_NETSCAN_DEBUG").is_some()
}

fn hex(b: &[u8]) -> String {
    b.iter()
        .map(|x| format!("{x:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Scan one flatbed page into memory. Returns the image file bytes and their extension.
// ponytail: holds the whole image in memory (gray 1200 dpi A4 is ~136 MB); use scan_to to stream.
pub fn scan(host: &str, opts: ScanOptions) -> Result<(Vec<u8>, &'static str), ScanError> {
    let mut out = Vec::new();
    let info = scan_to(host, opts, &mut out)?;
    Ok((out, info.ext))
}

/// Scan one flatbed page, streaming the image file (PGM or JPEG) into `out`.
pub fn scan_to<W: Write>(host: &str, opts: ScanOptions, out: W) -> Result<ScanInfo, ScanError> {
    let addr = (host, PORT)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("cannot resolve {host}")))?;
    let mut s = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).map_err(|e| {
        let msg = match e.kind() {
            io::ErrorKind::ConnectionRefused => {
                format!("{addr}: connection refused (scan service off, or not a Brother network scanner)")
            }
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => {
                format!("{addr}: no answer within 5 s (printer off or unreachable)")
            }
            _ => format!("{addr}: {e}"),
        };
        io::Error::new(e.kind(), msg)
    })?;
    s.set_read_timeout(Some(REPLY_TIMEOUT))?;
    s.set_write_timeout(Some(REPLY_TIMEOUT))?;
    s.set_nodelay(true)?;

    // 1. Greeting: "+OK 200\r\n" ready, "-NG 401\r\n" busy.
    let greet = read_line(&mut s).map_err(|e| ctx(e, "waiting for greeting"))?;
    let greet = String::from_utf8_lossy(&greet).trim().to_string();
    if debug() {
        eprintln!("< {greet:?}");
    }
    if greet.starts_with("-NG") {
        return Err(ScanError::Busy(greet));
    }
    if !greet.starts_with('+') {
        return Err(ScanError::Protocol(format!("greeting {greet:?}")));
    }

    // 2. ESC I: ask what the scanner will do for this resolution and mode.
    let mode = match opts.mode {
        Mode::Gray => "GRAY64",
        Mode::Color => "CGRAY",
    };
    let req = opts.dpi.value();
    send(&mut s, &command(&format!("I\nR={req},{req}\nM={mode}\n")))?;
    let reply = read_i_reply(&mut s).map_err(|e| ctx(e, "waiting for the ESC I reply"))?;
    if debug() {
        eprintln!("< [{}]", hex(&reply));
    }
    let p = parse_i_reply(&reply)
        .ok_or_else(|| ScanError::Protocol(format!("ESC I reply [{}]", hex(&reply))))?;
    let (xdpi, ydpi, width, height) = (p[0], p[1], p[4], p[6]);
    // The device's numbers size our buffers and arithmetic; refuse anything absurd.
    let sane = |v: u32, max: u32| (1..=max).contains(&v);
    if !(sane(xdpi, MAX_DPI) && sane(ydpi, MAX_DPI) && sane(width, MAX_PX) && sane(height, MAX_PX))
    {
        return Err(ScanError::Protocol(format!("implausible scan area {p:?}")));
    }
    let xrep = match (xdpi, ydpi) {
        (x, y) if x == req && y == req => 1,
        // Optical 600x1200: widen each row to get square pixels. JPEG can't be widened here.
        (x, y) if y == req && x * 2 == req && opts.mode == Mode::Gray => 2,
        offered => {
            return Err(ScanError::UnsupportedResolution {
                requested: req,
                offered,
            });
        }
    };

    // 3. ESC X: start the scan over the whole area, at exactly what the scanner offered.
    let comp = match opts.mode {
        Mode::Gray => "RLENGTH",
        Mode::Color => "JPEG",
    };
    send(
        &mut s,
        &command(&format!(
            "X\nR={xdpi},{ydpi}\nM={mode}\nC={comp}\nJ=MID\nB=50\nN=50\nA=0,0,{width},{height}\nD=SIN\nG=1\nL=128\n"
        )),
    )?;

    // 4. Image records until the end marker.
    s.set_read_timeout(Some(DATA_TIMEOUT))?;
    let mut sink = Sink::new(out, width as usize, height as usize, xrep);
    receive(&mut s, &mut sink).map_err(|e| match e {
        ScanError::Io(e) => ctx(e, "receiving image data"),
        e => e,
    })?;
    let _ = s.shutdown(std::net::Shutdown::Both);
    let ext = sink.finish()?;
    Ok(ScanInfo {
        ext,
        width: width * xrep as u32,
        height,
        scanned_dpi: (xdpi, ydpi),
    })
}

fn ctx(e: io::Error, what: &str) -> ScanError {
    let (kind, msg) = match e.kind() {
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => {
            (io::ErrorKind::TimedOut, format!("timed out {what}"))
        }
        io::ErrorKind::UnexpectedEof => (
            e.kind(),
            format!("scanner closed the connection while {what}"),
        ),
        k => (k, format!("{what}: {e}")),
    };
    ScanError::Io(io::Error::new(kind, msg))
}

/// Request packet: ESC, the ASCII body (type letter and `K=V` fields, each ending in `\n`), 0x80.
fn command(body: &str) -> Vec<u8> {
    let mut v = vec![0x1b];
    v.extend_from_slice(body.as_bytes());
    v.push(0x80);
    v
}

fn send(s: &mut TcpStream, b: &[u8]) -> Result<(), ScanError> {
    if debug() {
        eprintln!("> {:?}", String::from_utf8_lossy(b));
    }
    Ok(s.write_all(b)?)
}

fn read_line(r: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut b = [0u8];
    while out.len() < 64 && b[0] != b'\n' {
        r.read_exact(&mut b)?;
        out.push(b[0]);
    }
    Ok(out)
}

/// Reads the ESC I reply, which may arrive in more than one packet. It ends with a NUL on
/// the devices seen so far; without one, a short pause after some data ends it.
fn read_i_reply(s: &mut TcpStream) -> io::Result<Vec<u8>> {
    let (mut reply, mut buf) = (Vec::new(), [0u8; 256]);
    loop {
        match s.read(&mut buf) {
            Ok(0) if reply.is_empty() => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(0) => break,
            Ok(n) => reply.extend_from_slice(&buf[..n]),
            Err(e)
                if !reply.is_empty()
                    && matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
            {
                break;
            }
            Err(e) => return Err(e),
        }
        let digits = reply.iter().position(u8::is_ascii_digit);
        if digits.is_some_and(|d| reply[d..].contains(&0)) || reply.len() >= 256 {
            break;
        }
        s.set_read_timeout(Some(PIECE_TIMEOUT))?;
    }
    s.set_read_timeout(Some(REPLY_TIMEOUT))?;
    Ok(reply)
}

/// ESC I reply: binary length prefix, then ASCII
/// `xdpi,ydpi,?,width_mm,width_px,height_mm,height_px` (a trailing comma and NUL may follow).
fn parse_i_reply(b: &[u8]) -> Option<Vec<u32>> {
    let start = b.iter().position(u8::is_ascii_digit)?;
    let txt: String = b[start..]
        .iter()
        .take_while(|c| c.is_ascii_digit() || **c == b',')
        .map(|&c| c as char)
        .collect();
    let v: Vec<u32> = txt
        .split(',')
        .filter(|x| !x.is_empty())
        .map(|x| x.parse().ok())
        .collect::<Option<_>>()?;
    (v.len() >= 7).then_some(v)
}

fn u16le(b: &[u8]) -> usize {
    b[0] as usize | (b[1] as usize) << 8
}

/// Reads image records into `sink` until the end marker. Two framings exist:
/// - brscan3/4 (this device): `id, 07 00, page u16, ?, progress u16, ? u16, len u16, payload`
/// - brscan2: `id, len u16, payload`
///
/// `0x82` + 9 bytes ends a page, `0x80` ends the job (`0x81`: another page waits, ADF).
/// Record ids: `0x40` raw gray, `0x42` PackBits gray, `0x64` JPEG.
fn receive<R: Read, W: Write>(r: &mut R, sink: &mut Sink<W>) -> Result<(), ScanError> {
    let mut id = [0u8];
    let mut records = 0u32;
    let mut payload = Vec::new();
    loop {
        r.read_exact(&mut id)?;
        let id = id[0];
        match id {
            0x80 | 0x81 => {
                if debug() {
                    eprintln!("< end {id:02x} after {records} records");
                }
                return Ok(());
            }
            0x82 => {
                let mut rest = [0u8; 9];
                r.read_exact(&mut rest)?;
                if debug() {
                    eprintln!("< page end 82 [{}]", hex(&rest));
                }
                continue;
            }
            0x40 | 0x42 | 0x64 => {}
            other => return Err(ScanError::DeviceStatus(other)),
        }
        let mut h = [0u8; 2];
        r.read_exact(&mut h)?;
        let mut rest = [0u8; 9];
        let len = if h == [0x07, 0x00] {
            r.read_exact(&mut rest)?;
            u16le(&rest[7..9])
        } else {
            u16le(&h)
        };
        records += 1;
        if debug() && (records <= 2 || records.is_multiple_of(200)) {
            eprintln!(
                "< record #{records}: {id:02x} [{}] [{}] len={len}",
                hex(&h),
                hex(&rest)
            );
        }
        payload.resize(len, 0);
        r.read_exact(&mut payload)?;
        sink.record(id, &payload)?;
    }
}

/// Streaming PackBits decoder (TIFF/Apple RLE): a byte `n < 0x80` copies the next `n + 1`
/// bytes, `n > 0x80` repeats the next byte `257 - n` times, `0x80` is skipped. Runs may be
/// split across [`feed`](PackBits::feed) calls; a truncated run at the very end is dropped.
#[derive(Debug, Default)]
pub struct PackBits {
    literal: usize,
    repeat: usize,
}

impl PackBits {
    /// Decode `src`, appending the output to `out`.
    pub fn feed(&mut self, src: &[u8], out: &mut Vec<u8>) {
        for &b in src {
            if self.literal > 0 {
                out.push(b);
                self.literal -= 1;
            } else if self.repeat > 0 {
                out.extend(std::iter::repeat_n(b, self.repeat));
                self.repeat = 0;
            } else if b < 0x80 {
                self.literal = b as usize + 1;
            } else if b > 0x80 {
                self.repeat = 0x101 - b as usize;
            }
        }
    }
}

/// Turns records into an image file: JPEG passes through, gray becomes PGM rows
/// (each pixel repeated `xrep` times; missing rows padded white, extra rows dropped).
struct Sink<W: Write> {
    out: W,
    width: usize,
    height: usize,
    xrep: usize,
    kind: u8,
    rle: PackBits,
    px: Vec<u8>,
    row: Vec<u8>,
    rows: usize,
}

impl<W: Write> Sink<W> {
    fn new(out: W, width: usize, height: usize, xrep: usize) -> Self {
        Sink {
            out,
            width,
            height,
            xrep,
            kind: 0,
            rle: PackBits::default(),
            px: Vec::new(),
            row: Vec::new(),
            rows: 0,
        }
    }

    fn record(&mut self, kind: u8, data: &[u8]) -> Result<(), ScanError> {
        if self.kind == 0 {
            self.kind = kind;
            if kind != 0x64 {
                write!(
                    self.out,
                    "P5\n{} {}\n255\n",
                    self.width * self.xrep,
                    self.height
                )?;
            }
        } else if kind != self.kind {
            return Err(ScanError::Protocol(format!(
                "record type changed {:02x} -> {kind:02x}",
                self.kind
            )));
        }
        match kind {
            0x64 => return Ok(self.out.write_all(data)?),
            0x42 => self.rle.feed(data, &mut self.px),
            _ => self.px.extend_from_slice(data),
        }
        let full = self.px.len() / self.width * self.width;
        for i in (0..full).step_by(self.width) {
            self.write_row(i)?;
        }
        self.px.drain(..full);
        Ok(())
    }

    fn write_row(&mut self, start: usize) -> io::Result<()> {
        if self.rows < self.height {
            let src = &self.px[start..start + self.width];
            if self.xrep == 1 {
                self.out.write_all(src)?;
            } else {
                self.row.clear();
                self.row
                    .extend(src.iter().flat_map(|&p| std::iter::repeat_n(p, self.xrep)));
                self.out.write_all(&self.row)?;
            }
        }
        self.rows += 1;
        Ok(())
    }

    fn finish(mut self) -> Result<&'static str, ScanError> {
        if self.kind == 0 {
            return Err(ScanError::Protocol("scan ended without image data".into()));
        }
        if self.kind != 0x64 {
            if debug() && self.rows != self.height {
                eprintln!("note: got {} rows, expected {}", self.rows, self.height);
            }
            let white = vec![0xff; self.width * self.xrep];
            for _ in self.rows..self.height {
                self.out.write_all(&white)?;
            }
        }
        self.out.flush()?;
        Ok(if self.kind == 0x64 { "jpg" } else { "pgm" })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One brscan4-style record with a 12-byte header.
    fn rec12(id: u8, payload: &[u8]) -> Vec<u8> {
        let mut v = vec![id, 0x07, 0x00, 0x01, 0x00, 0x84, 0x00, 0x00, 0x00, 0x00];
        v.extend((payload.len() as u16).to_le_bytes());
        v.extend_from_slice(payload);
        v
    }

    fn run(
        stream: &[u8],
        width: usize,
        height: usize,
        xrep: usize,
    ) -> Result<(Vec<u8>, &'static str), ScanError> {
        let mut out = Vec::new();
        let mut sink = Sink::new(&mut out, width, height, xrep);
        receive(&mut &stream[..], &mut sink)?;
        let ext = sink.finish()?;
        Ok((out, ext))
    }

    #[test]
    fn decodes_rle_gray_frame_split_mid_run() {
        // 4x2 image: row 0 = run of four 0xff, row 1 = literal 00 10 20 30. The literal run
        // is split across two records, then page end and job end.
        let mut s = rec12(0x42, &[0xfd, 0xff, 0x03, 0x00]);
        s.extend(rec12(0x42, &[0x10, 0x20, 0x30]));
        s.extend([
            0x82, 0x07, 0x00, 0x01, 0x00, 0x84, 0x00, 0x00, 0x00, 0x00, 0x80,
        ]);
        let (img, ext) = run(&s, 4, 2, 1).unwrap();
        assert_eq!(ext, "pgm");
        assert_eq!(img, b"P5\n4 2\n255\n\xff\xff\xff\xff\x00\x10\x20\x30");
    }

    #[test]
    fn widens_pads_and_passes_jpeg_through() {
        // 2x2 at xrep 2, only one row sent: widened to 4 px, missing row padded white.
        let s = [0x40, 0x02, 0x00, 0x11, 0x22, 0x80];
        let (img, _) = run(&s, 2, 2, 2).unwrap();
        assert_eq!(img, b"P5\n4 2\n255\n\x11\x11\x22\x22\xff\xff\xff\xff");
        let mut s = rec12(0x64, b"\xff\xd8jpeg");
        s.push(0x80);
        assert_eq!(run(&s, 9, 9, 1).unwrap(), (b"\xff\xd8jpeg".to_vec(), "jpg"));
    }

    #[test]
    fn reports_status_bytes_and_parses_replies() {
        assert!(matches!(
            run(&[0xc2], 4, 4, 1),
            Err(ScanError::DeviceStatus(0xc2))
        ));
        assert!(matches!(run(&[0x80], 4, 4, 1), Err(ScanError::Protocol(_))));
        assert_eq!(
            parse_i_reply(b"\x00\x1d\x00300,300,2,209,2480,291,3437,\x00").unwrap(),
            [300, 300, 2, 209, 2480, 291, 3437]
        );
        assert_eq!(
            parse_i_reply(b"\x1b\x00300,300,2,209,2480,346,4086").unwrap()[6],
            4086
        );
        assert!(parse_i_reply(b"\x00\x03\x00300").is_none());
        assert_eq!(command("I\nR=300,300\n"), b"\x1bI\nR=300,300\n\x80");
    }

    #[test]
    fn dpi_parses_only_allowed_values() {
        assert_eq!("1200".parse(), Ok(Dpi::D1200));
        assert_eq!(Dpi::default().value(), 300);
        assert!(
            "250"
                .parse::<Dpi>()
                .unwrap_err()
                .contains("200, 300, 400, 600, 800, 1200")
        );
    }
}
