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
use std::io::{Read, Write};
use std::mem::{ManuallyDrop, MaybeUninit};
use std::path::{Path, PathBuf};

const TINYSRGB: &[u8] = include_bytes!("tinysrgb.icc");
// const TINYSRGB_DEFLATE: &[u8] = include_bytes!("tinysrgb.icc.deflate");

fn is_srgb(profile: &lcms2::Profile) -> bool {
    match profile
        .info(lcms2::InfoType::Description, lcms2::Locale::none())
        .as_ref()
        .map(String::as_str)
    {
        // Facebook's TINYsRGB
        Some("c2") => true,
        _ => false,
    }
}

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

    fn from_gray(data: Vec<GRAY8>, width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            data: data.iter().map(|c| RGB8::from(*c).alpha(255)).collect(),
            color_space: ColorSpace::Gray,
        }
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

// ICC profiles can be split into chunks and stored in multiple markers. Reconstruct the profile by
// reading these markers and concatenating their data.
fn jpeg_icc(dinfo: &mozjpeg::Decompress) -> Result<Option<Vec<u8>>, String> {
    let mut markers = dinfo.markers();
    let first_chunk = markers.find_map(|marker| match marker.data {
        [b'I', b'C', b'C', b'_', b'P', b'R', b'O', b'F', b'I', b'L', b'E', b'\0', 1, total, data @ ..] => Some((*total, data.to_vec())),
        _ => None
    });
    if let Some((total_chunks, mut buffer)) = first_chunk {
        let mut chunks_read = 1;
        for marker in markers {
            if chunks_read == total_chunks {
                break;
            }
            if let [b'I', b'C', b'C', b'_', b'P', b'R', b'O', b'F', b'I', b'L', b'E', b'\0', index, total, data @ ..] =
                marker.data
            {
                chunks_read += 1;
                if *index != chunks_read {
                    return Err(format!(
                        "Failed to read ICC profile: invalid index (expected {} found {})",
                        chunks_read, index
                    ));
                }
                if *total != total_chunks {
                    return Err(format!("Failed to read ICC profile: different totals in two chunks (expected {} found {})", total_chunks, total));
                }
                buffer.extend_from_slice(data);
            }
        }
        if chunks_read == total_chunks {
            Ok(Some(buffer))
        } else {
            Err(format!(
                "Failed to read ICC profile: {} chunks missing out of {} chunks",
                total_chunks - chunks_read,
                total_chunks
            ))
        }
    } else {
        Ok(None)
    }
}

