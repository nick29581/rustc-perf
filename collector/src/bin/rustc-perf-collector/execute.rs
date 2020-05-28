//! Execute benchmarks.

use std::cmp;
use std::env;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{self, Command};
use std::str;

use tempfile::TempDir;

use collector::{
    command_output, BenchmarkName, BenchmarkState, Patch, Run, SelfProfile, StatId, Stats,
};

use anyhow::{bail, Context};

use crate::{BuildKind, Compiler, RunKind};

fn touch_all(path: &Path) -> anyhow::Result<()> {
    let mut cmd = Command::new("bash");
    cmd.current_dir(path)
        .args(&["-c", "find . -name '*.rs' | xargs touch"]);
    command_output(&mut cmd)?;
    // We also delete the cmake caches to avoid errors when moving directories around.
    // This might be a bit slower but at least things build
    let mut cmd = Command::new("bash");
    cmd.current_dir(path)
        .args(&["-c", "find . -name 'CMakeCache.txt' -delete"]);
    command_output(&mut cmd)?;
    Ok(())
}

fn default_runs() -> usize {
    3
}

/// This is the internal representation of an individual benchmark's
/// perf-config.json file.
#[derive(Debug, Clone, serde::Deserialize)]
struct BenchmarkConfig {
    cargo_opts: Option<String>,
    cargo_rustc_opts: Option<String>,
    cargo_toml: Option<String>,
    #[serde(default)]
    disabled: bool,
    #[serde(default = "default_runs")]
    runs: usize,
    #[serde(default)]
    supports_stable: bool,
}

impl Default for BenchmarkConfig {
    fn default() -> BenchmarkConfig {
        BenchmarkConfig {
            cargo_opts: None,
            cargo_rustc_opts: None,
            cargo_toml: None,
            disabled: false,
            runs: default_runs(),
            supports_stable: false,
        }
    }
}

pub struct Benchmark {
    pub name: BenchmarkName,
    pub path: PathBuf,
    patches: Vec<Patch>,
    config: BenchmarkConfig,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Profiler {
    PerfStat,
    PerfStatSelfProfile,
    SelfProfile,
    TimePasses,
    PerfRecord,
    OProfile,
    Cachegrind,
    Callgrind,
    DHAT,
    Massif,
    Eprintln,
    LlvmLines,
}

#[derive(thiserror::Error, PartialEq, Eq, Debug)]
pub enum FromNameError {
    #[error("'perf-stat' cannot be used as the profiler")]
    PerfStat,
    #[error("'{:?}' is not a known profiler", .0)]
    UnknownProfiler(String),
}

impl Profiler {
    pub fn from_name(name: &str) -> Result<Profiler, FromNameError> {
        match name {
            // Even though `PerfStat` is a valid `Profiler` value, "perf-stat"
            // is rejected because it can't be used with the `profiler`
            // subcommand. (It's used with `bench_local` instead.)
            "perf-stat" => Err(FromNameError::PerfStat),
            "self-profile" => Ok(Profiler::SelfProfile),
            "time-passes" => Ok(Profiler::TimePasses),
            "perf-record" => Ok(Profiler::PerfRecord),
            "oprofile" => Ok(Profiler::OProfile),
            "cachegrind" => Ok(Profiler::Cachegrind),
            "callgrind" => Ok(Profiler::Callgrind),
            "dhat" => Ok(Profiler::DHAT),
            "massif" => Ok(Profiler::Massif),
            "eprintln" => Ok(Profiler::Eprintln),
            "llvm-lines" => Ok(Profiler::LlvmLines),
            _ => Err(FromNameError::UnknownProfiler(name.to_string())),
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Profiler::PerfStat => "perf-stat",
            Profiler::PerfStatSelfProfile => "perf-stat-self-profile",
            Profiler::SelfProfile => "self-profile",
            Profiler::TimePasses => "time-passes",
            Profiler::PerfRecord => "perf-record",
            Profiler::OProfile => "oprofile",
            Profiler::Cachegrind => "cachegrind",
            Profiler::Callgrind => "callgrind",
            Profiler::DHAT => "dhat",
            Profiler::Massif => "massif",
            Profiler::Eprintln => "eprintln",
            Profiler::LlvmLines => "llvm-lines",
        }
    }

