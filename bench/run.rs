use clap::{Parser, ValueEnum};
use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};
use std::collections::HashSet;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const XZ_LEVELS: &[u8] = &[1, 3, 6, 9];
const ZSTD_LEVELS: &[u8] = &[1, 3, 10, 19];
const DEFLATE_LEVELS: &[u8] = &[1, 6, 9];
const BZIP2_LEVELS: &[u8] = &[1, 6, 9];
const LZ4_LEVELS: &[u8] = &[1, 9, 12];
const BROTLI_LEVELS: &[u8] = &[1, 6, 11];

fn main() {
    if let Err(error) = run() {
        eprintln!("bench/run: {error}");
        std::process::exit(1);
    }
}

fn run() -> io::Result<()> {
    let config = Config::parse(env::args_os().skip(1))?;
    let corpus = Corpus::read(config.input)?;
    let git_revision = git_revision();
    let hash_tool = HashTool::detect();
    let physical_cores = physical_core_count();
    let cases = build_cases(&config.compress, &corpus.path, physical_cores);

    if config.mode == OutputMode::Tui {
        render_tui_header(
            &corpus,
            cases.len(),
            physical_cores,
            git_revision.as_deref(),
        );
    }

    let progress = ProgressReporter::new(config.mode, cases.len(), physical_cores);

    let results = run_cases(
        cases,
        corpus.clone(),
        hash_tool,
        git_revision.clone(),
        progress.clone(),
        physical_cores,
    )?;
    progress.finish()?;

    if config.mode == OutputMode::Jsonl {
        for result in &results {
            print_jsonl(result);
        }
    }

    if config.mode == OutputMode::Tui {
        render_tui_summary(&results, &corpus);
    }

    Ok(())
}

fn build_cases(compress: &OsStr, input: &Path, physical_cores: usize) -> Vec<BenchCase> {
    let mut cases = Vec::new();
    let thread_cases = thread_cases(physical_cores);

    for &level in XZ_LEVELS {
        for thread_case in &thread_cases {
            cases.push(BenchCase::compress_xz(compress, input, level, thread_case));
        }
    }

    for &level in BZIP2_LEVELS {
        for thread_case in &thread_cases {
            cases.push(BenchCase::compress_bzip2(
                compress,
                input,
                level,
                thread_case,
            ));
        }
    }

    if command_exists("xz") {
        for &level in XZ_LEVELS {
            for thread_case in &thread_cases {
                cases.push(BenchCase::xz(input, level, thread_case));
            }
        }
    }

    if command_exists("zstd") {
        for &level in ZSTD_LEVELS {
            for thread_case in &thread_cases {
                cases.push(BenchCase::zstd(input, level, thread_case));
            }
        }
    }

    if command_exists("gzip") {
        for &level in DEFLATE_LEVELS {
            cases.push(BenchCase::gzip(input, level));
        }
    }

    if command_exists("pigz") {
        for &level in DEFLATE_LEVELS {
            cases.push(BenchCase::pigz(input, level, physical_cores));
        }
    }

    if command_exists("bzip2") {
        for &level in BZIP2_LEVELS {
            cases.push(BenchCase::bzip2(input, level));
        }
    }

    if command_exists("pbzip2") {
        for &level in BZIP2_LEVELS {
            cases.push(BenchCase::pbzip2(input, level, physical_cores));
        }
    }

    if command_exists("lz4") {
        for &level in LZ4_LEVELS {
            cases.push(BenchCase::lz4(input, level));
        }
    }

    if command_exists("brotli") {
        for &level in BROTLI_LEVELS {
            cases.push(BenchCase::brotli(input, level));
        }
    }

    if command_exists("7z") {
        for &level in XZ_LEVELS {
            cases.push(BenchCase::seven_zip(input, level, physical_cores));
        }
    }

    cases
}

#[derive(Clone)]
struct ThreadCase {
    arg: String,
    cores: usize,
    label: String,
}

