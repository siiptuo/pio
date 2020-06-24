// SPDX-FileCopyrightText: 2019 Tuomas Siipola
// SPDX-License-Identifier: AGPL-3.0-or-later

use clap::{App, Arg};
use dssim::{Dssim, ToRGBAPLU, RGBAPLU};
use imgref::{Img, ImgVec};
use libwebp_sys::*;
use rgb::{alt::GRAY8, ComponentBytes, RGB8, RGBA8};

use std::ffi::OsStr;
use std::fs::File;
use std::io::Read;
use std::mem::MaybeUninit;
use std::path::Path;

enum ColorSpace {
    Gray,
    GrayAlpha,
    RGB,
    RGBA,
}

struct Image {
    width: usize,
    height: usize,
    data: Vec<RGBA8>,
    color_space: ColorSpace,
}

fn distance(a: u8, b: u8) -> u8 {
    if a < b {
        b - a
    } else {
        a - b
    }
}

fn is_gray(color: RGB8) -> bool {
    distance(color.r, color.g) <= 1 && distance(color.r, color.b) <= 1
}

impl Image {
    fn from_rgba(data: Vec<RGBA8>, width: usize, height: usize) -> Self {
        let has_color = data.iter().any(|c| !is_gray(c.rgb()));
        let has_alpha = data.iter().any(|c| c.a < 255);
        Self {
            width,
            height,
            data,
            color_space: match (has_color, has_alpha) {
                (false, false) => ColorSpace::Gray,
                (false, true) => ColorSpace::GrayAlpha,
                (true, false) => ColorSpace::RGB,
                (true, true) => ColorSpace::RGBA,
            },
        }
    }

    fn from_rgb(data: Vec<RGB8>, width: usize, height: usize) -> Self {
        Self::from_rgba(data.iter().map(|c| c.alpha(255)).collect(), width, height)
    }

    fn to_rgbaplu(&self) -> ImgVec<RGBAPLU> {
        Img::new(self.data.to_rgbaplu(), self.width, self.height)
    }

    fn to_gray(&self) -> ImgVec<GRAY8> {
        Img::new(
            self.data.iter().map(|c| c.r.into()).collect(),
            self.width,
            self.height,
        )
    }

    fn to_rgb(&self, bg: RGB8) -> ImgVec<RGB8> {
        Img::new(
            self.data
                .iter()
                .map(|c| {
                    // Alpha blending with gamma correction
                    let to_f32 = |x| x as f32 / 255.0;
                    let to_u8 = |x| (255.0 * x) as u8;
                    let gamma = 2.2;
                    let a = to_f32(c.a);
                    c.rgb()
                        .iter()
                        .map(to_f32)
                        .zip(bg.iter().map(to_f32))
                        .map(|(x, y)| {
                            to_u8((x.powf(gamma) * a + y.powf(gamma) * (1.0 - a)).powf(1.0 / gamma))
                        })
                        .collect()
                })
                .collect(),
            self.width,
            self.height,
        )
    }

    fn as_bytes(&self) -> &[u8] {
        self.data.as_bytes()
    }
}

type ReadResult = Result<Image, String>;
type CompressResult = Result<(Image, Vec<u8>), String>;