fn read_jpeg(buffer: &[u8]) -> ReadResult {
    let dinfo = mozjpeg::Decompress::with_markers(&[mozjpeg::Marker::APP(2)])
        .from_mem(buffer)
        .map_err(|err| err.to_string())?;

    let profile = match jpeg_icc(&dinfo) {
        Ok(Some(icc)) => match lcms2::Profile::new_icc(&icc) {
            Ok(x) => Some(x),
            Err(err) => {
                eprintln!("Failed to read ICC profile: {}", err);
                None
            }
        },
        Ok(None) => None,
        Err(err) => {
            eprintln!("Failed to read ICC profile: {}", err);
            None
        }
    };

    let (width, height) = dinfo.size();

    let image = match dinfo.image() {
        Ok(mozjpeg::decompress::Format::RGB(mut decompress)) => {
            let mut data: Vec<RGB8> = decompress
                .read_scanlines()
                .ok_or_else(|| "Failed decode image data".to_string())?;
            decompress.finish_decompress();

            if let Some(profile) = profile {
                if !is_srgb(&profile) {
                    eprintln!("Transforming RGB to sRGB...");
                    let transform = lcms2::Transform::new(
                        &profile,
                        lcms2::PixelFormat::RGB_8,
                        &lcms2::Profile::new_srgb(),
                        lcms2::PixelFormat::RGB_8,
                        lcms2::Intent::Perceptual,
                    )
                    .map_err(|err| err.to_string())?;
                    transform.transform_in_place(&mut data);
                }
            }

            Ok(Image::from_rgb(data, width, height))
        }
        Ok(mozjpeg::decompress::Format::Gray(mut decompress)) => {
            let mut data: Vec<GRAY8> = decompress
                .read_scanlines()
                .ok_or_else(|| "Failed decode image data".to_string())?;
            decompress.finish_decompress();

            if let Some(profile) = profile {
                eprintln!("Transforming Gray to sRGB...");
                let transform = lcms2::Transform::new(
                    &profile,
                    lcms2::PixelFormat::GRAY_8,
                    &lcms2::Profile::new_srgb(),
                    lcms2::PixelFormat::GRAY_8,
                    lcms2::Intent::Perceptual,
                )
                .map_err(|err| err.to_string())?;
                transform.transform_in_place(&mut data);
            }

            Ok(Image::from_gray(data, width, height))
        }
        Ok(mozjpeg::decompress::Format::CMYK(mut decompress)) => {
            let profile = profile
                .ok_or_else(|| "Expected ICC profile for JPEG in CMYK color space".to_string())?;

            let data: Vec<[u8; 4]> = decompress
                .read_scanlines()
                .ok_or_else(|| "Failed decode image data".to_string())?;
            decompress.finish_decompress();

            eprintln!("Transforming CMYK to sRGB...");
            let transform = lcms2::Transform::new(
                &profile,
                lcms2::PixelFormat::CMYK_8_REV,
                &lcms2::Profile::new_srgb(),
                lcms2::PixelFormat::RGB_8,
                lcms2::Intent::Perceptual,
            )
            .map_err(|err| err.to_string())?;

            let mut output = vec![RGB8::new(0, 0, 0); data.len()];
            transform.transform_pixels(&data, &mut output);

            Ok(Image::from_rgb(output, width, height))
        }
        Err(err) => Err(format!("Failed decode image data: {}", err)),
    }?;

    let orientation = exif::Reader::new()
        .read_from_container(&mut std::io::Cursor::new(buffer))
        .ok()
        .and_then(exif_orientation)
        .unwrap_or(1);

    Ok(orient_image(image, orientation))
}