fn thread_cases(physical_cores: usize) -> Vec<ThreadCase> {
    let mut cases = Vec::new();
    for cores in [1usize, 2, 4, 8, 16] {
        if cores <= physical_cores {
            cases.push(ThreadCase {
                arg: format!("-T{cores}"),
                cores,
                label: format!("t{cores}"),
            });
        }
    }

    cases.push(ThreadCase {
        arg: format!("-T{}", physical_cores.max(1)),
        cores: physical_cores.max(1),
        label: "t0".to_string(),
    });

    cases
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum OutputMode {
    Jsonl,
    Tui,
}

#[derive(Parser)]
#[command(name = "bench/run")]
#[command(about = "Run the compress benchmark matrix")]
struct Cli {
    #[arg(long, conflicts_with = "tui")]
    jsonl: bool,
    #[arg(long, conflicts_with = "jsonl")]
    tui: bool,
    #[arg(long, value_enum, conflicts_with_all = ["jsonl", "tui"])]
    mode: Option<OutputMode>,
    compress: OsString,
    input: PathBuf,
}

struct Config {
    compress: OsString,
    input: PathBuf,
    mode: OutputMode,
}

impl Config {
    fn parse(arguments: impl Iterator<Item = OsString>) -> io::Result<Config> {
        let args = std::iter::once(OsString::from("bench/run")).chain(arguments);
        let cli = Cli::parse_from(args);
        let mode = cli.mode.unwrap_or(if cli.tui {
            OutputMode::Tui
        } else {
            OutputMode::Jsonl
        });

        Ok(Config {
            compress: cli.compress,
            input: cli.input,
            mode,
        })
    }
}

fn run_cases(
    cases: Vec<BenchCase>,
    corpus: Corpus,
    hash_tool: Option<HashTool>,
    git_revision: Option<String>,
    progress: ProgressReporter,
    physical_cores: usize,
) -> io::Result<Vec<BenchResult>> {
    let total = cases.len();
    let mut pending: Vec<(usize, BenchCase)> = cases.into_iter().enumerate().collect();
    let mut running = Vec::<RunningCase>::new();
    let mut results: Vec<Option<BenchResult>> = (0..total).map(|_| None).collect();

    while !pending.is_empty() || !running.is_empty() {
        let mut launched = false;

        loop {
            let used_cores = running.iter().map(|case| case.core_cost).sum::<usize>();
            let free_cores = physical_cores.saturating_sub(used_cores);
            let selected = pending
                .iter()
                .position(|(_, case)| case.core_cost <= free_cores || running.is_empty());

            let Some(position) = selected else {
                break;
            };

            let (index, case) = pending.remove(position);
            let core_cost = case.core_cost;
            let case_corpus = corpus.clone();
            let case_hash_tool = hash_tool;
            let case_git_revision = git_revision.clone();
            let case_progress = progress.clone();

            let handle = thread::spawn(move || {
                let result = run_case(
                    &case,
                    &case_corpus,
                    case_hash_tool.as_ref(),
                    case_git_revision.as_deref(),
                    &case_progress,
                    index + 1,
                    total,
                )?;
                Ok::<_, io::Error>((index, result))
            });

            running.push(RunningCase { core_cost, handle });
            launched = true;
        }

        let mut index = 0usize;
        while index < running.len() {
            if running[index].handle.is_finished() {
                let running_case = running.remove(index);
                let (result_index, result) = running_case
                    .handle
                    .join()
                    .map_err(|_| io::Error::other("benchmark worker panicked"))??;
                results[result_index] = Some(result);
            } else {
                index += 1;
            }
        }

        if !launched {
            thread::sleep(Duration::from_millis(20));
        }
    }

    results
        .into_iter()
        .map(|result| result.ok_or_else(|| io::Error::other("benchmark result missing")))
        .collect()
}

struct RunningCase {
    core_cost: usize,
    handle: JoinHandle<io::Result<(usize, BenchResult)>>,
}

fn run_case(
    case: &BenchCase,
    corpus: &Corpus,
    hash_tool: Option<&HashTool>,
    git_revision: Option<&str>,
    progress_reporter: &ProgressReporter,
    index: usize,
    total: usize,
) -> io::Result<BenchResult> {
    let compressed_path = temporary_path(&case.name, &case.extension);
    let compression = write_stdout_command(
        &case.compress,
        &compressed_path,
        progress(
            progress_reporter,
            index,
            total,
            &case.name,
            "compress",
            Some(corpus.input_bytes),
            case.core_cost,
        ),
    )?;
    let output_bytes = fs::metadata(&compressed_path)?.len();
    let verification = verify_case(
        case,
        corpus,
        &compressed_path,
        hash_tool,
        progress_reporter,
        index,
        total,
    )?;
    let result = BenchResult {
        command: case.compress.display(),
        compression_wall_ms: compression.wall_ms,
        corpus_name: corpus.name.clone(),
        corpus_path: corpus.path.display().to_string(),
        decompressed_sha256: verification.sha256,
        decompression_wall_ms: verification.wall_ms,
        git_revision: git_revision.map(str::to_string),
        input_bytes: corpus.input_bytes,
        level: case.level,
        output_bytes,
        roundtrip_ok: verification.roundtrip_ok,
        threads: case.threads.clone(),
        tool: case.name.clone(),
        tool_version: case.version.clone(),
    };

    let _ = fs::remove_file(compressed_path);
    let _ = fs::remove_file(verification.path);

    Ok(result)
}

fn print_jsonl(result: &BenchResult) {
    println!(
        "{{\"tool\":\"{}\",\"command\":\"{}\",\"tool_version\":{},\"corpus_path\":\"{}\",\
         \"git_revision\":{},\"corpus_name\":\"{}\",\"level\":{},\"threads\":{},\
         \"input_bytes\":{},\"output_bytes\":{},\
         \"compression_wall_ms\":{},\"decompression_wall_ms\":{},\
         \"decompressed_sha256\":{},\"roundtrip_ok\":{}}}",
        escape_json(&result.tool),
        escape_json(&result.command),
        json_string(result.tool_version.as_deref()),
        escape_json(&result.corpus_path),
        json_string(result.git_revision.as_deref()),
        escape_json(&result.corpus_name),
        json_u8(result.level),
        json_string(result.threads.as_deref()),
        result.input_bytes,
        result.output_bytes,
        result.compression_wall_ms,
        result.decompression_wall_ms,
        json_string(result.decompressed_sha256.as_deref()),
        json_bool(result.roundtrip_ok),
    );
}

fn verify_case(
    case: &BenchCase,
    corpus: &Corpus,
    compressed_path: &Path,
    hash_tool: Option<&HashTool>,
    progress_reporter: &ProgressReporter,
    index: usize,
    total: usize,
) -> io::Result<Verification> {
    let output_path = temporary_path(&format!("{}-decoded", case.name), "out");
    let command = case.decompress.with_input(compressed_path);
    let decompression = write_stdout_command(
        &command,
        &output_path,
        progress(
            progress_reporter,
            index,
            total,
            &case.name,
            "verify",
            None,
            case.core_cost,
        ),
    )?;
    let sha256 = hash_tool
        .map(|tool| tool.hash_file(&output_path))
        .transpose()?;
    let roundtrip_ok = match (&corpus.sha256, &sha256) {
        (Some(expected), Some(actual)) => Some(expected == actual),
        _ => None,
    };

    Ok(Verification {
        path: output_path,
        roundtrip_ok,
        sha256,
        wall_ms: decompression.wall_ms,
    })
}

fn write_stdout_command(
    command: &CommandSpec,
    output_path: &Path,
    progress: Option<ProgressLine>,
) -> io::Result<Timing> {
    let output = fs::File::create(output_path)?;
    let start = Instant::now();

    if let Some(input_path) = command.stdin_path.as_ref() {
        return write_stdout_streaming_command(command, input_path, output, start, progress);
    }

    if progress.is_none() {
        let status = Command::new(&command.program)
            .args(&command.args)
            .stdout(Stdio::from(output))
            .stderr(Stdio::null())
            .status()?;
        let wall_ms = start.elapsed().as_millis();

        if !status.success() {
            return Err(io::Error::other(format!("{} failed", command.display())));
        }

        return Ok(Timing { wall_ms });
    }

    let mut child = Command::new(&command.program)
        .args(&command.args)
        .stdout(Stdio::from(output))
        .stderr(Stdio::null())
        .spawn()?;

    loop {
        if let Some(status) = child.try_wait()? {
            let wall_ms = start.elapsed().as_millis();

            if let Some(progress) = progress.as_ref() {
                progress.finish(wall_ms, None, None)?;
            }

            if !status.success() {
                return Err(io::Error::other(format!("{} failed", command.display())));
            }

            return Ok(Timing { wall_ms });
        }

        if let Some(progress) = progress.as_ref() {
            progress.tick(start.elapsed().as_millis(), None, None)?;
        }

        std::thread::sleep(Duration::from_millis(20));
    }
}

fn write_stdout_streaming_command(
    command: &CommandSpec,
    input_path: &Path,
    output: fs::File,
    start: Instant,
    progress: Option<ProgressLine>,
) -> io::Result<Timing> {
    let total_bytes = fs::metadata(input_path).map(|metadata| metadata.len()).ok();
    let mut input = fs::File::open(input_path)?;
    let mut child = Command::new(&command.program)
        .args(&command.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::from(output))
        .stderr(Stdio::null())
        .spawn()?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("failed to open compressor stdin"))?;
    let mut buffer = vec![0u8; 1024 * 1024];
    let mut fed_bytes = 0u64;
    let mut last_tick = Instant::now();

    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }

        if let Err(error) = stdin.write_all(&buffer[..read]) {
            drop(stdin);
            let status = child.wait()?;
            if !status.success() {
                return Err(io::Error::other(format!("{} failed", command.display())));
            }

            return Err(error);
        }

        fed_bytes += read as u64;
        if let Some(progress) = progress.as_ref() {
            if last_tick.elapsed() >= Duration::from_millis(33) {
                progress.tick(start.elapsed().as_millis(), Some(fed_bytes), total_bytes)?;
                last_tick = Instant::now();
            }
        }
    }

    drop(stdin);

    loop {
        if let Some(status) = child.try_wait()? {
            let wall_ms = start.elapsed().as_millis();

            if let Some(progress) = progress.as_ref() {
                progress.finish(wall_ms, Some(fed_bytes), total_bytes)?;
            }

            if !status.success() {
                return Err(io::Error::other(format!("{} failed", command.display())));
            }

            return Ok(Timing { wall_ms });
        }

        if let Some(progress) = progress.as_ref() {
            progress.tick(start.elapsed().as_millis(), Some(fed_bytes), total_bytes)?;
        }

        std::thread::sleep(Duration::from_millis(20));
    }
}

