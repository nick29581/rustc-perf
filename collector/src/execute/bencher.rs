use crate::benchmark::profile::Profile;
use crate::benchmark::scenario::Scenario;
use crate::benchmark::BenchmarkName;
use crate::execute;
use crate::execute::{
    rustc, DeserializeStatError, PerfTool, ProcessOutputData, Processor, Retry, SelfProfile,
    SelfProfileFiles, Stats, Upload,
};
use crate::toolchain::Compiler;
use anyhow::Context;
use futures::stream::FuturesUnordered;
use futures::StreamExt;
use std::path::PathBuf;
use std::process::Command;
use std::{env, process};
use tokio::runtime::Runtime;

// Tools usable with the benchmarking subcommands.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Bencher {
    PerfStat,
    PerfStatSelfProfile,
    XperfStat,
    XperfStatSelfProfile,
}

pub struct BenchProcessor<'a> {
    rt: &'a mut Runtime,
    benchmark: &'a BenchmarkName,
    conn: &'a mut dyn database::Connection,
    artifact: &'a database::ArtifactId,
    artifact_row_id: database::ArtifactIdNumber,
    upload: Option<Upload>,
    is_first_collection: bool,
    is_self_profile: bool,
    tries: u8,
}

impl<'a> BenchProcessor<'a> {
    pub fn new(
        rt: &'a mut Runtime,
        conn: &'a mut dyn database::Connection,
        benchmark: &'a BenchmarkName,
        artifact: &'a database::ArtifactId,
        artifact_row_id: database::ArtifactIdNumber,
        is_self_profile: bool,
    ) -> Self {
        // Check we have `perf` or (`xperf.exe` and `tracelog.exe`)  available.
        if cfg!(unix) {
            let has_perf = Command::new("perf").output().is_ok();
            assert!(has_perf);
        } else {
            let has_xperf = Command::new(env::var("XPERF").unwrap_or("xperf.exe".to_string()))
                .output()
                .is_ok();
            assert!(has_xperf);

            let has_tracelog =
                Command::new(env::var("TRACELOG").unwrap_or("tracelog.exe".to_string()))
                    .output()
                    .is_ok();
            assert!(has_tracelog);
        }

        BenchProcessor {
            rt,
            upload: None,
            conn,
            benchmark,
            artifact,
            artifact_row_id,
            is_first_collection: true,
            is_self_profile,
            tries: 0,
        }
    }

    fn insert_stats(
        &mut self,
        scenario: database::Scenario,
        profile: Profile,
        stats: (Stats, Option<SelfProfile>, Option<SelfProfileFiles>),
    ) {
        let version = String::from_utf8(
            Command::new("git")
                .arg("rev-parse")
                .arg("HEAD")
                .output()
                .context("git rev-parse HEAD")
                .unwrap()
                .stdout,
        )
        .context("utf8")
        .unwrap();

        let collection = self.rt.block_on(self.conn.collection_id(&version));
        let profile = match profile {
            Profile::Check => database::Profile::Check,
            Profile::Debug => database::Profile::Debug,
            Profile::Doc => database::Profile::Doc,
            Profile::JsonDoc => database::Profile::JsonDoc,
            Profile::Opt => database::Profile::Opt,
        };

        if let Some(files) = stats.2 {
            if env::var_os("RUSTC_PERF_UPLOAD_TO_S3").is_some() {
                // We can afford to have the uploads run concurrently with
                // rustc. Generally speaking, they take up almost no CPU time
                // (just copying data into the network). Plus, during
                // self-profile data timing noise doesn't matter as much. (We'll
                // be migrating to instructions soon, hopefully, where the
                // upload will cause even less noise). We may also opt at some
                // point to defer these uploads entirely to the *end* or
                // something like that. For now though this works quite well.
                if let Some(u) = self.upload.take() {
                    u.wait();
                }
                let prefix = PathBuf::from("self-profile")
                    .join(self.artifact_row_id.0.to_string())
                    .join(self.benchmark.0.as_str())
                    .join(profile.to_string())
                    .join(scenario.to_id());
                self.upload = Some(Upload::new(prefix, collection, files));
                self.rt.block_on(self.conn.record_raw_self_profile(
                    collection,
                    self.artifact_row_id,
                    self.benchmark.0.as_str(),
                    profile,
                    scenario,
                ));
            }
        }

        let mut buf = FuturesUnordered::new();
        for (stat, value) in stats.0.iter() {
            buf.push(self.conn.record_statistic(
                collection,
                self.artifact_row_id,
                self.benchmark.0.as_str(),
                profile,
                scenario,
                stat,
                value,
            ));
        }

        if let Some(sp) = &stats.1 {
            let conn = &*self.conn;
            let artifact_row_id = self.artifact_row_id;
            let benchmark = self.benchmark.0.as_str();
            for qd in &sp.query_data {
                buf.push(conn.record_self_profile_query(
                    collection,
                    artifact_row_id,
                    benchmark,
                    profile,
                    scenario,
                    qd.label.as_str(),
                    database::QueryDatum {
                        self_time: qd.self_time,
                        blocked_time: qd.blocked_time,
                        incremental_load_time: qd.incremental_load_time,
                        number_of_cache_hits: qd.number_of_cache_hits,
                        invocation_count: qd.invocation_count,
                    },
                ));
            }
        }

        self.rt
            .block_on(async move { while let Some(()) = buf.next().await {} });
    }

