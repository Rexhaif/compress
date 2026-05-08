use crate::algorithms::checks::CheckType;
use crate::algorithms::lzma::{CompressionMode, MatchFinderKind};
use crate::algorithms::xz::{self, XzOptions};
use crate::cli::{Algorithm, Operation, Options};
use crate::error::{Error, Result};
use crate::registry::{self, AdapterKind};

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub fn execute(options: &Options) -> Result<()> {
    let spec = registry::lookup(options.algorithm);
    if spec.adapter != AdapterKind::BuiltIn {
        return Err(Error::Unsupported("process adapter"));
    }

    if spec.name != "xz" {
        return Err(Error::Unsupported("algorithm"));
    }

    let xz_options = build_xz_options(options)?;

    if options.files.is_empty() {
        execute_stdio(options, &xz_options)
    } else {
        execute_files(options, &xz_options)
    }
}

fn execute_stdio(options: &Options, xz_options: &XzOptions) -> Result<()> {
    match options.operation {
        Operation::Compress => {
            let stdin = std::io::stdin();
            let stdout = std::io::stdout();

            xz::encode_reader_to_writer(stdin.lock(), stdout.lock(), xz_options)?;
        }
        Operation::Decompress => {
            let mut input = Vec::new();
            std::io::stdin().read_to_end(&mut input)?;
            let output = xz::decode_stream(&input)?;
            std::io::stdout().write_all(&output)?;
        }
        Operation::Test => {
            let mut input = Vec::new();
            std::io::stdin().read_to_end(&mut input)?;
            let _output = xz::decode_stream(&input)?;
        }
        Operation::List => {
            let mut input = Vec::new();
            std::io::stdin().read_to_end(&mut input)?;
            let info = xz::inspect_stream(&input)?;
            print_info("-", &info);
        }
    }

    Ok(())
}

fn execute_files(options: &Options, xz_options: &XzOptions) -> Result<()> {
    for file in &options.files {
        let path = Path::new(file);

        match options.operation {
            Operation::Compress => execute_file_compress(path, options, xz_options)?,
            Operation::Decompress => execute_file_decompress(path, options)?,
            Operation::Test => execute_file_test(path)?,
            Operation::List => execute_file_list(path)?,
        }
    }

    Ok(())
}

fn execute_file_compress(path: &Path, options: &Options, xz_options: &XzOptions) -> Result<()> {
    if options.stdout {
        let input = fs::File::open(path)?;
        let stdout = std::io::stdout();

        xz::encode_reader_to_writer(input, stdout.lock(), xz_options)?;
        return Ok(());
    }

    let target = compressed_path(path);
    write_compressed_output_file(path, &target, options, xz_options)?;

    Ok(())
}

fn execute_file_decompress(path: &Path, options: &Options) -> Result<()> {
    let input = fs::read(path)?;
    let output = xz::decode_stream(&input)?;

    if options.stdout {
        std::io::stdout().write_all(&output)?;
        return Ok(());
    }

    let target = decompressed_path(path)?;
    write_output_file(path, &target, &output, options)?;

    Ok(())
}

fn execute_file_test(path: &Path) -> Result<()> {
    let input = fs::read(path)?;
    let _output = xz::decode_stream(&input)?;

    Ok(())
}

fn execute_file_list(path: &Path) -> Result<()> {
    let input = fs::read(path)?;
    let info = xz::inspect_stream(&input)?;

    print_info(&path.display().to_string(), &info);

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
    xz_options: &XzOptions,
) -> Result<()> {
    if target.exists() && !options.force {
        return Err(Error::Message(format!(
            "{} already exists",
            target.display()
        )));
    }

    let temporary = temporary_path(target);
    let input = fs::File::open(source)?;
    let output = fs::File::create(&temporary)?;

    if let Err(error) = xz::encode_reader_to_writer(input, output, xz_options) {
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

fn build_xz_options(options: &Options) -> Result<XzOptions> {
    match options.algorithm {
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

fn compressed_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".xz");
    PathBuf::from(name)
}

fn decompressed_path(path: &Path) -> Result<PathBuf> {
    let text = path
        .to_str()
        .ok_or(Error::Usage("path is not valid UTF-8"))?;

    if let Some(base) = text.strip_suffix(".xz") {
        Ok(PathBuf::from(base))
    } else if let Some(base) = text.strip_suffix(".txz") {
        Ok(PathBuf::from(format!("{base}.tar")))
    } else {
        Err(Error::Usage("compressed file must end in .xz or .txz"))
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

fn print_info(name: &str, info: &xz::StreamInfo) {
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
