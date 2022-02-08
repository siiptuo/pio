// SPDX-FileCopyrightText: 2019-2020 Tuomas Siipola
// SPDX-License-Identifier: AGPL-3.0-or-later

use dssim_core::{Dssim, DssimImage};

use crate::common::Image;

pub struct Calculator {
    attr: Dssim,
    original: DssimImage<f32>,
}

impl Calculator {
    pub fn new(original: &Image) -> Option<Self> {
        let attr = Dssim::new();
        Some(Self {
            original: attr.create_image(&original.to_rgbaplu())?,
            attr,
        })
    }

    pub fn compare(&self, compressed: &Image) -> Option<f64> {
        let (dssim, _ssim_maps) = self.attr.compare(
            &self.original,
            self.attr.create_image(&compressed.to_rgbaplu())?,
        );
        Some(dssim.into())
    }
}
