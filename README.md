<!--
SPDX-FileCopyrightText: 2019 Tuomas Siipola
SPDX-License-Identifier: AGPL-3.0-or-later
-->

# Perceptual Image Optimizer

[![Build Status](https://travis-ci.com/siiptuo/pio.svg?branch=master)](https://travis-ci.com/siiptuo/pio)
[![reuse compliant](https://reuse.software/badge/reuse-compliant.svg)](https://reuse.software)

`pio` is a command-line utility to compress image files while maintaining the same perceived quality.
It's designed to automatically optimize images for the web.

## Features

- Optimize images automatically for the web
- Supports PNG, JPEG and WebP
- Easily installable statically linked binary

## Background

Images are an important part of the web but they usually use a lot of bandwidth.
Optimizing images makes them smaller and thus faster to load.

Many image editors and optimization tools give you parameters such as quality.
You could use the same parameters for each image.
This will certainly optimize your images but may not be optimal for all images.
You could also specify parameters by hand for each image but this isn't feasible if there are many images or they are uploaded by end users.

`pio` simplifies image optimization by finding optimal parameters automatically.
This is done by comparing [structural similarity (SSIM)](https://en.wikipedia.org/wiki/Structural_similarity) of the optimized image to the original.

## Usage

Basic usage:

```sh
$ pio input.jpeg --output output.jpeg
```

In order to achieve high-quality output, the input image should preferably be PNG or high-quality JPEG or WebP.

The target quality can be set using `--target` argument:

```
$ pio input.jpeg --target 0.001 --output output.jpeg
```

The target is a SSIM value between 0.0 and infinity where 0.0 means identical images.

For the full list of available options, run `pio --help`.

## Related projects

- [pio-loader](https://github.com/siiptuo/pio-loader): webpack integration

## Alternatives

`pio` is not really doing anything new and there are many similar projects including

- [imager](https://github.com/imager-io/imager)
- [imgmin](https://github.com/rflynn/imgmin)
- [jpeg-archive](https://github.com/danielgtaylor/jpeg-archive/)
- [optimal-image](https://github.com/optimal-image/optimal-image)
- [webp-recompress](https://github.com/AgentCosmic/webp-recompress)

## License

GNU Affero General Public License version 3 or later
