// SPDX-FileCopyrightText: 2019-2020 Tuomas Siipola
// SPDX-FileCopyrightText: 2019-2020 Johannes Siipola
//
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::fs::File;
use std::io::Read;
use std::path::PathBuf;

use clap::{ArgEnum, Parser};
use rgb::RGB8;

use pio::common::{ChromaSubsampling, ChromaSubsamplingOption, CompressResult, Format, Image};
use pio::output::Output;
use pio::{jpeg, png, ssim, webp};

type LossyCompressor = Box<dyn Fn(&Image, u8, ChromaSubsampling) -> CompressResult>;
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

fn parse_quality(x: &str) -> Result<u8, &'static str> {
    match x.parse::<u8>() {
        Ok(x) => {
            if (0..=100).contains(&x) {
                Ok(x)
            } else {
                Err("expected value between 0 and 100")
            }
        }
        Err(_) => Err("expected value between 0 and 100"),
    }
}

fn parse_color(input: &str) -> Result<RGB8, String> {
    if input.len() != 7 || !input.starts_with('#') {
        return Err("expected format #rrggbb".to_string());
    }
    Ok(RGB8::new(
        u8::from_str_radix(&input[1..=2], 16).map_err(|err| err.to_string())?,
        u8::from_str_radix(&input[3..=4], 16).map_err(|err| err.to_string())?,
        u8::from_str_radix(&input[5..=6], 16).map_err(|err| err.to_string())?,
    ))
}

#[derive(PartialEq, Copy, Clone, ArgEnum)]
enum FailStrategy {
    None, // TODO: split to Ignore and Warn?
    Exit,
    Copy,
}

#[derive(Parser)]
#[clap(version, about = "Perceptual Image Optimizer")]
struct Args {
    /// Input file to use, standard input is used when value is - or not set
    #[clap(parse(from_os_str))]
    input: Option<PathBuf>,

    /// Set output file
    #[clap(short, long, parse(from_os_str))]
    output: Option<PathBuf>,

    /// Set output file format
    #[clap(arg_enum, long, value_name = "FORMAT")]
    output_format: Option<Format>,

    /// Overwrite input file in-place
    #[clap(long, requires = "input", conflicts_with = "output")]
    in_place: bool,

    /// Set target quality for output
    #[clap(parse(try_from_str = parse_quality), default_value_t = 85, long)]
    quality: u8,

    /// Set minimum quality for output
    #[clap(parse(try_from_str = parse_quality), long)]
    min: Option<u8>,

    /// Set maximum quality for output
    #[clap(parse(try_from_str = parse_quality), long)]
    max: Option<u8>,

    /// Set deviation from the quality target
    #[clap(parse(try_from_str = parse_quality), default_value_t=10, long)]
    spread: u8,

    /// Set background color to use when output format doesn't support transparency
    #[clap(parse(try_from_str = parse_color), default_value = "#ffffff", long, value_name = "COLOR")]
    background_color: RGB8,

    /// Add background color even if output format supports transparency
    #[clap(long)]
    no_transparency: bool,

    /// Set strategy to use when output is larger than the input
    #[clap(arg_enum, long = "optimization_failed", default_value_t=FailStrategy::None, value_name = "STRATEGY")]
    fail_strategy: FailStrategy,

    /// Specify chroma subsampling
    #[clap(long, possible_values=["444", "422", "420", "auto"], default_value="auto")]
    chroma_subsampling: String,
}

fn find_image(
    image: &Image,
    attr: &ssim::Calculator,
    lossy_compress: &LossyCompressor,
    target: f64,
    min_quality: u8,
    max_quality: u8,
    original_size: u64,
    chroma_subsampling: ChromaSubsampling,
) -> Result<(f64, Vec<u8>), String> {
    let mut min = min_quality;
    let mut max = max_quality;
    let mut best_buffer = Vec::new();
    let mut best_dssim = f64::INFINITY;

    // Compress image with different qualities and find which is closest to the SSIM target. Binary
    // search is used to speed up the search. Since there are 101 possible quality values, only
    // ceil(log2(101)) = 7 comparisons are needed at maximum.
    loop {
        // Overflow is not possible because `min` and `max` are in range 0-100.
        let quality = (min + max) / 2;

        let (compressed, buffer) = lossy_compress(image, quality, chroma_subsampling)?;

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
            100 * buffer.len() as u64 / original_size,
        );

        // Last steps of the binary search are pretty close to each other, so the final step may
        // not actually have SSIM closest to the target. Instead of using the last step, keep track
        // of the best attempt so far.
        if (dssim - target).abs() < (best_dssim - target).abs() {
            best_buffer = buffer;
            best_dssim = dssim;
        }

        // Binary search step.
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

    Ok((best_dssim, best_buffer))
}

