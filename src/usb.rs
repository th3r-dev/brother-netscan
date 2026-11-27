//! Brother printers on a USB cable (feature `usb`): find them, read their status, scan
//! with the same protocol as TCP 54921, and send print jobs.
//!
//! Tested on a DCP-1610W (`04f9:035b`). Interface 0 is the printer (class 7, bulk IN `0x82`,
//! OUT `0x01`), interface 1 the scanner (class 255, bulk IN `0x85`, OUT `0x04`). The scanner
//! speaks the same ESC I / ESC X / record stream as the network. Two vendor control requests
//! open and close its session in place of the `+OK 200` greeting.
//!
//! ```no_run
//! # #[cfg(feature = "usb")] {
//! use brother_netscan::{usb, ScanOptions};
//! let printer = usb::find().into_iter().next().expect("no Brother printer on USB");
//! let file = std::fs::File::create("page.pgm")?;
//! printer.scan_to(ScanOptions::default(), std::io::BufWriter::new(file))?;
//! # }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use crate::{
    Dpi, Mode, REPLY_TIMEOUT, ScanError, ScanInfo, ScanOptions, Supported, debug, hex, negotiate,
    scan_core,
};
use rusb::{Direction, GlobalContext, TransferType};
use std::fmt;
use std::io::{self, Read, Write};
use std::thread::sleep;
use std::time::{Duration, Instant};

/// Brother's USB vendor id.
pub const BROTHER_VID: u16 = 0x04f9;
const CTRL_TIMEOUT: Duration = Duration::from_secs(2);
/// The printer stops reading while it is busy or out of paper.
const PRINT_CHUNK_TIMEOUT: Duration = Duration::from_secs(300);
const PRINT_CHUNK: usize = 64 * 1024;

#[derive(Clone, Copy, Debug)]
struct Iface {
    number: u8,
    alt: u8,
    bulk_in: u8,
    bulk_out: u8,
}

/// A Brother printer on USB. Holds no open handle: each call opens the device, claims the
/// interface it needs and releases it when done.
#[derive(Clone)]
pub struct UsbPrinter {
    device: rusb::Device<GlobalContext>,
    model: String,
    key: String,
    printer: Option<Iface>,
    scanner: Option<Iface>,
}

impl fmt::Debug for UsbPrinter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UsbPrinter")
            .field("model", &self.model)
            .field("key", &self.key)
            .field("bus", &self.device.bus_number())
            .field("address", &self.device.address())
            .finish()
    }
}

/// Every Brother device on USB with a printer or scanner interface. Empty when there is
/// none, or when USB can't be read at all.
pub fn find() -> Vec<UsbPrinter> {
    let Ok(list) = rusb::devices() else {
        return Vec::new();
    };
    list.iter().filter_map(UsbPrinter::new).collect()
}

impl UsbPrinter {
    fn new(device: rusb::Device<GlobalContext>) -> Option<Self> {
        let desc = device.device_descriptor().ok()?;
        if desc.vendor_id() != BROTHER_VID {
            return None;
        }
        let (mut printer, mut scanner) = (None, None);
        for i in device.active_config_descriptor().ok()?.interfaces() {
            for a in i.descriptors() {
                let (mut bulk_in, mut bulk_out) = (None, None);
                for e in a.endpoint_descriptors() {
                    match (e.transfer_type(), e.direction()) {
                        (TransferType::Bulk, Direction::In) => bulk_in = Some(e.address()),
                        (TransferType::Bulk, Direction::Out) => bulk_out = Some(e.address()),
                        _ => {}
                    }
                }
                let (Some(bulk_in), Some(bulk_out)) = (bulk_in, bulk_out) else {
                    continue;
                };
                let f = Iface {
                    number: a.interface_number(),
                    alt: a.setting_number(),
                    bulk_in,
                    bulk_out,
                };
                match a.class_code() {
                    7 => printer = printer.or(Some(f)),
                    255 => scanner = scanner.or(Some(f)),
                    _ => {}
                }
            }
        }
        if printer.is_none() && scanner.is_none() {
            return None;
        }
        let (mut serial, mut product, mut id) = (None, None, None);
        if let Ok(h) = device.open() {
            serial = h.read_serial_number_string_ascii(&desc).ok();
            product = h.read_product_string_ascii(&desc).ok();
            // Read without claiming the interface, so listing never gets in the way of a
            // status call or a print job. Linux needs a claim here and falls back.
            id = printer.and_then(|i| read_device_id(&h, i).ok());
        }
        // The device id names the model the way the network does ("DCP-1610W series").
        let model = id
            .as_deref()
            .and_then(|id| device_id_field(id, "MDL").or(device_id_field(id, "MODEL")))
            .map(str::to_string)
            .or(product)
            .unwrap_or_else(|| format!("USB device {:04x}", desc.product_id()));
        // ponytail: two printers without serial numbers share a key; none seen so far.
        let serial =
            serial.unwrap_or_else(|| format!("{:04x}:{:04x}", desc.vendor_id(), desc.product_id()));
        Some(UsbPrinter {
            device,
            model,
            key: key_of(&serial),
            printer,
            scanner,
        })
    }