fn progress(
    reporter: &ProgressReporter,
    index: usize,
    total: usize,
    tool: &str,
    phase: &'static str,
    total_bytes: Option<u64>,
    core_cost: usize,
) -> Option<ProgressLine> {
    reporter.line(index, total, tool, phase, total_bytes, core_cost)
}

#[derive(Clone)]
enum ProgressReporter {
    Silent,
    Tui(Arc<IndicatifProgress>),
}

impl ProgressReporter {
    fn new(mode: OutputMode, total: usize, physical_cores: usize) -> ProgressReporter {
        match mode {
            OutputMode::Jsonl => ProgressReporter::Silent,
            OutputMode::Tui => {
                ProgressReporter::Tui(Arc::new(IndicatifProgress::new(total, physical_cores)))
            }
        }
    }

    fn line(
        &self,
        index: usize,
        total: usize,
        tool: &str,
        phase: &'static str,
        total_bytes: Option<u64>,
        core_cost: usize,
    ) -> Option<ProgressLine> {
        match self {
            ProgressReporter::Silent => None,
            ProgressReporter::Tui(progress) => Some(ProgressLine {
                bar: progress.add_bar(index, total, tool, phase, total_bytes, core_cost),
                core_cost,
                phase,
                progress: Arc::clone(progress),
                total_bytes,
            }),
        }
    }

