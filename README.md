# brother-netscan

Scan from a Brother printer's flatbed over the network, in Rust, with no Brother driver installed.

Some Brother printers, such as the DCP-1610W, don't support AirScan (eSCL) or WSD scanning. They only scan over Brother's own protocol on TCP port 54921, which Brother's closed-source drivers (brscan4 on Linux, the ICA driver on macOS) use. This crate talks that protocol directly. You can scan a page from any machine Rust runs on and pass the image on, for example to OCR.

> **Status: prototype.** It works only on the models in the table below. The first working version arrives in the prototype pull request.

## Reference task

This crate exists to finish one concrete job. It's the yardstick for the prototype.

> **Task:** scan one page from the flatbed of a Brother DCP-1610W over Wi-Fi from a Mac with no Brother software installed, at 300 dpi grayscale, and save it as an image that Tesseract can read. The paper tray is empty.

Acceptance:

- [ ] `brother-netscan <host> --out page.pnm` returns a full-page image of what's on the glass.
- [ ] The same call works from the library: `brother_netscan::scan(host, ScanOptions::default())`.
- [ ] When the printer is in an error state (for example, a print job waiting for paper), the scan stops with a clear error. It doesn't hang.
- [ ] Every protocol and decoding function ported from brother-scand passes a recorded-behavior check against the C original (see [How it's built](#how-its-built)).
- [ ] `cargo test`, `cargo clippy -- -D warnings` and `cargo fmt --check` pass.

An empty tray doesn't matter: scanning never uses paper. A print job stuck waiting for paper *does* block the whole machine. Press **Stop/Exit** on the printer to cancel it, then scan.

## Models

| Model | Status |
|---|---|
| DCP-1610W | Prototype target |
| Other models that brscan4 supports | Untested, may work. Please report. |

Report results in an issue: the model, firmware date (shown on the printer's web page), resolution and mode, and what happened.

## Usage

Usage docs arrive with the prototype pull request.

## How it's built

The protocol logic is ported from [brother-scand](https://github.com/pietern/brother-scand) (C, MIT) with [Rust Anything](https://rustanything.ai/):

1. Rust Anything runs the C original on synthetic inputs and records its behavior: the commands it builds, the replies it parses, the image data it decodes.
2. Each Rust port is replayed against those recordings. `rust-anything gate` has to pass before the code merges.
3. The network exchange itself is checked against a real printer, because a recording can't stand in for a device.

No real scans, hostnames, network names or passwords go into the repository. Test cases use synthetic data and documentation addresses (`192.0.2.0/24`).

## References

| Source | What it's used for | License |
|---|---|---|
| [pietern/brother-scand](https://github.com/pietern/brother-scand) | Protocol and decoding logic, ported to Rust | MIT (see [NOTICE](NOTICE)) |
| [darsto/brother-scand](https://github.com/darsto/brother-scand) | Fork with wider device support, cross-checked | MIT |
| [jmesmon/brother2](https://github.com/jmesmon/brother2) | Protocol notes, read for facts only. No code or text copied. | No license |
| [Brother MFC network notes](https://brother-mfc.sourceforge.net/network.html) | Early notes on the port 54921 exchange | — |
| [Rust Anything](https://rustanything.ai/) | Record-and-replay checks for the port | MIT or Apache-2.0 |
| [sane-airscan](https://github.com/alexpevzner/sane-airscan) | The standard alternative (eSCL/WSD), for models that support it | GPL-2.0-or-later |

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option. Portions are ported from brother-scand under the MIT License; see [NOTICE](NOTICE).

Brother is a trademark of Brother Industries, Ltd. This project is not affiliated with or endorsed by Brother.