    // What cargo subcommand do we need to run for this profiler? If not
    // `rustc`, must be a subcommand that itself invokes `rustc`.
    fn subcommand(&self) -> &'static str {
        match self {
            Profiler::PerfStat
            | Profiler::PerfStatSelfProfile
            | Profiler::SelfProfile
            | Profiler::TimePasses
            | Profiler::PerfRecord
            | Profiler::OProfile
            | Profiler::Cachegrind
            | Profiler::Callgrind
            | Profiler::DHAT
            | Profiler::Massif
            | Profiler::Eprintln => "rustc",
            Profiler::LlvmLines => "llvm-lines",
        }
    }

    fn is_build_kind_allowed(&self, build_kind: BuildKind) -> bool {
        match self {
            Profiler::PerfStat
            | Profiler::PerfStatSelfProfile
            | Profiler::SelfProfile
            | Profiler::TimePasses
            | Profiler::PerfRecord
            | Profiler::OProfile
            | Profiler::Cachegrind
            | Profiler::Callgrind
            | Profiler::DHAT
            | Profiler::Massif
            | Profiler::Eprintln => true,
            Profiler::LlvmLines => build_kind != BuildKind::Check,
        }
    }

    fn is_run_kind_allowed(&self, run_kind: RunKind) -> bool {
        match self {
            Profiler::PerfStat
            | Profiler::PerfStatSelfProfile
            | Profiler::SelfProfile
            | Profiler::TimePasses
            | Profiler::PerfRecord
            | Profiler::OProfile
            | Profiler::Cachegrind
            | Profiler::Callgrind
            | Profiler::DHAT
            | Profiler::Massif
            | Profiler::Eprintln => true,
            Profiler::LlvmLines => run_kind == RunKind::Full,
        }
    }
}

struct CargoProcess<'a> {
    compiler: Compiler<'a>,
    cwd: &'a Path,
    build_kind: BuildKind,
    incremental: bool,
    processor_etc: Option<(&'a mut dyn Processor, RunKind, &'a str, Option<&'a Patch>)>,
    processor_name: BenchmarkName,
    manifest_path: String,
    cargo_args: Vec<String>,
    rustc_args: Vec<String>,
}

impl<'a> CargoProcess<'a> {
    fn incremental(mut self, incremental: bool) -> Self {
        self.incremental = incremental;
        self
    }

    fn processor(
        mut self,
        processor: &'a mut dyn Processor,
        run_kind: RunKind,
        run_kind_str: &'a str,
        patch: Option<&'a Patch>,
    ) -> Self {
        self.processor_etc = Some((processor, run_kind, run_kind_str, patch));
        self
    }

    fn base_command(&self, cwd: &Path, subcommand: &str) -> Command {
        let mut cmd = Command::new(Path::new(self.compiler.cargo));
        cmd
            // Not all cargo invocations (e.g. `cargo clean`) need all of these
            // env vars set, but it doesn't hurt to have them.
            .env_clear()
            // SHELL is needed for some benchmarks' build scripts.
            .env("SHELL", env::var_os("SHELL").unwrap_or_default())
            // PATH is needed to find things like linkers used by rustc/Cargo.
            .env("PATH", env::var_os("PATH").unwrap_or_default())
            // HOME is needed for cargo to find its home directory.
            .env("HOME", env::var_os("HOME").unwrap_or_default())
            .env(
                "RUSTC_THREAD_COUNT",
                env::var_os("RUSTC_THREAD_COUNT").unwrap_or_default(),
            )
            .env("RUSTC", &*FAKE_RUSTC)
            .env("RUSTC_REAL", &self.compiler.rustc)
            .env(
                "CARGO_INCREMENTAL",
                &format!("{}", self.incremental as usize),
            )
            .current_dir(cwd)
            .arg(subcommand)
            .arg("--manifest-path")
            .arg(&self.manifest_path);
        cmd
    }

    fn get_pkgid(&self, cwd: &Path) -> String {
        let mut pkgid_cmd = self.base_command(cwd, "pkgid");
        let out = command_output(&mut pkgid_cmd)
            .unwrap_or_else(|e| {
                panic!("failed to obtain pkgid in {:?}: {:?}", cwd, e);
            })
            .stdout;
        let package_id = str::from_utf8(&out).unwrap();
        package_id.trim().to_string()
    }

