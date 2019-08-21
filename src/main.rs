// SPDX-FileCopyrightText: 2019 Tuomas Siipola
// SPDX-License-Identifier: AGPL-3.0-or-later

use clap::{App, Arg};
use dssim::{Dssim, RGBAPLU};
use imgref::*;
use libwebp_sys::*;
use mozjpeg::{ColorSpace, Compress, Decompress};
use rgb::{ComponentBytes, FromSlice, RGB8, RGBA};

use std::fs::{self, File};
use std::io::prelude::*;
use std::path::Path;
use std::slice;

trait Image {
    /// Quality between 0 - 100
    fn compress(&self, quality: u8) -> Self;

    /// Get pixel data
    fn pixels(&self) -> ImgVec<RGBAPLU>;

    /// Get bytes of compressed image
    fn bytes(&self) -> &[u8];
}

struct Jpeg {
    width: usize,
    height: usize,
    color_space: ColorSpace,
    pixels: Vec<RGB8>,
    buffer: Vec<u8>,
}

impl Jpeg {
    fn new(path: impl AsRef<Path>) -> Self {
        let dinfo = Decompress::new_path(path).unwrap();
        let mut rgb = dinfo.rgb().unwrap();
        let color_space = rgb.color_space();
        let width = rgb.width();
        let height = rgb.height();
        let data: Vec<RGB8> = rgb.read_scanlines().unwrap();
        rgb.finish_decompress();
        Self {
            width,
            height,
            color_space,
            pixels: data,
            buffer: Vec::new(),
        }
    }
}

impl Image for Jpeg {
    fn compress(&self, quality: u8) -> Self {
        let mut cinfo = Compress::new(self.color_space);
        cinfo.set_size(self.width, self.height);
        cinfo.set_quality(quality as f32);
        cinfo.set_mem_dest();
        cinfo.start_compress();
        assert!(cinfo.write_scanlines(self.pixels.as_bytes()));
        cinfo.finish_compress();
        let cdata = cinfo.data_to_vec().unwrap();

        let dinfo = Decompress::new_mem(&cdata).unwrap();
        let mut rgb = dinfo.rgb().unwrap();
        let data: Vec<RGB8> = rgb.read_scanlines().unwrap();
        rgb.finish_decompress();

        Jpeg {
            width: self.width,
            height: self.height,
            color_space: self.color_space,
            pixels: data,
            buffer: cdata,
        }
    }

    fn pixels(&self) -> ImgVec<RGBAPLU> {
        ImgVec::new(
            self.pixels
                .iter()
                .map(|x| {
                    RGBA::new(
                        x.r as f32 / u8::max_value() as f32,
                        x.g as f32 / u8::max_value() as f32,
                        x.b as f32 / u8::max_value() as f32,
                        1.0,
                    )
                })
                .collect(),
            self.width,
            self.height,
        )
    }

    fn bytes(&self) -> &[u8] {
        assert!(!self.buffer.is_empty());
        &self.buffer
    }
}

struct Png {
    width: usize,
    height: usize,
    pixels: Vec<RGBA<u8, u8>>,
    buffer: Vec<u8>,
}

impl Png {
    fn new(path: impl AsRef<Path>) -> Self {
        let image = lodepng::decode32_file(path).unwrap();
        Png {
            width: image.width,
            height: image.height,
            pixels: image.buffer,
            buffer: Vec::new(),
        }
    }
}

impl Image for Png {
    fn compress(&self, quality: u8) -> Self {
        let mut liq = imagequant::new();
        liq.set_quality(0, quality as u32);
        let ref mut img = liq
            .new_image(&self.pixels, self.width, self.height, 0.0)
            .unwrap();
        let mut res = liq.quantize(&img).unwrap();
        res.set_dithering_level(1.0);
        let (palette, pixels) = res.remapped(img).unwrap();

        let mut state = lodepng::State::new();
        for color in &palette {
            state.info_raw.palette_add(*color).unwrap();
            state.info_png.color.palette_add(*color).unwrap();
        }
        state.info_raw.colortype = lodepng::ColorType::PALETTE;
        state.info_raw.set_bitdepth(8);
        state.info_png.color.colortype = lodepng::ColorType::PALETTE;
        state.info_png.color.set_bitdepth(8);
        state.set_auto_convert(false);
        let buffer = state.encode(&pixels, self.width, self.height).unwrap();

        Self {
            width: self.width,
            height: self.height,
            pixels: pixels.iter().map(|i| palette[*i as usize]).collect(),
            buffer,
        }
    }

