// SPDX-FileCopyrightText: 2019-2020 Tuomas Siipola
// SPDX-License-Identifier: AGPL-3.0-or-later

pub const SRGB_PROFILE: &[u8] = include_bytes!("../profiles/sRGB-v2-nano.icc");
pub const GRAY_PROFILE: &[u8] = include_bytes!("../profiles/sGrey-v2-nano.icc");

pub fn is_srgb(profile: &lcms2::Profile) -> bool {
    match profile
        .info(lcms2::InfoType::Description, lcms2::Locale::none())
        .as_deref()
    {
        // TINYsRGB by Facebook
        // (https://www.facebook.com/notes/facebook-engineering/under-the-hood-improving-facebook-photos/10150630639853920)
        Some("c2") => true,
        // sRGBz by Øyvind Kolås (https://pippin.gimp.org/sRGBz/)
        Some("sRGBz") | Some("z") => true,
        // Compact ICC Profiles by Clinton Ingram
        // (https://github.com/saucecontrol/Compact-ICC-Profiles/)
        Some("nRGB") | Some("uRGB") | Some("sRGB") | Some("nGry") | Some("uGry") | Some("sGry") => {
            true
        }
        Some(desc) => desc.to_ascii_lowercase().contains("srgb"),
        None => false,
    }
}