    fn run_rustc(&mut self) -> anyhow::Result<()> {
        loop {
            // Get the subcommand. If it's not `rustc` it must should be a
            // subcommand that itself invokes `rustc` (so that the `FAKE_RUSTC`
            // machinery works).
            let subcommand = if let Some((ref mut processor, run_kind, ..)) = self.processor_etc {
                let profiler = processor.profiler();
                if !profiler.is_build_kind_allowed(self.build_kind) {
                    return Err(anyhow::anyhow!(
                        "this profiler doesn't support {:?} builds",
                        self.build_kind
                    ));
                }
                if !profiler.is_run_kind_allowed(run_kind) {
                    return Err(anyhow::anyhow!(
                        "this profiler doesn't support {:?} runs",
                        run_kind
                    ));
                }

                profiler.subcommand()
            } else {
                "rustc"
            };

            let mut cmd = self.base_command(self.cwd, subcommand);
            cmd.arg("-p").arg(self.get_pkgid(self.cwd));
            match self.build_kind {
                BuildKind::Check => {
                    cmd.arg("--profile").arg("check");
                }
                BuildKind::Debug => {}
                BuildKind::Opt => {
                    cmd.arg("--release");
                }
            }
            cmd.args(&self.cargo_args);
            cmd.arg("--");
            // --wrap-rustc-with is not a valid rustc flag. But rustc-fake
            // recognizes it, strips it (and its argument) out, and uses it as an
            // indicator that the rustc invocation should be profiled. This works
            // out nicely because `cargo rustc` only passes arguments after '--'
            // onto rustc for the final crate, which is exactly the crate for which
            // we want to wrap rustc.
            if let Some((ref mut processor, ..)) = self.processor_etc {
                cmd.arg("--wrap-rustc-with");
                cmd.arg(processor.profiler().name());
                cmd.args(&self.rustc_args);
            }

            log::debug!("{:?}", cmd);

            touch_all(&self.cwd)?;

            let output = command_output(&mut cmd)?;
            if let Some((ref mut processor, run_kind, run_kind_str, patch)) = self.processor_etc {
                let data = ProcessOutputData {
                    name: self.processor_name,
                    cwd: self.cwd,
                    build_kind: self.build_kind,
                    run_kind,
                    run_kind_str,
                    patch,
                };
                match processor.process_output(&data, output) {
                    Ok(Retry::No) => return Ok(()),
                    Ok(Retry::Yes) => {}
                    Err(e) => return Err(e),
                }
            } else {
                return Ok(());
            }
        }
    }
}

lazy_static::lazy_static! {
    static ref FAKE_RUSTC: PathBuf = {
        let mut fake_rustc = env::current_exe().unwrap();
        fake_rustc.pop();
        fake_rustc.push("rustc-fake");
        fake_rustc
    };
}

/// Used to indicate if we need to retry a run.
pub enum Retry {
    No,
    Yes,
}

pub struct ProcessOutputData<'a> {
    name: BenchmarkName,
    cwd: &'a Path,
    build_kind: BuildKind,
    run_kind: RunKind,
    run_kind_str: &'a str,
    patch: Option<&'a Patch>,
}

/// Trait used by `Benchmark::measure()` to provide different kinds of
/// processing.
pub trait Processor {
    /// The `Profiler` being used.
    fn profiler(&self) -> Profiler;

    /// Process the output produced by the particular `Profiler` being used.
    fn process_output(
        &mut self,
        data: &ProcessOutputData<'_>,
        output: process::Output,
    ) -> anyhow::Result<Retry>;

    /// Provided to permit switching on more expensive profiling if it's needed
    /// for the "first" run for any given benchmark (we reuse the processor),
    /// e.g. disabling -Zself-profile.
    fn start_first_collection(&mut self) {}

    /// Provided to permit switching off more expensive profiling if it's needed
    /// for the "first" run, e.g. disabling -Zself-profile.
    ///
    /// Return "true" if planning on doing something different for second
    /// iteration.
    fn finished_first_collection(&mut self) -> bool {
        false
    }

    /// Called when all the runs of a benchmark for a particular `BuildKind`
    /// have been completed. Can be used to process/reset accumulated state.
    fn finish_build_kind(&mut self, _build_kind: BuildKind, _runs: &mut Vec<Run>) {}
}

pub struct MeasureProcessor {
    clean_stats: (Stats, Option<SelfProfile>),
    base_incr_stats: (Stats, Option<SelfProfile>),
    clean_incr_stats: (Stats, Option<SelfProfile>),
    patched_incr_stats: Vec<(Patch, (Stats, Option<SelfProfile>))>,
    is_first_collection: bool,
    self_profile: bool,
}

