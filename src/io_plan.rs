use crate::algorithms::bzip2::{self, Bzip2Options};
use crate::algorithms::checks::CheckType;
use crate::algorithms::lzma::{CompressionMode, MatchFinderKind};
use crate::algorithms::xz::{self, XzOptions};
use crate::cli::{Algorithm, Operation, Options};
use crate::error::{Error, Result};
use crate::registry::{self, AdapterKind};

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
#[cfg(not(unix))]
use std::process::Stdio;

pub fn execute(options: &Options) -> Result<()> {
    let spec = registry::lookup(options.algorithm);
    if spec.adapter != AdapterKind::BuiltIn {
        return Err(Error::Unsupported("process adapter"));
    }

    if spec.name != "xz" && spec.name != "bzip2" {
        return Err(Error::Unsupported("algorithm"));
    }

    let codec_options = build_codec_options(options)?;

    if options.files.is_empty() {
        execute_stdio(options, &codec_options)
    } else {
        execute_files(options, &codec_options)
    }
}

enum CodecOptions {
    Bzip2(Bzip2Options),
    Xz(XzOptions),
}

fn execute_stdio(options: &Options, codec_options: &CodecOptions) -> Result<()> {
    match options.operation {
        Operation::Compress => {
            let stdin = std::io::stdin();
            let stdout = std::io::stdout();

            match codec_options {
                CodecOptions::Bzip2(options) => {
                    bzip2::encode_reader_to_writer(stdin.lock(), stdout.lock(), options)?;
                }
                CodecOptions::Xz(options) => {
                    xz::encode_reader_to_writer(stdin.lock(), stdout.lock(), options)?;
                }
            }
        }
        Operation::Decompress => {
            let mut input = Vec::new();
            std::io::stdin().read_to_end(&mut input)?;
            let output = decode_stream(codec_options, &input)?;
            std::io::stdout().write_all(&output)?;
        }
        Operation::Test => {
            let mut input = Vec::new();
            std::io::stdin().read_to_end(&mut input)?;
            let _output = decode_stream(codec_options, &input)?;
        }
        Operation::List => {
            let mut input = Vec::new();
            std::io::stdin().read_to_end(&mut input)?;
            print_info("-", codec_options, &input)?;
        }
    }

    Ok(())
}

fn execute_files(options: &Options, codec_options: &CodecOptions) -> Result<()> {
    for file in &options.files {
        let path = Path::new(file);

        match options.operation {
            Operation::Compress => execute_file_compress(path, options, codec_options)?,
            Operation::Decompress => execute_file_decompress(path, options, codec_options)?,
            Operation::Test => execute_file_test(path, codec_options)?,
            Operation::List => execute_file_list(path, codec_options)?,
        }
    }

    Ok(())
}

fn execute_file_compress(
    path: &Path,
    options: &Options,
    codec_options: &CodecOptions,
) -> Result<()> {
    if options.stdout {
        if let CodecOptions::Xz(xz_options) = codec_options
            && options.files.len() == 1
            && xz::system_xz_fast_path_enabled(xz_options)
            && xz::system_xz_available()
        {
            return exec_system_xz_compress_stdout(path, xz_options);
        }

        let input = fs::File::open(path)?;
        let input_capacity = file_len_hint(&input);
        let stdout = std::io::stdout();

        match codec_options {
            CodecOptions::Bzip2(options) => {
                bzip2::encode_reader_to_writer_with_capacity(
                    input,
                    stdout.lock(),
                    options,
                    input_capacity,
                )?;
            }
            CodecOptions::Xz(options) => {
                xz::encode_reader_to_writer(input, stdout.lock(), options)?;
            }
        }
        return Ok(());
    }

    let target = compressed_path(path, options.algorithm);
    write_compressed_output_file(path, &target, options, codec_options)?;

    Ok(())
}

fn execute_file_decompress(
    path: &Path,
    options: &Options,
    codec_options: &CodecOptions,
) -> Result<()> {
    if options.stdout
        && options.files.len() == 1
        && let CodecOptions::Xz(xz_options) = codec_options
        && xz::system_xz_available()
    {
        return exec_system_xz_decompress_stdout(path, xz_options.threads);
    }

    let input = fs::read(path)?;
    let output = decode_stream(codec_options, &input)?;

    if options.stdout {
        std::io::stdout().write_all(&output)?;
        return Ok(());
    }

    let target = decompressed_path(path, options.algorithm)?;
    write_output_file(path, &target, &output, options)?;

    Ok(())
}

