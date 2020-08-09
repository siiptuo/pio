// SPDX-FileCopyrightText: 2019-2020 Tuomas Siipola
// SPDX-FileCopyrightText: 2019-2020 Johannes Siipola
//
// SPDX-License-Identifier: AGPL-3.0-or-later

use clap::{App, Arg};
use dssim::{Dssim, ToRGBAPLU, RGBAPLU};
use imgref::{Img, ImgVec};
use libwebp_sys::*;
use rgb::{alt::GRAY8, ComponentBytes, RGB8, RGBA8};

use std::ffi::OsStr;
use std::fs::File;
use std::io::{Read, Stdout};
use std::mem::{ManuallyDrop, MaybeUninit};
use std::path::{Path, PathBuf};

#[derive(PartialEq)]
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
    distance(color.r, color.g) <= 1 && distance(color.g, color.b) <= 1
}

fn srgb_to_linear(u: u8) -> f32 {
    let u = u as f32 / 255.0;
    if u <= 0.04045 {
        u / 12.92
    } else {
        ((u + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(u: f32) -> u8 {
    if u <= 0.0031308 {
        (255.0 * (12.92 * u)).round() as u8
    } else {
        (255.0 * (1.055 * u.powf(1.0 / 2.4) - 0.055)).round() as u8
    }
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
            self.data.iter().map(|c| c.g.into()).collect(),
            self.width,
            self.height,
        )
    }

    fn alpha_blend(&mut self, bg: RGB8) {
        use rayon::prelude::*;
        use rgb::ComponentMap;

        let bg = bg.map(srgb_to_linear);
        self.data.par_iter_mut().for_each(|pixel| {
            let a = pixel.a as f32 / 255.0;
            *pixel = pixel
                .rgb()
                .iter()
                .map(srgb_to_linear)
                .zip(bg.iter())
                .map(|(fg, bg)| fg * a + bg * (1.0 - a))
                .map(linear_to_srgb)
                .collect::<RGB8>()
                .alpha(255);
        });
    }

    fn to_rgb(&self) -> ImgVec<RGB8> {
        Img::new(
            self.data.iter().map(|c| c.rgb()).collect(),
            self.width,
            self.height,
        )
    }

    fn as_bytes(&self) -> &[u8] {
        self.data.as_bytes()
    }

    fn into_image_rs(self) -> image::RgbaImage {
        image::RgbaImage::from_raw(self.width as u32, self.height as u32, unsafe {
            let mut v_clone = std::mem::ManuallyDrop::new(self.data);
            Vec::from_raw_parts(
                v_clone.as_mut_ptr() as *mut u8,
                v_clone.len() * 4,
                v_clone.capacity() * 4,
            )
        })
        .unwrap()
    }

    fn from_image_rs(image: image::RgbaImage) -> Self {
        let width = image.width();
        let height = image.height();
        Self::from_rgba(
            unsafe {
                let mut v_clone = std::mem::ManuallyDrop::new(image.into_raw());
                Vec::from_raw_parts(
                    v_clone.as_mut_ptr() as *mut RGBA8,
                    v_clone.len() / 4,
                    v_clone.capacity() / 4,
                )
            },
            width as usize,
            height as usize,
        )
    }
}

type ReadResult = Result<Image, String>;
type CompressResult = Result<(Image, Vec<u8>), String>;
type LossyCompressor = Box<dyn Fn(&Image, u8) -> CompressResult>;
type LosslessCompressor = Box<dyn Fn(&Image) -> CompressResult>;

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

// Rotate and flip image according to Exif orientation.
fn orient_image(image: Image, orientation: u32) -> Image {
    if orientation == 1 {
        return image;
    }
    let mut output = image.into_image_rs();
    match orientation {
        2 => image::imageops::flip_horizontal_in_place(&mut output),
        3 => image::imageops::rotate180_in_place(&mut output),
        4 => image::imageops::flip_vertical_in_place(&mut output),
        5 => {
            output = image::imageops::rotate90(&output);
            image::imageops::flip_horizontal_in_place(&mut output);
        }
        6 => output = image::imageops::rotate90(&output),
        7 => {
            output = image::imageops::rotate90(&output);
            image::imageops::flip_vertical_in_place(&mut output);
        }
        8 => output = image::imageops::rotate270(&output),
        _ => unreachable!(),
    }
    Image::from_image_rs(output)
}

fn exif_orientation(exif: exif::Exif) -> Option<u32> {
    exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY)
        .and_then(|field| field.value.get_uint(0))
        .filter(|x| *x >= 1 && *x <= 8)
}

fn read_jpeg(buffer: &[u8]) -> ReadResult {
    let dinfo = mozjpeg::Decompress::new_mem(buffer).map_err(|err| err.to_string())?;
    let mut rgb = dinfo.rgb().map_err(|err| err.to_string())?;
    let width = rgb.width();
    let height = rgb.height();
    let data: Vec<RGB8> = rgb
        .read_scanlines()
        .ok_or_else(|| "Failed decode image data".to_string())?;
    rgb.finish_decompress();
    let orientation = exif::Reader::new()
        .read_from_container(&mut std::io::Cursor::new(buffer))
        .ok()
        .and_then(exif_orientation)
        .unwrap_or(1);
    Ok(orient_image(
        Image::from_rgb(data, width, height),
        orientation,
    ))
}

fn compress_jpeg(
    image: &Image,
    quality: u8,
    chroma_subsampling: ChromaSubsampling,
) -> CompressResult {
    let mut cinfo = mozjpeg::Compress::new(match image.color_space {
        ColorSpace::Gray => mozjpeg::ColorSpace::JCS_GRAYSCALE,
        _ => mozjpeg::ColorSpace::JCS_RGB,
    });
    cinfo.set_size(image.width, image.height);
    cinfo.set_quality(quality as f32);
    cinfo.set_mem_dest();

    if image.color_space != ColorSpace::Gray {
        let chroma_subsampling = match chroma_subsampling {
            ChromaSubsampling::_444 => [[1, 1], [1, 1], [1, 1]],
            ChromaSubsampling::_422 => [[2, 2], [2, 1], [2, 1]],
            ChromaSubsampling::_420 => [[2, 2], [1, 1], [1, 1]],
        };
        for (c, samp) in cinfo
            .components_mut()
            .iter_mut()
            .zip(chroma_subsampling.iter())
        {
            c.v_samp_factor = samp[0];
            c.h_samp_factor = samp[1];
        }
    }

    cinfo.start_compress();
    if !match image.color_space {
        ColorSpace::Gray => cinfo.write_scanlines(image.to_gray().buf().as_bytes()),
        _ => cinfo.write_scanlines(image.to_rgb().buf().as_bytes()),
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
    let mut decoder = lodepng::Decoder::new();
    decoder.remember_unknown_chunks(true);
    decoder.info_raw_mut().colortype = lodepng::ColorType::RGBA;
    let png = match decoder.decode(&buffer) {
        Ok(lodepng::Image::RGBA(data)) => data,
        Ok(_) => return Err("Color conversion failed".to_string()),
        Err(err) => return Err(err.to_string()),
    };
    let orientation = decoder
        .info_png()
        .get("eXIf")
        .and_then(|raw| exif::Reader::new().read_raw(raw.data().to_vec()).ok())
        .and_then(exif_orientation)
        .unwrap_or(1);
    Ok(orient_image(
        Image::from_rgba(png.buffer, png.width, png.height),
        orientation,
    ))
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
        let mut encoder = lodepng::Encoder::new();
        for color in &palette {
            encoder
                .info_raw_mut()
                .palette_add(*color)
                .map_err(|err| err.to_string())?;
            encoder
                .info_png_mut()
                .color
                .palette_add(*color)
                .map_err(|err| err.to_string())?;
        }
        encoder.info_raw_mut().colortype = lodepng::ColorType::PALETTE;
        encoder.info_raw_mut().set_bitdepth(8);
        encoder.info_png_mut().color.colortype = lodepng::ColorType::PALETTE;
        encoder.info_png_mut().color.set_bitdepth(8);
        encoder.set_auto_convert(false);
        encoder
            .encode(&pixels, image.width, image.height)
            .map_err(|err| err.to_string())?
    };
    let result = pixels.iter().map(|i| palette[*i as usize]).collect();
    Ok((Image::from_rgba(result, image.width, image.height), buffer))
}

fn read_webp(buffer: &[u8]) -> ReadResult {
    unsafe {
        let data = WebPData {
            bytes: buffer.as_ptr(),
            size: buffer.len(),
        };

        let mux = WebPMuxCreateInternal(&data, 0, WEBP_MUX_ABI_VERSION);
        if mux.is_null() {
            return Err("failed to create mux".to_string());
        }

        let mut image = MaybeUninit::uninit();
        let ret = WebPMuxGetFrame(mux, 1, image.as_mut_ptr());
        if ret != WebPMuxError::WEBP_MUX_OK {
            return Err("failed to get frame 1".to_string());
        }
        let mut image = image.assume_init();

        let mut width = 0;
        let mut height = 0;
        let rgba = WebPDecodeRGBA(
            image.bitstream.bytes,
            image.bitstream.size,
            &mut width,
            &mut height,
        );
        if rgba.is_null() {
            return Err("failed to decode image data".to_string());
        }

        WebPDataClear(&mut image.bitstream);

        let mut exif_chunk = MaybeUninit::uninit();
        let ret = WebPMuxGetChunk(
            mux,
            b"EXIF" as *const _ as *const _,
            exif_chunk.as_mut_ptr(),
        );
        let exif = match ret {
            WebPMuxError::WEBP_MUX_OK => {
                let exif_chunk = exif_chunk.assume_init();
                let raw = std::slice::from_raw_parts(exif_chunk.bytes, exif_chunk.size);
                exif::Reader::new().read_raw(raw.to_vec()).ok()
            }
            WebPMuxError::WEBP_MUX_NOT_FOUND => None,
            error => return Err(format!("error while reading EXIF chunk: {:?}", error)),
        };
        let orientation = exif.and_then(exif_orientation).unwrap_or(1);

        WebPMuxDelete(mux);

        // XXX: Not safe because `buffer` is not allocated by `Vec`.
        //      Probably fine because size is not changed :)
        let buffer: Vec<RGBA8> = Vec::from_raw_parts(
            rgba as *mut _,
            (width * height) as usize,
            (width * height) as usize,
        );

        Ok(orient_image(
            Image::from_rgba(buffer, width as usize, height as usize),
            orientation,
        ))
    }
}

fn compress_webp(image: &Image, quality: u8, lossless: bool) -> CompressResult {
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
        config.use_sharp_yuv = 1;
        if lossless {
            config.lossless = 1;
        }

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
        // This behavior is copied from `cwebp`. For example `use_sharp_yuv` doesn't seem to do
        // anything if `use_argb` is not enabled.
        if config.lossless == 1 || config.use_sharp_yuv == 1 || config.preprocessing > 0 {
            pic.use_argb = 1;
        }

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

        // XXX: Not safe because `buffer` is not allocated by `Vec`.
        //      Probably fine because size is not changed :)
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
    fn from_ext(input: &str) -> Option<Self> {
        match input {
            "jpeg" | "jpg" => Some(Self::JPEG),
            "png" => Some(Self::PNG),
            "webp" => Some(Self::WEBP),
            _ => None,
        }
    }

    fn from_path(path: impl AsRef<Path>) -> Option<Self> {
        path.as_ref()
            .extension()
            .and_then(OsStr::to_str)
            .and_then(|ext| Self::from_ext(&ext.to_ascii_lowercase()))
    }

    fn from_magic(buffer: &[u8]) -> Option<Self> {
        match buffer {
            [0xff, 0xd8, 0xff, ..] => Some(Self::JPEG),
            [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, ..] => Some(Self::PNG),
            [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => Some(Self::WEBP),
            _ => None,
        }
    }

    fn supports_transparency(&self) -> bool {
        match self {
            Self::JPEG => false,
            Self::PNG => true,
            Self::WEBP => true,
        }
    }
}

#[derive(Copy, Clone)]
enum ChromaSubsampling {
    _420,
    _422,
    _444,
}

fn compress_image(
    image: Image,
    lossy_compress: LossyCompressor,
    lossless_compress: Option<LosslessCompressor>,
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

    // Compress image with different qualities and find which is closest to the SSIM target. Binary
    // search is used to speed up the search. Since there are 101 possible quality values, only
    // ceil(log2(101)) = 7 comparisons are needed at maximum.
    loop {
        // Overflow is not possible because `min` and `max` are in range 0-100.
        let quality = (min + max) / 2;

        let (a, b) = lossy_compress(&image, quality)?;
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
            // Prevent underflow because comparison is unreliable at low qualities.
            if quality == 0 {
                break;
            }
            max = quality - 1;
        }

        if min > max {
            break;
        }
    }

    // Try lossless compression if the format supports it. For example, lossless WebP can sometimes
    // be smaller than lossy WebP for non-photographic images.
    if let Some(compress) = lossless_compress {
        eprint!("|                        |");
        let (_, b) = compress(&image)?;
        eprintln!(
            "    lossless  0.000000 SSIM  {:>3} % of original",
            100 * b.len() as u64 / original_size
        );
        if b.len() < buffer.len() {
            return Ok(b);
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

fn validate_spread(x: String) -> Result<(), String> {
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

fn parse_color(input: &str) -> Result<RGB8, String> {
    if !input.starts_with('#') {
        return Err("color must start #".to_string());
    }
    if input.len() != 7 {
        return Err("color must have 7 characters".to_string());
    }
    Ok(RGB8::new(
        u8::from_str_radix(&input[1..=2], 16).map_err(|err| err.to_string())?,
        u8::from_str_radix(&input[3..=4], 16).map_err(|err| err.to_string())?,
        u8::from_str_radix(&input[5..=6], 16).map_err(|err| err.to_string())?,
    ))
}

enum Output {
    Stdout(Stdout),
    File {
        path: PathBuf,
        file: ManuallyDrop<File>,
        empty: bool,
        is_file: bool,
    },
}

impl Output {
    fn file(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let path = path.as_ref();
        File::create(path).and_then(|file| {
            Ok(Self::File {
                is_file: file.metadata()?.is_file(),
                path: path.to_path_buf(),
                file: ManuallyDrop::new(file),
                empty: true,
            })
        })
    }

    fn stdout() -> Self {
        Self::Stdout(std::io::stdout())
    }

    fn write(mut self, buf: &[u8]) -> std::io::Result<()> {
        use std::io::Write;
        match self {
            Output::Stdout(ref mut stdout) => {
                stdout.write_all(buf)?;
                stdout.flush()?;
            }
            Output::File {
                ref mut file,
                ref mut empty,
                is_file,
                ..
            } => {
                file.write_all(buf)?;
                if is_file {
                    file.sync_all()?;
                } else {
                    file.flush()?;
                }
                *empty = false;
            }
        };
        Ok(())
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        if let Output::File {
            path,
            file,
            empty,
            is_file,
        } = self
        {
            unsafe { ManuallyDrop::drop(file) };
            if *empty && *is_file {
                std::fs::remove_file(path).unwrap_or_else(|_err| {});
            }
        }
    }
}

fn pio(matches: clap::ArgMatches) -> Result<(), String> {
    let quality = matches.value_of("quality").unwrap().parse::<u8>().unwrap();

    let spread = matches.value_of("spread").unwrap().parse::<u8>().unwrap();

    let target = QUALITY_SSIM[quality as usize];

    let min = match matches.value_of("min") {
        Some(s) => s.parse().unwrap(),
        None => std::cmp::max(0, quality - std::cmp::min(quality, spread)),
    };
    let max = match matches.value_of("max") {
        Some(s) => s.parse().unwrap(),
        None => std::cmp::min(quality + spread, 100),
    };
    if min > max {
        return Err("min must be smaller or equal to max".to_string());
    }

    let fail_strategy = matches.value_of("optimization-failed").unwrap();

    let chroma_subsampling = match matches.value_of("chroma-subsampling").unwrap() {
        "420" => ChromaSubsampling::_420,
        "422" => ChromaSubsampling::_422,
        "444" => ChromaSubsampling::_444,
        _ => unreachable!(),
    };

    let (output_format, output_writer): (Format, Output) = match matches.value_of_os("output") {
        Some(path) => {
            let format = match matches.value_of("output-format") {
                Some(format) => Format::from_ext(format).unwrap(),
                None => Format::from_path(path).ok_or_else(|| {
                    "failed to determine output format: either use a known file extension (jpeg, png or webp) or specify the format using `--output-format`".to_string()
                })?,
            };
            let output =
                Output::file(path).map_err(|err| format!("failed to open output file: {}", err))?;
            (format, output)
        }
        None => {
            let format = Format::from_ext(matches.value_of("output-format").ok_or_else(|| "use `--output` to write to a file or `--output-format` to write to standard output".to_string())?).unwrap();
            (format, Output::stdout())
        }
    };

    let mut input_reader: Box<dyn std::io::Read> = match matches
        .value_of_os("INPUT")
        .and_then(|s| if s == "-" { None } else { Some(s) })
    {
        None => Box::new(std::io::stdin()),
        Some(path) => {
            Box::new(File::open(path).map_err(|err| format!("failed to open input file: {}", err))?)
        }
    };

    // Read enough data to determine input file format by magic number.
    let mut input_buffer = vec![0; 16];
    input_reader
        .read_exact(&mut input_buffer)
        .map_err(|err| format!("failed to read magic number: {}", err))?;
    let input_format = Format::from_magic(&input_buffer)
        .ok_or_else(|| "unknown input format, expected jpeg, png or webp".to_string())?;
    // Read rest of the input.
    input_reader
        .read_to_end(&mut input_buffer)
        .map_err(|err| format!("failed to read input: {}", err))?;

    let original_size = input_buffer.len();

    let mut input_image = match input_format {
        Format::JPEG => read_jpeg(&input_buffer),
        Format::PNG => read_png(&input_buffer),
        Format::WEBP => read_webp(&input_buffer),
    }
    .map_err(|err| format!("failed to read input: {}", err))?;

    let (lossy_compress, lossless_compress): (LossyCompressor, Option<LosslessCompressor>) =
        match output_format {
            Format::JPEG => (
                Box::new(move |img, q| compress_jpeg(img, q, chroma_subsampling)),
                None,
            ),
            Format::PNG => (Box::new(compress_png), None),
            Format::WEBP => (
                Box::new(|img, q| compress_webp(img, q, false)),
                Some(Box::new(|img| compress_webp(img, 100, true))),
            ),
        };

    if !output_format.supports_transparency() || matches.is_present("no-transparency") {
        let bg = parse_color(matches.value_of("background-color").unwrap()).unwrap();
        input_image.alpha_blend(bg);
    }

    match compress_image(
        input_image,
        lossy_compress,
        lossless_compress,
        target,
        min,
        max,
        original_size as u64,
    ) {
        Ok(output_buffer) => {
            if output_buffer.len() <= original_size as usize {
                output_writer
                    .write(&output_buffer)
                    .map_err(|err| format!("failed to write output: {}", err))?;
                Ok(())
            } else {
                match fail_strategy {
                    "none" => {
                        eprintln!("warning: Output is larger than input but still writing output normally. This behavior can be changed with `--optimization-failed` option.");
                        output_writer
                            .write(&output_buffer)
                            .map_err(|err| format!("failed to write output: {}", err))?;
                        Ok(())
                    }
                    "exit" => {
                        Err("error: Output would be larger than input, exiting now...".to_string())
                    }
                    "copy" => {
                        eprintln!("warning: Output would be larger than input, copying input to output...");
                        output_writer
                            .write(&output_buffer)
                            .map_err(|err| format!("failed to write output: {}", err))?;
                        Ok(())
                    }
                    _ => unreachable!(),
                }
            }
        }
        Err(err) => Err(format!("failed to compress image: {}", err)),
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
                .value_name("format")
                .takes_value(true)
                .possible_values(&["jpeg", "png", "webp"]),
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
        .arg(
            Arg::with_name("spread")
                .long("spread")
                .value_name("spread")
                .help("Sets deviation from the quality target")
                .default_value("10")
                .takes_value(true)
                .validator(validate_spread),
        )
        .arg(
            Arg::with_name("background-color")
                .long("background-color")
                .value_name("color")
                .help(
                    "Sets background color to use when output format doesn't support transparency",
                )
                .takes_value(true)
                .default_value("#ffffff")
                .validator(|x| parse_color(&x).map(|_| ())),
        )
        .arg(
            Arg::with_name("no-transparency")
                .long("no-transparency")
                .help("Adds background color even if output format supports transparency"),
        )
        .arg(
            Arg::with_name("optimization-failed")
                .long("optimization-failed")
                .value_name("strategy")
                .help("Sets strategy to use when output is larger than the input")
                .takes_value(true)
                .default_value("none")
                .possible_values(&["none", "exit", "copy"]),
        )
        .arg(
            Arg::with_name("chroma-subsampling")
                .long("chroma-subsampling")
                .value_name("xxx")
                .help("Specifies chroma subsampling")
                .takes_value(true)
                .default_value("420")
                .possible_values(&["444", "422", "420"]),
        )
        .get_matches();

    pio(matches).unwrap_or_else(|err| {
        eprintln!("{}", err);
        std::process::exit(1);
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use assert_cmd::Command;
    use tempfile::tempdir;

    fn convert_image(
        input: impl AsRef<Path>,
        output: impl AsRef<Path>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let output = Command::new("convert")
            .arg(input.as_ref())
            .arg("-quality")
            .arg("100")
            .arg(output.as_ref())
            .output()?;
        assert!(output.status.success());
        Ok(())
    }

    fn assert_image_similarity(
        image1: impl AsRef<Path>,
        image2: impl AsRef<Path>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let output = Command::new("compare")
            .arg("-metric")
            .arg("PSNR")
            .arg(image1.as_ref())
            .arg(image2.as_ref())
            .arg("/dev/null")
            .output()?;
        let psnr: f32 = String::from_utf8(output.stderr)?.parse()?;
        assert!(psnr > 30.0);
        Ok(())
    }

    #[test]
    fn fails_with_no_arguments() -> Result<(), Box<dyn std::error::Error>> {
        let mut cmd = Command::cargo_bin("pio")?;
        cmd.assert().failure().stderr(
            "use `--output` to write to a file or `--output-format` to write to standard output\n",
        );
        Ok(())
    }

    #[test]
    fn reads_jpeg() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempdir()?;
        let input = dir.path().join("input.jpeg");
        convert_image("images/image1-original.png", &input)?;
        let output = dir.path().join("output.jpeg");
        let mut cmd = Command::cargo_bin("pio")?;
        cmd.arg(&input).arg("-o").arg(&output).assert().success();
        assert_image_similarity(input, output)?;
        Ok(())
    }

    #[test]
    fn outputs_jpeg() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempdir()?;
        let input = "images/image1-original.png";
        let output = dir.path().join("output.jpeg");
        let mut cmd = Command::cargo_bin("pio")?;
        cmd.arg(input).arg("-o").arg(&output).assert().success();
        assert_image_similarity(input, output)?;
        Ok(())
    }

    #[test]
    fn reads_webp() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempdir()?;
        let input = dir.path().join("input.webp");
        convert_image("images/image1-original.png", &input)?;
        let output = dir.path().join("output.jpeg");
        let mut cmd = Command::cargo_bin("pio")?;
        cmd.arg(&input).arg("-o").arg(&output).assert().success();
        assert_image_similarity(input, output)?;
        Ok(())
    }

    #[test]
    fn outputs_webp() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempdir()?;
        let input = "images/image1-original.png";
        let output = dir.path().join("output.webp");
        let mut cmd = Command::cargo_bin("pio")?;
        cmd.arg(input).arg("-o").arg(&output).assert().success();
        assert_image_similarity(input, output)?;
        Ok(())
    }

    #[test]
    fn outputs_png() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempdir()?;
        let input = "images/image1-original.png";
        let output = dir.path().join("output.png");
        let mut cmd = Command::cargo_bin("pio")?;
        cmd.arg(input).arg("-o").arg(&output).assert().success();
        assert_image_similarity(input, output)?;
        Ok(())
    }

    #[test]
    fn does_not_create_empty_output_on_invalid_input() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempdir()?;
        let output = dir.path().join("output.png");
        let mut cmd = Command::cargo_bin("pio")?;
        cmd.arg("-o")
            .arg(&output)
            .write_stdin("RIFF....WEBP....")
            .assert()
            .failure();
        assert!(std::fs::read(&output).is_err());
        Ok(())
    }

    #[test]
    fn outputs_to_special_files() -> Result<(), Box<dyn std::error::Error>> {
        let mut cmd = Command::cargo_bin("pio")?;
        cmd.args(&[
            "images/image1-original.png",
            "-o",
            "/dev/null",
            "--output-format",
            "jpeg",
        ])
        .assert()
        .success();
        Ok(())
    }
}
