#![allow(dead_code)]

use crate::cli::Algorithm;

#[derive(Clone, Copy, Debug)]
pub struct AlgorithmSpec {
    pub adapter: AdapterKind,
    pub built_in: Option<BuiltInAdapter>,
    pub model: Option<ModelAdapter>,
    pub name: &'static str,
    pub process: Option<ProcessAdapter>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdapterKind {
    BuiltIn,
    Model,
    Process,
}

#[derive(Clone, Copy, Debug)]
pub struct BuiltInAdapter {
    pub capabilities: AdapterCapabilities,
    pub implementation: BuiltInImplementation,
}

#[derive(Clone, Copy, Debug)]
pub enum BuiltInImplementation {
    RustXz,
}

#[derive(Clone, Copy, Debug)]
pub struct AdapterCapabilities {
    pub decode: bool,
    pub encode: bool,
    pub list: bool,
    pub test: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct ProcessAdapter {
    pub commands: ProcessCommandTemplates,
    pub executable: &'static str,
    pub version_args: &'static [&'static str],
}

#[derive(Clone, Copy, Debug)]
pub struct ProcessCommandTemplates {
    pub decode: Option<CommandTemplate>,
    pub encode: Option<CommandTemplate>,
    pub list: Option<CommandTemplate>,
    pub test: Option<CommandTemplate>,
}

#[derive(Clone, Copy, Debug)]
pub struct CommandTemplate {
    pub args: &'static [&'static str],
    pub stdout: CommandOutput,
}

#[derive(Clone, Copy, Debug)]
pub enum CommandOutput {
    File,
    Stdout,
}

#[derive(Clone, Copy, Debug)]
pub struct ModelAdapter {
    pub capabilities: AdapterCapabilities,
    pub model_format: &'static str,
    pub runtime: &'static str,
}

pub fn lookup(algorithm: Algorithm) -> &'static AlgorithmSpec {
    match algorithm {
        Algorithm::Lzma2 | Algorithm::Xz => &XZ_LZMA2,
    }
}

const XZ_LZMA2: AlgorithmSpec = AlgorithmSpec {
    adapter: AdapterKind::BuiltIn,
    built_in: Some(BuiltInAdapter {
        capabilities: AdapterCapabilities {
            decode: true,
            encode: true,
            list: true,
            test: true,
        },
        implementation: BuiltInImplementation::RustXz,
    }),
    model: None,
    name: "xz",
    process: None,
};

#[allow(dead_code)]
const PROCESS_ADAPTER_RESERVED: AlgorithmSpec = AlgorithmSpec {
    adapter: AdapterKind::Process,
    built_in: None,
    model: None,
    name: "reserved-process-wrapper",
    process: Some(ProcessAdapter {
        commands: ProcessCommandTemplates {
            decode: Some(CommandTemplate {
                args: &["-d", "-c", "{input}"],
                stdout: CommandOutput::Stdout,
            }),
            encode: Some(CommandTemplate {
                args: &["-c", "{input}"],
                stdout: CommandOutput::Stdout,
            }),
            list: Some(CommandTemplate {
                args: &["-l", "{input}"],
                stdout: CommandOutput::Stdout,
            }),
            test: Some(CommandTemplate {
                args: &["-t", "{input}"],
                stdout: CommandOutput::Stdout,
            }),
        },
        executable: "{tool}",
        version_args: &["--version"],
    }),
};

#[allow(dead_code)]
const MODEL_ADAPTER_RESERVED: AlgorithmSpec = AlgorithmSpec {
    adapter: AdapterKind::Model,
    built_in: None,
    model: Some(ModelAdapter {
        capabilities: AdapterCapabilities {
            decode: true,
            encode: true,
            list: false,
            test: true,
        },
        model_format: "reserved",
        runtime: "reserved",
    }),
    name: "reserved-model",
    process: None,
};