#[rustfmt::skip]
const QUALITY_SSIM: [f64; 101] = [
    0.64405, 0.64405, 0.493921, 0.3717685, 0.2875005, 0.226447, 0.18505, 0.155942,
    0.13402550000000002, 0.1161245, 0.10214999999999999, 0.09164900000000001, 0.0830645,
    0.0747825, 0.0686465, 0.0636275, 0.058777499999999996, 0.054973999999999995, 0.0509935,
    0.048128000000000004, 0.0452685, 0.0428175, 0.0404645, 0.0387125, 0.036169999999999994,
    0.034700999999999996, 0.03334, 0.0319895, 0.029954, 0.029339499999999998, 0.028261,
    0.0271415, 0.025916, 0.0248545, 0.0244545, 0.023451, 0.022603, 0.022269, 0.021344, 0.020581,
    0.0202495, 0.019450000000000002, 0.019161499999999998, 0.0189065, 0.018063, 0.017832,
    0.0169555, 0.016857999999999998, 0.016676, 0.0159105, 0.0157275, 0.015555,
    0.014891499999999998, 0.014727, 0.0145845, 0.013921, 0.0137565, 0.0135065, 0.012928,
    0.012669, 0.0125305, 0.011922499999999999, 0.011724, 0.011544, 0.0112675, 0.0107825,
    0.010481, 0.010245, 0.009772, 0.0095075, 0.009262, 0.008721, 0.0084715, 0.008324999999999999,
    0.007556500000000001, 0.0074540000000000006, 0.007243, 0.0067735, 0.0066254999999999994,
    0.006356499999999999, 0.005924499999999999, 0.005674500000000001, 0.005422, 0.0050215,
    0.0047565, 0.0044755, 0.0041294999999999995, 0.0038510000000000003, 0.00361, 0.003372,
    0.0029255, 0.0027010000000000003, 0.0024415, 0.002091, 0.0017955, 0.001591, 0.001218,
    0.0009805, 0.000749, 0.000548, 0.0004,
];

fn read_jpeg(buffer: &[u8]) -> ReadResult {
    let dinfo = mozjpeg::Decompress::new_mem(buffer).map_err(|err| err.to_string())?;
    let mut rgb = dinfo.rgb().map_err(|err| err.to_string())?;
    let width = rgb.width();
    let height = rgb.height();
    let data: Vec<RGB8> = rgb
        .read_scanlines()
        .ok_or_else(|| "Failed decode image data".to_string())?;
    rgb.finish_decompress();
    Ok(Image::from_rgb(data, width, height))
}

fn compress_jpeg(image: &Image, quality: u8) -> CompressResult {
    let mut cinfo = mozjpeg::Compress::new(match image.color_space {
        ColorSpace::Gray => mozjpeg::ColorSpace::JCS_GRAYSCALE,
        _ => mozjpeg::ColorSpace::JCS_RGB,
    });
    cinfo.set_size(image.width, image.height);
    cinfo.set_quality(quality as f32);
    cinfo.set_mem_dest();
    cinfo.start_compress();
    if !match image.color_space {
        ColorSpace::Gray => cinfo.write_scanlines(image.to_gray().buf().as_bytes()),
        _ => cinfo.write_scanlines(image.to_rgb(RGB8::new(255, 255, 255)).buf().as_bytes()),
    } {
        return Err("Failed to compress image data".to_string());
    }
    cinfo.finish_compress();
    let cdata = cinfo
        .data_to_vec()
        .map_err(|_err| "Failed to compress image".to_string())?;
    let image = read_jpeg(&cdata)?;
    Ok((image, cdata))
}

fn read_png(buffer: &[u8]) -> ReadResult {
    let png = lodepng::decode32(buffer).map_err(|err| err.to_string())?;
    Ok(Image::from_rgba(png.buffer, png.width, png.height))
}

fn compress_png(image: &Image, quality: u8) -> CompressResult {
    let (palette, pixels) = {
        let mut liq = imagequant::new();
        liq.set_quality(0, quality as u32);
        let img = &mut (liq
            .new_image(&image.data, image.width, image.height, 0.0)
            .map_err(|err| err.to_string())?);
        let mut res = liq.quantize(&img).map_err(|err| err.to_string())?;
        res.set_dithering_level(1.0);
        res.remapped(img).map_err(|err| err.to_string())?
    };
    let buffer = {
        let mut state = lodepng::State::new();
        for color in &palette {
            state
                .info_raw
                .palette_add(*color)
                .map_err(|err| err.to_string())?;
            state
                .info_png
                .color
                .palette_add(*color)
                .map_err(|err| err.to_string())?;
        }
        state.info_raw.colortype = lodepng::ColorType::PALETTE;
        state.info_raw.set_bitdepth(8);
        state.info_png.color.colortype = lodepng::ColorType::PALETTE;
        state.info_png.color.set_bitdepth(8);
        state.set_auto_convert(false);
        state
            .encode(&pixels, image.width, image.height)
            .map_err(|err| err.to_string())?
    };
    let result = pixels.iter().map(|i| palette[*i as usize]).collect();
    Ok((Image::from_rgba(result, image.width, image.height), buffer))
}

