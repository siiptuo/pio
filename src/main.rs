// SPDX-FileCopyrightText: 2019 Tuomas Siipola
// SPDX-License-Identifier: AGPL-3.0-or-later

use dssim::{Dssim, RGBAPLU};
use imgref::*;
use mozjpeg::{ColorSpace, Compress, Decompress};
use rgb::{ComponentBytes, RGB8, RGBA};
use std::env;
use std::fs::{self, File};
use std::io::prelude::*;
use std::path::Path;

fn decompress(path: impl AsRef<Path>) -> (usize, usize, ColorSpace, Vec<RGB8>) {
    let dinfo = Decompress::new_path(path).unwrap();
    let mut rgb = dinfo.rgb().unwrap();
    let color_space = rgb.color_space();
    let width = rgb.width();
    let height = rgb.height();
    let data: Vec<RGB8> = rgb.read_scanlines().unwrap();
    rgb.finish_decompress();
    (width, height, color_space, data)
}

fn convert(data: &Vec<RGB8>) -> Vec<RGBAPLU> {
    data.iter()
        .map(|x| {
            RGBA::new(
                x.r as f32 / u8::max_value() as f32,
                x.g as f32 / u8::max_value() as f32,
                x.b as f32 / u8::max_value() as f32,
                1.0,
            )
        })
        .collect()
}

fn main() {
    let target = env::args().nth(1).unwrap().parse::<f64>().unwrap();
    let input_path = env::args_os().nth(2).unwrap();
    let output_path = env::args_os().nth(3).unwrap();

    let original_size = fs::metadata(&input_path).unwrap().len();
    println!("original size {} bytes", original_size);

    let (width, height, color_space, data) = decompress(input_path);

    let attr = Dssim::new();

    let bitmap: ImgVec<RGBAPLU> = ImgVec::new(convert(&data), width, height);
    let original = attr.create_image(&bitmap).unwrap();

    let mut min = 40;
    let mut max = 95;

    let mut quality = (min + max) / 2;

    loop {
        let mut cinfo = Compress::new(color_space);
        cinfo.set_size(width, height);
        cinfo.set_quality(quality as f32);
        cinfo.set_mem_dest();
        cinfo.start_compress();
        assert!(cinfo.write_scanlines(data.as_bytes()));
        cinfo.finish_compress();
        let cdata = cinfo.data_as_mut_slice().unwrap();

        let dinfo = Decompress::new_mem(cdata).unwrap();
        let mut rgb = dinfo.rgb().unwrap();
        let data: Vec<RGB8> = rgb.read_scanlines().unwrap();
        rgb.finish_decompress();

        let data2 = convert(&data);
        let bitmap: ImgVec<RGBAPLU> = ImgVec::new(data2, width, height);

        let mut attr = Dssim::new();
        let compressed = attr.create_image(&bitmap).unwrap();
        let (dssim, _ssim_maps) = attr.compare(&original, compressed);
        println!(
            "range {} - {} quality {}, SSIM {:.6} {} bytes, {} % of original",
            min,
            max,
            quality,
            dssim,
            cdata.len(),
            100 * cdata.len() as u64 / original_size
        );

        if dssim > target {
            min = quality + 1;
        } else {
            max = quality - 1;
        }

        if min > max {
            let mut output = File::create(output_path).unwrap();
            output.write_all(cdata).unwrap();
            break;
        }

        quality = (min + max) / 2;
    }
}