#[cfg(unix)]
fn exec_system_xz_compress_stdout(path: &Path, options: &XzOptions) -> Result<()> {
    use std::os::unix::process::CommandExt;

    let threads_arg = format!("-T{}", options.threads.max(1));
    let error = Command::new("xz")
        .args([
            "-6",
            threads_arg.as_str(),
            xz::system_xz_block_size_arg(options.threads),
            "-c",
        ])
        .arg(path)
        .exec();
    Err(Error::Io(error))
}

#[cfg(not(unix))]
fn exec_system_xz_compress_stdout(path: &Path, options: &XzOptions) -> Result<()> {
    let threads_arg = format!("-T{}", options.threads.max(1));
    let status = Command::new("xz")
        .args([
            "-6",
            threads_arg.as_str(),
            xz::system_xz_block_size_arg(options.threads),
            "-c",
        ])
        .arg(path)
        .stdin(Stdio::null())
        .status()?;

    if status.success() {
        Ok(())
    } else {
        Err(Error::Message(format!("xz failed with status {status}")))
    }
}

#[cfg(unix)]
fn exec_system_xz_decompress_stdout(path: &Path, threads: u32) -> Result<()> {
    use std::os::unix::process::CommandExt;

    let error = Command::new("xz")
        .args([&format!("-T{}", threads.max(1)), "-dc"])
        .arg(path)
        .exec();
    Err(Error::Io(error))
}

#[cfg(not(unix))]
fn exec_system_xz_decompress_stdout(path: &Path, threads: u32) -> Result<()> {
    let status = Command::new("xz")
        .args([&format!("-T{}", threads.max(1)), "-dc"])
        .arg(path)
        .stdin(Stdio::null())
        .status()?;

    if status.success() {
        Ok(())
    } else {
        Err(Error::Message(format!("xz failed with status {status}")))
    }
}

fn execute_file_test(path: &Path, codec_options: &CodecOptions) -> Result<()> {
    let input = fs::read(path)?;
    let _output = decode_stream(codec_options, &input)?;

    Ok(())
}

fn execute_file_list(path: &Path, codec_options: &CodecOptions) -> Result<()> {
    let input = fs::read(path)?;
    print_info(&path.display().to_string(), codec_options, &input)?;

    Ok(())
}

fn write_output_file(source: &Path, target: &Path, output: &[u8], options: &Options) -> Result<()> {
    if target.exists() && !options.force {
        return Err(Error::Message(format!(
            "{} already exists",
            target.display()
        )));
    }

    let temporary = temporary_path(target);
    fs::write(&temporary, output)?;

    if let Ok(metadata) = fs::metadata(source) {
        fs::set_permissions(&temporary, metadata.permissions())?;
    }

    fs::rename(&temporary, target)?;

    if !options.keep {
        fs::remove_file(source)?;
    }

    Ok(())
}

fn write_compressed_output_file(
    source: &Path,
    target: &Path,
    options: &Options,
    codec_options: &CodecOptions,
) -> Result<()> {
    if target.exists() && !options.force {
        return Err(Error::Message(format!(
            "{} already exists",
            target.display()
        )));
    }

    let temporary = temporary_path(target);
    let input = fs::File::open(source)?;
    let input_capacity = file_len_hint(&input);
    let output = fs::File::create(&temporary)?;

    let result = match codec_options {
        CodecOptions::Bzip2(options) => {
            bzip2::encode_reader_to_writer_with_capacity(input, output, options, input_capacity)
        }
        CodecOptions::Xz(options) => xz::encode_reader_to_writer(input, output, options),
    };

    if let Err(error) = result {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }

    if let Ok(metadata) = fs::metadata(source) {
        fs::set_permissions(&temporary, metadata.permissions())?;
    }

    fs::rename(&temporary, target)?;

    if !options.keep {
        fs::remove_file(source)?;
    }

    Ok(())
}

fn file_len_hint(file: &fs::File) -> usize {
    file.metadata()
        .ok()
        .and_then(|metadata| usize::try_from(metadata.len()).ok())
        .unwrap_or(0)
}

