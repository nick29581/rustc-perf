// Copyright 2016 The rustc-perf Project Developers. See the COPYRIGHT
// file at the top-level directory.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::str;
use std::env;
use std::cmp::max;
use std::fs::File;
use std::io::Read;
use std::sync::{RwLock, Arc};
use std::collections::{HashMap, BTreeMap, BTreeSet};
use std::path::Path;
use std::net::SocketAddr;
use std::sync::atomic::{Ordering, AtomicBool};

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json;
use futures::{self, Future, Stream};
use futures_cpupool::CpuPool;
use hyper::{self, Get, Post, StatusCode};
use hyper::header::{ContentLength, CacheControl, CacheDirective, ContentType};
use hyper::mime;
use hyper::server::{Http, Service, Request, Response};

use git;
use date::{DeltaTime, Date};
use util::{self, get_repo_path};
pub use api::{self, summary, info, data, tabular, days, stats};
use load::{Pass, CommitData, InputData, Comparison};

use errors::*;

/// Data associated with a specific date
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DateData {
    /// The date of this run
    pub date: Date,

    /// Git commit hash of the compiler these results were obtained with.
    pub commit: String,

    /// Keyed by crate names / phases depending on request's group by field;
    ///   - rss: u64 of memory usage in megabytes
    ///   - time: f64 of duration for compiling in seconds
    pub data: HashMap<String, Recording>,
}

impl DateData {
    pub fn for_day(
        day: &CommitData,
        crates: &BTreeSet<String>,
        phases: &BTreeSet<String>,
        group_by: GroupBy
    ) -> DateData {
        let crates = day.benchmarks.values().filter(|v| v.is_ok())
            .flat_map(|patches| patches.as_ref().unwrap())
            .filter(|patch| crates.contains(&patch.full_name()))
            .collect::<Vec<_>>();

        let mut data = HashMap::new();
        for phase_name in phases {
            for patch in &crates {
                let entry = match group_by {
                    GroupBy::Crate => data.entry(patch.full_name()),
                    GroupBy::Phase => data.entry(phase_name.to_string()),
                };

                entry
                    .or_insert(Recording::new())
                    .record(patch.run().get_pass(phase_name));
            }
        }

        DateData {
            date: day.commit.date,
            commit: day.commit.sha.clone(),
            data: data,
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Serialize, Deserialize)]
pub struct Recording {
    #[serde(with = "util::round_float")]
    pub time: f64,
    pub rss: u64,
}

impl Recording {
    fn new() -> Recording {
        Recording {
            time: 0.0,
            rss: 0,
        }
    }