fn compress_jpeg(
    image: &Image,
    quality: u8,
    chroma_subsampling: ChromaSubsampling,
) -> CompressResult {
    let mut cinfo = mozjpeg::Compress::new(match image.color_space {
        ColorSpace::Gray => mozjpeg::ColorSpace::JCS_GRAYSCALE,
        _ => mozjpeg::ColorSpace::JCS_EXT_RGBX,
    });
    cinfo.set_size(image.width, image.height);
    cinfo.set_quality(quality as f32);
    cinfo.set_mem_dest();

    if image.color_space != ColorSpace::Gray {
        let chroma_subsampling = match chroma_subsampling {
            ChromaSubsampling::_444 => [[1, 1], [1, 1], [1, 1]],
            ChromaSubsampling::_422 => [[2, 1], [1, 1], [1, 1]],
            ChromaSubsampling::_420 => [[2, 2], [1, 1], [1, 1]],
        };
        for (c, samp) in cinfo
            .components_mut()
            .iter_mut()
            .zip(chroma_subsampling.iter())
        {
            c.h_samp_factor = samp[0];
            c.v_samp_factor = samp[1];
        }
    }

    cinfo.start_compress();
    // TODO: gray profile?
    cinfo.write_marker(
        mozjpeg::Marker::APP(2),
        &[b"ICC_PROFILE\0\x01\x01", TINYSRGB].concat(),
    );
    if !match image.color_space {
        ColorSpace::Gray => cinfo.write_scanlines(image.to_gray().buf().as_bytes()),
        _ => cinfo.write_scanlines(image.as_bytes()),
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

    let mut png = match decoder.decode(&buffer) {
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

    if let Ok(icc) = decoder.get_icc() {
        eprintln!("transforming to srgb...");
        match lcms2::Profile::new_icc(&icc) {
            Ok(profile) => {
                if !is_srgb(&profile) {
                    let transform = lcms2::Transform::new(
                        &profile,
                        lcms2::PixelFormat::RGBA_8,
                        &lcms2::Profile::new_srgb(),
                        lcms2::PixelFormat::RGBA_8,
                        lcms2::Intent::Perceptual,
                    )
                    .map_err(|err| err.to_string())?;
                    transform.transform_in_place(&mut png.buffer);
                }
            }
            Err(err) => {
                eprintln!("Failed to read ICC profile: {}", err);
            }
        }
    }

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

        // `sRGB` chunk with perceptual rendering intent.
        encoder
            .info_png_mut()
            .create_chunk(lodepng::ChunkPosition::IHDR, b"sRGB", b"\x00")
            .map_err(|err| err.to_string())?;
        // Recommended chunks from PNG 1.2 specification for compatibility with applications that
        // do not support the `sRGB` chunk.
        encoder
            .info_png_mut()
            .create_chunk(
                lodepng::ChunkPosition::IHDR,
                b"gAMA",
                &45455u32.to_be_bytes(),
            )
            .map_err(|err| err.to_string())?;
        encoder
            .info_png_mut()
            .create_chunk(
                lodepng::ChunkPosition::IHDR,
                b"cHRM",
                &[
                    31270u32.to_be_bytes(),
                    32900u32.to_be_bytes(),
                    64000u32.to_be_bytes(),
                    33000u32.to_be_bytes(),
                    30000u32.to_be_bytes(),
                    60000u32.to_be_bytes(),
                    15000u32.to_be_bytes(),
                    6000u32.to_be_bytes(),
                ]
                .concat(),
            )
            .map_err(|err| err.to_string())?;

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

        // XXX: Not safe because `buffer` is not allocated by `Vec`.
        //      Probably fine because size is not changed :)
        let mut buffer: Vec<RGBA8> = Vec::from_raw_parts(
            rgba as *mut _,
            (width * height) as usize,
            (width * height) as usize,
        );

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

        let mut icc = MaybeUninit::uninit();
        let ret = WebPMuxGetChunk(mux, b"ICCP" as *const _ as *const _, icc.as_mut_ptr());
        let icc_data = match ret {
            WebPMuxError::WEBP_MUX_OK => {
                let icc = icc.assume_init();
                Some(std::slice::from_raw_parts(icc.bytes, icc.size))
            }
            WebPMuxError::WEBP_MUX_NOT_FOUND => None,
            error => return Err(format!("{:?}", error)),
        };
        if let Some(icc) = icc_data {
            eprintln!("transforming to srgb...");
            match lcms2::Profile::new_icc(&icc) {
                Ok(profile) => {
                    if !is_srgb(&profile) {
                        let transform = lcms2::Transform::new(
                            &profile,
                            lcms2::PixelFormat::RGBA_8,
                            &lcms2::Profile::new_srgb(),
                            lcms2::PixelFormat::RGBA_8,
                            lcms2::Intent::Perceptual,
                        )
                        .map_err(|err| err.to_string())?;
                        transform.transform_in_place(&mut buffer);
                    }
                }
                Err(err) => {
                    eprintln!("Failed to read ICC profile: {}", err);
                }
            }
        }

        WebPMuxDelete(mux);

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

        let data = WebPData {
            bytes: wrt.mem,
            size: wrt.size,
        };

        let mux = WebPMuxCreateInternal(&data, 0, WEBP_MUX_ABI_VERSION);
        if mux.is_null() {
            return Err("failed to create mux".to_string());
        }

        let profile = WebPData {
            bytes: TINYSRGB.as_ptr(),
            size: TINYSRGB.len(),
        };

        let ret = WebPMuxSetChunk(
            mux,
            b"ICCP" as *const _ as *const _,
            &profile as *const _,
            0,
        );
        if ret != WebPMuxError::WEBP_MUX_OK {
            return Err("failed set ICCP chunk".to_string());
        }

        let mut output = MaybeUninit::<WebPData>::uninit();
        let ret = WebPMuxAssemble(mux, output.as_mut_ptr());
        if ret != WebPMuxError::WEBP_MUX_OK {
            return Err("failed to assemble".to_string());
        }
        let mut output = output.assume_init();

        WebPMuxDelete(mux);

        let capacity = image.width * image.height;
        let mut pixels: Vec<RGBA8> = Vec::with_capacity(capacity);
        pixels.set_len(capacity);

        let ret = WebPDecodeRGBAInto(
            output.bytes,
            output.size,
            pixels.as_mut_ptr() as *mut u8,
            4 * image.width * image.height,
            (4 * image.width) as i32,
        );
        if ret.is_null() {
            WebPDataClear(&mut output);
            return Err("Failed to decode image data".to_string());
        }

        // XXX: unnecessary copy
        let buffer = std::slice::from_raw_parts(output.bytes, output.size as usize).to_vec();

        WebPDataClear(&mut output);

        Ok((Image::from_rgba(pixels, image.width, image.height), buffer))
    }
}

#[derive(PartialEq, Copy, Clone)]
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
    /// Write to standard output or special file (e.g. /dev/null)
    Stream(Box<dyn Write>),

    /// Write to regular file
    WriteFile {
        path: PathBuf,
        file: ManuallyDrop<File>,
        dir: File,
        finished: bool,
    },

    /// Overwrite file atomically
    OverwriteFile {
        dst_path: PathBuf,
        tmp_path: PathBuf,
        dst_dir: File,
        tmp_file: ManuallyDrop<File>,
        tmp_file_closed: bool,
        finished: bool,
    },
}