impl MeasureProcessor {
    pub fn new(self_profile: bool) -> Self {
        // Check we have `perf` available.
        let has_perf = Command::new("perf").output().is_ok();
        assert!(has_perf);

        MeasureProcessor {
            clean_stats: (Stats::new(), None),
            base_incr_stats: (Stats::new(), None),
            clean_incr_stats: (Stats::new(), None),
            patched_incr_stats: Vec::new(),
            is_first_collection: true,
            // Command::new("summarize").status().is_ok()
            self_profile,
        }
    }
}

impl Processor for MeasureProcessor {
    fn profiler(&self) -> Profiler {
        if self.is_first_collection && self.self_profile {
            Profiler::PerfStatSelfProfile
        } else {
            Profiler::PerfStat
        }
    }

    fn start_first_collection(&mut self) {
        self.is_first_collection = true;
    }

    fn finished_first_collection(&mut self) -> bool {
        self.is_first_collection = false;
        true
    }

    fn process_output(
        &mut self,
        data: &ProcessOutputData<'_>,
        output: process::Output,
    ) -> anyhow::Result<Retry> {
        match process_perf_stat_output(output) {
            Ok((stats, profile)) => {
                match data.run_kind {
                    RunKind::Full => {
                        self.clean_stats.0.combine_with(stats);
                        if profile.is_some() {
                            self.clean_stats.1 = profile;
                        }
                    }
                    RunKind::IncrFull => {
                        self.base_incr_stats.0.combine_with(stats);
                        if profile.is_some() {
                            self.base_incr_stats.1 = profile;
                        }
                    }
                    RunKind::IncrUnchanged => {
                        self.clean_incr_stats.0.combine_with(stats);
                        if profile.is_some() {
                            self.clean_incr_stats.1 = profile;
                        }
                    }
                    RunKind::IncrPatched => {
                        let patch = data.patch.unwrap();
                        if let Some(entry) =
                            self.patched_incr_stats.iter_mut().find(|s| &s.0 == patch)
                        {
                            (entry.1).0.combine_with(stats);
                            if profile.is_some() {
                                (entry.1).1 = profile;
                            }
                            return Ok(Retry::No);
                        }
                        self.patched_incr_stats
                            .push((patch.clone(), (stats, profile)));
                    }
                }
                Ok(Retry::No)
            }
            Err(DeserializeStatError::NoOutput(output)) => {
                log::warn!(
                    "failed to deserialize stats, retrying; output: {:?}",
                    output
                );
                Ok(Retry::Yes)
            }
            Err(e @ DeserializeStatError::ParseError { .. }) => {
                panic!("process_perf_stat_output failed: {:?}", e);
            }
        }
    }

    fn finish_build_kind(&mut self, build_kind: BuildKind, runs: &mut Vec<Run>) {
        if !self.clean_stats.0.is_empty() {
            runs.push(process_stats(
                build_kind,
                BenchmarkState::Clean,
                self.clean_stats.0.clone(),
                self.clean_stats.1.clone(),
            ));
        }
        if !self.base_incr_stats.0.is_empty() {
            runs.push(process_stats(
                build_kind,
                BenchmarkState::IncrementalStart,
                self.base_incr_stats.0.clone(),
                self.base_incr_stats.1.clone(),
            ));
        }
        if !self.clean_incr_stats.0.is_empty() {
            runs.push(process_stats(
                build_kind,
                BenchmarkState::IncrementalClean,
                self.clean_incr_stats.0.clone(),
                self.clean_incr_stats.1.clone(),
            ));
        }
        if !self.patched_incr_stats.is_empty() {
            for (patch, results) in self.patched_incr_stats.iter() {
                runs.push(process_stats(
                    build_kind,
                    BenchmarkState::IncrementalPatched(patch.clone()),
                    results.0.clone(),
                    results.1.clone(),
                ));
            }
        }

        // Empty all the vectors.
        self.clean_stats.0.clear();
        self.base_incr_stats.0.clear();
        self.clean_incr_stats.0.clear();
        self.patched_incr_stats.clear();
        self.clean_stats.1.take();
        self.base_incr_stats.1.take();
        self.clean_incr_stats.1.take();
    }
}

pub struct ProfileProcessor<'a> {
    profiler: Profiler,
    output_dir: &'a Path,
    id: &'a str,
}

impl<'a> ProfileProcessor<'a> {
    pub fn new(profiler: Profiler, output_dir: &'a Path, id: &'a str) -> Self {
        ProfileProcessor {
            profiler,
            output_dir,
            id,
        }
    }
}

