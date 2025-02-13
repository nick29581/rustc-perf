pub mod client;
pub mod comparison_summary;

use crate::api::github::Commit;
use crate::load::{MissingReason, SiteCtxt, TryCommit};
use std::sync::LazyLock;
use std::time::Duration;

use serde::Deserialize;

type BoxedError = Box<dyn std::error::Error + Send + Sync>;

pub const RUST_REPO_GITHUB_API_URL: &str = "https://api.github.com/repos/rust-lang/rust";

/// Comments that are temporary and do not add any value once there has been a new development
/// (a rustc build or a perf. run was finished) are marked with this comment.
///
/// They are removed once a perf. run comparison summary is posted on a PR.
pub const COMMENT_MARK_TEMPORARY: &str = "<!-- rust-timer: temporary -->";

/// Used for comment that contains unrolled commits for merged rolled-up PRs.
pub const COMMENT_MARK_ROLLUP: &str = "<!-- rust-timer: rollup -->";

pub use comparison_summary::post_finished;
use database::Connection;

/// Enqueues try build artifacts and posts a message about them on the original rollup PR
pub async fn unroll_rollup(
    gh_client: client::Client,
    rollup_merges: impl Iterator<Item = &Commit>,
    previous_master: &str,
    rollup_pr_number: u32,
) -> Result<(), String> {
    let commit_link = |sha: &str| format!("https://github.com/rust-lang/rust/commit/{sha}");

    let format_commit = |s: &str, truncate: bool| {
        let display = truncate.then(|| s.split_at(10).0).unwrap_or(s);
        format!("[{display}]({})", commit_link(s))
    };

    // Sort rolled up commits by their PR number in ascending order, so that they have the
    // same ordering as in the rollup PR description.
    let mut unrolled_builds: Vec<UnrolledCommit> =
        enqueue_unrolled_try_builds(&gh_client, rollup_merges, previous_master).await?;
    // The number should really be an integer, but if not, we will just sort the "non-integer" PRs
    // first.
    unrolled_builds.sort_by_cached_key(|commit| commit.original_pr_number.parse::<u64>().ok());

    let mapping = unrolled_builds
        .into_iter()
        .fold(String::new(), |mut string, c| {
            use std::fmt::Write;
            let commit = c
                .sha
                .as_deref()
                .map(|s| {
                    // Format the SHA as a code block to make it easy to copy-paste verbatim
                    let link = commit_link(s);
                    format!("`{s}` ([link]({link}))")
                })
                .unwrap_or_else(|| {
                    let head = format_commit(&c.rolled_up_head, true);
                    format!("❌ conflicts merging '{head}' into previous master ❌")
                });
            let message = c
                .rollup_merge
                .message
                .split('\n')
                // Skip over "Rollup merge of ..." and an empty line
                .nth(2)
                .map(|m| {
                    if m.len() <= 60 {
                        m.to_string()
                    } else {
                        format!("{}…", m.split_at(59).0)
                    }
                })
                .unwrap_or_else(|| format!("#{}", c.original_pr_number))
                .replace('|', "\\|");
            writeln!(
                &mut string,
                "|#{pr}|{message}|{commit}|",
                pr = c.original_pr_number
            )
            .unwrap();
            string
        });
    let previous_master = format_commit(previous_master, true);
    let msg =
        format!("📌 Perf builds for each rolled up PR:\n\n\
        | PR# | Message | Perf Build Sha |\n|----|----|:-----:|\n\
        {mapping}\n\n*previous master*: {previous_master}\n\nIn the case of a perf regression, \
        run the following command for each PR you suspect might be the cause: `@rust-timer build $SHA`\n\
        {COMMENT_MARK_ROLLUP}");
    gh_client.post_comment(rollup_pr_number, msg).await;
    Ok(())
}

