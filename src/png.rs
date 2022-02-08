// SPDX-FileCopyrightText: 2019-2020 Tuomas Siipola
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::common::{exif_orientation, orient_image, CompressResult, Image, ReadResult};
use crate::profile::is_srgb;

pub fn read(buffer: &[u8]) -> ReadResult {
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

pub fn compress(image: &Image, quality: u8) -> CompressResult {
    let (palette, pixels) = {
        let mut liq = imagequant::new();
        liq.set_quality(0, quality).unwrap();
        let img = &mut (liq
            .new_image(&*image.data, image.width, image.height, 0.0)
            .map_err(|err| err.to_string())?);
        let mut res = liq.quantize(img).map_err(|err| err.to_string())?;
        res.set_dithering_level(1.0).unwrap();
        res.remapped(img).map_err(|err| err.to_string())?
    };
    let buffer = {
        let mut encoder = lodepng::Encoder::new();

        // `sRGB` chunk where 0x00 specifies perceptual rendering intent.
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
                /* Gamma: 0. */ &45455u32.to_be_bytes(),
            )
            .map_err(|err| err.to_string())?;
        encoder
            .info_png_mut()
            .create_chunk(
                lodepng::ChunkPosition::IHDR,
                b"cHRM",
                &[
                    /* White Point x: 0. */ 31270u32.to_be_bytes(),
                    /* White Point y: 0. */ 32900u32.to_be_bytes(),
                    /* Red x:         0. */ 64000u32.to_be_bytes(),
                    /* Red y:         0. */ 33000u32.to_be_bytes(),
                    /* Green x:       0. */ 30000u32.to_be_bytes(),
                    /* Green y:       0. */ 60000u32.to_be_bytes(),
                    /* Blue x:        0. */ 15000u32.to_be_bytes(),
                    /* Blue y:        0.0 */ 6000u32.to_be_bytes(),
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