impl<'a> Processor for ProfileProcessor<'a> {
    fn profiler(&self) -> Profiler {
        self.profiler
    }

    fn process_output(
        &mut self,
        data: &ProcessOutputData<'_>,
        output: process::Output,
    ) -> anyhow::Result<Retry> {
        // Produce a name of the form $PREFIX-$ID-$BENCHMARK-$BUILDKIND-$RUNKIND.
        let out_file = |prefix: &str| -> String {
            format!(
                "{}-{}-{}-{:?}-{}",
                prefix, self.id, data.name, data.build_kind, data.run_kind_str
            )
        };

        // Combine a dir and a file.
        let filepath = |dir: &Path, file: &str| {
            let mut path = dir.to_path_buf();
            path.push(file);
            path
        };

        match self.profiler {
            Profiler::PerfStat | Profiler::PerfStatSelfProfile => {
                panic!("unexpected profiler");
            }

            // -Zself-profile produces (via rustc-fake) a data directory called
            // 'Zsp' containing three files with names of the form
            // `$BENCHMARK-$PID.{events,string_data,string_index}`. We copy it
            // from the temp dir to the output dir, renaming the files within
            // as `Zsp.{events,string_data,string_index}` in the process, then
            // post-process them with `summarize`, `flamegraph`, and `crox` to
            // produce several data files in the output dir.
            Profiler::SelfProfile => {
                let tmp_zsp_dir = filepath(data.cwd.as_ref(), "Zsp");
                let zsp_dir = filepath(self.output_dir, &out_file("Zsp"));
                let zsp_files_prefix = filepath(&zsp_dir, "Zsp");
                let summarize_file = filepath(self.output_dir, &out_file("summarize"));
                let flamegraph_file = filepath(self.output_dir, &out_file("flamegraph"));
                let crox_file = filepath(self.output_dir, &out_file("crox"));

                // Move the directory.
                if zsp_dir.exists() {
                    fs::remove_dir_all(&zsp_dir)?;
                }
                fs::rename(&tmp_zsp_dir, &zsp_dir)?;

                // Rename the data files. There should be exactly three.
                let mut num_files = 0;
                for entry in fs::read_dir(&zsp_dir).unwrap() {
                    num_files += 1;
                    let filename = entry.unwrap().file_name();
                    let filename_str = filename.to_str().unwrap();
                    let path = filepath(&zsp_dir, filename_str);
                    if filename_str.ends_with(".events") {
                        fs::rename(path, filepath(&zsp_dir, "Zsp.events"))?;
                    } else if filename_str.ends_with(".string_data") {
                        fs::rename(path, filepath(&zsp_dir, "Zsp.string_data"))?;
                    } else if filename_str.ends_with(".string_index") {
                        fs::rename(path, filepath(&zsp_dir, "Zsp.string_index"))?;
                    } else {
                        panic!("unexpected file {:?}", path);
                    }
                }
                assert_eq!(num_files, 3);

                // Run `summarize`.
                let mut summarize_cmd = Command::new("summarize");
                summarize_cmd.arg("summarize").arg(&zsp_files_prefix);
                let output = summarize_cmd.output()?;
                fs::write(&summarize_file, &output.stdout)?;

                // Run `flamegraph`.
                let mut flamegraph_cmd = Command::new("flamegraph");
                flamegraph_cmd.arg(&zsp_files_prefix);
                flamegraph_cmd.status()?;
                fs::write(&summarize_file, &output.stdout)?;
                fs::rename("rustc.svg", flamegraph_file)?;

                // Run `crox`.
                let mut crox_cmd = Command::new("crox");
                crox_cmd.arg(&zsp_files_prefix);
                crox_cmd.status()?;
                fs::write(&summarize_file, &output.stdout)?;
                fs::rename("chrome_profiler.json", crox_file)?;
            }

            // -Ztime-passes writes its output to stdout. We copy that output
            // into a file in the output dir.
            Profiler::TimePasses => {
                let ztp_file = filepath(self.output_dir, &out_file("Ztp"));

                fs::write(ztp_file, &output.stdout)?;
            }

            // perf-record produces (via rustc-fake) a data file called 'perf'.
            // We copy it from the temp dir to the output dir, giving it a new
            // name in the process.
            Profiler::PerfRecord => {
                let tmp_perf_file = filepath(data.cwd.as_ref(), "perf");
                let perf_file = filepath(self.output_dir, &out_file("perf"));

                fs::copy(&tmp_perf_file, &perf_file)?;
            }

            // OProfile produces (via rustc-fake) a data directory called
            // 'oprofile_data'. We copy it from the temp dir to the output dir,
            // giving it a new name in the process, and then post-process it
            // twice to produce another two data files in the output dir.
            Profiler::OProfile => {
                let tmp_opout_dir = filepath(data.cwd.as_ref(), "oprofile_data");
                let opout_dir = filepath(self.output_dir, &out_file("opout"));
                let oprep_file = filepath(self.output_dir, &out_file("oprep"));
                let opann_file = filepath(self.output_dir, &out_file("opann"));

                // Move the directory.
                if opout_dir.exists() {
                    fs::remove_dir_all(&opout_dir)?;
                }
                fs::rename(&tmp_opout_dir, &opout_dir)?;

                let mut session_dir_arg = "--session-dir=".to_string();
                session_dir_arg.push_str(opout_dir.to_str().unwrap());

                let mut op_report_cmd = Command::new("opreport");
                // Other possibly useful args: --callgraph (requires
                // --callgraph for operf), --details
                op_report_cmd
                    .arg("--symbols")
                    .arg("--debug-info")
                    .arg("--threshold")
                    .arg("0.5")
                    .arg(&session_dir_arg);
                let output = op_report_cmd.output()?;
                fs::write(oprep_file, &output.stdout)?;

                let mut op_annotate_cmd = Command::new("opannotate");
                // Other possibly useful args: --assembly
                op_annotate_cmd
                    .arg("--source")
                    .arg("--threshold")
                    .arg("0.5")
                    .arg(&session_dir_arg);
                let output = op_annotate_cmd.output()?;
                fs::write(opann_file, &output.stdout)?;
            }

            // Cachegrind produces (via rustc-fake) a data file called 'cgout'.
            // We copy it from the temp dir to the output dir, giving it a new
            // name in the process, and then post-process it to produce another
            // data file in the output dir.
            Profiler::Cachegrind => {
                let tmp_cgout_file = filepath(data.cwd.as_ref(), "cgout");
                let cgout_file = filepath(self.output_dir, &out_file("cgout"));
                let cgann_file = filepath(self.output_dir, &out_file("cgann"));

                fs::copy(&tmp_cgout_file, &cgout_file)?;

                let mut cg_annotate_cmd = Command::new("cg_annotate");
                cg_annotate_cmd
                    .arg("--auto=yes")
                    .arg("--show-percs=yes")
                    .arg(&cgout_file);
                let output = cg_annotate_cmd.output()?;
                fs::write(cgann_file, &output.stdout)?;
            }

            // Callgrind produces (via rustc-fake) a data file called 'clgout'.
            // We copy it from the temp dir to the output dir, giving it a new
            // name in the process, and then post-process it to produce another
            // data file in the output dir.
            Profiler::Callgrind => {
                let tmp_clgout_file = filepath(data.cwd.as_ref(), "clgout");
                let clgout_file = filepath(self.output_dir, &out_file("clgout"));
                let clgann_file = filepath(self.output_dir, &out_file("clgann"));

                fs::copy(&tmp_clgout_file, &clgout_file)?;

                let mut clg_annotate_cmd = Command::new("callgrind_annotate");
                clg_annotate_cmd
                    .arg("--auto=yes")
                    .arg("--show-percs=yes")
                    .arg(&clgout_file);
                let output = clg_annotate_cmd.output()?;
                fs::write(clgann_file, &output.stdout)?;
            }

            // DHAT produces (via rustc-fake) a data file called 'dhout'. We
            // copy it from the temp dir to the output dir, giving it a new
            // name in the process.
            Profiler::DHAT => {
                let tmp_dhout_file = filepath(data.cwd.as_ref(), "dhout");
                let dhout_file = filepath(self.output_dir, &out_file("dhout"));

                fs::copy(&tmp_dhout_file, &dhout_file)?;
            }

            // Massif produces (via rustc-fake) a data file called 'msout'. We
            // copy it from the temp dir to the output dir, giving it a new
            // name in the process.
            Profiler::Massif => {
                let tmp_msout_file = filepath(data.cwd.as_ref(), "msout");
                let msout_file = filepath(self.output_dir, &out_file("msout"));

                fs::copy(&tmp_msout_file, &msout_file)?;
            }

            // `eprintln!` statements writes their output to stderr. We copy
            // that output into a file in the output dir.
            Profiler::Eprintln => {
                let eprintln_file = filepath(self.output_dir, &out_file("eprintln"));

                fs::write(eprintln_file, &output.stderr)?;
            }

            // `cargo llvm-lines` writes its output to stdout. We copy that
            // output into a file in the output dir.
            Profiler::LlvmLines => {
                let ll_file = filepath(self.output_dir, &out_file("ll"));

                fs::write(ll_file, &output.stdout)?;
            }
        }
        Ok(Retry::No)
    }
}