/// Enqueues try builds on the try-perf branch for every rollup merge in `rollup_merges`.
/// Returns a mapping between the rollup merge commit and the try build sha.
async fn enqueue_unrolled_try_builds<'a>(
    client: &client::Client,
    rollup_merges: impl Iterator<Item = &'a Commit>,
    previous_master: &str,
) -> Result<Vec<UnrolledCommit<'a>>, String> {
    let mut mapping = Vec::new();
    for rollup_merge in rollup_merges {
        // Grab the number of the rolled up PR from its commit message
        let original_pr_number = ROLLEDUP_PR_NUMBER
            .captures(&rollup_merge.message)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str())
            .ok_or_else(|| {
                format!(
                    "Could not get PR number from message: '{}'",
                    rollup_merge.message
                )
            })?;

        // Fetch the rollup merge commit which should have two parents.
        // The first parent is in the chain of rollup merge commits all the way back to `previous_master`.
        // The second parent is the head of the PR that was rolled up. We want the second parent.
        let commit = client.get_commit(&rollup_merge.sha).await.map_err(|e| {
            format!(
                "Error getting rollup merge commit '{}': {e:?}",
                rollup_merge.sha
            )
        })?;
        assert!(
            commit.parents.len() == 2,
            "What we thought was a merge commit was not a merge commit. sha: {}",
            rollup_merge.sha
        );
        let rolled_up_head = commit.parents[1].sha.clone();

        // Reset perf-tmp to the previous master
        client
            .update_branch("perf-tmp", previous_master)
            .await
            .map_err(|e| format!("Error updating perf-tmp with previous master: {e:?}"))?;

        // Try to merge in the rolled up PR's head commit into the previous master
        let sha = client
            .merge_branch(
                "perf-tmp",
                &rolled_up_head,
                &format!("Unrolled build for #{original_pr_number}\n{}", rollup_merge.message),
            )
            .await
            .map_err(|e| {
                format!("Error merging #{original_pr_number}'s commit '{rolled_up_head}' into perf-tmp: {e:?}")
            })?;

        // Handle success and merge conflicts
        match &sha {
            Some(s) => {
                // Force the `try-perf` branch to point to what the perf-tmp branch points to
                client
                    .update_branch("try-perf", s)
                    .await
                    .map_err(|e| format!("Error updating the try-perf branch: {e:?}"))?;
            }
            None => {
                // Merge conflict
                log::debug!(
                    "Could not create unrolled commit for #{original_pr_number}. \
                Merging the rolled up HEAD '{rolled_up_head}' into the previous master \
                '{previous_master}' leads to a merge conflict."
                );
            }
        };

        mapping.push(UnrolledCommit {
            original_pr_number,
            rollup_merge,
            rolled_up_head,
            sha,
        });
        // Wait to ensure there's enough time for GitHub to checkout these changes before they are overwritten
        tokio::time::sleep(std::time::Duration::from_secs(15)).await
    }

    Ok(mapping)
}

/// A commit representing a rolled up PR as if it had been merged into master directly
pub struct UnrolledCommit<'a> {
    /// The PR number that was rolled up
    pub original_pr_number: &'a str,
    /// The original rollup merge commit
    pub rollup_merge: &'a Commit,
    /// The HEAD commit for the rolled up PR
    pub rolled_up_head: String,
    /// The sha of the new unrolled merge commit. `None` when creation failed due to merge conflicts.
    pub sha: Option<String>,
}

static ROLLUP_PR_NUMBER: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^Auto merge of #(\d+)").unwrap());
static ROLLEDUP_PR_NUMBER: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^Rollup merge of #(\d+)").unwrap());