    fn pixels(&self) -> ImgVec<RGBAPLU> {
        ImgVec::new(
            self.pixels
                .iter()
                .map(|x| {
                    RGBA::new(
                        x.r as f32 / u8::max_value() as f32,
                        x.g as f32 / u8::max_value() as f32,
                        x.b as f32 / u8::max_value() as f32,
                        1.0,
                        // TODO: x.a as f32 / u8::max_value() as f32
                    )
                })
                .collect(),
            self.width,
            self.height,
        )
    }

    fn bytes(&self) -> &[u8] {
        assert!(!self.buffer.is_empty());
        &self.buffer
    }
}

struct WebP {
    width: usize,
    height: usize,
    pixels: *mut u8,
    buffer: Vec<u8>,
}

impl WebP {
    fn new(path: impl AsRef<Path>) -> Self {
        let mut width = 0;
        let mut height = 0;

        let mut file = File::open(path).unwrap();
        let mut buffer = Vec::new();
        file.read_to_end(&mut buffer).unwrap();

        let pixels =
            unsafe { WebPDecodeRGB(buffer.as_ptr(), buffer.len(), &mut width, &mut height) };
        assert!(!pixels.is_null());

        Self {
            width: width as usize,
            height: height as usize,
            pixels,
            buffer,
        }
    }
}

impl Drop for WebP {
    fn drop(&mut self) {
        unsafe {
            WebPFree(self.pixels as *mut std::ffi::c_void);
        }
    }
}

impl Image for WebP {
    fn compress(&self, quality: u8) -> Self {
        unsafe {
            let mut buffer = Box::into_raw(Box::new(0u8)) as *mut _;
            let stride = self.width as i32 * 3;
            let len = WebPEncodeRGB(
                self.pixels,
                self.width as i32,
                self.height as i32,
                stride,
                quality as f32,
                &mut buffer as *mut _,
            );
            assert!(len != 0);
            let pixels = WebPDecodeRGB(buffer, len, std::ptr::null_mut(), std::ptr::null_mut());
            assert!(!pixels.is_null());
            Self {
                width: self.width,
                height: self.height,
                pixels,
                buffer: Vec::from_raw_parts(buffer, len as usize, len as usize),
            }
        }
    }

    fn pixels(&self) -> ImgVec<RGBAPLU> {
        ImgVec::new(
            unsafe { slice::from_raw_parts(self.pixels, 3 * self.width * self.height) }
                .as_rgb()
                .iter()
                .map(|x| {
                    RGBA::new(
                        x.r as f32 / u8::max_value() as f32,
                        x.g as f32 / u8::max_value() as f32,
                        x.b as f32 / u8::max_value() as f32,
                        1.0,
                    )
                })
                .collect(),
            self.width,
            self.height,
        )
    }

    fn bytes(&self) -> &[u8] {
        assert!(!self.buffer.is_empty());
        &self.buffer
    }
}

#[derive(PartialEq)]
enum Format {
    JPEG,
    PNG,
    WEBP,
}

impl Format {
    fn detect(path: &Path) -> Option<Format> {
        path.extension()
            .and_then(std::ffi::OsStr::to_str)
            .and_then(|ext| match ext {
                "jpeg" | "jpg" => Some(Format::JPEG),
                "png" => Some(Format::PNG),
                "webp" => Some(Format::WEBP),
                _ => None,
            })
    }
}