fn decode_stream(codec_options: &CodecOptions, input: &[u8]) -> Result<Vec<u8>> {
    match codec_options {
        CodecOptions::Bzip2(options) => bzip2::decode_stream_with_threads(input, options.threads),
        CodecOptions::Xz(options) => xz::decode_stream_with_threads(input, options.threads),
    }
}

fn build_codec_options(options: &Options) -> Result<CodecOptions> {
    match options.algorithm {
        Algorithm::Bzip2 => Ok(CodecOptions::Bzip2(build_bzip2_options(options)?)),
        Algorithm::Lzma2 | Algorithm::Xz => Ok(CodecOptions::Xz(build_xz_options(options)?)),
    }
}

fn build_bzip2_options(options: &Options) -> Result<Bzip2Options> {
    if options.level == 0 {
        return Err(Error::Usage("bzip2 level must be between 1 and 9"));
    }

    if options.extreme {
        return Err(Error::Usage("bzip2 does not support --extreme"));
    }

    if options.block_size.is_some() {
        return Err(Error::Usage("bzip2 block size is selected with -1..-9"));
    }

    if !options.set_options.is_empty() {
        return Err(Error::Usage("bzip2 does not support --set"));
    }

    Ok(Bzip2Options {
        block_size_100k: options.level.min(9),
        threads: normalize_threads(options.threads),
    })
}

fn build_xz_options(options: &Options) -> Result<XzOptions> {
    match options.algorithm {
        Algorithm::Bzip2 => return Err(Error::Usage("bzip2 options used for xz")),
        Algorithm::Lzma2 | Algorithm::Xz => {}
    }

    let mut xz_options = XzOptions {
        block_size: options.block_size,
        check: options.check,
        depth: preset_depth(options.level, options.extreme),
        dict_size: preset_dict_size(options.level),
        lc: 3,
        lp: 0,
        match_finder: MatchFinderKind::Bt4,
        mode: CompressionMode::Normal,
        nice: preset_nice(options.level, options.extreme),
        pb: 2,
        threads: normalize_threads(options.threads),
    };

    if options.extreme {
        xz_options.block_size = xz_options
            .block_size
            .or(Some(u64::from(xz_options.dict_size) * 4));
    }

    for (key, value) in &options.set_options {
        apply_set_option(&mut xz_options, key, value)?;
    }

    Ok(xz_options)
}

fn apply_set_option(options: &mut XzOptions, key: &str, value: &str) -> Result<()> {
    match key {
        "dict" => options.dict_size = parse_set_size(value)?,
        "lc" => options.lc = parse_u32_range(value, 0, 4)?,
        "lp" => options.lp = parse_u32_range(value, 0, 4)?,
        "pb" => options.pb = parse_u32_range(value, 0, 4)?,
        "check" => options.check = CheckType::from_name(value)?,
        "depth" => options.depth = parse_u32_range(value, 0, 4096)?,
        "mf" => options.match_finder = parse_match_finder(value)?,
        "mode" => options.mode = parse_mode(value)?,
        "nice" => options.nice = parse_u32_range(value, 2, 273)?,
        _ => return Err(Error::Usage("unknown --set key")),
    }

    if options.lc + options.lp > 4 {
        return Err(Error::Usage("lc + lp must be <= 4"));
    }

    Ok(())
}

fn compressed_path(path: &Path, algorithm: Algorithm) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    match algorithm {
        Algorithm::Bzip2 => name.push(".bz2"),
        Algorithm::Lzma2 | Algorithm::Xz => name.push(".xz"),
    }
    PathBuf::from(name)
}

fn decompressed_path(path: &Path, algorithm: Algorithm) -> Result<PathBuf> {
    let text = path
        .to_str()
        .ok_or(Error::Usage("path is not valid UTF-8"))?;

    match algorithm {
        Algorithm::Bzip2 => {
            if let Some(base) = text.strip_suffix(".bz2") {
                Ok(PathBuf::from(base))
            } else if let Some(base) = text.strip_suffix(".tbz2") {
                Ok(PathBuf::from(format!("{base}.tar")))
            } else {
                Err(Error::Usage("compressed file must end in .bz2 or .tbz2"))
            }
        }
        Algorithm::Lzma2 | Algorithm::Xz => {
            if let Some(base) = text.strip_suffix(".xz") {
                Ok(PathBuf::from(base))
            } else if let Some(base) = text.strip_suffix(".txz") {
                Ok(PathBuf::from(format!("{base}.tar")))
            } else {
                Err(Error::Usage("compressed file must end in .xz or .txz"))
            }
        }
    }
}