// Gets the pr number for the associated rollup PR message. Returns None if this is not a rollup PR
pub async fn rollup_pr_number(
    client: &client::Client,
    message: &str,
) -> Result<Option<u32>, String> {
    if !message.starts_with("Auto merge of") {
        return Ok(None);
    }

    let number = ROLLUP_PR_NUMBER
        .captures(message)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().parse::<u64>())
        .transpose()
        .map_err(|e| format!("Error parsing PR number from '{message}': {e:?}"))?;

    let number = match number {
        Some(n) => n,
        None => return Ok(None),
    };

    let issue = client
        .get_issue(number)
        .await
        .map_err(|e| format!("Error fetching PR #{number} {e:?}"))?;

    Ok(issue
        .labels
        .iter()
        .any(|l| l.name == "rollup")
        .then_some(issue.number))
}

pub async fn enqueue_shas(
    ctxt: &SiteCtxt,
    gh_client: &client::Client,
    pr_number: u32,
    commits: impl Iterator<Item = &str>,
) -> Result<(), String> {
    let mut msg = String::new();
    for commit in commits {
        let mut commit_response = gh_client
            .get_commit(commit)
            .await
            .map_err(|e| e.to_string())?;
        if commit_response.parents.len() != 2 {
            log::error!(
                "Bors try commit {} unexpectedly has {} parents.",
                commit_response.sha,
                commit_response.parents.len()
            );
            return Ok(());
        }
        let try_commit = TryCommit {
            sha: commit_response.sha,
            parent_sha: commit_response.parents.remove(0).sha,
        };
        let conn = ctxt.conn().await;
        let queued = conn
            .pr_attach_commit(
                pr_number,
                &try_commit.sha,
                &try_commit.parent_sha,
                Some(commit_response.commit.committer.date),
            )
            .await;
        if queued {
            if !msg.is_empty() {
                msg.push('\n');
            }

            let (preceding_artifacts, expected_duration) =
                estimate_queue_info(ctxt, conn.as_ref(), &try_commit).await;

            let verb = if preceding_artifacts == 1 {
                "is"
            } else {
                "are"
            };
            let suffix = if preceding_artifacts == 1 { "" } else { "s" };
            let queue_msg = format!(
                r#"There {verb} currently {preceding_artifacts} preceding artifact{suffix} in the [queue](https://perf.rust-lang.org/status.html).
It will probably take at least ~{:.1} hours until the benchmark run finishes."#,
                (expected_duration.as_secs_f64() / 3600.0)
            );

            msg.push_str(&format!(
                "Queued {} with parent {}, future [comparison URL]({}).\n{queue_msg}",
                try_commit.sha,
                try_commit.parent_sha,
                try_commit.comparison_url(),
            ));
        }
    }

    if !msg.is_empty() {
        msg.push_str(&format!("\n{COMMENT_MARK_TEMPORARY}"));
        gh_client.post_comment(pr_number, msg).await;
    }

    Ok(())
}

