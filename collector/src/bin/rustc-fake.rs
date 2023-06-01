use anyhow::Context;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

fn determinism_env(cmd: &mut Command) {
    // Since rust-lang/rust#89836, rustc stable crate IDs include a hash of the
    // rustc version (including the git commit it's built from), which means
    // that hashmaps or other structures have different behavior when comparing
    // different rustc builds. This is bad for rustc-perf, as it means that
    // comparing two commits has a source of noise that makes it harder to know
    // what the actual change between two artifacts is.
    cmd.env("RUSTC_FORCE_INCR_COMP_ARTIFACT_HEADER", "rustc-perf");
    cmd.env("RUSTC_FORCE_RUSTC_VERSION", "rustc-perf");
}

fn run_with_determinism_env(mut cmd: Command) {
    determinism_env(&mut cmd);
    let status = cmd.status().expect("failed to spawn");
    assert!(
        status.success(),
        "command did not complete successfully: {:?}",
        cmd
    );
}

fn main() {
    let mut args_os = env::args_os();
    let name = args_os.next().unwrap().into_string().unwrap();

    let actually_rustdoc = name.ends_with("rustdoc-fake");
    let tool = if actually_rustdoc {
        let rustdoc = env::var_os("RUSTDOC_REAL").unwrap();
        rustdoc
    } else {
        // rustc
        args_os.next().unwrap()
    };

    let mut args = args_os.collect::<Vec<_>>();

    if let Some(count) = env::var("RUSTC_THREAD_COUNT")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
    {
        args.push(OsString::from(format!("-Zthreads={}", count)));
    }

    args.push(OsString::from("-Adeprecated"));
    args.push(OsString::from("-Aunknown-lints"));

    // This forces incremental query hash verification on. Currently, rustc
    // hashes 1/32 of queries loaded from disk without this flag, but that 1/32
    // is based on the (expected) hash of the data, which can vary from build to
    // build, adding a source of noise to our measurements, which we prefer to
    // avoid. rustc-perf can accept the higher cost of always verifying hashes,
    // and we currently prefer to avoid exposing a means of hard-disabling
    // verification.
    args.push(OsString::from("-Zincremental-verify-ich"));

    if let Some(pos) = args.iter().position(|arg| arg == "--wrap-rustc-with") {
        // Strip out the flag and its argument, and run rustc under the wrapper
        // program named by the argument.
        args.remove(pos);
        let wrapper = args.remove(pos);
        let wrapper = wrapper.to_str().unwrap();

        benchlib::process::raise_process_priority();

        // These strings come from `PerfTool::name()`.
        match wrapper {
            "PerfStat" | "PerfStatSelfProfile" => {
                let mut cmd = Command::new("perf");
                let has_perf = cmd.output().is_ok();
                assert!(has_perf);
                cmd.arg("stat")
                    // perf respects this environment variable for e.g., percents in
                    // the output, but we want standard output on all systems.
                    // See #753 for more details.
                    .env("LC_NUMERIC", "C")
                    .arg("-x;")
                    .arg("-e")
                    .arg("instructions:u,cycles:u,task-clock,cpu-clock,faults,context-switches,branch-misses,cache-misses")
                    .arg("--log-fd")
                    .arg("1")
                    .arg("setarch")
                    .arg(std::env::consts::ARCH)
                    .arg("-R")
                    .arg(&tool)
                    .args(&args);

                let prof_out_dir = std::env::current_dir().unwrap().join("self-profile-output");
                if wrapper == "PerfStatSelfProfile" {
                    cmd.arg(&format!(
                        "-Zself-profile={}",
                        prof_out_dir.to_str().unwrap()
                    ));
                    let _ = fs::remove_dir_all(&prof_out_dir);
                    let _ = fs::create_dir_all(&prof_out_dir);
                }

                let start = Instant::now();
                run_with_determinism_env(cmd);
                let dur = start.elapsed();
                print_memory();
                print_time(dur);
                if wrapper == "PerfStatSelfProfile" {
                    process_self_profile_output(prof_out_dir, &args[..]);
                }
            }

            "XperfStat" | "XperfStatSelfProfile" => {
                // For Windows, we use a combination of xperf and tracelog to capture ETW events
                // including hardware performance counters. To do this, we start an ETW trace using
                // tracelog, telling it to include the InstructionRetired and TotalCycles PMCs for
                // each CSwitch event that is recorded. Then when ETW records a context switch
                // event, it will be preceeded by a PMC event which contains the raw counters at
                // that instant. After we've finished compilation, we then use xperf to stop the
                // trace and dump the results to a plain text file. This file is then processed by
                // the `etw_parser` module which finds events related to the rustc process and
                // calculates the total values for those performance counters. Conceptually, this
                // is similar to how `perf` works on Linux except we have to do more of the work
                // ourselves as there isn't an out of the box way to get the data we care about.

                // Read the path to xperf.exe and tracelog.exe from an environment variable,
                // falling back to assuming it's on the PATH.
                let xperf = std::env::var("XPERF").unwrap_or("xperf.exe".to_string());
                let mut cmd = Command::new(&xperf);
                assert!(cmd.output().is_ok(), "xperf.exe could not be started");

                // Go ahead and run `xperf -stop rustc-perf-counters` in case there are leftover
                // counters running from a failed prior attempt.
                let mut cmd = Command::new(&xperf);
                cmd.args(&["-stop", "rustc-perf-counters"]);
                cmd.status().expect("failed to spawn xperf");

                let tracelog = std::env::var("TRACELOG").unwrap_or("tracelog.exe".to_string());
                let mut cmd = Command::new(tracelog);
                assert!(cmd.output().is_ok(), "tracelog.exe could not be started");

                cmd.args(&[
                    "-start",
                    "rustc-perf-counters",
                    "-f",
                    "counters.etl",
                    "-eflag",
                    "CSWITCH+PROC_THREAD+LOADER",
                    "-PMC",
                    "InstructionRetired,TotalCycles:CSWITCH",
                    "-b",
                    "1024",
                    "-min",
                    "512",
                    "-max",
                    "2048",
                ]);
                let status = cmd.status().expect("failed to spawn tracelog");
                assert!(status.success(), "tracelog did not complete successfully");

                let mut tool = Command::new(tool);
                tool.args(&args);

                let prof_out_dir = std::env::current_dir().unwrap().join("self-profile-output");
                if wrapper == "XperfStatSelfProfile" {
                    tool.arg(&format!(
                        "-Zself-profile={}",
                        prof_out_dir.to_str().unwrap()
                    ));
                    let _ = fs::remove_dir_all(&prof_out_dir);
                    let _ = fs::create_dir_all(&prof_out_dir);
                }

                let start = Instant::now();
                run_with_determinism_env(tool);
                let dur = start.elapsed();
                println!("!wall-time:{}.{:09}", dur.as_secs(), dur.subsec_nanos());

                let xperf = |args: &[&str]| {
                    let mut cmd = Command::new(&xperf);
                    cmd.args(args);
                    assert!(cmd.status().expect("failed to spawn xperf").success());
                };

                xperf(&["-stop", "rustc-perf-counters"]);
                xperf(&["-merge", "counters.etl", "pmc_counters_merged.etl"]);
                xperf(&["-i", "pmc_counters_merged.etl", "-o", "pmc_counters.txt"]);

                let counters_file = std::env::current_dir().unwrap().join("pmc_counters.txt");
                println!("!counters-file:{}", counters_file.to_str().unwrap());

                if wrapper == "XperfStatSelfProfile" {
                    process_self_profile_output(prof_out_dir, &args[..]);
                }
            }

            "SelfProfile" => {
                let mut cmd = Command::new(&tool);
                cmd.arg("-Zself-profile-events=all")
                    .arg("-Zself-profile=Zsp")
                    .args(&args);

                run_with_determinism_env(cmd);
            }

            "PerfRecord" => {
                let mut cmd = Command::new("perf");
                let has_perf = cmd.output().is_ok();
                assert!(has_perf);
                cmd.arg("record")
                    .arg("--call-graph=dwarf")
                    .arg("--output=perf")
                    .arg("--freq=299")
                    .arg("--event=cycles:u,instructions:u")
                    .arg(&tool)
                    .args(&args);

                run_with_determinism_env(cmd);
            }

            "Oprofile" => {
                let mut cmd = Command::new("operf");
                let has_oprofile = cmd.output().is_ok();
                assert!(has_oprofile);
                // Other possibly useful args: --callgraph, --separate-thread
                cmd.arg("operf").arg(&tool).args(&args);

                run_with_determinism_env(cmd);
            }

            "Samply" => {
                let mut cmd = Command::new("samply");
                let has_samply = cmd.output().is_ok();
                assert!(has_samply);
                cmd.arg("record")
                    .arg("--no-open")
                    .arg("--save-only")
                    .arg(&tool)
                    .args(&args);

                run_with_determinism_env(cmd);
            }

            "Cachegrind" => {
                let mut cmd = Command::new("valgrind");
                let has_valgrind = cmd.output().is_ok();
                assert!(has_valgrind);

                // With --cache-sim=no and --branch-sim=no, Cachegrind just
                // collects instruction counts.
                cmd
                    // We disable jemalloc's delayed purging to eliminate noise
                    // when benchmarks are around the 10 second mark.
                    //
                    // See https://github.com/rust-lang/rust/pull/77162 for some
                    // further details.
                    .env("MALLOC_CONF", "dirty_decay_ms:0,muzzy_decay_ms:0")
                    .arg("--tool=cachegrind")
                    .arg("--cache-sim=no")
                    .arg("--branch-sim=no")
                    .arg("--cachegrind-out-file=cgout")
                    .arg(&tool)
                    .args(&args);

                run_with_determinism_env(cmd);
            }

            "Callgrind" => {
                let mut cmd = Command::new("valgrind");
                let has_valgrind = cmd.output().is_ok();
                assert!(has_valgrind);

                // With --cache-sim=no and --branch-sim=no, Callgrind just
                // collects instruction counts.
                cmd.arg("--tool=callgrind")
                    .arg("--cache-sim=no")
                    .arg("--branch-sim=no")
                    .arg("--callgrind-out-file=clgout")
                    .arg(&tool)
                    .args(&args);

                run_with_determinism_env(cmd);
            }

            "Dhat" => {
                let mut cmd = Command::new("valgrind");
                let has_valgrind = cmd.output().is_ok();
                assert!(has_valgrind);
                cmd.arg("--tool=dhat")
                    .arg("--num-callers=8")
                    .arg("--dhat-out-file=dhout")
                    .arg(&tool)
                    .args(&args);

                run_with_determinism_env(cmd);
            }

            "DhatCopy" => {
                let mut cmd = Command::new("valgrind");
                let has_valgrind = cmd.output().is_ok();
                assert!(has_valgrind);
                cmd.arg("--tool=dhat")
                    .arg("--mode=copy")
                    .arg("--num-callers=8")
                    .arg("--dhat-out-file=dhcopy")
                    .arg(&tool)
                    .args(&args);

                run_with_determinism_env(cmd);
            }

            "Massif" => {
                let mut cmd = Command::new("valgrind");
                let has_valgrind = cmd.output().is_ok();
                assert!(has_valgrind);
                cmd.arg("--tool=massif")
                    .arg("--heap-admin=0")
                    .arg("--depth=15")
                    .arg("--threshold=0.2")
                    .arg("--massif-out-file=msout")
                    .arg("--alloc-fn=__rdl_alloc")
                    .arg(&tool)
                    .args(&args);

                run_with_determinism_env(cmd);
            }

            "Bytehound" => {
                let mut cmd = Command::new(tool);
                cmd.args(args);
                cmd.env("MEMORY_PROFILER_OUTPUT", "bytehound.dat");
                cmd.env("LD_PRELOAD", "libbytehound.so");

                run_with_determinism_env(cmd);
            }

            "Eprintln" => {
                let mut cmd = Command::new(tool);
                cmd.args(args).stderr(std::process::Stdio::from(
                    std::fs::File::create("eprintln").unwrap(),
                ));

                run_with_determinism_env(cmd);
            }

            "LlvmLines" => {
                // `cargo llvm-lines` writes its output to stdout. But we can't
                // redirect it to a file here like we do for "eprintln". This
                // is because `cargo llvm-lines` invokes rustc as part of its
                // processing and then it does some analysis of its output and
                // then it prints out some results. Because `rustc` (which this
                // file wraps) doesn't produce the output, this file can't
                // redirect that output.
                let mut cmd = Command::new(&tool);
                cmd.args(&args);

                run_with_determinism_env(cmd);
            }

            "LlvmIr" => {
                let mut cmd = Command::new(tool);
                cmd.arg("--emit=llvm-ir=./llvm-ir")
                    .arg("-Cno-prepopulate-passes")
                    .arg("-Cpasses=name-anon-globals")
                    .args(args);

                run_with_determinism_env(cmd);
            }

            "MonoItems" => {
                // Lazy item collection is the default (i.e., without this
                // option)
                let mut cmd = Command::new(tool);
                cmd.arg("-Zprint-mono-items=lazy")
                    .args(args)
                    .stdout(std::process::Stdio::from(
                        std::fs::File::create("mono-items").unwrap(),
                    ));

                run_with_determinism_env(cmd);
            }

            "DepGraph" => {
                let mut cmd = Command::new(tool);
                cmd.arg("-Zdump-dep-graph")
                    .arg("-Zquery-dep-graph")
                    .args(&args);

                run_with_determinism_env(cmd);
            }

            _ => {
                panic!("unknown wrapper: {}", wrapper);
            }
        }
    } else if let Some(_) = args.iter().position(|arg| arg == "--skip-this-rustc") {
        // do nothing
    } else {
        // Abort if non-wrapped rustc
        if env::var_os("EXPECT_ONLY_WRAPPED_RUSTC").is_some() {
            // ... but not if we're just getting the rustc version.
            if !args
                .iter()
                .any(|arg| arg == "-vV" || arg == "--print=file-names")
            {
                eprintln!("{:?} {:?}", tool, args);
                eprintln!("exiting -- non-wrapped rustc");
                std::process::exit(1);
            }
        }

        let mut cmd = Command::new(&tool);
        determinism_env(&mut cmd);
        cmd.args(&args);
        exec(&mut cmd);
    }
}

