use crate::algorithms::checks::CheckType;
use crate::error::{Error, Result};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operation {
    Compress,
    Decompress,
    List,
    Test,
}

#[derive(Debug)]
pub struct Options {
    pub algorithm: Algorithm,
    pub block_size: Option<u64>,
    pub check: CheckType,
    pub extreme: bool,
    pub files: Vec<String>,
    pub force: bool,
    pub keep: bool,
    pub level: u8,
    pub operation: Operation,
    pub set_options: Vec<(String, String)>,
    pub stdout: bool,
    pub threads: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Algorithm {
    Lzma2,
    Xz,
}

pub fn parse(arguments: impl Iterator<Item = String>) -> Result<Options> {
    let mut parser = Parser {
        algorithm: Algorithm::Xz,
        arguments: arguments.skip(1).collect(),
        block_size: None,
        check: CheckType::Crc64,
        extreme: false,
        files: Vec::new(),
        force: false,
        index: 0,
        keep: false,
        level: 6,
        operation: Operation::Compress,
        set_options: Vec::new(),
        stdout: false,
        threads: 1,
    };

    parser.parse_arguments()?;

    Ok(Options {
        algorithm: parser.algorithm,
        block_size: parser.block_size,
        check: parser.check,
        extreme: parser.extreme,
        files: parser.files,
        force: parser.force,
        keep: parser.keep,
        level: parser.level,
        operation: parser.operation,
        set_options: parser.set_options,
        stdout: parser.stdout,
        threads: parser.threads,
    })
}

struct Parser {
    algorithm: Algorithm,
    arguments: Vec<String>,
    block_size: Option<u64>,
    check: CheckType,
    extreme: bool,
    files: Vec<String>,
    force: bool,
    index: usize,
    keep: bool,
    level: u8,
    operation: Operation,
    set_options: Vec<(String, String)>,
    stdout: bool,
    threads: u32,
}

impl Parser {
    fn parse_arguments(&mut self) -> Result<()> {
        while self.index < self.arguments.len() {
            let argument = self.arguments[self.index].clone();
            self.index += 1;

            if argument == "xz" {
                self.algorithm = Algorithm::Xz;
            } else if argument == "--" {
                self.files.extend_from_slice(&self.arguments[self.index..]);
                self.index = self.arguments.len();
            } else if argument == "-a" {
                let name = self.take_value("-a")?;
                self.algorithm = parse_algorithm(&name)?;
            } else if let Some(name) = argument.strip_prefix("-a") {
                self.algorithm = parse_algorithm(name)?;
            } else if argument.starts_with("--") {
                self.parse_long_argument(&argument)?;
            } else if argument.starts_with('-') && argument.len() > 1 {
                self.parse_short_argument(&argument)?;
            } else {
                self.files.push(argument);
            }
        }

        if self.algorithm == Algorithm::Lzma2 {
            self.algorithm = Algorithm::Xz;
        }

        Ok(())
    }

    fn parse_long_argument(&mut self, argument: &str) -> Result<()> {
        if argument == "--compress" {
            self.operation = Operation::Compress;
        } else if argument == "--decompress" {
            self.operation = Operation::Decompress;
        } else if argument == "--test" {
            self.operation = Operation::Test;
        } else if argument == "--list" {
            self.operation = Operation::List;
        } else if argument == "--stdout" {
            self.stdout = true;
            self.keep = true;
        } else if argument == "--keep" {
            self.keep = true;
        } else if argument == "--force" {
            self.force = true;
        } else if argument == "--extreme" {
            self.extreme = true;
        } else if let Some(value) = argument.strip_prefix("--threads=") {
            self.threads = parse_u32(value)?;
        } else if argument == "--threads" {
            let value = self.take_value("--threads")?;
            self.threads = parse_u32(&value)?;
        } else if let Some(value) = argument.strip_prefix("--block-size=") {
            self.block_size = Some(parse_size(value)?);
        } else if argument == "--block-size" {
            let value = self.take_value("--block-size")?;
            self.block_size = Some(parse_size(&value)?);
        } else if let Some(value) = argument.strip_prefix("--check=") {
            self.check = CheckType::from_name(value)?;
        } else if argument == "--check" {
            let value = self.take_value("--check")?;
            self.check = CheckType::from_name(&value)?;
        } else if let Some(value) = argument.strip_prefix("--set=") {
            self.set_options.push(parse_key_value(value)?);
        } else if argument == "--set" {
            let value = self.take_value("--set")?;
            self.set_options.push(parse_key_value(&value)?);
        } else if argument == "--help" {
            return Err(Error::Info(help_text().to_string()));
        } else if argument == "--version" {
            return Err(Error::Info("compress 0.1.0".to_string()));
        } else {
            return Err(Error::Usage("unknown long option"));
        }

        Ok(())
    }