    fn finish(&self) -> io::Result<()> {
        match self {
            ProgressReporter::Silent => Ok(()),
            ProgressReporter::Tui(progress) => progress.finish_display(),
        }
    }
}

struct ProgressLine {
    bar: ProgressBar,
    core_cost: usize,
    phase: &'static str,
    progress: Arc<IndicatifProgress>,
    total_bytes: Option<u64>,
}

impl ProgressLine {
    fn tick(
        &self,
        wall_ms: u128,
        bytes_done: Option<u64>,
        observed_total: Option<u64>,
    ) -> io::Result<()> {
        self.progress.tick_bar(
            &self.bar,
            wall_ms,
            bytes_done,
            observed_total.or(self.total_bytes),
        );
        Ok(())
    }

    fn finish(
        &self,
        wall_ms: u128,
        bytes_done: Option<u64>,
        observed_total: Option<u64>,
    ) -> io::Result<()> {
        self.progress.finish_bar(
            &self.bar,
            self.phase,
            self.core_cost,
            wall_ms,
            bytes_done,
            observed_total.or(self.total_bytes),
        );
        Ok(())
    }
}

struct IndicatifProgress {
    multi: MultiProgress,
    state: Mutex<ProgressState>,
    status: ProgressBar,
}

struct ProgressState {
    active_cores: usize,
    active_jobs: usize,
    completed_cases: usize,
    physical_cores: usize,
    total_cases: usize,
}

impl IndicatifProgress {
    fn new(total_cases: usize, physical_cores: usize) -> IndicatifProgress {
        let multi = MultiProgress::with_draw_target(ProgressDrawTarget::stdout_with_hz(12));
        let status = multi.add(ProgressBar::new_spinner());
        status.set_style(
            ProgressStyle::with_template("{spinner:.cyan} {msg}")
                .expect("status progress template is valid"),
        );
        status.enable_steady_tick(Duration::from_millis(120));

        let progress = IndicatifProgress {
            multi,
            state: Mutex::new(ProgressState {
                active_cores: 0,
                active_jobs: 0,
                completed_cases: 0,
                physical_cores,
                total_cases,
            }),
            status,
        };
        progress.refresh_status();
        progress
    }

    fn add_bar(
        &self,
        index: usize,
        total: usize,
        tool: &str,
        phase: &'static str,
        total_bytes: Option<u64>,
        core_cost: usize,
    ) -> ProgressBar {
        let bar = if let Some(total_bytes) = total_bytes {
            let bar = self.multi.add(ProgressBar::new(total_bytes));
            bar.set_style(progress_bar_style());
            bar
        } else {
            let bar = self.multi.add(ProgressBar::new_spinner());
            bar.set_style(spinner_style());
            bar.enable_steady_tick(Duration::from_millis(120));
            bar
        };
        bar.set_message(progress_message(index, total, tool, phase, core_cost));

        self.register_job(core_cost);

        bar
    }

    fn tick_bar(
        &self,
        bar: &ProgressBar,
        _wall_ms: u128,
        bytes_done: Option<u64>,
        total_bytes: Option<u64>,
    ) {
        if let Some(bytes_done) = bytes_done {
            bar.set_position(bytes_done);
        } else {
            bar.tick();
        }

        if let Some(total_bytes) = total_bytes {
            let percent = progress_percent(bytes_done.unwrap_or(0), Some(total_bytes));
            bar.set_prefix(format!("{percent:>5.1}%"));
        }
    }

    fn finish_bar(
        &self,
        bar: &ProgressBar,
        phase: &'static str,
        core_cost: usize,
        wall_ms: u128,
        bytes_done: Option<u64>,
        total_bytes: Option<u64>,
    ) {
        self.tick_bar(bar, wall_ms, bytes_done, total_bytes);
        bar.finish_and_clear();
        self.complete_job(phase, core_cost);
    }

    fn finish_display(&self) -> io::Result<()> {
        self.status.finish_and_clear();
        Ok(())
    }

    fn register_job(&self, core_cost: usize) {
        if let Ok(mut state) = self.state.lock() {
            state.active_jobs += 1;
            state.active_cores += core_cost;
            refresh_status_bar(&self.status, &state);
        }
    }

    fn complete_job(&self, phase: &'static str, core_cost: usize) {
        if let Ok(mut state) = self.state.lock() {
            state.active_jobs = state.active_jobs.saturating_sub(1);
            state.active_cores = state.active_cores.saturating_sub(core_cost);
            if phase == "verify" {
                state.completed_cases += 1;
            }
            refresh_status_bar(&self.status, &state);
        }
    }

    fn refresh_status(&self) {
        if let Ok(state) = self.state.lock() {
            refresh_status_bar(&self.status, &state);
        }
    }
}

fn refresh_status_bar(status: &ProgressBar, state: &ProgressState) {
    status.set_message(format!(
        "running {}/{} cases | active {} | cores {}/{} physical",
        state.completed_cases,
        state.total_cases,
        state.active_jobs,
        state.active_cores,
        state.physical_cores,
    ));
}

fn progress_bar_style() -> ProgressStyle {
    ProgressStyle::with_template(
        "{wide_msg:.dim} {prefix:.cyan} {bar:24.cyan/blue} {bytes:>10}/{total_bytes:<10} {bytes_per_sec:>12} {elapsed_precise}",
    )
    .expect("progress template is valid")
    .progress_chars("##.")
}