fn process_self_profile_output(prof_out_dir: PathBuf, args: &[OsString]) {
    let crate_name = args
        .windows(2)
        .find(|args| args[0] == "--crate-name")
        .and_then(|args| args[1].to_str())
        .expect("rustc to be invoked with crate name");
    let mut prefix = None;
    let mut full_path = None;
    // We don't know the pid of rustc, and can't easily get it -- we only know the
    // `perf` pid. So just blindly look in the directory to hopefully find it.
    for entry in fs::read_dir(&prof_out_dir).unwrap() {
        let entry = entry.unwrap();
        if entry
            .file_name()
            .to_str()
            .map_or(false, |s| s.starts_with(crate_name))
        {
            if entry.file_name().to_str().unwrap().ends_with("mm_profdata") {
                full_path = Some(entry.path());
                break;
            }
            let file = entry.file_name().to_str().unwrap().to_owned();
            let new_prefix = Some(file[..file.find('.').unwrap()].to_owned());
            assert!(
                prefix.is_none() || prefix == new_prefix,
                "prefix={:?}, new_prefix={:?}",
                prefix,
                new_prefix
            );
            prefix = new_prefix;
        }
    }
    if let Some(profile_data) = full_path {
        // measureme 0.8 has a single file
        println!("!self-profile-file:{}", profile_data.to_str().unwrap());
        let filename = profile_data.file_name().unwrap().to_str().unwrap();
        let json = match run_summarize("summarize", &prof_out_dir, filename) {
            Ok(s) => s,
            Err(e1) => match run_summarize("summarize-9.0", &prof_out_dir, filename) {
                Ok(s) => s,
                Err(e2) => {
                    panic!("failed to run summarize and summarize-9.0. Errors:\nsummarize: {:?}\nsummarize-9.0: {:?}", e1, e2);
                }
            },
        };
        println!("!self-profile-output:{}", json);
    } else {
        let prefix = prefix.expect(&format!("found prefix {:?}", prof_out_dir));
        let json = run_summarize("summarize", &prof_out_dir, &prefix)
            .or_else(|_| run_summarize("summarize-0.7", &prof_out_dir, &prefix))
            .expect("able to run summarize or summarize-0.7");
        println!("!self-profile-dir:{}", prof_out_dir.to_str().unwrap());
        println!("!self-profile-prefix:{}", prefix);
        println!("!self-profile-output:{}", json);
    }
}