    /// The model from the device id's `MDL` field, e.g. `DCP-1610W series` (the USB
    /// product string when the device id can't be read).
    pub fn model(&self) -> &str {
        &self.model
    }

    /// A short, stable name for this device: a hash of its USB serial number, so the
    /// serial itself never has to be shown or stored.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Whether the device has a scanner interface (DCP and MFC models do, HL models don't).
    pub fn can_scan(&self) -> bool {
        self.scanner.is_some()
    }

    fn claim_printer(&self) -> io::Result<Claimed> {
        let iface = self
            .printer
            .ok_or_else(|| io::Error::other("usb: this device has no printer interface"))?;
        Claimed::open(&self.device, iface).map_err(|e| match e {
            rusb::Error::Access | rusb::Error::Busy => io::Error::new(
                io::ErrorKind::ResourceBusy,
                format!("usb: the printer interface is in use by another program ({e})"),
            ),
            e => usb_io(e),
        })
    }

    /// IEEE 1284 device id (printer class request GET_DEVICE_ID), e.g.
    /// `MFG:Brother;CMD:PJL,HBP;MDL:DCP-1610W series;CLS:PRINTER;…`. Serial number
    /// fields (`SN`, `SERN`, `SERIALNUMBER`) are removed.
    pub fn device_id(&self) -> io::Result<String> {
        let c = self.claim_printer()?;
        read_device_id(&c.handle, c.iface).map_err(usb_io)
    }

    /// IEEE 1284 port status (printer class request GET_PORT_STATUS).
    pub fn port_status(&self) -> io::Result<PortStatus> {
        let c = self.claim_printer()?;
        let mut buf = [0u8; 1];
        c.handle
            .read_control(0xa1, 1, 0, c.iface.number as u16, &mut buf, CTRL_TIMEOUT)
            .map_err(usb_io)?;
        Ok(PortStatus(buf[0]))
    }