impl Benchmark {
    pub fn new(name: String, path: PathBuf) -> anyhow::Result<Self> {
        let mut patches = vec![];
        for entry in fs::read_dir(&path)? {
            let entry = entry?;
            let path = entry.path();
            if let Some(ext) = path.extension() {
                if ext == "patch" {
                    patches.push(path.clone());
                }
            }
        }

        patches.sort();

        let patches = patches.into_iter().map(|p| Patch::new(p)).collect();

        let config_path = path.join("perf-config.json");
        let config: BenchmarkConfig = if config_path.exists() {
            serde_json::from_reader(
                File::open(&config_path)
                    .with_context(|| format!("failed to open {:?}", config_path))?,
            )
            .with_context(|| format!("failed to parse {:?}", config_path))?
        } else {
            BenchmarkConfig::default()
        };

        Ok(Benchmark {
            name: name.as_str().into(),
            path,
            patches,
            config,
        })
    }

    pub fn supports_stable(&self) -> bool {
        self.config.supports_stable
    }

    fn make_temp_dir(&self, base: &Path) -> anyhow::Result<TempDir> {
        // Appending `.` means we copy just the contents of `base` into
        // `tmp_dir`, rather than `base` itself.
        let mut base_dot = base.to_path_buf();
        base_dot.push(".");
        let tmp_dir = TempDir::new()?;
        let mut cmd = Command::new("cp");
        cmd.arg("-LR").arg(base_dot).arg(tmp_dir.path());
        command_output(&mut cmd).with_context(|| format!("copying {} to tmp dir", self.name))?;
        Ok(tmp_dir)
    }

