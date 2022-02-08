// SPDX-FileCopyrightText: 2019-2020 Tuomas Siipola
// SPDX-License-Identifier: AGPL-3.0-or-later

use libwebp_sys::*;
use rgb::RGBA8;
use std::mem::MaybeUninit;

use crate::common::{exif_orientation, orient_image, CompressResult, Image, ReadResult};
use crate::profile::{is_srgb, SRGB_PROFILE};

pub fn read(buffer: &[u8]) -> ReadResult {
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
            match lcms2::Profile::new_icc(icc) {
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

pub fn compress(image: &Image, quality: u8, lossless: bool) -> CompressResult {
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
            bytes: SRGB_PROFILE.as_ptr(),
            size: SRGB_PROFILE.len(),
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
        let mut pixels = vec![RGBA8::new(0, 0, 0, 0); capacity];

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