fn spinner_style() -> ProgressStyle {
    ProgressStyle::with_template("{spinner:.cyan} {wide_msg:.dim} {elapsed_precise}")
        .expect("spinner template is valid")
}

fn progress_message(
    index: usize,
    total: usize,
    tool: &str,
    phase: &'static str,
    core_cost: usize,
) -> String {
    format!("[{index}/{total}] {tool:<24} {phase:<9} cores {core_cost:>2}")
}

struct BenchResult {
    command: String,
    compression_wall_ms: u128,
    corpus_name: String,
    corpus_path: String,
    decompressed_sha256: Option<String>,
    decompression_wall_ms: u128,
    git_revision: Option<String>,
    input_bytes: u64,
    level: Option<u8>,
    output_bytes: u64,
    roundtrip_ok: Option<bool>,
    threads: Option<String>,
    tool: String,
    tool_version: Option<String>,
}

fn render_tui_header(
    corpus: &Corpus,
    cases: usize,
    physical_cores: usize,
    git_revision: Option<&str>,
) {
    println!("\x1b[1;35mcompress bench\x1b[0m");
    println!("corpus    {}", corpus.path.display());
    println!("input     {}", human_bytes(corpus.input_bytes));
    println!("cases     {cases}");
    println!("cores     {physical_cores} physical");
    println!("git       {}", git_revision.unwrap_or("unknown"));
    println!();
}

fn render_tui_summary(results: &[BenchResult], corpus: &Corpus) {
    let mut ordered: Vec<&BenchResult> = results.iter().collect();
    ordered.sort_by_key(|result| result.output_bytes);
    let best_size = ordered
        .first()
        .map(|result| result.output_bytes)
        .unwrap_or(0);
    let worst_size = ordered
        .last()
        .map(|result| result.output_bytes)
        .unwrap_or(0);

    println!();
    println!("\x1b[1mresults by compressed size\x1b[0m");
    render_results_table(&ordered, corpus, best_size, worst_size);
}

fn render_results_table(
    results: &[&BenchResult],
    corpus: &Corpus,
    best_size: u64,
    worst_size: u64,
) {
    let mut rows = Vec::with_capacity(results.len());

    for (rank, result) in results.iter().enumerate() {
        let ratio = compression_ratio(result.output_bytes, corpus.input_bytes);
        rows.push(vec![
            table_cell((rank + 1).to_string(), Align::Right),
            table_cell(result.tool.clone(), Align::Left),
            table_cell(level_label(result.level), Align::Right),
            table_cell(
                result.threads.clone().unwrap_or_else(|| "-".to_string()),
                Align::Right,
            ),
            table_cell(human_bytes(result.output_bytes), Align::Right),
            table_cell(format!("{ratio:.3}"), Align::Right),
            table_cell(result.compression_wall_ms.to_string(), Align::Right),
            table_cell(
                format!(
                    "{:.1}",
                    throughput_mib_s(corpus.input_bytes, result.compression_wall_ms)
                ),
                Align::Right,
            ),
            table_cell(result.decompression_wall_ms.to_string(), Align::Right),
            table_cell(
                relative_size_bar(result.output_bytes, best_size, worst_size),
                Align::Left,
            ),
            table_cell(
                roundtrip_plain_label(result.roundtrip_ok).to_string(),
                Align::Left,
            ),
        ]);
    }

    render_table(
        &[
            ("Rank", Align::Right),
            ("Tool", Align::Left),
            ("Lvl", Align::Right),
            ("Thr", Align::Right),
            ("Size", Align::Right),
            ("Ratio", Align::Right),
            ("Comp ms", Align::Right),
            ("MiB/s", Align::Right),
            ("Dec ms", Align::Right),
            ("Size bar", Align::Left),
            ("RT", Align::Left),
        ],
        &rows,
    );
}

fn render_table(headers: &[(&str, Align)], rows: &[Vec<TableCell>]) {
    let mut widths: Vec<usize> = headers.iter().map(|(header, _)| header.len()).collect();

    for row in rows {
        for (index, cell) in row.iter().enumerate() {
            widths[index] = widths[index].max(cell.text.len());
        }
    }

    print_table_border(&widths);
    print!("|");
    for (index, (header, align)) in headers.iter().enumerate() {
        print_table_cell(header, widths[index], *align);
    }
    println!();
    print_table_border(&widths);

    for row in rows {
        print!("|");
        for (index, cell) in row.iter().enumerate() {
            print_table_cell(&cell.text, widths[index], cell.align);
        }
        println!();
    }

    print_table_border(&widths);
}

fn print_table_border(widths: &[usize]) {
    print!("+");
    for width in widths {
        print!("{}+", "-".repeat(width + 2));
    }
    println!();
}

fn print_table_cell(text: &str, width: usize, align: Align) {
    let padding = width.saturating_sub(text.len());
    match align {
        Align::Left => print!(" {text}{} |", " ".repeat(padding)),
        Align::Right => print!(" {}{text} |", " ".repeat(padding)),
    }
}

fn table_cell(text: String, align: Align) -> TableCell {
    TableCell { align, text }
}

#[derive(Clone, Copy)]
enum Align {
    Left,
    Right,
}