    fn mk_cargo_process<'a>(
        &'a self,
        compiler: Compiler<'a>,
        cwd: &'a Path,
        build_kind: BuildKind,
    ) -> CargoProcess<'a> {
        let mut cargo_args = self
            .config
            .cargo_opts
            .clone()
            .unwrap_or_default()
            .split_whitespace()
            .map(String::from)
            .collect::<Vec<_>>();
        if let Some(count) = env::var("CARGO_THREAD_COUNT")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
        {
            cargo_args.push(format!("-j{}", count));
        }

        CargoProcess {
            compiler,
            processor_name: self.name,
            cwd: cwd,
            build_kind: build_kind,
            incremental: false,
            processor_etc: None,
            manifest_path: self
                .config
                .cargo_toml
                .clone()
                .unwrap_or_else(|| String::from("Cargo.toml")),
            cargo_args,
            rustc_args: self
                .config
                .cargo_rustc_opts
                .clone()
                .unwrap_or_default()
                .split_whitespace()
                .map(String::from)
                .collect(),
        }
    }

    /// Run a specific benchmark under a processor + profiler combination.
    pub fn measure(
        &self,
        processor: &mut dyn Processor,
        build_kinds: &[BuildKind],
        run_kinds: &[RunKind],
        compiler: Compiler<'_>,
        iterations: usize,
    ) -> anyhow::Result<Vec<Run>> {
        let iterations = cmp::min(iterations, self.config.runs);

        if self.config.disabled {
            eprintln!("Skipping {}: disabled", self.name);
            bail!("disabled benchmark");
        }

        let mut runs = Vec::new();

        for &build_kind in build_kinds {
            eprintln!("Running {}: {:?} + {:?}", self.name, build_kind, run_kinds);

            // Build everything, including all dependent crates, in a temp dir.
            // We do this before the iterations so that dependent crates aren't
            // built on every iteration. A different temp dir is used for the
            // timing builds.
            let prep_dir = self.make_temp_dir(&self.path)?;
            self.mk_cargo_process(compiler, prep_dir.path(), build_kind)
                .run_rustc()?;

            // We want at least two runs for all benchmarks (since we run
            // self-profile separately).
            processor.start_first_collection();
            for i in 0..cmp::max(iterations, 2) {
                if i == 1 {
                    let different = processor.finished_first_collection();
                    if iterations == 1 && !different {
                        // Don't run twice if this processor doesn't need it and
                        // we've only been asked to run once.
                        break;
                    }
                }
                log::debug!("Benchmark iteration {}/{}", i + 1, iterations);
                let timing_dir = self.make_temp_dir(prep_dir.path())?;
                let cwd = timing_dir.path();

                // A full non-incremental build.
                if run_kinds.contains(&RunKind::Full) {
                    self.mk_cargo_process(compiler, cwd, build_kind)
                        .processor(processor, RunKind::Full, "Full", None)
                        .run_rustc()?;
                }

                // An incremental build from scratch (slowest incremental case).
                // This is required for any subsequent incremental builds.
                if run_kinds.contains(&RunKind::IncrFull)
                    || run_kinds.contains(&RunKind::IncrUnchanged)
                    || run_kinds.contains(&RunKind::IncrPatched)
                {
                    self.mk_cargo_process(compiler, cwd, build_kind)
                        .incremental(true)
                        .processor(processor, RunKind::IncrFull, "IncrFull", None)
                        .run_rustc()?;
                }

                // An incremental build with no changes (fastest incremental case).
                if run_kinds.contains(&RunKind::IncrUnchanged) {
                    self.mk_cargo_process(compiler, cwd, build_kind)
                        .incremental(true)
                        .processor(processor, RunKind::IncrUnchanged, "IncrUnchanged", None)
                        .run_rustc()?;
                }

                if run_kinds.contains(&RunKind::IncrPatched) {
                    for (i, patch) in self.patches.iter().enumerate() {
                        log::debug!("applying patch {}", patch.name);
                        patch.apply(cwd).map_err(|s| anyhow::anyhow!("{}", s))?;

                        // An incremental build with some changes (realistic
                        // incremental case).
                        let run_kind_str = format!("IncrPatched{}", i);
                        self.mk_cargo_process(compiler, cwd, build_kind)
                            .incremental(true)
                            .processor(processor, RunKind::IncrPatched, &run_kind_str, Some(&patch))
                            .run_rustc()?;
                    }
                }
            }

            processor.finish_build_kind(build_kind, &mut runs);
        }

        Ok(runs)
    }
}