fn random_file(root: impl AsRef<Path>) -> std::io::Result<(PathBuf, File)> {
    use rand::distributions::Alphanumeric;
    use rand::{thread_rng, Rng};
    use std::fs::OpenOptions;

    let rng = thread_rng();

    loop {
        let path = root.as_ref().with_file_name(format!(
            ".pio-{}.tmp",
            rng.sample_iter(&Alphanumeric).take(16).collect::<String>()
        ));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => break Ok((path, file)),
            Err(err) => match err.kind() {
                std::io::ErrorKind::AlreadyExists => continue,
                _ => break Err(err),
            },
        };
    }
}

fn file_directory(path: impl AsRef<Path>) -> PathBuf {
    match path.as_ref().parent() {
        Some(parent) => {
            if parent.as_os_str().len() == 0 {
                PathBuf::from(".")
            } else {
                parent.to_path_buf()
            }
        }
        None => PathBuf::from("."),
    }
}

impl Output {
    fn write_file(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let path = path.as_ref();
        let file = File::create(path)?;
        if file.metadata()?.is_file() {
            Ok(Self::WriteFile {
                path: path.to_path_buf(),
                file: ManuallyDrop::new(file),
                dir: File::open(file_directory(path))?,
                finished: false,
            })
        } else {
            Ok(Self::Stream(Box::new(file)))
        }
    }

    fn overwrite_file(path: impl AsRef<Path>) -> Result<Self, Box<dyn std::error::Error>> {
        if !std::fs::metadata(&path)?.is_file() {
            return Err("expected regular file".into());
        }
        let path = path.as_ref();
        let (tmp_path, tmp_file) = random_file(path)?;
        let dst_dir = File::open(file_directory(path))?;
        Ok(Self::OverwriteFile {
            dst_path: path.to_path_buf(),
            tmp_path,
            tmp_file: ManuallyDrop::new(tmp_file),
            tmp_file_closed: false,
            dst_dir,
            finished: false,
        })
    }

    fn stdout() -> Self {
        Self::Stream(Box::new(std::io::stdout()))
    }