struct TableCell {
    align: Align,
    text: String,
}

fn compression_ratio(output_bytes: u64, input_bytes: u64) -> f64 {
    if input_bytes == 0 {
        return 0.0;
    }

    output_bytes as f64 / input_bytes as f64
}

fn relative_size_bar(output_bytes: u64, best: u64, worst: u64) -> String {
    let width = 20usize;
    let span = worst.saturating_sub(best);
    let filled = if span == 0 {
        width
    } else {
        let distance = output_bytes.saturating_sub(best) as f64 / span as f64;
        ((distance * width as f64).round() as usize).min(width)
    };
    let mut bar = String::with_capacity(width + 2);

    bar.push('[');
    for index in 0..width {
        if index < filled {
            bar.push('#');
        } else {
            bar.push('.');
        }
    }
    bar.push(']');

    bar
}

fn progress_percent(done: u64, total: Option<u64>) -> f64 {
    let Some(total) = total.filter(|total| *total > 0) else {
        return 0.0;
    };

    (done.min(total) as f64 / total as f64) * 100.0
}

fn level_label(level: Option<u8>) -> String {
    level
        .map(|level| level.to_string())
        .unwrap_or_else(|| "-".to_string())
}

fn throughput_mib_s(bytes: u64, wall_ms: u128) -> f64 {
    if wall_ms == 0 {
        return 0.0;
    }

    bytes as f64 / 1024.0 / 1024.0 / (wall_ms as f64 / 1000.0)
}

fn roundtrip_plain_label(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "ok",
        Some(false) => "fail",
        None => "unknown",
    }
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0usize;

    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }

    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[derive(Clone)]
struct Corpus {
    input_bytes: u64,
    name: String,
    path: PathBuf,
    sha256: Option<String>,
}

impl Corpus {
    fn read(path: PathBuf) -> io::Result<Self> {
        let input_bytes = fs::metadata(&path)?.len();
        let name = path
            .file_name()
            .unwrap_or_else(|| OsStr::new(""))
            .to_string_lossy()
            .into_owned();
        let sha256 = HashTool::detect()
            .map(|tool| tool.hash_file(&path))
            .transpose()?;

        Ok(Self {
            input_bytes,
            name,
            path,
            sha256,
        })
    }
}

#[derive(Clone)]
struct BenchCase {
    compress: CommandSpec,
    core_cost: usize,
    decompress: DecompressSpec,
    extension: &'static str,
    level: Option<u8>,
    name: String,
    threads: Option<String>,
    version: Option<String>,
}

impl BenchCase {
    fn compress_xz(compress: &OsStr, input: &Path, level: u8, thread_case: &ThreadCase) -> Self {
        let level_arg = format!("-{level}");
        Self::new(
            &format!("compress-xz-{level}-{}", thread_case.label),
            stdin_command(compress, &["xz", &level_arg, &thread_case.arg, "-c"], input),
            decompress(command(
                compress,
                &["xz", "-T1", "-dc"],
                Path::new("{input}"),
            )),
            thread_case.cores,
            "xz",
            Some(level),
            Some(thread_case.label.clone()),
            version_line(compress, &["--version"]),
        )
    }

    fn xz(input: &Path, level: u8, thread_case: &ThreadCase) -> Self {
        let level_arg = format!("-{level}");
        Self::new(
            &format!("xz-{level}-{}", thread_case.label),
            stdin_command("xz", &[&level_arg, &thread_case.arg, "-c"], input),
            decompress(command("xz", &["-T1", "-dc"], Path::new("{input}"))),
            thread_case.cores,
            "xz",
            Some(level),
            Some(thread_case.label.clone()),
            version_line("xz", &["--version"]),
        )
    }

    fn compress_bzip2(compress: &OsStr, input: &Path, level: u8, thread_case: &ThreadCase) -> Self {
        let level_arg = format!("-{level}");
        Self::new(
            &format!("compress-bzip2-{level}-{}", thread_case.label),
            stdin_command(
                compress,
                &["bzip2", &level_arg, &thread_case.arg, "-c"],
                input,
            ),
            decompress(command(
                compress,
                &["bzip2", "-T1", "-dc"],
                Path::new("{input}"),
            )),
            thread_case.cores,
            "bz2",
            Some(level),
            Some(thread_case.label.clone()),
            version_line(compress, &["--version"]),
        )
    }

    fn zstd(input: &Path, level: u8, thread_case: &ThreadCase) -> Self {
        let level_arg = format!("-{level}");
        Self::new(
            &format!("zstd-{level}-{}", thread_case.label),
            stdin_command("zstd", &["-q", &level_arg, &thread_case.arg, "-c"], input),
            decompress(command("zstd", &["-q", "-dc"], Path::new("{input}"))),
            thread_case.cores,
            "zst",
            Some(level),
            Some(thread_case.label.clone()),
            version_line("zstd", &["--version"]),
        )
    }

    fn gzip(input: &Path, level: u8) -> Self {
        let level_arg = format!("-{level}");
        Self::new(
            &format!("gzip-{level}"),
            stdin_command("gzip", &[&level_arg, "-c"], input),
            decompress(command("gzip", &["-dc"], Path::new("{input}"))),
            1,
            "gz",
            Some(level),
            Some("t1".to_string()),
            version_line("gzip", &["--version"]),
        )
    }