fn read_webp(buffer: &[u8]) -> ReadResult {
    let mut width = 0;
    let mut height = 0;

    let ret = unsafe { WebPGetInfo(buffer.as_ptr(), buffer.len(), &mut width, &mut height) };
    if ret == 0 {
        return Err("Failed to decode file".to_string());
    }

    let len = (width * height) as usize;
    let mut data: Vec<RGBA8> = Vec::with_capacity(len);
    unsafe {
        data.set_len(len);
    }

    let ret = unsafe {
        WebPDecodeRGBAInto(
            buffer.as_ptr(),
            buffer.len(),
            data.as_mut_ptr() as *mut u8,
            (4 * width * height) as usize,
            4 * width,
        )
    };
    if ret.is_null() {
        return Err("Failed to decode image data".to_string());
    }

    Ok(Image::from_rgba(data, width as usize, height as usize))
}

fn compress_webp(image: &Image, quality: u8) -> CompressResult {
    unsafe {
        let mut config = MaybeUninit::<WebPConfig>::uninit();
        let ret = WebPConfigInitInternal(
            config.as_mut_ptr(),
            WebPPreset::WEBP_PRESET_DEFAULT,
            quality as f32,
            WEBP_ENCODER_ABI_VERSION as i32,
        );
        if ret == 0 {
            return Err("libwebp version mismatch".to_string());
        }
        let mut config = config.assume_init();
        config.method = 6;

        let mut wrt = MaybeUninit::<WebPMemoryWriter>::uninit();
        WebPMemoryWriterInit(wrt.as_mut_ptr());
        let mut wrt = wrt.assume_init();

        let mut pic = MaybeUninit::<WebPPicture>::uninit();
        WebPPictureInitInternal(pic.as_mut_ptr(), WEBP_ENCODER_ABI_VERSION as i32);
        if ret == 0 {
            return Err("libwebp version mismatch".to_string());
        }
        let mut pic = pic.assume_init();
        pic.width = image.width as i32;
        pic.height = image.height as i32;
        pic.writer = Some(WebPMemoryWrite);
        pic.custom_ptr = &mut wrt as *mut _ as *mut std::ffi::c_void;

        let stride = image.width as i32 * 4;
        let ret = WebPPictureImportRGBA(&mut pic, image.as_bytes().as_ptr(), stride);
        if ret == 0 {
            WebPPictureFree(&mut pic);
            WebPMemoryWriterClear(&mut wrt);
            return Err("Failed to import image data".to_string());
        }

        let ret = WebPEncode(&config, &mut pic);
        WebPPictureFree(&mut pic);

        if ret == 0 {
            WebPMemoryWriterClear(&mut wrt);
            return Err("Failed to encode image data".to_string());
        }

        let buffer = wrt.mem;
        let len = wrt.size;

        let capacity = image.width * image.height;
        let mut pixels: Vec<RGBA8> = Vec::with_capacity(capacity);
        pixels.set_len(capacity);

        let ret = WebPDecodeRGBAInto(
            buffer,
            len,
            pixels.as_mut_ptr() as *mut u8,
            4 * image.width * image.height,
            (4 * image.width) as i32,
        );
        if ret.is_null() {
            return Err("Failed to decode image data".to_string());
        }

        // XXX: Not safe because `buffer` is not allocated by `Vec`
        let buffer = Vec::from_raw_parts(buffer, len as usize, len as usize);

        Ok((Image::from_rgba(pixels, image.width, image.height), buffer))
    }
}