    fn record(&mut self, phase: Option<&Pass>) {
        if let Some(phase) = phase {
            self.time += phase.time;
            self.rss = max(self.rss, phase.mem);
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GroupBy {
    #[serde(rename="crate")]
    Crate,
    #[serde(rename="phase")]
    Phase,
}

#[test]
fn serialize_kind() {
    assert_eq!(serde_json::to_string(&GroupBy::Crate).unwrap(),
               r#""crate""#);
    assert_eq!(serde_json::from_str::<GroupBy>(r#""crate""#).unwrap(),
               GroupBy::Crate);
    assert_eq!(serde_json::to_string(&GroupBy::Phase).unwrap(),
               r#""phase""#);
    assert_eq!(serde_json::from_str::<GroupBy>(r#""phase""#).unwrap(),
               GroupBy::Phase);
}

/// Data associated with a specific date
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DateData2 {
    pub date: Date,
    pub commit: String,
    pub data: HashMap<String, f64>,
}

impl DateData2 {
    pub fn for_day(
        day: &CommitData,
        crates: &BTreeSet<String>,
        stat: &str,
    ) -> DateData2 {
        let crates = day.benchmarks.values().filter(|v| v.is_ok())
            .flat_map(|patches| patches.as_ref().unwrap())
            .filter(|patch| crates.contains(&patch.full_name()))
            .collect::<Vec<_>>();

        let mut data = HashMap::new();
        for patch in &crates {
            if let Some(stat) = patch.run().get_stat(stat) {
                *data.entry(patch.full_name()).or_insert(0.0) += stat;
            }
        }

        DateData2 {
            date: day.commit.date,
            commit: day.commit.sha.clone(),
            data: data,
        }
    }
}

pub fn handle_summary(data: &InputData) -> summary::Response {
    fn summarize(comparison: &Comparison, stat: &str) -> DeltaTime {
        let mut sum = 0.0;
        let mut count = 0; // crate count
        for krate in comparison.by_crate.values() {
            count += 1;
            sum += krate.stats[stat] / 1000.0;
        }

        DeltaTime(sum / (count as f64))
    }

    fn breakdown(comparison: &Comparison, stat: &str) -> BTreeMap<String, DeltaTime> {
        let mut per_bench = BTreeMap::new();

        for (crate_name, krate) in &comparison.by_crate {
            let time = krate.stats[stat] / 1000.0;
            per_bench.insert(crate_name.to_string(), DeltaTime(time));
        }

        per_bench
    }

    let stat = "cpu-clock";
    let dates = data.summary.comparisons.iter().map(|c| c.b.date).collect::<Vec<_>>();

    // overall number for each week
    let summaries = data.summary.comparisons.iter().map(|x| summarize(x, stat)).collect();

    // per benchmark, per week
    let breakdown_data = data.summary.comparisons.iter().map(|x| breakdown(x, stat)).collect();

    summary::Response {
        total_summary: summarize(&data.summary.total, stat),
        total_breakdown: breakdown(&data.summary.total, stat),
        breakdown: breakdown_data,
        summaries: summaries,
        dates: dates,
        stat: stat.to_string(),
    }
}

pub fn handle_info(data: &InputData) -> info::Response {
    info::Response {
        crates: data.crate_list.clone(),
        phases: data.phase_list.clone(),
        stats: data.stats_list.clone(),
        as_of: data.last_date,
    }
}

pub fn handle_data(body: data::Request, data: &InputData) -> data::Response {
    let mut result = util::optional_data_range(data, body.start_date.clone(), body.end_date.clone())
        .map(|(_, day)| day)
        .map(|day| DateData::for_day(
            day,
            &body.crates.into_set(&data.crate_list),
            &body.phases.into_set(&data.phase_list),
            body.group_by
        ))
        .collect::<Vec<_>>();

    // Return everything from the first non-empty data to the last non-empty data.
    // Data may contain "holes" of empty data.
    let first_idx = result
        .iter()
        .position(|day| !day.data.is_empty())
        .unwrap_or(0);
    let last_idx = result
        .iter()
        .rposition(|day| !day.data.is_empty())
        .unwrap_or(0);
    let result = result.drain(first_idx..(last_idx + 1)).collect();
    data::Response {
        data: result,
        start: body.start_date.as_date(data.last_date),
        end: body.end_date.as_date(data.last_date),
        crates: body.crates.into_set(&data.crate_list),
        phases: body.phases.into_set(&data.phase_list),
    }
}

pub fn handle_data2(body: data::Request2, data: &InputData) -> data::Response2 {
    let mut result = util::optional_data_range(data, body.start_date.clone(), body.end_date.clone())
        .map(|(_, day)| day)
        .map(|day| DateData2::for_day(
            day,
            &body.crates.into_set(&data.crate_list),
            &body.stat,
        ))
        .collect::<Vec<_>>();

    // Return everything from the first non-empty data to the last non-empty data.
    // Data may contain "holes" of empty data.
    let first_idx = result
        .iter()
        .position(|day| !day.data.is_empty())
        .unwrap_or(0);
    let last_idx = result
        .iter()
        .rposition(|day| !day.data.is_empty())
        .unwrap_or(0);
    let result = result.drain(first_idx..(last_idx + 1)).collect();
    data::Response2 {
        data: result,
        start: body.start_date.as_date(data.last_date),
        end: body.end_date.as_date(data.last_date),
        crates: body.crates.into_set(&data.crate_list),
    }
}

pub fn handle_tabular(body: tabular::Request, data: &InputData) -> tabular::Response {
    let day = util::get_commit_data_from_end(data, body.date.as_date(data.last_date));

    let mut by_crate = HashMap::new();
    let patches = day.benchmarks.values().filter(|v| v.is_ok())
        .flat_map(|patches| patches.as_ref().unwrap());
    for patch in patches {
        let by_phase = by_crate.entry(patch.full_name()).or_insert_with(HashMap::new);
        for phase in &patch.run().passes {
            by_phase.insert(phase.name.clone(), Recording { time: phase.time, rss: phase.mem });
        }
    }

    tabular::Response {
        date: day.commit.date,
        commit: day.commit.sha.clone(),
        data: by_crate,
    }
}

pub fn handle_days(body: days::Request, data: &InputData) -> days::Response {
    days::Response {
        a: DateData::for_day(
            util::get_commit_data_from_start(data, body.date_a.as_date(data.last_date)),
            &body.crates.into_set(&data.crate_list),
            &body.phases.into_set(&data.phase_list),
            body.group_by
        ),
        b: DateData::for_day(
            util::get_commit_data_from_end(data, body.date_b.as_date(data.last_date)),
            &body.crates.into_set(&data.crate_list),
            &body.phases.into_set(&data.phase_list),
            body.group_by
        ),
    }
}

pub fn handle_stats(body: stats::Request, data: &InputData) -> stats::Response {
    let mut counted: HashMap<String, Vec<f64>> = HashMap::new();
    let mut start_date = body.start_date.as_date(data.last_date);
    let mut end_date = body.end_date.as_date(data.last_date);
    for (_, commit_data) in util::data_range(&data.data, start_date, end_date) {
        if counted.is_empty() {
            start_date = commit_data.commit.date;
        }
        end_date = commit_data.commit.date;
        let data = DateData::for_day(
            commit_data,
            &body.crates.into_set(&data.crate_list),
            &body.phases.into_set(&data.phase_list),
            body.group_by
        );
        for (name, rec) in data.data {
            counted.entry(name).or_insert_with(Vec::new).push(rec.time);
        }
    }

    let out = counted.into_iter().map(|(key, values)| {
        (key, Stats::from(&values))
    }).collect();

    stats::Response {
        start_date: start_date,
        end_date: end_date,
        data: out,
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Stats {
    first: f64,
    last: f64,
    min: f64,
    max: f64,
    mean: f64,
    variance: f64,
    #[serde(deserialize_with = "util::null_means_nan")]
    trend: f64,
    #[serde(deserialize_with = "util::null_means_nan")]
    trend_b: f64,
    n: usize,
}

impl Stats {
    fn from(sums: &[f64]) -> Stats {
        if sums.is_empty() {
            return Stats::default();
        }

        let first = sums[0];
        let last = *sums.last().unwrap();

        let mut min = first;
        let mut max = first;
        let q1_idx = sums.len() / 4;
        let q4_idx = 3 * sums.len() / 4;
        let mut total = 0.0;
        let mut q1_total = 0.0;
        let mut q4_total = 0.0;
        for (i, &cur) in sums.iter().enumerate() {
            min = min.min(cur);
            max = max.max(cur);

            total += cur;
            if i < q1_idx {
                // Within the first quartile
                q1_total += cur;
            }
            if i >= q4_idx {
                // Within the fourth quartile
                q4_total += cur;
            }
        }

        // Calculate the variance
        let mean = total / (sums.len() as f64);
        let mut var_total = 0.0;
        for sum in sums {
            let diff = sum - mean;
            var_total += diff * diff;
        }
        let variance = var_total / ((sums.len() - 1) as f64);

        let trend = if sums.len() >= 10 {
            let q1_mean = q1_total / (q1_idx as f64);
            let q4_mean = q4_total / ((sums.len() - q4_idx) as f64);
            100.0 * ((q4_mean - q1_mean) / first)
        } else {
            0.0
        };
        let trend_b = 100.0 * ((last - first) / first);

        Stats {
            first: first,
            last: last,
            min: min,
            max: max,
            mean: mean,
            variance: variance,
            trend: if trend.is_nan() { trend } else { 0.0 },
            trend_b: if trend_b.is_nan() { trend_b } else { 0.0 },
            n: sums.len(),
        }
    }
}

struct Server {
    data: Arc<RwLock<InputData>>,
    pool: CpuPool,
    updating: Arc<AtomicBool>,
}

impl Server {
    fn handle_get<F, S>(&self, req: &Request, handler: F) -> <Server as Service>::Future
        where F: FnOnce(&InputData) -> S,
              S: Serialize
    {
        assert_eq!(*req.method(), Get);
        let data = self.data.clone();
        let data = data.read().unwrap();
        let result = handler(&data);
        let response = Response::new()
            .with_header(ContentType::json())
            .with_body(serde_json::to_string(&result).unwrap());
        futures::future::ok(response).boxed()
    }

    fn handle_post<'de, F, D, S>(&self, req: Request, handler: F) -> <Server as Service>::Future
        where F: FnOnce(D, &InputData) -> S + Send + 'static,
              D: DeserializeOwned,
              S: Serialize,
    {
        assert_eq!(*req.method(), Post);
        let length = req.headers().get::<ContentLength>()
        .expect("content-length to exist").0;
        if length > 10_000 { // 10 kB
            return futures::future::err(hyper::Error::TooLarge).boxed();
        }
        let data = self.data.clone();
        self.pool.spawn_fn(move || {
            req.body().fold(Vec::new(), |mut acc, chunk| {
                acc.extend_from_slice(&*chunk);
                futures::future::ok::<_, <Self as Service>::Error>(acc)
            }).map(move |body| {
                let data = data.read().unwrap();
                let body: D = match serde_json::from_slice(&body) {
                    Ok(d) => d,
                    Err(err) => {
                        error!("failed to deserialize request {}: {:?}",
                            String::from_utf8_lossy(&body), err);
                        return Response::new()
                            .with_header(ContentType::plaintext())
                            .with_body(format!("Failed to deserialize request; {:?}", err))
                    }
                };
                let result = handler(body, &data);
                Response::new()
                    .with_header(ContentType::json())
                    .with_header(CacheControl(vec![
                            CacheDirective::NoCache,
                            CacheDirective::NoStore,
                    ]))
                    .with_body(serde_json::to_string(&result).unwrap())
            })
        }).boxed()
    }

    fn handle_push(&self, _req: Request) -> <Self as Service>::Future {
        // set to updating
        let was_updating = self.updating.compare_and_swap(false, true, Ordering::AcqRel);

        if was_updating {
            return futures::future::ok(Response::new()
                .with_body(format!("Already updating!"))
                .with_status(StatusCode::Ok)
                .with_header(ContentType(mime::TEXT_PLAIN_UTF_8)))
                .boxed();
        }

        // FIXME we are throwing everything away and starting again. It would be
        // better to read just the added files. These should be available in the
        // body of the request.

        debug!("received onpush hook");

        let rwlock = self.data.clone();
        let updating = self.updating.clone();
        let response = self.pool.spawn_fn(move || -> Result<serde_json::Value> {
            let repo_path = get_repo_path()?;

            git::update_repo(&repo_path)?;

            info!("updating from filesystem...");
            let new_data = InputData::from_fs(&repo_path)?;

            // Retrieve the stored InputData from the request.
            let mut data = rwlock.write().unwrap();

            // Write the new data back into the request
            *data = new_data;

            updating.store(false, Ordering::Release);

            Ok(serde_json::to_value("Successfully updated from filesystem")?)
        });

        let updating = self.updating.clone();
        response.map(|value| {
            Response::new().with_body(serde_json::to_string(&value).unwrap())
        }).or_else(move |err| {
            updating.store(false, Ordering::Release);
            futures::future::ok(Response::new()
                .with_body(format!("Internal Server Error: {:?}", err))
                .with_status(StatusCode::InternalServerError)
                .with_header(ContentType(mime::TEXT_PLAIN_UTF_8)))
        }).boxed()
    }
}

impl Service for Server {
    type Request = Request;
    type Response = Response;
    type Error = hyper::Error;
    type Future = Box<Future<Item = Self::Response, Error = Self::Error>>;

    fn call(&self, req: Request) -> Self::Future {
        let fs_path = format!("static{}", if req.path() == "" || req.path() == "/" {
            "/index.html"
        } else {
            req.path()
        });

        info!("handling: req.path()={:?}, fs_path={:?}", req.path(), fs_path);

        if fs_path.contains("./") | fs_path.contains("../") {
            return futures::future::ok(Response::new()
                .with_header(ContentType::html())
                .with_status(StatusCode::NotFound)).boxed();
        }

        if Path::new(&fs_path).is_file() {
            return self.pool.spawn_fn(move || {
                let mut f = File::open(&fs_path).unwrap();
                let mut source = Vec::new();
                f.read_to_end(&mut source).unwrap();
                futures::future::ok(Response::new().with_body(source))
            }).boxed();
        }

        match req.path() {
            "/perf/summary" => self.handle_get(&req, handle_summary),
            "/perf/info" => self.handle_get(&req, handle_info),
            "/perf/data" => self.handle_post(req, handle_data),
            "/perf/data2" => self.handle_post(req, handle_data2),
            "/perf/get_tabular" => self.handle_post(req, handle_tabular),
            "/perf/get" => self.handle_post(req, handle_days),
            "/perf/stats" => self.handle_post(req, handle_stats),
            "/perf/onpush" => self.handle_push(req),
            _ => {
                futures::future::ok(Response::new()
                    .with_header(ContentType::html())
                    .with_status(StatusCode::NotFound)).boxed()
            }
        }
    }
}

pub fn start(data: InputData) {
    let server = Arc::new(Server {
        data: Arc::new(RwLock::new(data)),
        pool: CpuPool::new_num_cpus(),
        updating: Arc::new(AtomicBool::new(false)),
    });
    let mut server_address: SocketAddr = "0.0.0.0:2346".parse().unwrap();
    server_address.set_port(env::var("PORT").ok().and_then(|x| x.parse().ok()).unwrap_or(2346));
    let server = Http::new().bind(&server_address, move || Ok(server.clone()));
    server.unwrap().run().unwrap();
}
