// SPDX-FileCopyrightText: 2019-2020 Tuomas Siipola
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::ffi::OsStr;
use std::path::Path;

use dssim::{ToRGBAPLU, RGBAPLU};
use imgref::{Img, ImgVec};
use rgb::{alt::GRAY8, ComponentBytes, RGB8, RGBA8};

#[derive(PartialEq)]
pub enum ColorSpace {
    Gray,
    GrayAlpha,
    RGB,
    RGBA,
}

pub struct Image {
    pub width: usize,
    pub height: usize,
    pub data: Vec<RGBA8>,
    pub color_space: ColorSpace,
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

pub fn srgb_to_linear(u: u8) -> f32 {
    let u = u as f32 / 255.0;
    if u <= 0.04045 {
        u / 12.92
    } else {
        ((u + 0.055) / 1.055).powf(2.4)
    }
}

pub fn linear_to_srgb(u: f32) -> u8 {
    if u <= 0.0031308 {
        (255.0 * (12.92 * u)).round() as u8
    } else {
        (255.0 * (1.055 * u.powf(1.0 / 2.4) - 0.055)).round() as u8
    }
}

impl Image {
    pub fn from_rgba(data: Vec<RGBA8>, width: usize, height: usize) -> Self {
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

    pub fn from_rgb(data: Vec<RGB8>, width: usize, height: usize) -> Self {
        Self::from_rgba(data.iter().map(|c| c.alpha(255)).collect(), width, height)
    }

    pub fn from_gray(data: Vec<GRAY8>, width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            data: data.iter().map(|c| RGB8::from(*c).alpha(255)).collect(),
            color_space: ColorSpace::Gray,
        }
    }

    pub fn to_rgbaplu(&self) -> ImgVec<RGBAPLU> {
        Img::new(self.data.to_rgbaplu(), self.width, self.height)
    }

    pub fn to_gray(&self) -> ImgVec<GRAY8> {
        Img::new(
            self.data.iter().map(|c| c.g.into()).collect(),
            self.width,
            self.height,
        )
    }

    pub fn alpha_blend(&mut self, bg: RGB8) {
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

    pub fn as_bytes(&self) -> &[u8] {
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

// Rotate and flip image according to Exif orientation.
pub fn orient_image(image: Image, orientation: u32) -> Image {
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

pub fn exif_orientation(exif: exif::Exif) -> Option<u32> {
    exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY)
        .and_then(|field| field.value.get_uint(0))
        .filter(|x| *x >= 1 && *x <= 8)
}

#[derive(Copy, Clone)]
pub enum ChromaSubsampling {
    _420,
    _422,
    _444,
}

#[derive(PartialEq, Copy, Clone)]
pub enum Format {
    JPEG,
    PNG,
    WEBP,
}

impl Format {
    pub fn from_ext(input: &str) -> Option<Self> {
        match input {
            "jpeg" | "jpg" => Some(Self::JPEG),
            "png" => Some(Self::PNG),
            "webp" => Some(Self::WEBP),
            _ => None,
        }
    }

    pub fn from_path(path: impl AsRef<Path>) -> Option<Self> {
        path.as_ref()
            .extension()
            .and_then(OsStr::to_str)
            .and_then(|ext| Self::from_ext(&ext.to_ascii_lowercase()))
    }

    pub fn from_magic(buffer: &[u8]) -> Option<Self> {
        match buffer {
            [0xff, 0xd8, 0xff, ..] => Some(Self::JPEG),
            [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, ..] => Some(Self::PNG),
            [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => Some(Self::WEBP),
            _ => None,
        }
    }

    pub fn supports_transparency(&self) -> bool {
        match self {
            Self::JPEG => false,
            Self::PNG => true,
            Self::WEBP => true,
        }
    }
}

pub type ReadResult = Result<Image, String>;
pub type CompressResult = Result<(Image, Vec<u8>), String>;