fn compress_image(
    image: impl Image,
    target: f64,
    min_quality: u8,
    max_quality: u8,
    input_path: &Path,
    output_path: &Path,
) {
    let original_size = fs::metadata(&input_path).unwrap().len();
    eprintln!("original size {} bytes", original_size);

    let attr = Dssim::new();
    let original = attr.create_image(&image.pixels()).unwrap();

    let mut min = min_quality;
    let mut max = max_quality;
    let mut compressed;

    loop {
        let quality = (min + max) / 2;
        compressed = image.compress(quality);

        let mut attr = Dssim::new();
        let (dssim, _ssim_maps) =
            attr.compare(&original, attr.create_image(&compressed.pixels()).unwrap());
        eprintln!(
            "range {} - {} quality {}, SSIM {:.6} {} bytes, {} % of original",
            min,
            max,
            quality,
            dssim,
            compressed.bytes().len(),
            100 * compressed.bytes().len() as u64 / original_size
        );

        if dssim > target {
            min = quality + 1;
        } else {
            max = quality - 1;
        }

        if min > max {
            break;
        }
    }

    if compressed.bytes().len() < original_size as usize {
        let mut output = File::create(output_path).unwrap();
        output.write_all(compressed.bytes()).unwrap();
    } else {
        eprintln!("Failed to optimize the input image, copying the input image to output...");
        fs::copy(input_path, output_path).unwrap();
    }
}

fn validate_target(x: String) -> Result<(), String> {
    match x.parse::<f64>() {
        Ok(x) => {
            if x >= 0.0 {
                Ok(())
            } else {
                Err("expected value between 0.0 and infinity".to_string())
            }
        }
        Err(_) => Err("expected value between 0.0 and infinity".to_string()),
    }
}

fn validate_quality(x: String) -> Result<(), String> {
    match x.parse::<i8>() {
        Ok(x) => {
            if x >= 0 || x <= 100 {
                Ok(())
            } else {
                Err("expected value between 0 and 100".to_string())
            }
        }
        Err(_) => Err("expected value between 0 and 100".to_string()),
    }
}

fn main() {
    let matches = App::new("pio")
        .about("Perceptual Image Optimizer")
        .version(clap::crate_version!())
        .arg(
            Arg::with_name("INPUT")
                .help("Sets the input file to use")
                .required(true)
                .index(1),
        )
        .arg(
            Arg::with_name("OUTPUT")
                .help("Set the output file to use")
                .required(true)
                .index(2),
        )
        .arg(
            Arg::with_name("target")
                .long("target")
                .value_name("SSIM")
                .help("Set the target SSIM")
                .takes_value(true)
                .default_value("0.01")
                .validator(validate_target),
        )
        .arg(
            Arg::with_name("min")
                .long("min")
                .value_name("quality")
                .help("Sets the minimum quality for output")
                .takes_value(true)
                .default_value("40")
                .validator(validate_quality),
        )
        .arg(
            Arg::with_name("max")
                .long("max")
                .value_name("quality")
                .help("Sets the maximum quality for output")
                .takes_value(true)
                .default_value("95")
                .validator(validate_quality),
        )
        .get_matches();

    let input = matches.value_of("INPUT").unwrap();
    let input_path = Path::new(input);
    let input_format = match Format::detect(input_path) {
        Some(ext) => ext,
        None => {
            eprintln!("input must be jpeg, png or webp");
            std::process::exit(1);
        }
    };

    let output = matches.value_of("OUTPUT").unwrap();
    let output_path = Path::new(output);
    let output_format = match Format::detect(output_path) {
        Some(ext) => ext,
        None => {
            eprintln!("output must be jpeg, png or webp");
            std::process::exit(1);
        }
    };

    assert!(input_format == output_format);

    let target = matches.value_of("target").unwrap().parse().unwrap();

    let min: u8 = matches.value_of("min").unwrap().parse().unwrap();
    let max: u8 = matches.value_of("max").unwrap().parse().unwrap();
    if min > max {
        eprintln!("min must be smaller or equal to max");
        std::process::exit(1);
    }

    match input_format {
        Format::JPEG => compress_image(
            Jpeg::new(input_path),
            target,
            min,
            max,
            input_path,
            output_path,
        ),
        Format::PNG => compress_image(
            Png::new(input_path),
            target,
            min,
            max,
            input_path,
            output_path,
        ),
        Format::WEBP => compress_image(
            WebP::new(input_path),
            target,
            min,
            max,
            input_path,
            output_path,
        ),
    };
}
