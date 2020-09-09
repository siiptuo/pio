// SPDX-FileCopyrightText: 2019-2020 Tuomas Siipola
// SPDX-FileCopyrightText: 2019-2020 Johannes Siipola
//
// SPDX-License-Identifier: AGPL-3.0-or-later

use rgb::{alt::GRAY8, ComponentBytes, RGB8};

use crate::common::{
    exif_orientation, orient_image, ChromaSubsampling, ColorSpace, CompressResult, Image,
    ReadResult,
};
use crate::profile::{is_srgb, GRAY_PROFILE, SRGB_PROFILE};

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

pub fn read(buffer: &[u8]) -> ReadResult {
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
            let data: Vec<GRAY8> = decompress
                .read_scanlines()
                .ok_or_else(|| "Failed decode image data".to_string())?;
            decompress.finish_decompress();

            if let Some(profile) = profile {
                if !is_srgb(&profile) {
                    eprintln!("Transforming Gray to sRGB...");
                    let transform = lcms2::Transform::new(
                        &profile,
                        lcms2::PixelFormat::GRAY_8,
                        &lcms2::Profile::new_srgb(),
                        lcms2::PixelFormat::RGB_8,
                        lcms2::Intent::Perceptual,
                    )
                    .map_err(|err| err.to_string())?;

                    let mut output = vec![RGB8::new(0, 0, 0); data.len()];
                    transform.transform_pixels(&data, &mut output);

                    Ok(Image::from_rgb(output, width, height))
                } else {
                    Ok(Image::from_gray(data, width, height))
                }
            } else {
                Ok(Image::from_gray(data, width, height))
            }
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

pub fn compress(
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
    let profile = match image.color_space {
        ColorSpace::Gray => GRAY_PROFILE,
        _ => SRGB_PROFILE,
    };
    cinfo.write_marker(
        mozjpeg::Marker::APP(2),
        &[b"ICC_PROFILE\0\x01\x01", profile].concat(),
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
    let image = read(&cdata)?;

    Ok((image, cdata))
}