    fn pigz(input: &Path, level: u8, physical_cores: usize) -> Self {
        let level_arg = format!("-{level}");
        let threads_arg = physical_cores.max(1).to_string();
        Self::new(
            &format!("pigz-{level}"),
            stdin_command("pigz", &["-p", &threads_arg, &level_arg, "-c"], input),
            decompress(command("pigz", &["-dc"], Path::new("{input}"))),
            physical_cores.max(1),
            "gz",
            Some(level),
            Some("auto".to_string()),
            version_line("pigz", &["--version"]),
        )
    }

    fn bzip2(input: &Path, level: u8) -> Self {
        let level_arg = format!("-{level}");
        Self::new(
            &format!("bzip2-{level}"),
            stdin_command("bzip2", &[&level_arg, "-c"], input),
            decompress(command("bzip2", &["-dc"], Path::new("{input}"))),
            1,
            "bz2",
            Some(level),
            Some("t1".to_string()),
            version_line("bzip2", &["--version"]),
        )
    }

    fn pbzip2(input: &Path, level: u8, physical_cores: usize) -> Self {
        let level_arg = format!("-{level}");
        let threads_arg = format!("-p{}", physical_cores.max(1));
        Self::new(
            &format!("pbzip2-{level}"),
            stdin_command("pbzip2", &[&level_arg, &threads_arg, "-c"], input),
            decompress(command("pbzip2", &["-dc"], Path::new("{input}"))),
            physical_cores.max(1),
            "bz2",
            Some(level),
            Some("auto".to_string()),
            version_line("pbzip2", &["--version"]),
        )
    }

    fn lz4(input: &Path, level: u8) -> Self {
        let level_arg = format!("-{level}");
        Self::new(
            &format!("lz4-{level}"),
            stdin_command("lz4", &["-q", &level_arg, "-c"], input),
            decompress(command("lz4", &["-q", "-dc"], Path::new("{input}"))),
            1,
            "lz4",
            Some(level),
            Some("t1".to_string()),
            version_line("lz4", &["--version"]),
        )
    }

    fn brotli(input: &Path, level: u8) -> Self {
        let level_arg = level.to_string();
        Self::new(
            &format!("brotli-{level}"),
            stdin_command("brotli", &["-q", &level_arg, "-c"], input),
            decompress(command("brotli", &["-d", "-c"], Path::new("{input}"))),
            1,
            "br",
            Some(level),
            Some("t1".to_string()),
            version_line("brotli", &["--version"]),
        )
    }

    fn seven_zip(input: &Path, level: u8, physical_cores: usize) -> Self {
        let level_arg = format!("-mx={level}");
        let threads_arg = format!("-mmt={}", physical_cores.max(1));

        Self::new(
            &format!("7z-{level}"),
            stdin_command(
                "7z",
                &[
                    "a",
                    "-txz",
                    "-bd",
                    "-bb0",
                    &level_arg,
                    &threads_arg,
                    "-si",
                    "-so",
                    "bench",
                    "-y",
                ],
                input,
            ),
            decompress(command(
                "7z",
                &["x", "-bd", "-bb0", "-so"],
                Path::new("{input}"),
            )),
            physical_cores.max(1),
            "xz",
            Some(level),
            Some("auto".to_string()),
            version_line("7z", &["i"]),
        )
    }

    fn new(
        name: &str,
        compress: CommandSpec,
        decompress: DecompressSpec,
        core_cost: usize,
        extension: &'static str,
        level: Option<u8>,
        threads: Option<String>,
        version: Option<String>,
    ) -> Self {
        Self {
            compress,
            core_cost,
            decompress,
            extension,
            level,
            name: name.to_string(),
            threads,
            version,
        }
    }
}

#[derive(Clone)]
struct CommandSpec {
    args: Vec<OsString>,
    program: OsString,
    stdin_path: Option<PathBuf>,
}

impl CommandSpec {
    fn display(&self) -> String {
        let mut parts = vec![self.program.to_string_lossy().into_owned()];
        parts.extend(
            self.args
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned()),
        );
        if let Some(stdin_path) = self.stdin_path.as_ref() {
            parts.push("<".to_string());
            parts.push(stdin_path.display().to_string());
        }
        parts.join(" ")
    }
}

#[derive(Clone)]
struct DecompressSpec {
    command: CommandSpec,
}

impl DecompressSpec {
    fn with_input(&self, path: &Path) -> CommandSpec {
        let mut command = self.command.clone();
        command.args = command
            .args
            .iter()
            .map(|arg| replace_input_placeholder(arg, path))
            .collect();
        command.stdin_path = command
            .stdin_path
            .as_ref()
            .map(|arg| replace_input_path_placeholder(arg, path));
        command
    }
}

struct Timing {
    wall_ms: u128,
}

struct Verification {
    path: PathBuf,
    roundtrip_ok: Option<bool>,
    sha256: Option<String>,
    wall_ms: u128,
}

#[derive(Clone, Copy)]
enum HashTool {
    Openssl,
    Shasum,
    Sha256sum,
}

impl HashTool {
    fn detect() -> Option<Self> {
        if command_exists("sha256sum") {
            Some(Self::Sha256sum)
        } else if command_exists("shasum") {
            Some(Self::Shasum)
        } else if command_exists("openssl") {
            Some(Self::Openssl)
        } else {
            None
        }
    }

