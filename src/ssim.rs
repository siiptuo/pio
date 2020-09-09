// SPDX-FileCopyrightText: 2019-2020 Tuomas Siipola
// SPDX-License-Identifier: AGPL-3.0-or-later

use dssim::{Dssim, DssimImage};

use crate::common::Image;

const SCALES: [f64; 5] = [0.0448, 0.2856, 0.3001, 0.2363, 0.1333];

pub struct Calculator {
    attr: Dssim,
    original: DssimImage<f32>,
}

fn sum(xs: &[f32]) -> f64 {
    xs.iter().map(|x| *x as f64).sum()
}

impl Calculator {
    pub fn new(original: &Image) -> Option<Self> {
        let mut attr = Dssim::new();
        attr.set_scales(&SCALES);
        attr.set_save_ssim_maps(SCALES.len() as u8);
        Some(Self {
            original: attr.create_image(&original.to_rgbaplu())?,
            attr,
        })
    }

    pub fn compare(&self, compressed: &Image) -> Option<f64> {
        let (_dssim, ssim_maps) = self.attr.compare(
            &self.original,
            self.attr.create_image(&compressed.to_rgbaplu())?,
        );

        // P-SSIM from Moorthy, A. K., & Bovik, A. C. (2009).  Visual importance pooling for image
        // quality assessment. IEEE journal of selected topics in signal processing, 3(2), 193-201.
        let p = 0.06;
        let r = 4000.0;
        let mut n = 0.0;
        let mut d = 0.0;
        for (ssim_map, weight) in ssim_maps.iter().zip(SCALES.iter()) {
            let mut values: Vec<f32> = ssim_map.map.pixels().collect();
            values.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let (a, b) = values.split_at((p * values.len() as f64) as usize);
            let pssim = (r * sum(a) + sum(b)) / (r * a.len() as f64 + b.len() as f64);
            n += *weight * pssim;
            d += *weight;
        }
        let pssim = n / d;
        let dssim = 1.0 / pssim.max(std::f64::EPSILON) - 1.0;

        Some(dssim)
    }
}