    pub fn measure_rustc(&mut self, compiler: Compiler<'_>) -> anyhow::Result<()> {
        rustc::measure(
            self.rt,
            self.conn,
            compiler,
            self.artifact,
            self.artifact_row_id,
        )
    }
}

impl<'a> Processor for BenchProcessor<'a> {
    fn perf_tool(&self) -> PerfTool {
        if self.is_first_collection && self.is_self_profile {
            if cfg!(unix) {
                PerfTool::BenchTool(Bencher::PerfStatSelfProfile)
            } else {
                PerfTool::BenchTool(Bencher::XperfStatSelfProfile)
            }
        } else {
            if cfg!(unix) {
                PerfTool::BenchTool(Bencher::PerfStat)
            } else {
                PerfTool::BenchTool(Bencher::XperfStat)
            }
        }
    }

    fn start_first_collection(&mut self) {
        self.is_first_collection = true;
    }

    fn finished_first_collection(&mut self) -> bool {
        let original = self.perf_tool();
        self.is_first_collection = false;
        // We need to run again if we're going to use a different perf tool
        self.perf_tool() != original
    }

    fn process_output(
        &mut self,
        data: &ProcessOutputData<'_>,
        output: process::Output,
    ) -> anyhow::Result<Retry> {
        match execute::process_stat_output(output) {
            Ok(mut res) => {
                if let Some(ref profile) = res.1 {
                    execute::store_artifact_sizes_into_stats(&mut res.0, profile);
                }
                if let Profile::Doc = data.profile {
                    let doc_dir = data.cwd.join("target/doc");
                    if doc_dir.is_dir() {
                        execute::store_documentation_size_into_stats(&mut res.0, &doc_dir);
                    }
                }

                match data.scenario {
                    Scenario::Full => {
                        self.insert_stats(database::Scenario::Empty, data.profile, res);
                    }
                    Scenario::IncrFull => {
                        self.insert_stats(database::Scenario::IncrementalEmpty, data.profile, res);
                    }
                    Scenario::IncrUnchanged => {
                        self.insert_stats(database::Scenario::IncrementalFresh, data.profile, res);
                    }
                    Scenario::IncrPatched => {
                        let patch = data.patch.unwrap();
                        self.insert_stats(
                            database::Scenario::IncrementalPatch(patch.name),
                            data.profile,
                            res,
                        );
                    }
                }
                Ok(Retry::No)
            }
            Err(DeserializeStatError::NoOutput(output)) => {
                if self.tries < 5 {
                    log::warn!(
                        "failed to deserialize stats, retrying (try {}); output: {:?}",
                        self.tries,
                        output
                    );
                    self.tries += 1;
                    Ok(Retry::Yes)
                } else {
                    panic!("failed to collect statistics after 5 tries");
                }
            }
            Err(
                e
                @ (DeserializeStatError::ParseError { .. } | DeserializeStatError::XperfError(..)),
            ) => {
                panic!("process_perf_stat_output failed: {:?}", e);
            }
        }
    }
}