#[derive(PartialEq)]
enum Format {
    JPEG,
    PNG,
    WEBP,
}

impl Format {
    fn from_str(input: &str) -> Option<Self> {
        match input {
            "jpeg" | "jpg" => Some(Self::JPEG),
            "png" => Some(Self::PNG),
            "webp" => Some(Self::WEBP),
            _ => None,
        }
    }

    fn detect(path: impl AsRef<Path>) -> Option<Self> {
        path.as_ref()
            .extension()
            .and_then(OsStr::to_str)
            .and_then(|ext| Self::from_str(&ext.to_ascii_lowercase()))
    }
}

fn compress_image(
    image: Image,
    compressor: impl Fn(&Image, u8) -> CompressResult,
    target: f64,
    min_quality: u8,
    max_quality: u8,
    original_size: u64,
) -> Result<Vec<u8>, String> {
    let attr = Dssim::new();
    let original = attr
        .create_image(&image.to_rgbaplu())
        .ok_or_else(|| "Failed to create DSSIM image".to_string())?;

    let mut min = min_quality;
    let mut max = max_quality;
    let mut compressed;
    let mut buffer;

    loop {
        let quality = (min + max) / 2;
        let (a, b) = compressor(&image, quality)?;
        compressed = a;
        buffer = b;

        for x in 0..=100 / 4 {
            if x == quality / 4 {
                eprint!("O")
            } else if x == 0 || x == 100 / 4 {
                eprint!("|");
            } else if x == min / 4 {
                eprint!("[");
            } else if x == max / 4 {
                eprint!("]");
            } else if x > min / 4 && x < max / 4 {
                eprint!("-");
            } else {
                eprint!(" ");
            }
        }

        let attr = Dssim::new();
        let (dssim, _ssim_maps) = attr.compare(
            &original,
            attr.create_image(&compressed.to_rgbaplu())
                .ok_or_else(|| "Failed create DSSIM image")?,
        );

        eprintln!(
            " {:>3} quality  {:.6} SSIM  {:>3} % of original",
            quality,
            dssim,
            100 * buffer.len() as u64 / original_size
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

    Ok(buffer)
}

fn validate_quality(x: String) -> Result<(), String> {
    match x.parse::<i8>() {
        Ok(x) => {
            if (0..=100).contains(&x) {
                Ok(())
            } else {
                Err("expected value between 0 and 100".to_string())
            }
        }
        Err(_) => Err("expected value between 0 and 100".to_string()),
    }
}

fn validate_format(x: String) -> Result<(), String> {
    if Format::from_str(&x).is_none() {
        Err("supported formats are jpeg, png and webp".to_string())
    } else {
        Ok(())
    }
}

fn main() {
    let matches = App::new("pio")
        .about("Perceptual Image Optimizer")
        .version(clap::crate_version!())
        .arg(
            Arg::with_name("INPUT")
                .help("Input file to use, standard input is used when value is - or not set")
                .index(1),
        )
        .arg(
            Arg::with_name("input-format")
                .long("input-format")
                .help("Sets input file format")
                .takes_value(true)
                .validator(validate_format),
        )
        .arg(
            Arg::with_name("output")
                .long("output")
                .short("o")
                .help("Sets output file")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("output-format")
                .long("output-format")
                .help("Sets output file format")
                .takes_value(true)
                .validator(validate_format),
        )
        .arg(
            Arg::with_name("quality")
                .long("quality")
                .value_name("quality")
                .help("Sets target quality for output")
                .takes_value(true)
                .default_value("85")
                .validator(validate_quality),
        )
        .arg(
            Arg::with_name("min")
                .long("min")
                .value_name("quality")
                .help("Sets minimum quality for output")
                .takes_value(true)
                .validator(validate_quality),
        )
        .arg(
            Arg::with_name("max")
                .long("max")
                .value_name("quality")
                .help("Sets maximum quality for output")
                .takes_value(true)
                .validator(validate_quality),
        )
        .get_matches();

    let (input_format, input_buffer) = match matches.value_of("INPUT") {
        None | Some("-") => {
            let format = Format::from_str(matches.value_of("input-format").unwrap_or_else(|| {
                eprintln!("--input-format is required when reading from standard input");
                std::process::exit(1);
            }))
            .unwrap();
            let mut buffer = Vec::new();
            std::io::stdin()
                .read_to_end(&mut buffer)
                .unwrap_or_else(|err| {
                    eprintln!("failed to read standard input: {}", err);
                    std::process::exit(1);
                });
            (format, buffer)
        }
        Some(path) => {
            let format = match matches.value_of("input-format") {
                Some(format) => Format::from_str(format).unwrap(),
                None => Format::detect(path).unwrap_or_else(|| {
                    eprintln!("unknown input file extension, expected jpeg, png or webp");
                    std::process::exit(1);
                }),
            };
            let buffer = std::fs::read(path).unwrap_or_else(|err| {
                eprintln!("failed to read input file: {}", err);
                std::process::exit(1);
            });
            (format, buffer)
        }
    };

    let original_size = input_buffer.len();

    let (output_format, mut output_writer): (Format, Box<dyn std::io::Write>) = match matches
        .value_of("output")
    {
        Some(path) => {
            let format = match matches.value_of("output-format") {
                Some(format) => Format::from_str(format).unwrap(),
                None => Format::detect(path).unwrap_or_else(|| {
                    eprintln!("unknown output file extension, expected jpeg, png or webp");
                    std::process::exit(1);
                }),
            };
            let output = File::create(path).unwrap_or_else(|err| {
                eprintln!("failed to open output file: {}", err);
                std::process::exit(1);
            });
            (format, Box::new(output))
        }
        None => {
            let format = Format::from_str(matches.value_of("output-format").unwrap_or_else(|| {
                eprintln!("--output-format is required when writing to standard output");
                std::process::exit(1);
            }))
            .unwrap();
            (format, Box::new(std::io::stdout()))
        }
    };

    let quality = matches.value_of("quality").unwrap().parse::<u8>().unwrap();

    let target = QUALITY_SSIM[quality as usize];

    let min = match matches.value_of("min") {
        Some(s) => s.parse().unwrap(),
        None => std::cmp::max(0, quality - 10),
    };
    let max = match matches.value_of("max") {
        Some(s) => s.parse().unwrap(),
        None => std::cmp::min(quality + 10, 100),
    };
    if min > max {
        eprintln!("min must be smaller or equal to max");
        std::process::exit(1);
    }

    let input_image = match match input_format {
        Format::JPEG => read_jpeg(&input_buffer),
        Format::PNG => read_png(&input_buffer),
        Format::WEBP => read_webp(&input_buffer),
    } {
        Ok(image) => image,
        Err(err) => {
            eprintln!("Failed to read input: {}", err);
            std::process::exit(1);
        }
    };

    let compressor = match output_format {
        Format::JPEG => compress_jpeg,
        Format::PNG => compress_png,
        Format::WEBP => compress_webp,
    };

    match compress_image(
        input_image,
        compressor,
        target,
        min,
        max,
        original_size as u64,
    ) {
        Ok(output_buffer) => {
            if output_buffer.len() < original_size as usize {
                output_writer.write_all(&output_buffer).unwrap();
            } else {
                eprintln!(
                    "Failed to optimize the input image, copying the input image to output..."
                );
                output_writer.write_all(&input_buffer).unwrap();
            }
        }
        Err(err) => {
            eprintln!("Failed to compress image: {}", err);
            std::process::exit(1);
        }
    }
}