    fn parse_short_argument(&mut self, argument: &str) -> Result<()> {
        let mut chars = argument[1..].chars().peekable();

        while let Some(character) = chars.next() {
            match character {
                'z' => self.operation = Operation::Compress,
                'd' => self.operation = Operation::Decompress,
                't' => self.operation = Operation::Test,
                'l' => self.operation = Operation::List,
                'c' => {
                    self.stdout = true;
                    self.keep = true;
                }
                'k' => self.keep = true,
                'f' => self.force = true,
                'e' => self.extreme = true,
                '0'..='9' => self.level = character as u8 - b'0',
                'T' => {
                    let rest: String = chars.collect();
                    let value = if rest.is_empty() {
                        self.take_value("-T")?
                    } else {
                        rest
                    };

                    self.threads = parse_u32(&value)?;
                    break;
                }
                _ => return Err(Error::Usage("unknown short option")),
            }
        }

        Ok(())
    }

    fn take_value(&mut self, option: &'static str) -> Result<String> {
        if self.index < self.arguments.len() {
            let value = self.arguments[self.index].clone();
            self.index += 1;
            Ok(value)
        } else {
            Err(Error::Usage(option))
        }
    }
}

fn parse_algorithm(name: &str) -> Result<Algorithm> {
    match name {
        "lzma2" => Ok(Algorithm::Lzma2),
        "xz" => Ok(Algorithm::Xz),
        _ => Err(Error::Usage("unknown algorithm")),
    }
}

fn parse_key_value(value: &str) -> Result<(String, String)> {
    if let Some((key, value)) = value.split_once('=') {
        if key.is_empty() {
            return Err(Error::Usage("empty --set key"));
        }

        Ok((key.to_string(), value.to_string()))
    } else {
        Err(Error::Usage("--set expects key=value"))
    }
}

fn parse_size(value: &str) -> Result<u64> {
    let (digits, multiplier) = match value.as_bytes().last() {
        Some(b'k') | Some(b'K') => (&value[..value.len() - 1], 1024),
        Some(b'm') | Some(b'M') => (&value[..value.len() - 1], 1024 * 1024),
        Some(b'g') | Some(b'G') => (&value[..value.len() - 1], 1024 * 1024 * 1024),
        _ => (value, 1),
    };

    let number = digits
        .parse::<u64>()
        .map_err(|_| Error::Usage("invalid size"))?;

    number
        .checked_mul(multiplier)
        .ok_or(Error::Usage("size is too large"))
}

fn parse_u32(value: &str) -> Result<u32> {
    value
        .parse::<u32>()
        .map_err(|_| Error::Usage("invalid number"))
}

fn help_text() -> &'static str {
    "usage: compress [-a lzma2] [xz] [options] [file...]\n\
     options:\n\
       -z --compress        compress input\n\
       -d --decompress      decompress input\n\
       -t --test            test compressed input\n\
       -l --list            list stream metadata\n\
       -c --stdout          write to stdout\n\
       -k --keep            keep input files\n\
       -f --force           replace output files\n\
       -0..-9 -e            compression preset and extreme mode\n\
       -T N --threads N     worker count, 0 means available parallelism\n\
       --block-size N       XZ Block size\n\
       --check NAME         none, crc32, crc64, or sha256\n\
       --set KEY=VALUE      dict, lc, lp, pb, check, mode, mf, nice, depth\n\
                            mode is fast, normal, or optimal"
}