    fn write(mut self, buf: &[u8]) -> std::io::Result<()> {
        match self {
            Output::Stream(ref mut write) => {
                write.write_all(buf)?;
                write.flush()?;
            }
            Output::WriteFile {
                ref mut file,
                ref mut finished,
                ref mut dir,
                ..
            } => {
                file.write_all(buf)?;
                file.sync_all()?;
                dir.sync_all()?;
                *finished = true;
            }
            Output::OverwriteFile {
                ref dst_path,
                ref tmp_path,
                ref mut tmp_file,
                ref mut dst_dir,
                ref mut finished,
                ref mut tmp_file_closed,
            } => {
                tmp_file.write_all(buf)?;
                tmp_file.sync_all()?;
                unsafe { ManuallyDrop::drop(tmp_file) }
                *tmp_file_closed = true;
                std::fs::rename(tmp_path, dst_path)?;
                dst_dir.sync_all()?;
                *finished = true;
            }
        };
        Ok(())
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        match self {
            Output::Stream(_) => {}
            Output::WriteFile {
                path,
                file,
                finished,
                ..
            } => {
                unsafe { ManuallyDrop::drop(file) }
                if !*finished {
                    std::fs::remove_file(path).unwrap_or_else(|_err| {});
                }
            }
            Output::OverwriteFile {
                tmp_path,
                tmp_file,
                finished,
                tmp_file_closed,
                ..
            } => {
                if !*tmp_file_closed {
                    unsafe { ManuallyDrop::drop(tmp_file) }
                }
                if !*finished {
                    std::fs::remove_file(tmp_path).unwrap_or_else(|_err| {});
                }
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

    let (input_format, input_buffer) = {
        let mut reader: Box<dyn std::io::Read> = match matches.value_of_os("INPUT") {
            None => {
                if matches.value_of("output").is_none()
                    && matches.value_of("output-format").is_none()
                {
                    return Err("reading from standard input, use `--output` to write to a file or `--output-format` to write to standard output".to_string());
                }
                Box::new(std::io::stdin())
            }
            Some(path) => Box::new(
                File::open(path).map_err(|err| format!("failed to open input file: {}", err))?,
            ),
        };

        // Read enough data to determine input file format by magic number.
        let mut buf = vec![0; 16];
        reader
            .read_exact(&mut buf)
            .map_err(|err| format!("failed to read magic number: {}", err))?;
        let fmt = Format::from_magic(&buf)
            .ok_or_else(|| "unknown input format, expected jpeg, png or webp".to_string())?;
        // Read rest of the input.
        reader
            .read_to_end(&mut buf)
            .map_err(|err| format!("failed to read input: {}", err))?;

        (fmt, buf)
    };

    let (output_format, output_writer) = if matches.is_present("in-place") {
        let format = match matches.value_of("output-format") {
            Some(format) => Format::from_ext(format).unwrap(),
            None => input_format.clone(),
        };
        let path = matches.value_of_os("INPUT").unwrap();
        let output = Output::overwrite_file(path)
            .map_err(|err| format!("unable to overwrite file: {}", err))?;
        (format, output)
    } else {
        match matches.value_of_os("output") {
            Some(path) => {
                let format = match matches.value_of("output-format") {
                    Some(format) => Format::from_ext(format).unwrap(),
                    None => Format::from_path(path).ok_or_else(|| {
                        "failed to determine output format: either use a known file extension (jpeg, png or webp) or specify the format using `--output-format`".to_string()
                    })?,
                };
                let output = Output::write_file(path)
                    .map_err(|err| format!("failed to open output file: {}", err))?;
                (format, output)
            }
            None => {
                let format = Format::from_ext(matches.value_of("output-format").ok_or_else(|| "use `--output` to write to a file or `--output-format` to write to standard output".to_string())?).unwrap();
                (format, Output::stdout())
            }
        }
    };

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
            Arg::with_name("in-place")
                .long("in-place")
                .help("Overwrite input file in-place")
                .conflicts_with("output")
                .requires("INPUT"),
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
            "reading from standard input, use `--output` to write to a file or `--output-format` to write to standard output\n",
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
