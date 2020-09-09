// SPDX-FileCopyrightText: 2019-2020 Tuomas Siipola
// SPDX-License-Identifier: AGPL-3.0-or-later

extern crate pio;
use pio::{common::ChromaSubsampling, jpeg, png, ssim};

fn main() {
    let filename = std::env::args_os().nth(1).unwrap();
    let buffer = std::fs::read(filename).unwrap();
    let image = png::read(&buffer).unwrap();
    let attr = ssim::Calculator::new(&image).unwrap();

    println!("quality,ssim,size");

    for quality in 0..=100 {
        let (compressed, buffer) =
            jpeg::compress(&image, quality, ChromaSubsampling::_420).unwrap();
        let dssim = attr.compare(&compressed).unwrap();
        println!("{},{},{}", quality, dssim, buffer.len());
    }
}
