// SPDX-FileCopyrightText: 2019-2020 Tuomas Siipola
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::fs::File;
use std::io::Write;
use std::mem::ManuallyDrop;
use std::path::{Path, PathBuf};

pub enum Output {
    /// Write to standard output or special file (e.g. /dev/null)
    Stream(Box<dyn Write>),

    /// Write to regular file
    WriteFile {
        path: PathBuf,
        file: ManuallyDrop<File>,
        dir: File,
        finished: bool,
    },

    /// Overwrite file atomically
    OverwriteFile {
        dst_path: PathBuf,
        tmp_path: PathBuf,
        dst_dir: File,
        tmp_file: ManuallyDrop<File>,
        tmp_file_closed: bool,
        finished: bool,
    },
}

fn random_file(root: impl AsRef<Path>) -> std::io::Result<(PathBuf, File)> {
    use rand::distributions::Alphanumeric;
    use rand::{thread_rng, Rng};
    use std::fs::OpenOptions;

    loop {
        let path = root.as_ref().with_file_name(format!(
            ".pio-{}.tmp",
            thread_rng()
                .sample_iter(&Alphanumeric)
                .take(16)
                .map(char::from)
                .collect::<String>()
        ));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => break Ok((path, file)),
            Err(err) => match err.kind() {
                std::io::ErrorKind::AlreadyExists => continue,
                _ => break Err(err),
            },
        };
    }
}

fn file_directory(path: impl AsRef<Path>) -> PathBuf {
    match path.as_ref().parent() {
        Some(parent) => {
            if parent.as_os_str().is_empty() {
                PathBuf::from(".")
            } else {
                parent.to_path_buf()
            }
        }
        None => PathBuf::from("."),
    }
}

impl Output {
    pub fn write_file(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let path = path.as_ref();
        let file = File::create(path)?;
        if file.metadata()?.is_file() {
            Ok(Self::WriteFile {
                path: path.to_path_buf(),
                file: ManuallyDrop::new(file),
                dir: File::open(file_directory(path))?,
                finished: false,
            })
        } else {
            Ok(Self::Stream(Box::new(file)))
        }
    }

    pub fn overwrite_file(path: impl AsRef<Path>) -> Result<Self, Box<dyn std::error::Error>> {
        if !std::fs::metadata(&path)?.is_file() {
            return Err("expected regular file".into());
        }
        let path = path.as_ref();
        let (tmp_path, tmp_file) = random_file(path)?;
        let dst_dir = File::open(file_directory(path))?;
        Ok(Self::OverwriteFile {
            dst_path: path.to_path_buf(),
            tmp_path,
            tmp_file: ManuallyDrop::new(tmp_file),
            tmp_file_closed: false,
            dst_dir,
            finished: false,
        })
    }

    pub fn stdout() -> Self {
        Self::Stream(Box::new(std::io::stdout()))
    }

    pub fn write(mut self, buf: &[u8]) -> std::io::Result<()> {
        match self {
            Output::Stream(ref mut write) => {
                write.write_all(buf)?;
                write.flush()?;
            }
            Output::WriteFile {
                ref mut file,
                ref mut finished,
                ref mut dir,
                ..
            } => {
                file.write_all(buf)?;
                file.sync_all()?;
                dir.sync_all()?;
                *finished = true;
            }
            Output::OverwriteFile {
                ref dst_path,
                ref tmp_path,
                ref mut tmp_file,
                ref mut dst_dir,
                ref mut finished,
                ref mut tmp_file_closed,
            } => {
                tmp_file.write_all(buf)?;
                tmp_file.sync_all()?;
                unsafe { ManuallyDrop::drop(tmp_file) }
                *tmp_file_closed = true;
                std::fs::rename(tmp_path, dst_path)?;
                dst_dir.sync_all()?;
                *finished = true;
            }
        };
        Ok(())
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        match self {
            Output::Stream(_) => {}
            Output::WriteFile {
                path,
                file,
                finished,
                ..
            } => {
                unsafe { ManuallyDrop::drop(file) }
                if !*finished {
                    std::fs::remove_file(path).unwrap_or_else(|_err| {});
                }
            }
            Output::OverwriteFile {
                tmp_path,
                tmp_file,
                finished,
                tmp_file_closed,
                ..
            } => {
                if !*tmp_file_closed {
                    unsafe { ManuallyDrop::drop(tmp_file) }
                }
                if !*finished {
                    std::fs::remove_file(tmp_path).unwrap_or_else(|_err| {});
                }
            }
        }
    }
}
