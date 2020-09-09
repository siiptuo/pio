// SPDX-FileCopyrightText: 2019-2020 Tuomas Siipola
// SPDX-FileCopyrightText: 2019-2020 Johannes Siipola
//
// SPDX-License-Identifier: AGPL-3.0-or-later

mod common;
mod jpeg;
mod output;
mod png;
mod profile;
mod ssim;
mod webp;

use std::fs::File;
use std::io::Read;

use clap::{App, Arg};
use rgb::RGB8;

use crate::common::{ChromaSubsampling, CompressResult, Format, Image};
use crate::output::Output;

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

fn compress_image(
    image: Image,
    lossy_compress: LossyCompressor,
    lossless_compress: Option<LosslessCompressor>,
    target: f64,
    min_quality: u8,
    max_quality: u8,
    original_size: u64,
) -> Result<Vec<u8>, String> {
    let attr = ssim::Calculator::new(&image)
        .ok_or_else(|| "Failed to calculate SSIM image".to_string())?;

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

        let dssim = attr
            .compare(&compressed)
            .ok_or_else(|| "Failed to calculate SSIM image".to_string())?;

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
            None => input_format,
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
        Format::JPEG => jpeg::read(&input_buffer),
        Format::PNG => png::read(&input_buffer),
        Format::WEBP => webp::read(&input_buffer),
    }
    .map_err(|err| format!("failed to read input: {}", err))?;

    let (lossy_compress, lossless_compress): (LossyCompressor, Option<LosslessCompressor>) =
        match output_format {
            Format::JPEG => (
                Box::new(move |img, q| jpeg::compress(img, q, chroma_subsampling)),
                None,
            ),
            Format::PNG => (Box::new(png::compress), None),
            Format::WEBP => (
                Box::new(|img, q| webp::compress(img, q, false)),
                Some(Box::new(|img| webp::compress(img, 100, true))),
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