    /// Sends `@PJL <cmd>` (for example `INFO STATUS` or `INFO PAGECOUNT`) inside a UEL
    /// wrapper on the printer interface and returns the reply, without its form feed.
    /// Empty when the printer stays silent for 10 s. Read it with [`pjl_fields`].
    pub fn pjl(&self, cmd: &str) -> io::Result<String> {
        if cmd.contains(['\x1b', '\r', '\n']) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a PJL command is one line without ESC",
            ));
        }
        let c = self.claim_printer()?;
        c.drain();
        let job = format!("\x1b%-12345X@PJL\r\n@PJL {cmd}\r\n\x1b%-12345X");
        c.write_all(job.as_bytes(), REPLY_TIMEOUT)?;
        let (mut out, mut buf) = (Vec::new(), vec![0u8; 16384]);
        let mut wait = REPLY_TIMEOUT;
        loop {
            let n = c.read_some(&mut buf, wait)?;
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
            // A reply ends with a form feed; stop there, or after a short pause.
            if out.contains(&0x0c) {
                break;
            }
            wait = Duration::from_millis(500);
        }
        Ok(String::from_utf8_lossy(&out)
            .trim_end_matches(['\x0c', '\r', '\n'])
            .to_string())
    }

    /// Like [`crate::probe`]: which of [`Dpi::ALL`] the scanner accepts in `mode`, one
    /// session per value, without moving the head.
    pub fn probe(&self, mode: Mode) -> Result<Vec<Supported>, ScanError> {
        let mut out = Vec::new();
        for dpi in Dpi::ALL {
            let opts = ScanOptions { dpi, mode };
            match self.session(|s| negotiate(s, opts, usb_timeout)) {
                Ok(plan) => out.push(Supported {
                    dpi,
                    scanned: (plan.xdpi, plan.ydpi),
                }),
                Err(ScanError::UnsupportedResolution { .. }) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }

    /// Like [`crate::scan_to`]: scans one flatbed page, streaming PGM (gray) or the
    /// scanner's JPEG (color) into `out`.
    pub fn scan_to<W: Write>(&self, opts: ScanOptions, out: W) -> Result<ScanInfo, ScanError> {
        self.session(|s| scan_core(s, opts, out, usb_timeout))
    }

    /// Writes a finished print job (for example brother-print's `encode` output) to the
    /// printer interface. Waits up to 5 minutes per 64 KiB while the printer isn't reading
    /// (warming up, out of paper).
    pub fn print(&self, job: &[u8]) -> io::Result<()> {
        self.claim_printer()?.write_all(job, PRINT_CHUNK_TIMEOUT)
    }

    /// Runs `f` inside a scanner session; always closes the session.
    fn session<T>(
        &self,
        f: impl FnOnce(&mut UsbStream) -> Result<T, ScanError>,
    ) -> Result<T, ScanError> {
        let iface = self
            .scanner
            .ok_or_else(|| ScanError::Protocol("this device has no scanner interface".into()))?;
        let c = Claimed::open(&self.device, iface).map_err(|e| match e {
            // macOS claims an interface exclusively: another program (or another scan in
            // this one) has the scanner.
            rusb::Error::Access | rusb::Error::Busy => {
                ScanError::Busy(format!("scanner interface in use by another program, {e}"))
            }
            e => ScanError::Io(usb_io(e)),
        })?;
        c.drain();
        open_session(&c)?;
        let mut s = UsbStream {
            c: &c,
            buf: vec![0u8; 64 * 1024],
            pos: 0,
            len: 0,
            timeout: REPLY_TIMEOUT,
        };
        let r = f(&mut s);
        let close = scanner_ctrl(&c, 2);
        if debug() {
            eprintln!("usb: close session {close:?}");
        }
        r
    }
}

fn read_device_id(h: &rusb::DeviceHandle<GlobalContext>, iface: Iface) -> rusb::Result<String> {
    let mut buf = [0u8; 1024];
    let index = (iface.number as u16) << 8 | iface.alt as u16;
    let n = h.read_control(0xa1, 0, 0, index, &mut buf, CTRL_TIMEOUT)?;
    Ok(parse_device_id(&buf[..n]))
}

/// The key a printer with this serial number gets: FNV-1a over the serial, 48 bits
/// as 12 hex digits. Public so an app can match a network printer (whose web page
/// shows its serial) to the same printer on USB. Stable across Rust versions, unlike
/// `std`'s `DefaultHasher`.
pub fn key_of(serial: &str) -> String {
    let h = serial.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
        (h ^ b as u64).wrapping_mul(0x0100_0000_01b3)
    });
    format!("{:012x}", h >> 16)
}

/// Device id text from a GET_DEVICE_ID reply (2-byte big-endian length, then text),
/// without serial number fields.
pub fn parse_device_id(reply: &[u8]) -> String {
    if reply.len() < 2 {
        return String::new();
    }
    let len = (u16::from_be_bytes([reply[0], reply[1]]) as usize).clamp(2, reply.len());
    let id = String::from_utf8_lossy(&reply[2..len]);
    id.trim_end_matches('\0')
        .split_inclusive(';')
        .filter(|f| {
            let key = f.split(':').next().unwrap_or("").trim();
            !["SN", "SERN", "SERIALNUMBER"]
                .iter()
                .any(|k| key.eq_ignore_ascii_case(k))
        })
        .collect()
}

/// One `KEY:value` field of a device id (keys compared without case).
pub fn device_id_field<'a>(id: &'a str, key: &str) -> Option<&'a str> {
    id.split(';')
        .filter_map(|kv| kv.split_once(':'))
        .find(|(k, _)| k.trim().eq_ignore_ascii_case(key))
        .map(|(_, v)| v.trim())
}

