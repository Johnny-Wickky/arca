//! Times decoding of the files given on the command line at several output sizes.
#![allow(
    clippy::print_stdout,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "command-line tool"
)]

use std::time::Instant;

fn main() {
    for path in std::env::args().skip(1) {
        let data = std::fs::read(&path).unwrap();
        for side in [2048u32, 1024, 512, 256] {
            let limits = arca_image::Limits { max_out_side: side, ..arca_image::Limits::default() };
            let t = Instant::now();
            let img = arca_image::decode(&data, &limits).unwrap();
            println!("{path}: out {}x{} in {} ms", img.width, img.height, t.elapsed().as_millis());
        }
    }
}
