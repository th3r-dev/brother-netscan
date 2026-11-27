# brother-netscan

Scan from a Brother printer's flatbed over the network or a USB cable, in Rust, with no Brother driver installed.

Some Brother printers, such as the DCP-1610W, don't support AirScan (eSCL) or WSD scanning. They only scan over Brother's own protocol on TCP port 54921, which Brother's closed-source drivers (brscan4 on Linux, the ICA driver on macOS) use. This crate talks that protocol directly. You can scan a page from any machine Rust runs on and pass the image on, for example to OCR.

> **Status: prototype.** It works on a DCP-1610W. Other models are untested. The library uses only the Rust standard library; the optional `usb` feature adds [rusb](https://github.com/a1ien/rusb).

## Usage

```sh
cargo install --git https://github.com/th3r-dev/brother-netscan

brother-netscan 192.0.2.10 --out page                       # 300 dpi gray → page.pgm
brother-netscan 192.0.2.10 --dpi 600 --out page             # 600 dpi gray
brother-netscan 192.0.2.10 --mode color --out page          # 300 dpi color → page.jpg
brother-netscan 192.0.2.10 --probe                          # resolutions this scanner accepts
sips -s format png page.pgm --out page.png                   # macOS: PGM to PNG
tesseract page.pgm page --dpi 300 -l eng                     # OCR at the scan's real DPI
```

PGM files carry no resolution, so pass the scan's DPI to Tesseract with `--dpi`. Without it, Tesseract guesses.

```rust
use brother_netscan::{scan, scan_to, Dpi, Mode, ScanOptions};

let (image, ext) = scan("192.0.2.10", ScanOptions::default())?;   // ext: "pgm" or "jpg"
std::fs::write(format!("page.{ext}"), image)?;

// Stream large scans to a file instead of holding them in memory:
let opts = ScanOptions { dpi: Dpi::D600, mode: Mode::Gray };
let info = scan_to("192.0.2.10", opts, std::io::BufWriter::new(std::fs::File::create("page.pgm")?))?;
println!("{}x{} px", info.width, info.height);
```

`probe(host, mode)` lists the resolutions the scanner accepts, without scanning. It negotiates each value and hangs up before the head moves, which takes a second or two per value. On a DCP-1610W it returns 200, 300, 400, 600 and 1200 for gray (1200 is scanned at 600 × 1200, then widened), and 200 to 600 for color.

Set `BROTHER_NETSCAN_DEBUG=1` to log the exchange with the scanner to stderr.

## Resolution

You can pick 200, 300 (the default, and the best choice for OCR), 400, 600, 800 or 1200 dpi. The scanner decides what it can actually do. If it won't scan at the resolution you asked for, you get `ScanError::UnsupportedResolution` with what it offered instead. It never quietly falls back to another resolution.

Measured on a DCP-1610W (600 × 1200 dpi optical), whole flatbed:

| Requested | Scanner used | Image (px) | Bytes | Time |
|---|---|---|---|---|
| 200 gray | 200 × 200 | 1653 × 2291 | 3.8 MB | 13.7 s |
| 300 gray | 300 × 300 | 2480 × 3437 | 8.5 MB | 12.5 s |
| 400 gray | 400 × 400 | 3306 × 4582 | 15.1 MB | 19.5 s |
| 600 gray | 600 × 600 | 4960 × 6874 | 34.1 MB | 23.0 s |
| 800 | refused, offered 600 × 600 | `UnsupportedResolution` | — | 2 s |
| 1200 gray | 600 × 1200 optical, each row widened 2× | 9920 × 13748 | 136 MB | 37.2 s |
| 300 color | 300 × 300, scanner's own JPEG | 2480 × 3437 | 1.7 MB | 17.9 s |
| 1200 color | refused, offered 600 × 600 | `UnsupportedResolution` | — | — |

## Reference task

This crate exists to finish one concrete job, and that job is the yardstick for the prototype.

> **Task:** scan one page from the flatbed of a Brother DCP-1610W over Wi-Fi from a Mac with no Brother software installed, at 300 dpi grayscale, and save it as an image that Tesseract can read. The paper tray is empty.

- [x] `brother-netscan <host> --out page` returns a full-page image of what's on the glass.
- [x] The same call works from the library: `brother_netscan::scan(host, ScanOptions::default())`.
- [x] DPI selectable: 200, 300 (default), 400, 600, 800, 1200. Unsupported values fail with a clear error.
- [x] Errors are clear and nothing hangs: busy scanner, unreachable host, refused resolution, unexpected replies; every wait has a timeout.
- [x] The ported decoder passes a recorded-behavior check against the original (see [How it's built](#how-its-built)).
- [x] `cargo test`, `cargo clippy -- -D warnings` and `cargo fmt --check` pass.

**Paper doesn't matter.** Scanning worked on the test printer while its display said "No Paper" and SNMP reported `printing / down`. The scan service ignores the print path's error state. The only time a scan was refused was when another connection was already scanning: the printer answers `-NG 401`, which becomes `ScanError::Busy`.

## How it works

One TCP connection to port 54921 per scan:

1. The scanner greets you with `+OK 200\r\n` (ready) or `-NG 401\r\n` (busy).
2. You send `ESC "I\nR=300,300\nM=GRAY64\n" 0x80`. Color uses `M=CGRAY`.
3. The scanner replies `xdpi,ydpi,?,width_mm,width_px,height_mm,height_px`. If it can't scan at the requested resolution, it offers another one here.
4. You send `ESC "X\n…" 0x80` with the accepted resolution, the compression (`RLENGTH` for gray, `JPEG` for color) and the area (`A=0,0,width,height`).
5. Image records follow, each with a 12-byte header (`id 07 00 page … len`). `0x42` is a gray row compressed with PackBits, `0x64` is a block of JPEG, `0x40` is raw gray. The older 3-byte header is also accepted.
6. `0x82` plus 9 bytes ends the page, and a single `0x80` ends the job. Any other byte where a record should start becomes `ScanError::DeviceStatus`.

Gray output is a binary PGM, streamed row by row. Color output is the scanner's JPEG, unchanged. Some scans come back a row or two short of the announced height, so the PGM is padded with white rows to that height.

## USB

Build with the `usb` feature to use a printer on a USB cable: scan, read its status, and send print jobs.

```sh
cargo install --git https://github.com/th3r-dev/brother-netscan --features usb

brother-netscan --usb --out page          # 300 dpi gray over USB → page.pgm
brother-netscan --usb --probe             # resolutions, about 1 s over USB
brother-netscan --usb --status            # model, port status, PJL status and page counter
```

```rust
use brother_netscan::{usb, ScanOptions};

let printer = usb::find().into_iter().next().ok_or("no Brother printer on USB")?;
println!("{} ({})", printer.model(), printer.key());   // "DCP-1610W series (4b63…)"
let info = printer.scan_to(ScanOptions::default(), std::io::BufWriter::new(std::fs::File::create("page.pgm")?))?;
let status = printer.pjl("INFO STATUS")?;               // CODE=10001, DISPLAY="Ready", ONLINE=TRUE
printer.print(&job)?;                                   // a finished job, e.g. brother-print's encode()
```

The feature is off by default. It pulls in rusb with its `vendored` feature, so libusb is built from source and linked statically. Nothing has to be installed with Homebrew or apt.

- `find()` lists every Brother device with a printer or scanner interface.
- `model()` comes from the device id's `MDL` field.
- `key()` is a 12-digit hash of the USB serial number, short and stable. It exists so an app can tell printers apart without showing or storing the serial; the serial itself is never returned. `device_id()` drops `SN` fields too.
- `scan_to`, `probe`, `device_id`, `port_status`, `pjl` and `print` each open the device, claim the one interface they need, and release it when done.

### What goes over the cable

A DCP-1610W (`04f9:035b`) has two interfaces:

| Interface | Class | Bulk IN / OUT | Used for |
|---|---|---|---|
| 0 | 7/1/2 (printer) | `0x82` / `0x01` | print jobs, PJL, IEEE 1284 status |
| 1 | 255 (vendor) | `0x85` / `0x04` | scanning |

Scanning:

1. Claim interface 1. Throw away anything an interrupted session left in bulk IN.
2. Open the session with a vendor control request: `bmRequestType 0xC0`, `bRequest 1`, `wValue 2`, `wIndex 0`, 5 bytes back. `05 10 01 02 00` means OK. Anything else becomes `ScanError::Busy`. A request that times out or stalls is retried once after 2 s, for a printer that is waking up.
3. Run steps 2 to 6 of the network protocol ([How it works](#how-it-works)) on the bulk endpoints: ESC I, ESC X, then image records. The bytes are the same as over TCP.
4. Close the session with the same request and `bRequest 2`, even when the scan failed.

Status, on interface 0:

- `GET_DEVICE_ID` (class request 0): `MFG:Brother;CMD:PJL,HBP;MDL:DCP-1610W series;CLS:PRINTER;CID:Brother Laser Type1;`
- `GET_PORT_STATUS` (class request 1): one byte. Bit 5 is paper empty, bit 4 selected, bit 3 no error. `0x18` is ready with paper.
- PJL in a UEL wrapper (`ESC %-12345X@PJL … ESC %-12345X`) on bulk OUT; the reply ends with a form feed on bulk IN. `INFO STATUS` returns `CODE`, `DISPLAY` (the text on the printer's screen) and `ONLINE`; `INFO PAGECOUNT` returns `PAGECOUNT`. `usb::pjl_fields` splits a reply into key/value pairs. The toner level isn't reported over USB.

Printing writes the finished job to bulk OUT in 64 KiB pieces. Each piece may wait up to 5 minutes while the printer isn't reading, for example while it warms up or is out of paper.

Timeouts: 2 s for control requests, 10 s for the ESC I and PJL replies, 60 s of silence while the page is scanned.

Measured on a DCP-1610W over USB 2.0 from a Mac: `--status` in 1.0 s, `--probe` in 1.0 s (gray: 200, 300, 400, 600 and 1200, widened; color: 200 to 600), a 300 dpi gray scan (2480 × 3437 px, 8.5 MB) in 11.4 s.

### macOS and Linux

- **macOS:** no driver or permission is needed. macOS lets only one program claim an interface at a time, so a scan fails with `ScanError::Busy` ("scanner interface in use by another program") while Image Capture, a Brother driver, or another scan is using the scanner. Status calls fail with an `io::ErrorKind::ResourceBusy` error while something holds the printer interface, for example a print job from CUPS.
- **Linux:** the `usblp` kernel driver is detached from interface 0 while it is claimed and reattached afterwards. You need write access to the device node, either through a udev rule for `04f9` or by running as root. Linux is untested.

## How it's built

The protocol logic is ported from [brother-scand](https://github.com/pietern/brother-scand) (C and Python, MIT). [NOTICE](NOTICE) lists the source of each piece. [Rust Anything](https://rustanything.ai/) checks the port against the original's recorded behavior:

- **PackBits decoder.** It's bridged from brother-scand's `rle_decode` ([`parity/rle/rle.py`](parity/rle/rle.py), copied unchanged except for added type hints) and recorded on 213 synthetic cases. The Rust port ([`parity/rle/rle-rs/src/port.rs`](parity/rle/rle-rs/src/port.rs)) calls this crate's own `PackBits`, so the check covers the decoder that ships. Re-run it:

  ```sh
  cd parity/rle/rle-rs
  rust-anything build && rust-anything diff && rust-anything gate .
  # 213/213 cases match … Gate passed.
  ```

- **Command building, reply parsing, record framing.** Rust Anything can't bridge these. In brother-scand they take connection structs, and the packets contain `0x00` bytes, so they can't be passed as C strings (`analyze` reports `data_channel.c` and `connection.c` as 0 bridgeable). These parts were checked against a real DCP-1610W instead, as recorded in the resolution table above, and are covered by synthetic unit tests.

No real scans, hostnames, network names or passwords are in the repository. Test cases use synthetic data and documentation addresses (`192.0.2.0/24`).

## Models

| Model | Status |
|---|---|
| DCP-1610W | Works: gray 200–600 and 1200 dpi, color 300 dpi. Over USB: scan, probe, status and printing |
| Other models that brscan3 or brscan4 support | Untested, may work. Please report. |

To report a model, open an issue with the model, the firmware date (shown on the printer's web page), the `BROTHER_NETSCAN_DEBUG=1` output, and what happened.

## Limits

- **Flatbed only:** one page, the whole glass. No document feeder, duplex or scan-button (push) support.
- **Color:** the scanner's JPEG is passed through unchanged, and color was tested at 300 dpi only.
- **Brightness and contrast** are fixed at the scanner's defaults (50).
- **Memory:** `scan()` holds the whole image in memory (about 136 MB for gray at 1200 dpi). Use `scan_to()` to stream instead.
- **USB:** tested on macOS only. Printing sends a job that is already encoded; making one is up to you, for example with [brother-print](https://github.com/th3r-dev/brother-print). Two printers with no USB serial number would get the same `key()`.

## References

| Source | What it's used for | License |
|---|---|---|
| [pietern/brother-scand](https://github.com/pietern/brother-scand) | Protocol and decoding logic, ported to Rust | MIT (see [NOTICE](NOTICE)) |
| [darsto/brother-scand](https://github.com/darsto/brother-scand) | Upstream of the fork above, cross-checked | MIT |
| [jmesmon/brother2](https://github.com/jmesmon/brother2) | Protocol notes, read for facts only (older 3-byte framing, `-NG 401`, status `0xc2`). No code or text copied. | No license |
| [Brother MFC network notes](https://brother-mfc.sourceforge.net/network.html) | Early notes on the port 54921 exchange | — |
| [maciekmm/brscanner](https://github.com/maciekmm/brscanner) | USB scanner session requests, read for facts only. No code or text copied. | No license |
| [rusb](https://github.com/a1ien/rusb) | USB access for the `usb` feature, with libusb vendored | MIT (rusb), LGPL-2.1 (libusb) |
| [Rust Anything](https://rustanything.ai/) | Record-and-replay check for the port | MIT or Apache-2.0 |
| [sane-airscan](https://github.com/alexpevzner/sane-airscan) | The standard alternative (eSCL/WSD), for models that support it | GPL-2.0-or-later |

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option. Portions are ported from brother-scand under the MIT License; see [NOTICE](NOTICE).

The `usb` feature links libusb statically, and libusb is LGPL-2.1-or-later. A program you build with that feature and share has to meet the LGPL's terms for libusb, such as letting people relink it against their own libusb. This crate's own code stays MIT or Apache-2.0.

Brother is a trademark of Brother Industries, Ltd. This project is not affiliated with or endorsed by Brother.