/// `KEY=VALUE` lines of a PJL reply, quotes removed. The echoed `@PJL …` line and lines
/// without `=` are skipped. `CODE=10001` / `DISPLAY="Ready"` / `ONLINE=TRUE` for
/// `INFO STATUS`, `PAGECOUNT=3591` for `INFO PAGECOUNT` on a DCP-1610W.
pub fn pjl_fields(reply: &str) -> Vec<(&str, &str)> {
    reply
        .lines()
        .filter(|l| !l.starts_with("@PJL"))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim(), v.trim().trim_matches('"')))
        .collect()
}

/// IEEE 1284 port status byte. `0x18` is selected, no error, paper present.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PortStatus(pub u8);

impl PortStatus {
    /// Out of paper (bit 5).
    pub fn paper_empty(self) -> bool {
        self.0 & 0x20 != 0
    }
    /// Online (bit 4).
    pub fn selected(self) -> bool {
        self.0 & 0x10 != 0
    }
    /// No error (bit 3; set means fine).
    pub fn no_error(self) -> bool {
        self.0 & 0x08 != 0
    }
}

/// An open, claimed interface; released on drop.
struct Claimed {
    handle: rusb::DeviceHandle<GlobalContext>,
    iface: Iface,
}

impl Claimed {
    fn open(device: &rusb::Device<GlobalContext>, iface: Iface) -> rusb::Result<Self> {
        let handle = device.open()?;
        // No kernel driver binds these interfaces on macOS (NotSupported there); on Linux
        // this moves usblp out of the way and back afterwards.
        let _ = handle.set_auto_detach_kernel_driver(true);
        handle.claim_interface(iface.number)?;
        if iface.alt != 0 {
            handle.set_alternate_setting(iface.number, iface.alt)?;
        }
        Ok(Claimed { handle, iface })
    }

    /// Bulk IN, skipping zero-length packets, until data or `timeout`. `Ok(0)` on timeout.
    fn read_some(&self, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
        let end = Instant::now() + timeout;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(0);
            }
            match self.handle.read_bulk(self.iface.bulk_in, buf, left) {
                Ok(0) => sleep(Duration::from_millis(20)),
                Ok(n) => return Ok(n),
                Err(rusb::Error::Timeout) => return Ok(0),
                Err(e) => return Err(usb_io(e)),
            }
        }
    }

    fn write_all(&self, mut data: &[u8], chunk_timeout: Duration) -> io::Result<()> {
        while !data.is_empty() {
            let n = data.len().min(PRINT_CHUNK);
            // ponytail: rusb drops the partial count on timeout, so a timed-out chunk is
            // fatal; the long timeout covers a printer that pauses for paper.
            let sent = self
                .handle
                .write_bulk(self.iface.bulk_out, &data[..n], chunk_timeout)
                .map_err(usb_io)?;
            data = &data[sent..];
        }
        Ok(())
    }

    /// Throws away anything an earlier, interrupted session left in bulk IN.
    fn drain(&self) {
        let mut buf = vec![0u8; 16384];
        while matches!(self.read_some(&mut buf, Duration::from_millis(100)), Ok(n) if n > 0) {}
    }
}

impl Drop for Claimed {
    fn drop(&mut self) {
        let _ = self.handle.release_interface(self.iface.number);
    }
}

fn usb_io(e: rusb::Error) -> io::Error {
    let kind = match e {
        rusb::Error::Timeout => io::ErrorKind::TimedOut,
        rusb::Error::Busy | rusb::Error::Access => io::ErrorKind::ResourceBusy,
        rusb::Error::NoDevice => io::ErrorKind::NotConnected,
        _ => io::ErrorKind::Other,
    };
    io::Error::new(kind, format!("usb: {e}"))
}

/// Vendor request to the scanner: bmRequestType 0xC0, bRequest 1 (open) or 2 (close),
/// wValue 2, wIndex 0, 5 bytes back.
fn scanner_ctrl(c: &Claimed, request: u8) -> rusb::Result<Vec<u8>> {
    let mut buf = [0u8; 5];
    let n = c
        .handle
        .read_control(0xc0, request, 0x0002, 0, &mut buf, CTRL_TIMEOUT)?;
    if debug() {
        eprintln!("usb: control {request} -> [{}]", hex(&buf[..n]));
    }
    Ok(buf[..n].to_vec())
}