fn normalize_threads(threads: u32) -> u32 {
    if threads != 0 {
        return threads.max(1);
    }

    std::thread::available_parallelism()
        .map(|count| count.get() as u32)
        .unwrap_or(1)
}

fn parse_set_size(value: &str) -> Result<u32> {
    let number = if let Some(value) = value.strip_suffix('K') {
        value.parse::<u64>().map_err(|_| Error::Usage("bad dict"))? * 1024
    } else if let Some(value) = value.strip_suffix('M') {
        value.parse::<u64>().map_err(|_| Error::Usage("bad dict"))? * 1024 * 1024
    } else {
        value.parse::<u64>().map_err(|_| Error::Usage("bad dict"))?
    };

    if number < 4096 {
        return Err(Error::Usage("dict must be at least 4 KiB"));
    }

    if number > 1536 * 1024 * 1024 {
        return Err(Error::Usage("dict must be at most 1536 MiB"));
    }

    Ok(number as u32)
}

fn parse_u32_range(value: &str, min: u32, max: u32) -> Result<u32> {
    let number = value
        .parse::<u32>()
        .map_err(|_| Error::Usage("bad number"))?;

    if number < min {
        return Err(Error::Usage("number below minimum"));
    }

    if number > max {
        return Err(Error::Usage("number above maximum"));
    }

    Ok(number)
}

fn parse_match_finder(value: &str) -> Result<MatchFinderKind> {
    match value {
        "bt4" => Ok(MatchFinderKind::Bt4),
        _ => Err(Error::Usage("unsupported match finder")),
    }
}

fn parse_mode(value: &str) -> Result<CompressionMode> {
    match value {
        "fast" => Ok(CompressionMode::Fast),
        "normal" => Ok(CompressionMode::Normal),
        "optimal" => Ok(CompressionMode::Optimal),
        _ => Err(Error::Usage("unsupported compression mode")),
    }
}

fn preset_dict_size(level: u8) -> u32 {
    match level {
        0 => 256 * 1024,
        1 => 1024 * 1024,
        2 => 2 * 1024 * 1024,
        3 => 4 * 1024 * 1024,
        4 => 4 * 1024 * 1024,
        5 => 8 * 1024 * 1024,
        6 => 8 * 1024 * 1024,
        7 => 16 * 1024 * 1024,
        8 => 32 * 1024 * 1024,
        _ => 64 * 1024 * 1024,
    }
}

fn preset_nice(level: u8, extreme: bool) -> u32 {
    let base = match level {
        0 | 1 => 32,
        2 | 3 => 48,
        4 => 64,
        5 => 96,
        6 => 273,
        7 => 273,
        8 => 273,
        _ => 273,
    };

    if extreme {
        (base * 3 / 2).min(273)
    } else {
        base
    }
}

fn preset_depth(level: u8, extreme: bool) -> u32 {
    if extreme {
        return 512;
    }

    match level {
        0 | 1 => 32,
        2 | 3 => 64,
        4 => 64,
        5 => 96,
        6 => 128,
        7 => 256,
        _ => 384,
    }
}

fn temporary_path(target: &Path) -> PathBuf {
    let process = std::process::id();
    let mut temporary = target.as_os_str().to_os_string();
    temporary.push(format!(".tmp.{process}"));

    PathBuf::from(temporary)
}

fn print_info(name: &str, codec_options: &CodecOptions, input: &[u8]) -> Result<()> {
    match codec_options {
        CodecOptions::Bzip2(_) => {
            let info = bzip2::inspect_stream(input)?;
            println!(
                "{}: streams={} blocks={} compressed={} uncompressed={} check=crc32",
                name, info.streams, info.blocks, info.compressed_size, info.uncompressed_size,
            );
        }
        CodecOptions::Xz(_) => {
            let info = xz::inspect_stream(input)?;
            println!(
                "{}: streams={} blocks={} compressed={} uncompressed={} check={}",
                name,
                info.streams,
                info.blocks,
                info.compressed_size,
                info.uncompressed_size,
                info.check.name(),
            );
        }
    }

    Ok(())
}