/// Counts how many artifacts are in the queue before the specified commit, and what is the expected
/// duration until the specified commit will be finished.
async fn estimate_queue_info(
    ctxt: &SiteCtxt,
    conn: &dyn Connection,
    commit: &TryCommit,
) -> (u64, Duration) {
    let queue = ctxt.missing_commits().await;

    // Queue without in-progress artifacts
    let queue_waiting: Vec<_> = queue
        .into_iter()
        .filter_map(|(c, reason)| match reason {
            MissingReason::InProgress(_) if c.sha != commit.sha => None,
            _ => Some(c),
        })
        .collect();

    // Measure expected duration of waiting artifacts
    // How many commits are waiting (i.e. not running) in the queue before the specified commit?
    let preceding_waiting = queue_waiting
        .iter()
        .position(|c| c.sha == commit.sha())
        .unwrap_or(queue_waiting.len().saturating_sub(1)) as u64;

    // Guess the expected full run duration of a waiting commit
    let last_duration = conn
        .last_n_artifact_collections(1)
        .await
        .into_iter()
        .next()
        .map(|collection| collection.duration)
        .unwrap_or(Duration::ZERO);

    // Guess that the duration will take about an hour if we don't have data or it's
    // suspiciously fast.
    let last_duration = last_duration.max(Duration::from_secs(3600));

    let mut expected_duration = last_duration * (preceding_waiting + 1) as u32;
    let mut preceding = preceding_waiting;

    // Add in-progress artifact duration and count (if any)
    if let Some(aid) = conn.in_progress_artifacts().await.pop() {
        preceding += 1;
        expected_duration += conn
            .in_progress_steps(&aid)
            .await
            .into_iter()
            .map(|s| s.expected.saturating_sub(s.duration))
            .sum();
    }
    (preceding, expected_duration)
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum HomuComment {
    TryBuildCompleted { merge_sha: String },
}

/// Parse comment from homu containing try build sha
pub async fn parse_homu_comment(comment_body: &str) -> Option<String> {
    if !comment_body.contains("Try build successful") {
        return None;
    }

    let start = "<!-- homu: ";
    let start_idx = comment_body.find(start)? + start.len();
    let end_idx = start_idx + comment_body[start_idx..].find(" -->")?;

    let sha = match serde_json::from_str(&comment_body[start_idx..end_idx]) {
        Ok(HomuComment::TryBuildCompleted { merge_sha }) => merge_sha,
        Err(err) => {
            log::warn!(
                "failed to parse try build result; comment: {:?}, part: {:?}, err: {:?}",
                comment_body,
                &comment_body[start_idx..end_idx],
                err
            );
            return None;
        }
    };

    Some(sha)
}

#[derive(serde::Deserialize)]
pub(crate) struct PullRequest {
    pub number: u64,
    pub title: String,
}

#[derive(serde::Deserialize)]
struct PullRequestResponse {
    items: Vec<PullRequest>,
}

/// Fetch all merged PRs that are labeled with `perf-regression` and not `perf-regression-triaged`
pub(crate) async fn untriaged_perf_regressions() -> Result<Vec<PullRequest>, BoxedError> {
    let url = "https://api.github.com/search/issues?q=repo:rust-lang/rust+label:perf-regression+-label:perf-regression-triaged+is:merged".to_owned();
    let request = github_request(&url);
    Ok(send_request::<PullRequestResponse>(request).await?.items)
}

/// Get the title of a PR with the given number
pub(crate) async fn pr_title(pr: u32) -> String {
    let url = format!("https://api.github.com/repos/rust-lang/rust/pulls/{}", pr);
    let request = github_request(&url);

    async fn send(request: reqwest::RequestBuilder) -> Result<String, BoxedError> {
        let body = send_request::<serde_json::Value>(request).await?;
        Ok(body
            .get("title")
            .ok_or_else(malformed_json_error)?
            .as_str()
            .ok_or_else(malformed_json_error)?
            .to_owned())
    }
    let request_dbg = format!("{:?}", request);
    match send(request).await {
        Ok(t) => t,
        Err(e) => {
            log::error!("Error fetching {}: {:?}", request_dbg, e);
            String::from("<UNKNOWN>")
        }
    }
}

fn github_request(url: &str) -> reqwest::RequestBuilder {
    let client = reqwest::Client::new();
    let mut request = client
        .get(url)
        .header("Content-Type", "application/json")
        .header("User-Agent", "rustc-perf");
    if let Ok(token) = std::env::var("GITHUB_TOKEN") {
        let mut value =
            reqwest::header::HeaderValue::from_str(&format!("token {}", token)).unwrap();
        value.set_sensitive(true);
        request = request.header("Authorization", value);
    }
    request
}

async fn send_request<T: serde::de::DeserializeOwned>(
    request: reqwest::RequestBuilder,
) -> Result<T, BoxedError> {
    let response = request.send().await?;
    match response.error_for_status_ref() {
        Ok(_) => {}
        Err(e) => {
            return Err(anyhow::anyhow!("response = {:?}", response)
                .context(e)
                .into())
        }
    }
    Ok(response.json::<T>().await?)
}

fn malformed_json_error() -> String {
    "JSON was malformed".to_owned()
}