#[cfg(windows)]
fn exec(cmd: &mut Command) -> ! {
    let cmd_d = format!("{:?}", cmd);
    match cmd.status() {
        Ok(status) => std::process::exit(status.code().unwrap_or(1)),
        Err(e) => panic!("failed to spawn `{}`: {}", cmd_d, e),
    }
}

#[cfg(unix)]
fn exec(cmd: &mut Command) -> ! {
    use std::os::unix::prelude::*;
    let cmd_d = format!("{:?}", cmd);
    let error = cmd.exec();
    panic!("failed to exec `{}`: {}", cmd_d, error);
}

#[cfg(unix)]
fn print_memory() {
    use std::mem;

    unsafe {
        let mut usage = mem::zeroed();
        let r = libc::getrusage(libc::RUSAGE_CHILDREN, &mut usage);
        if r == 0 {
            // for explanation of all the semicolons, see `print_time` below
            println!("{};;max-rss;3;100.00", usage.ru_maxrss);
        }
    }
}

fn print_time(dur: Duration) {
    // Format output the same as `perf stat` in CSV mode, explained at
    // http://man7.org/linux/man-pages/man1/perf-stat.1.html#CSV_FORMAT
    //
    // tl;dr; it's:
    //
    //      $value ; $unit ; $name ; $runtime ; $pct
    println!(
        "{}.{:09};;wall-time;4;100.00",
        dur.as_secs(),
        dur.subsec_nanos()
    );
}

fn run_summarize(name: &str, prof_out_dir: &Path, prefix: &str) -> anyhow::Result<String> {
    let mut cmd = Command::new(name);
    cmd.current_dir(&prof_out_dir);
    cmd.arg("summarize").arg("--json");
    cmd.arg(&prefix);
    let status = cmd
        .status()
        .with_context(|| format!("Command::new({}).status() failed", name))?;
    if !status.success() {
        anyhow::bail!(
            "failed to run {} in {:?} with prefix {:?}",
            name,
            prof_out_dir,
            prefix
        )
    }
    let json = prof_out_dir.join(&format!(
        "{}.json",
        prefix.strip_suffix(".mm_profdata").unwrap_or(prefix)
    ));
    fs::read_to_string(&json).with_context(|| format!("failed to read {:?}", json))
}

#[cfg(windows)]
fn print_memory() {}
