//! `brother-netscan <host> [--dpi 300] [--mode gray|color] --out page.<ext>`
//! `brother-netscan --usb …` does the same over a USB cable (feature `usb`).

use brother_netscan::{Dpi, Mode, ScanError, ScanInfo, ScanOptions, Supported};
use std::io::BufWriter;
use std::path::Path;
use std::process::exit;

const USAGE: &str = "usage: brother-netscan <host> [--dpi 200|300|400|600|800|1200] [--mode gray|color] --out page.<ext>\n       brother-netscan <host> --probe [--mode gray|color]   list the resolutions the scanner accepts\n       brother-netscan --usb …   the same over USB instead of <host> (feature usb); --usb --status shows the printer's state";

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut host, mut out, mut opts, mut probe) = (None, None, ScanOptions::default(), false);
    let (mut usb, mut status) = (false, false);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--dpi" => {
                opts.dpi = args
                    .next()
                    .unwrap_or_default()
                    .parse::<Dpi>()
                    .unwrap_or_else(|e| die(&e))
            }
            "--mode" => {
                opts.mode = match args.next().as_deref() {
                    Some("gray") => Mode::Gray,
                    Some("color") => Mode::Color,
                    _ => die(USAGE),
                }
            }
            "--out" => out = args.next(),
            "--probe" => probe = true,
            "--usb" => usb = true,
            "--status" => status = true,
            "-h" | "--help" => die(USAGE),
            _ if host.is_none() && !a.starts_with('-') => host = Some(a),
            _ => die(USAGE),
        }
    }
    if usb {
        return run_usb(out, opts, probe, status);
    }
    let Some(host) = host.filter(|_| !status) else {
        die(USAGE)
    };
    if probe {
        return print_probe(brother_netscan::probe(&host, opts.mode));
    }
    save(out, opts, |w| brother_netscan::scan_to(&host, opts, w));
}

#[cfg(feature = "usb")]
fn run_usb(out: Option<String>, opts: ScanOptions, probe: bool, status: bool) {
    use brother_netscan::usb;
    let Some(p) = usb::find().into_iter().next() else {
        die("no Brother printer on USB: check the cable and that it's switched on")
    };
    if status {
        println!("{} (usb key {})", p.model(), p.key());
        match p.port_status() {
            Ok(s) => println!(
                "port status 0x{:02x}: selected {}, no error {}, paper empty {}",
                s.0,
                s.selected(),
                s.no_error(),
                s.paper_empty()
            ),
            Err(e) => println!("port status: {e}"),
        }
        for q in ["INFO STATUS", "INFO PAGECOUNT"] {
            match p.pjl(q) {
                Ok(r) => usb::pjl_fields(&r)
                    .iter()
                    .for_each(|(k, v)| println!("{k}: {v}")),
                Err(e) => println!("PJL {q}: {e}"),
            }
        }
        return;
    }
    if probe {
        return print_probe(p.probe(opts.mode));
    }
    save(out, opts, |w| p.scan_to(opts, w));
}

#[cfg(not(feature = "usb"))]
fn run_usb(_: Option<String>, _: ScanOptions, _: bool, _: bool) {
    die(
        "this build has no USB support: cargo install --git https://github.com/th3r-dev/brother-netscan --features usb",
    )
}

fn print_probe(found: Result<Vec<Supported>, ScanError>) {
    match found {
        Ok(list) => {
            for s in list {
                let (x, y) = s.scanned;
                let note = if x == y {
                    String::new()
                } else {
                    format!(" (scans {x}x{y}, widened)")
                };
                println!("{}{note}", s.dpi);
            }
        }
        Err(e) => die(&format!("probe failed: {e}")),
    }
}

/// Scans into `<out>.pgm` or `<out>.jpg` (the extension follows the mode), removing the
/// file again if the scan fails.
fn save(
    out: Option<String>,
    opts: ScanOptions,
    scan: impl FnOnce(BufWriter<std::fs::File>) -> Result<ScanInfo, ScanError>,
) {
    let Some(out) = out else { die(USAGE) };
    let ext = if opts.mode == Mode::Gray {
        "pgm"
    } else {
        "jpg"
    };
    let path = Path::new(&out).with_extension(ext);
    let file =
        std::fs::File::create(&path).unwrap_or_else(|e| die(&format!("{}: {e}", path.display())));
    let t = std::time::Instant::now();
    match scan(BufWriter::new(file)) {
        Ok(i) => println!(
            "{}: {}x{} px, scanned at {}x{} dpi, {} bytes, {:.1} s",
            path.display(),
            i.width,
            i.height,
            i.scanned_dpi.0,
            i.scanned_dpi.1,
            std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0),
            t.elapsed().as_secs_f32()
        ),
        Err(e) => {
            let _ = std::fs::remove_file(&path);
            die(&format!("scan failed: {e}"))
        }
    }
}

fn die(msg: &str) -> ! {
    eprintln!("{msg}");
    exit(1)
}