fn compress_image(
    image: Image,
    lossy_compress: LossyCompressor,
    lossless_compress: Option<LosslessCompressor>,
    target: f64,
    min_quality: u8,
    max_quality: u8,
    original_size: u64,
    chroma_subsampling: ChromaSubsamplingOption,
) -> Result<Vec<u8>, String> {
    let attr = ssim::Calculator::new(&image)
        .ok_or_else(|| "Failed to calculate SSIM image".to_string())?;

    let mut best_buffer = Vec::new();
    let mut best_dssim = f64::INFINITY;

    let samplings = match chroma_subsampling {
        ChromaSubsamplingOption::Auto => vec![
            ChromaSubsampling::_444,
            ChromaSubsampling::_422,
            ChromaSubsampling::_420,
        ],
        ChromaSubsamplingOption::Manual(sampling) => vec![sampling],
        ChromaSubsamplingOption::None => vec![ChromaSubsampling::_444],
    };

    for sampling in samplings {
        eprintln!("chroma subsampling: {:?}", sampling);
        let (dssim, buffer) = find_image(
            &image,
            &attr,
            &lossy_compress,
            target,
            min_quality,
            max_quality,
            original_size,
            sampling,
        )?;
        if (dssim - target).abs() < (best_dssim - target).abs() {
            best_buffer = buffer;
            best_dssim = dssim;
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
        if b.len() < best_buffer.len() {
            return Ok(b);
        }
    }

    Ok(best_buffer)
}

fn pio(args: Args) -> Result<(), String> {
    let target = QUALITY_SSIM[args.quality as usize];

    let min = args
        .min
        .unwrap_or_else(|| args.quality.saturating_sub(args.spread));
    let max = args
        .max
        .unwrap_or_else(|| std::cmp::min(args.quality + args.spread, 100));
    if min > max {
        return Err("value of `--min` must be less or equal to value of `--max`".to_string());
    }

    let (input_format, input_buffer) = {
        let mut reader: Box<dyn std::io::Read> = match &args.input {
            None => {
                if args.output.is_none() && args.output_format.is_none() {
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

    let (output_format, output_writer) = if args.in_place {
        let format = args.output_format.unwrap_or(input_format);
        let path = args.input.unwrap(); // validated by clap
        let output = Output::overwrite_file(path)
            .map_err(|err| format!("unable to overwrite file: {}", err))?;
        (format, output)
    } else {
        match &args.output {
            Some(path) => {
                let format = args.output_format.or_else(|| Format::from_path(path)).ok_or_else(|| {
                    "failed to determine output format: either use a known file extension (jpeg, png or webp) or specify the format using `--output-format`".to_string()
                })?;
                let output = Output::write_file(path)
                    .map_err(|err| format!("failed to open output file: {}", err))?;
                (format, output)
            }
            None => {
                let format = args.output_format.ok_or_else(|| "use `--output` to write to a file or `--output-format` to write to standard output".to_string())?;
                (format, Output::stdout())
            }
        }
    };

    let chroma_subsampling = if output_format.supports_chroma_subsampling() {
        match args.chroma_subsampling.as_str() {
            "420" => ChromaSubsamplingOption::Manual(ChromaSubsampling::_420),
            "422" => ChromaSubsamplingOption::Manual(ChromaSubsampling::_422),
            "444" => ChromaSubsamplingOption::Manual(ChromaSubsampling::_444),
            "auto" => ChromaSubsamplingOption::Auto,
            _ => unreachable!(),
        }
    } else {
        ChromaSubsamplingOption::None
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
            Format::JPEG => (Box::new(jpeg::compress), None),
            Format::PNG => (Box::new(|img, q, _cs| png::compress(img, q)), None),
            Format::WEBP => (
                Box::new(|img, q, _cs| webp::compress(img, q, false)),
                Some(Box::new(|img| webp::compress(img, 100, true))),
            ),
        };

    if !output_format.supports_transparency() || args.no_transparency {
        input_image.alpha_blend(args.background_color);
    }

    match compress_image(
        input_image,
        lossy_compress,
        lossless_compress,
        target,
        min,
        max,
        original_size as u64,
        chroma_subsampling,
    ) {
        Ok(output_buffer) => {
            if output_buffer.len() <= original_size as usize {
                output_writer
                    .write(&output_buffer)
                    .map_err(|err| format!("failed to write output: {}", err))?;
                Ok(())
            } else {
                match args.fail_strategy {
                    FailStrategy::None => {
                        eprintln!("warning: Output is larger than input but still writing output normally. This behavior can be changed with `--optimization-failed` option.");
                        output_writer
                            .write(&output_buffer)
                            .map_err(|err| format!("failed to write output: {}", err))?;
                        Ok(())
                    }
                    FailStrategy::Exit => {
                        Err("error: Output would be larger than input, exiting now...".to_string())
                    }
                    FailStrategy::Copy => {
                        eprintln!("warning: Output would be larger than input, copying input to output...");
                        output_writer
                            .write(&output_buffer)
                            .map_err(|err| format!("failed to write output: {}", err))?;
                        Ok(())
                    }
                }
            }
        }
        Err(err) => Err(format!("failed to compress image: {}", err)),
    }
}

fn main() {
    let args = Args::parse();
    pio(args).unwrap_or_else(|err| {
        eprintln!("{}", err);
        std::process::exit(1);
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use assert_cmd::Command;
    use tempfile::tempdir;

    fn convert_image(input: impl AsRef<Path>, output: impl AsRef<Path>) {
        Command::new("convert")
            .arg(input.as_ref())
            .arg("-quality")
            .arg("100")
            .arg(output.as_ref())
            .assert()
            .success();
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

    fn assert_jpeg_sampling_factors(image: impl AsRef<Path>, sampling_factors: &'static str) {
        Command::new("identify")
            .arg("-format")
            .arg("%[jpeg:sampling-factor]")
            .arg(image.as_ref())
            .assert()
            .success()
            .stdout(sampling_factors);
    }

    #[test]
    fn fails_with_no_arguments() -> Result<(), Box<dyn std::error::Error>> {
        Command::cargo_bin("pio")?
            .assert()
            .failure()
            .stderr("reading from standard input, use `--output` to write to a file or `--output-format` to write to standard output\n");
        Ok(())
    }

    #[test]
    fn reads_jpeg() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempdir()?;
        let input = dir.path().join("input.jpeg");
        convert_image("images/image1-original.png", &input);
        let output = dir.path().join("output.jpeg");
        Command::cargo_bin("pio")?
            .arg(&input)
            .arg("-o")
            .arg(&output)
            .assert()
            .success();
        assert_image_similarity(input, output)?;
        Ok(())
    }

    #[test]
    fn outputs_jpeg() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempdir()?;
        let input = "images/image1-original.png";
        let output = dir.path().join("output.jpeg");
        Command::cargo_bin("pio")?
            .arg(input)
            .arg("-o")
            .arg(&output)
            .assert()
            .success();
        assert_image_similarity(input, output)?;
        Ok(())
    }

    #[test]
    fn reads_webp() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempdir()?;
        let input = dir.path().join("input.webp");
        convert_image("images/image1-original.png", &input);
        let output = dir.path().join("output.jpeg");
        Command::cargo_bin("pio")?
            .arg(&input)
            .arg("-o")
            .arg(&output)
            .assert()
            .success();
        assert_image_similarity(input, output)?;
        Ok(())
    }

    #[test]
    fn outputs_webp() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempdir()?;
        let input = "images/image1-original.png";
        let output = dir.path().join("output.webp");
        Command::cargo_bin("pio")?
            .arg(input)
            .arg("-o")
            .arg(&output)
            .assert()
            .success();
        assert_image_similarity(input, output)?;
        Ok(())
    }

    #[test]
    fn outputs_png() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempdir()?;
        let input = "images/image1-original.png";
        let output = dir.path().join("output.png");
        Command::cargo_bin("pio")?
            .arg(input)
            .arg("-o")
            .arg(&output)
            .assert()
            .success();
        assert_image_similarity(input, output)?;
        Ok(())
    }

    #[test]
    fn does_not_create_empty_output_on_invalid_input() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempdir()?;
        let output = dir.path().join("output.png");
        Command::cargo_bin("pio")?
            .arg("-o")
            .arg(&output)
            .write_stdin("RIFF....WEBP....")
            .assert()
            .failure();
        assert!(std::fs::read(&output).is_err());
        Ok(())
    }

    #[test]
    fn outputs_to_special_files() -> Result<(), Box<dyn std::error::Error>> {
        Command::cargo_bin("pio")?
            .arg("images/image1-original.png")
            .arg("-o")
            .arg("/dev/null")
            .arg("--output-format")
            .arg("jpeg")
            .assert()
            .success();
        Ok(())
    }

    #[test]
    fn uses_420_chroma_subsampling_automatically() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempdir()?;
        let output = dir.path().join("output.jpg");
        Command::cargo_bin("pio")?
            .arg("-o")
            .arg(&output)
            .arg("images/biandintz-eta-zaldiak.png")
            .assert()
            .success();
        assert_jpeg_sampling_factors(output, "2x2,1x1,1x1");
        Ok(())
    }

    #[test]
    fn uses_420_chroma_subsampling_automatically2() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempdir()?;
        let output = dir.path().join("output.jpg");
        Command::cargo_bin("pio")?
            .arg("-o")
            .arg(&output)
            .arg("images/gluhlampe-explodiert.png")
            .assert()
            .success();
        assert_jpeg_sampling_factors(output, "2x2,1x1,1x1");
        Ok(())
    }

    #[test]
    fn uses_444_chroma_subsampling_automatically() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempdir()?;
        let output = dir.path().join("output.jpg");
        Command::cargo_bin("pio")?
            .arg("-o")
            .arg(&output)
            .arg("images/image-subsampling-test.png")
            .assert()
            .success();
        assert_jpeg_sampling_factors(output, "1x1,1x1,1x1");
        Ok(())
    }
}