#[derive(thiserror::Error, PartialEq, Eq, Debug)]
enum DeserializeStatError {
    #[error("could not deserialize empty output to stats, output: {:?}", .0)]
    NoOutput(process::Output),
    #[error("could not parse `{}` as a float", .0)]
    ParseError(String, #[source] ::std::num::ParseFloatError),
}

fn process_perf_stat_output(
    output: process::Output,
) -> Result<(Stats, Option<SelfProfile>), DeserializeStatError> {
    let stdout = String::from_utf8(output.stdout.clone()).expect("utf8 output");
    let mut stats = Stats::new();

    let mut profile: Option<SelfProfile> = None;
    for line in stdout.lines() {
        if line.starts_with("!self-profile-output:") {
            profile = Some(serde_json::from_str(&line["!self-profile-output:".len()..]).unwrap());
            continue;
        }

        // github.com/torvalds/linux/blob/bc78d646e708/tools/perf/Documentation/perf-stat.txt#L281
        macro_rules! get {
            ($e: expr) => {
                match $e {
                    Some(s) => s,
                    None => {
                        log::warn!("unhandled line: {}", line);
                        continue;
                    }
                }
            };
        }
        let mut parts = line.split(';').map(|s| s.trim());
        let cnt = get!(parts.next());
        let _unit = get!(parts.next());
        let name = get!(parts.next());
        let _time = get!(parts.next());
        let pct = get!(parts.next());
        if cnt == "<not supported>" || cnt.len() == 0 {
            continue;
        }
        if !pct.starts_with("100.") {
            panic!(
                "measurement of `{}` only active for {}% of the time",
                name, pct
            );
        }
        stats.insert(
            StatId::from_str(name).unwrap(),
            cnt.parse()
                .map_err(|e| DeserializeStatError::ParseError(cnt.to_string(), e))?,
        );
    }

    if stats.is_empty() {
        return Err(DeserializeStatError::NoOutput(output));
    }

    Ok((stats, profile))
}

fn process_stats(
    build_kind: BuildKind,
    state: BenchmarkState,
    runs: Stats,
    prof: Option<SelfProfile>,
) -> Run {
    Run {
        stats: runs,
        self_profile: prof,
        check: build_kind == BuildKind::Check,
        release: build_kind == BuildKind::Opt,
        state: state,
    }
}
