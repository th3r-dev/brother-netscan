//! `brother-netscan <host> [--dpi 300] [--mode gray|color] --out page.<ext>`

use brother_netscan::{Dpi, Mode, ScanOptions};
use std::io::BufWriter;
use std::path::Path;
use std::process::exit;

const USAGE: &str = "usage: brother-netscan <host> [--dpi 200|300|400|600|800|1200] [--mode gray|color] --out page.<ext>\n       brother-netscan <host> --probe [--mode gray|color]   list the resolutions the scanner accepts";

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut host, mut out, mut opts, mut probe) = (None, None, ScanOptions::default(), false);
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
            "-h" | "--help" => die(USAGE),
            _ if host.is_none() && !a.starts_with('-') => host = Some(a),
            _ => die(USAGE),
        }
    }
    if let (Some(host), true) = (&host, probe) {
        match brother_netscan::probe(host, opts.mode) {
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
                return;
            }
            Err(e) => die(&format!("probe failed: {e}")),
        }
    }
    let (Some(host), Some(out)) = (host, out) else {
        die(USAGE)
    };
    // The extension follows the mode: the scanner sends PGM-able gray or JPEG color.
    let ext = if opts.mode == Mode::Gray {
        "pgm"
    } else {
        "jpg"
    };
    let path = Path::new(&out).with_extension(ext);
    let file =
        std::fs::File::create(&path).unwrap_or_else(|e| die(&format!("{}: {e}", path.display())));
    let t = std::time::Instant::now();
    match brother_netscan::scan_to(&host, opts, BufWriter::new(file)) {
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