    fn hash_file(&self, path: &Path) -> io::Result<String> {
        let output = match self {
            Self::Openssl => Command::new("openssl")
                .args(["dgst", "-sha256"])
                .arg(path)
                .output()?,
            Self::Shasum => Command::new("shasum")
                .args(["-a", "256"])
                .arg(path)
                .output()?,
            Self::Sha256sum => Command::new("sha256sum").arg(path).output()?,
        };

        if !output.status.success() {
            return Err(io::Error::other("sha256 command failed"));
        }

        parse_sha256(&String::from_utf8_lossy(&output.stdout))
    }
}

fn command(program: impl AsRef<OsStr>, args: &[&str], input: &Path) -> CommandSpec {
    let mut all_args: Vec<OsString> = args.iter().map(OsString::from).collect();
    all_args.push(input.as_os_str().to_os_string());

    CommandSpec {
        args: all_args,
        program: program.as_ref().to_os_string(),
        stdin_path: None,
    }
}

fn stdin_command(program: impl AsRef<OsStr>, args: &[&str], input: &Path) -> CommandSpec {
    CommandSpec {
        args: args.iter().map(OsString::from).collect(),
        program: program.as_ref().to_os_string(),
        stdin_path: Some(input.to_path_buf()),
    }
}

fn decompress(command: CommandSpec) -> DecompressSpec {
    DecompressSpec { command }
}

fn replace_input_placeholder(argument: &OsStr, path: &Path) -> OsString {
    if argument == OsStr::new("{input}") {
        path.as_os_str().to_os_string()
    } else {
        argument.to_os_string()
    }
}

fn replace_input_path_placeholder(argument: &Path, path: &Path) -> PathBuf {
    if argument.as_os_str() == OsStr::new("{input}") {
        path.to_path_buf()
    } else {
        argument.to_path_buf()
    }
}

fn command_exists(command: &str) -> bool {
    let Some(paths) = env::var_os("PATH") else {
        return false;
    };

    env::split_paths(&paths).any(|path| path.join(command).is_file())
}

fn physical_core_count() -> usize {
    physical_core_count_from_sysfs()
        .or_else(physical_core_count_from_cpuinfo)
        .or_else(|| {
            thread::available_parallelism()
                .ok()
                .map(|count| count.get())
        })
        .unwrap_or(1)
        .max(1)
}

fn physical_core_count_from_sysfs() -> Option<usize> {
    let mut cores = HashSet::new();
    let entries = fs::read_dir("/sys/devices/system/cpu").ok()?;

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(cpu_id) = name.strip_prefix("cpu") else {
            continue;
        };
        if cpu_id.is_empty() || !cpu_id.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }

        let topology = entry.path().join("topology");
        let package = read_trimmed(topology.join("physical_package_id"))?;
        let core = read_trimmed(topology.join("core_id"))?;
        cores.insert((package, core));
    }

    (!cores.is_empty()).then_some(cores.len())
}

fn physical_core_count_from_cpuinfo() -> Option<usize> {
    let cpuinfo = fs::read_to_string("/proc/cpuinfo").ok()?;
    let mut cores = HashSet::new();
    let mut package = String::new();
    let mut core = String::new();

    for line in cpuinfo.lines().chain(std::iter::once("")) {
        let line = line.trim();
        if line.is_empty() {
            if !package.is_empty() && !core.is_empty() {
                cores.insert((package.clone(), core.clone()));
            }
            package.clear();
            core.clear();
            continue;
        }

        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim();
            let value = value.trim();
            if key == "physical id" {
                package = value.to_string();
            } else if key == "core id" {
                core = value.to_string();
            }
        }
    }

    (!cores.is_empty()).then_some(cores.len())
}

fn read_trimmed(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn version_line(program: impl AsRef<OsStr>, args: &[&str]) -> Option<String> {
    let output = Command::new(program.as_ref()).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }

    first_line(&output.stdout).or_else(|| first_line(&output.stderr))
}

fn first_line(output: &[u8]) -> Option<String> {
    String::from_utf8_lossy(output)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

fn git_revision() -> Option<String> {
    version_line("git", &["rev-parse", "--short=12", "HEAD"])
}

fn parse_sha256(output: &str) -> io::Result<String> {
    output
        .split_whitespace()
        .rev()
        .find(|word| word.len() == 64 && word.chars().all(|ch| ch.is_ascii_hexdigit()))
        .map(|word| word.to_ascii_lowercase())
        .ok_or_else(|| io::Error::other("sha256 output did not contain a digest"))
}

fn temporary_path(name: &str, extension: &str) -> PathBuf {
    let mut path = env::temp_dir();
    path.push(format!(
        "compress-bench-{name}-{}.{}",
        std::process::id(),
        extension
    ));

    path
}

fn json_string(value: Option<&str>) -> String {
    value
        .map(|text| format!("\"{}\"", escape_json(text)))
        .unwrap_or_else(|| "null".to_string())
}

fn json_bool(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "true",
        Some(false) => "false",
        None => "null",
    }
}

fn json_u8(value: Option<u8>) -> String {
    value
        .map(|number| number.to_string())
        .unwrap_or_else(|| "null".to_string())
}

fn escape_json(value: &str) -> String {
    let mut escaped = String::new();

    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            control if control.is_control() => {
                escaped.push_str(&format!("\\u{:04x}", control as u32))
            }
            other => escaped.push(other),
        }
    }

    escaped
}