/// A session control reply says OK: `05 10 <request> 02 00`.
fn ctrl_ok(reply: &[u8], request: u8) -> bool {
    reply.len() == 5 && reply[0] == 5 && reply[2] == request
}

/// Opens a scanner session, retrying once after 2 s for a printer that is waking up.
fn open_session(c: &Claimed) -> Result<(), ScanError> {
    let r = match scanner_ctrl(c, 1) {
        Ok(r) => r,
        Err(rusb::Error::Timeout | rusb::Error::Pipe) => {
            sleep(Duration::from_secs(2));
            scanner_ctrl(c, 1).map_err(|e| ScanError::Io(usb_io(e)))?
        }
        Err(e) => return Err(ScanError::Io(usb_io(e))),
    };
    if !ctrl_ok(&r, 1) {
        return Err(ScanError::Busy(format!(
            "session open refused [{}]",
            hex(&r)
        )));
    }
    Ok(())
}

/// The scanner's bulk endpoints as the byte stream a TCP socket is on the network.
struct UsbStream<'a> {
    c: &'a Claimed,
    buf: Vec<u8>,
    pos: usize,
    len: usize,
    timeout: Duration,
}

fn usb_timeout(s: &mut UsbStream, d: Duration) -> io::Result<()> {
    s.timeout = d;
    Ok(())
}

impl Read for UsbStream<'_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.pos == self.len {
            self.len = self.c.read_some(&mut self.buf, self.timeout)?;
            self.pos = 0;
            if self.len == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "usb: no data from the scanner",
                ));
            }
        }
        let n = out.len().min(self.len - self.pos);
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

impl Write for UsbStream<'_> {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.c.write_all(b, REPLY_TIMEOUT)?;
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_device_ids_without_serials() {
        let mut b = vec![0u8, 0];
        b.extend_from_slice(
            b"MFG:Brother;CMD:PJL,HBP;MDL:DCP-1610W series;SN:E7X000000;CLS:PRINTER;",
        );
        b[1] = b.len() as u8;
        b.extend_from_slice(b"\0\0garbage");
        let id = parse_device_id(&b);
        assert_eq!(
            id,
            "MFG:Brother;CMD:PJL,HBP;MDL:DCP-1610W series;CLS:PRINTER;"
        );
        assert_eq!(device_id_field(&id, "mdl"), Some("DCP-1610W series"));
        assert_eq!(device_id_field(&id, "SN"), None);
        assert_eq!(
            parse_device_id(&[0xff, 0xff, b'A']),
            "A",
            "length past the buffer"
        );
        assert_eq!(parse_device_id(&[0]), "");
    }

    #[test]
    fn reads_port_status_bits() {
        let s = PortStatus(0x18);
        assert!(s.selected() && s.no_error() && !s.paper_empty());
        let s = PortStatus(0x30);
        assert!(s.paper_empty() && !s.no_error());
    }

    #[test]
    fn reads_pjl_replies() {
        let status = "@PJL INFO STATUS\r\nCODE=10209\r\nDISPLAY=\"Toner Low\"\r\nONLINE=TRUE";
        assert_eq!(
            pjl_fields(status),
            [
                ("CODE", "10209"),
                ("DISPLAY", "Toner Low"),
                ("ONLINE", "TRUE")
            ]
        );
        assert_eq!(
            pjl_fields("@PJL INFO PAGECOUNT\r\nPAGECOUNT=3591"),
            [("PAGECOUNT", "3591")]
        );
        assert!(pjl_fields("").is_empty());
    }

    #[test]
    fn keys_and_session_replies() {
        assert_eq!(key_of("E7X000000"), key_of("E7X000000"));
        assert_ne!(key_of("E7X000000"), key_of("E7X000001"));
        assert_eq!(key_of("").len(), 12);
        assert!(!key_of("E7X000000").contains("E7X"));
        assert!(ctrl_ok(&[0x05, 0x10, 0x01, 0x02, 0x00], 1));
        assert!(!ctrl_ok(&[0x05, 0x10, 0x81, 0x02, 0x00], 1));
        assert!(!ctrl_ok(&[0x05], 1));
    }
}
