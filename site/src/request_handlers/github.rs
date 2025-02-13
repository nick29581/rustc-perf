use crate::api::{github, ServerResult};
use crate::github::{
    client, enqueue_shas, parse_homu_comment, rollup_pr_number, unroll_rollup,
    COMMENT_MARK_TEMPORARY, RUST_REPO_GITHUB_API_URL,
};
use crate::load::SiteCtxt;

use hashbrown::HashMap;
use std::sync::Arc;

pub async fn handle_github(
    request: github::Request,
    ctxt: Arc<SiteCtxt>,
) -> ServerResult<github::Response> {
    log::info!("handle_github({:?})", request);
    match request {
        github::Request::Issue { issue, comment } => handle_issue(ctxt, issue, comment).await,
        github::Request::Push(p) => handle_push(ctxt, p).await,
    }
}

async fn handle_push(ctxt: Arc<SiteCtxt>, push: github::Push) -> ServerResult<github::Response> {
    let gh_client = client::Client::from_ctxt(&ctxt, RUST_REPO_GITHUB_API_URL.to_owned());
    if push.r#ref != "refs/heads/master" || push.sender.login != "bors" {
        return Ok(github::Response);
    }
    let rollup_pr_number = match rollup_pr_number(&gh_client, &push.head_commit.message).await? {
        Some(pr) => pr,
        None => return Ok(github::Response),
    };

    let previous_master = push.before;
    let commits = push.commits;

    // GitHub webhooks have a timeout of 10 seconds, so we process this
    // in the background.
    tokio::spawn(async move {
        let rollup_merges = commits
            .iter()
            .rev()
            .filter(|c| c.message.starts_with("Rollup merge of #"));
        let result =
            unroll_rollup(gh_client, rollup_merges, &previous_master, rollup_pr_number).await;
        log::info!("Processing of rollup merge finished: {:#?}", result);
    });
    Ok(github::Response)
}

async fn handle_issue(
    ctxt: Arc<SiteCtxt>,
    issue: github::Issue,
    comment: github::Comment,
) -> ServerResult<github::Response> {
    let gh_client = client::Client::from_ctxt(&ctxt, RUST_REPO_GITHUB_API_URL.to_owned());
    if comment.body.contains(" homu: ") {
        if let Some(sha) = parse_homu_comment(&comment.body).await {
            enqueue_shas(
                &ctxt,
                &gh_client,
                issue.number,
                std::iter::once(sha.as_str()),
            )
            .await?;
            return Ok(github::Response);
        }
    }

    if comment.body.contains("@rust-timer ") {
        return handle_rust_timer(ctxt, &gh_client, comment, issue).await;
    }

    Ok(github::Response)
}

async fn handle_rust_timer(
    ctxt: Arc<SiteCtxt>,
    main_client: &client::Client,
    comment: github::Comment,
    issue: github::Issue,
) -> ServerResult<github::Response> {
    if comment.author_association != github::Association::Owner
        && !get_authorized_users().await?.contains(&comment.user.id)
    {
        main_client
            .post_comment(
                issue.number,
                format!(
                    "Insufficient permissions to issue commands to rust-timer.
{COMMENT_MARK_TEMPORARY}"
                ),
            )
            .await;
        return Ok(github::Response);
    }

    if let Some(queue) = parse_queue_command(&comment.body) {
        let msg = match queue {
            Ok(cmd) => {
                let conn = ctxt.conn().await;
                conn.queue_pr(
                    issue.number,
                    cmd.params.include,
                    cmd.params.exclude,
                    cmd.params.runs,
                    cmd.params.backends,
                )
                .await;
                format!(
                    "Awaiting bors try build completion.

@rustbot label: +S-waiting-on-perf

{COMMENT_MARK_TEMPORARY}"
                )
            }
            Err(error) => {
                format!("Error occurred while parsing comment: {error}")
            }
        };
        main_client.post_comment(issue.number, msg).await;
        return Ok(github::Response);
    }

    let mut valid_build_cmds = vec![];
    let mut errors = String::new();
    for cmd in parse_build_commands(&comment.body) {
        match cmd {
            Ok(cmd) => valid_build_cmds.push(cmd),
            Err(error) => errors.push_str(&format!("Cannot parse build command: {error}\n")),
        }
    }
    if !errors.is_empty() {
        main_client.post_comment(issue.number, errors).await;
        return Ok(github::Response);
    }

    {
        let conn = ctxt.conn().await;
        for command in &valid_build_cmds {
            conn.queue_pr(
                issue.number,
                command.params.include,
                command.params.exclude,
                command.params.runs,
                command.params.backends,
            )
            .await;
        }
    }

    enqueue_shas(
        &ctxt,
        main_client,
        issue.number,
        valid_build_cmds.iter().map(|c| c.sha),
    )
    .await?;

    Ok(github::Response)
}

/// Parses the first occurrence of a `@rust-timer queue <shared-args>` command
/// in the input string.
fn parse_queue_command(body: &str) -> Option<Result<QueueCommand, String>> {
    let args = get_command_lines(body, "queue").next()?;
    let args = match parse_command_arguments(args) {
        Ok(args) => args,
        Err(error) => return Some(Err(error)),
    };
    let params = match parse_benchmark_parameters(args) {
        Ok(params) => params,
        Err(error) => return Some(Err(error)),
    };

    Some(Ok(QueueCommand { params }))
}

/// Parses all occurrences of a `@rust-timer build <shared-args>` command in the input string.
fn parse_build_commands(body: &str) -> impl Iterator<Item = Result<BuildCommand, String>> {
    get_command_lines(body, "build").map(|line| {
        let mut iter = line.splitn(2, ' ');
        let Some(sha) = iter.next().filter(|s| !s.is_empty() && !s.contains('=')) else {
            return Err("Missing SHA in build command".to_string());
        };

        let sha = sha.trim_start_matches("https://github.com/rust-lang/rust/commit/");
        let args = iter.next().unwrap_or("");
        let args = parse_command_arguments(args)?;
        let params = parse_benchmark_parameters(args)?;
        Ok(BuildCommand { sha, params })
    })
}

fn get_command_lines<'a>(body: &'a str, command: &'a str) -> impl Iterator<Item = &'a str> {
    let prefix = "@rust-timer";
    body.lines()
        .filter_map(move |line| {
            line.find(prefix)
                .map(|index| line[index + prefix.len()..].trim())
        })
        .filter_map(move |line| line.strip_prefix(command))
        .map(|l| l.trim_start())
}

fn parse_benchmark_parameters<'a>(
    mut args: HashMap<&'a str, &'a str>,
) -> Result<BenchmarkParameters<'a>, String> {
    let mut params = BenchmarkParameters {
        include: args.remove("include").filter(|s| !s.is_empty()),
        exclude: args.remove("exclude").filter(|s| !s.is_empty()),
        runs: None,
        backends: args.remove("backends").filter(|s| !s.is_empty()),
    };
    if let Some(runs) = args.remove("runs").filter(|s| !s.is_empty()) {
        let Ok(runs) = runs.parse::<u32>() else {
            return Err(format!("Cannot parse runs {runs} as a number"));
        };
        params.runs = Some(runs as i32);
    }

    if !args.is_empty() {
        Err(format!(
            "Unknown command argument(s) `{}`",
            args.into_keys().collect::<Vec<_>>().join(",")
        ))
    } else {
        Ok(params)
    }
}

/// Parses command arguments from a single line of text.
/// Expects that arguments are separated by whitespace, and each argument
/// has the format `<key>=<value>`.
fn parse_command_arguments(args: &str) -> Result<HashMap<&str, &str>, String> {
    let mut argmap = HashMap::new();
    for arg in args.split_whitespace() {
        let Some((key, value)) = arg.split_once('=') else {
            return Err(format!(
                "Invalid command argument `{arg}` (there may be no spaces around the `=` character)"
            ));
        };
        let key = key.trim();
        let value = value.trim();
        if argmap.insert(key, value).is_some() {
            return Err(format!("Duplicate command argument `{key}`"));
        }
    }

    Ok(argmap)
}

#[derive(Debug)]
struct QueueCommand<'a> {
    params: BenchmarkParameters<'a>,
}

#[derive(Debug)]
struct BuildCommand<'a> {
    sha: &'a str,
    params: BenchmarkParameters<'a>,
}

#[derive(Debug)]
struct BenchmarkParameters<'a> {
    include: Option<&'a str>,
    exclude: Option<&'a str>,
    runs: Option<i32>,
    backends: Option<&'a str>,
}

pub async fn get_authorized_users() -> Result<Vec<u64>, String> {
    let url = format!("{}/permissions/perf.json", ::rust_team_data::v1::BASE_URL);
    let client = reqwest::Client::new();
    client
        .get(&url)
        .send()
        .await
        .map_err(|err| format!("failed to fetch authorized users: {}", err))?
        .error_for_status()
        .map_err(|err| format!("failed to fetch authorized users: {}", err))?
        .json::<rust_team_data::v1::Permission>()
        .await
        .map_err(|err| format!("failed to fetch authorized users: {}", err))
        .map(|perms| perms.github_ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_command_missing() {
        assert!(get_build_commands("").is_empty());
    }

    #[test]
    fn build_unknown_command() {
        assert!(get_build_commands("@rust-timer foo").is_empty());
    }

    #[test]
    fn build_command_missing_sha() {
        insta::assert_compact_debug_snapshot!(get_build_commands("@rust-timer build"),
            @r###"[Err("Missing SHA in build command")]"###);
    }

    #[test]
    fn build_command() {
        insta::assert_compact_debug_snapshot!(get_build_commands("@rust-timer build 5832462aa1d9373b24ace96ad2c50b7a18af9952"),
            @r#"[Ok(BuildCommand { sha: "5832462aa1d9373b24ace96ad2c50b7a18af9952", params: BenchmarkParameters { include: None, exclude: None, runs: None, backends: None } })]"#);
    }

    #[test]
    fn build_command_multiple() {
        insta::assert_compact_debug_snapshot!(get_build_commands(r#"
@rust-timer build 5832462aa1d9373b24ace96ad2c50b7a18af9952
@rust-timer build 23936af287657fa4148aeab40cc2a780810fae52
"#),
            @r#"[Ok(BuildCommand { sha: "5832462aa1d9373b24ace96ad2c50b7a18af9952", params: BenchmarkParameters { include: None, exclude: None, runs: None, backends: None } }), Ok(BuildCommand { sha: "23936af287657fa4148aeab40cc2a780810fae52", params: BenchmarkParameters { include: None, exclude: None, runs: None, backends: None } })]"#);
    }

    #[test]
    fn build_command_unknown_arg() {
        insta::assert_compact_debug_snapshot!(get_build_commands("@rust-timer build foo=bar"),
            @r###"[Err("Missing SHA in build command")]"###);
    }

    #[test]
    fn build_command_complex() {
        insta::assert_compact_debug_snapshot!(get_build_commands("  @rust-timer  build    sha123456  exclude=baz    include=foo,bar runs=4"),
            @r#"[Ok(BuildCommand { sha: "sha123456", params: BenchmarkParameters { include: Some("foo,bar"), exclude: Some("baz"), runs: Some(4), backends: None } })]"#);
    }

    #[test]
    fn build_command_link() {
        insta::assert_compact_debug_snapshot!(get_build_commands(r#"
@rust-timer build https://github.com/rust-lang/rust/commit/323f521bc6d8f2b966ba7838a3f3ee364e760b7e"#),
            @r#"[Ok(BuildCommand { sha: "323f521bc6d8f2b966ba7838a3f3ee364e760b7e", params: BenchmarkParameters { include: None, exclude: None, runs: None, backends: None } })]"#);
    }

    #[test]
    fn queue_command_missing() {
        assert!(parse_queue_command("").is_none());
    }

    #[test]
    fn queue_unknown_command() {
        assert!(parse_queue_command("@rust-timer foo").is_none());
    }

    #[test]
    fn queue_command() {
        insta::assert_compact_debug_snapshot!(parse_queue_command("@rust-timer queue"),
            @"Some(Ok(QueueCommand { params: BenchmarkParameters { include: None, exclude: None, runs: None, backends: None } }))");
    }

    #[test]
    fn queue_command_unknown_arg() {
        insta::assert_compact_debug_snapshot!(parse_queue_command("@rust-timer queue foo=bar"),
            @r###"Some(Err("Unknown command argument(s) `foo`"))"###);
    }

    #[test]
    fn queue_command_duplicate_arg() {
        insta::assert_compact_debug_snapshot!(parse_queue_command("@rust-timer queue include=a exclude=c include=b"),
            @r###"Some(Err("Duplicate command argument `include`"))"###);
    }

    #[test]
    fn queue_command_include() {
        insta::assert_compact_debug_snapshot!(parse_queue_command("@rust-timer queue include=abcd,feih"),
            @r#"Some(Ok(QueueCommand { params: BenchmarkParameters { include: Some("abcd,feih"), exclude: None, runs: None, backends: None } }))"#);
    }

    #[test]
    fn queue_command_exclude() {
        insta::assert_compact_debug_snapshot!(parse_queue_command("@rust-timer queue exclude=foo134,barzbaz41baf"),
            @r#"Some(Ok(QueueCommand { params: BenchmarkParameters { include: None, exclude: Some("foo134,barzbaz41baf"), runs: None, backends: None } }))"#);
    }

    #[test]
    fn queue_command_runs() {
        insta::assert_compact_debug_snapshot!(parse_queue_command("@rust-timer queue runs=5"),
            @"Some(Ok(QueueCommand { params: BenchmarkParameters { include: None, exclude: None, runs: Some(5), backends: None } }))");
    }

    #[test]
    fn queue_command_runs_nan() {
        insta::assert_compact_debug_snapshot!(parse_queue_command("@rust-timer queue runs=xxx"),
            @r###"Some(Err("Cannot parse runs xxx as a number"))"###);
    }

    #[test]
    fn queue_command_combination() {
        insta::assert_compact_debug_snapshot!(parse_queue_command("@rust-timer queue include=acda,13asd exclude=c13,DA runs=5"),
            @r#"Some(Ok(QueueCommand { params: BenchmarkParameters { include: Some("acda,13asd"), exclude: Some("c13,DA"), runs: Some(5), backends: None } }))"#);
    }

    #[test]
    fn queue_command_argument_spaces() {
        insta::assert_compact_debug_snapshot!(parse_queue_command("@rust-timer queue include  =  abcd,das"),
            @r###"Some(Err("Invalid command argument `include` (there may be no spaces around the `=` character)"))"###);
    }

    #[test]
    fn queue_command_spaces() {
        insta::assert_compact_debug_snapshot!(parse_queue_command("@rust-timer     queue     include=abcd,das   "),
            @r#"Some(Ok(QueueCommand { params: BenchmarkParameters { include: Some("abcd,das"), exclude: None, runs: None, backends: None } }))"#);
    }

    #[test]
    fn queue_command_with_bors() {
        insta::assert_compact_debug_snapshot!(parse_queue_command("@bors try @rust-timer queue include=foo,bar"),
            @r#"Some(Ok(QueueCommand { params: BenchmarkParameters { include: Some("foo,bar"), exclude: None, runs: None, backends: None } }))"#);
    }

    #[test]
    fn queue_command_parameter_order() {
        insta::assert_compact_debug_snapshot!(parse_queue_command("@rust-timer queue runs=3 exclude=c,a include=b"),
            @r#"Some(Ok(QueueCommand { params: BenchmarkParameters { include: Some("b"), exclude: Some("c,a"), runs: Some(3), backends: None } }))"#);
    }

    #[test]
    fn queue_command_multiline() {
        insta::assert_compact_debug_snapshot!(parse_queue_command(r#"Ok, this looks good now.
Let's do a perf run quickly and then we can merge it.

@bors try @rust-timer queue include=foo,bar

Otherwise LGTM."#),
            @r#"Some(Ok(QueueCommand { params: BenchmarkParameters { include: Some("foo,bar"), exclude: None, runs: None, backends: None } }))"#);
    }

    fn get_build_commands(body: &str) -> Vec<Result<BuildCommand, String>> {
        parse_build_commands(body).collect()
    }

    #[test]
    fn build_command_with_backends() {
        insta::assert_compact_debug_snapshot!(get_build_commands(r#"@rust-timer build 5832462aa1d9373b24ace96ad2c50b7a18af995G"#),
            @r#"[Ok(BuildCommand { sha: "5832462aa1d9373b24ace96ad2c50b7a18af995G", params: BenchmarkParameters { include: None, exclude: None, runs: None, backends: None } })]"#);
        insta::assert_compact_debug_snapshot!(get_build_commands(r#"@rust-timer build 5832462aa1d9373b24ace96ad2c50b7a18af995A backends=Llvm"#),
            @r#"[Ok(BuildCommand { sha: "5832462aa1d9373b24ace96ad2c50b7a18af995A", params: BenchmarkParameters { include: None, exclude: None, runs: None, backends: Some("Llvm") } })]"#);
        insta::assert_compact_debug_snapshot!(get_build_commands(r#"@rust-timer build 23936af287657fa4148aeab40cc2a780810fae5B backends=Cranelift"#),
            @r#"[Ok(BuildCommand { sha: "23936af287657fa4148aeab40cc2a780810fae5B", params: BenchmarkParameters { include: None, exclude: None, runs: None, backends: Some("Cranelift") } })]"#);
        insta::assert_compact_debug_snapshot!(get_build_commands(r#"@rust-timer build 23936af287657fa4148aeab40cc2a780810fae5C backends=Cranelift,Llvm"#),
            @r#"[Ok(BuildCommand { sha: "23936af287657fa4148aeab40cc2a780810fae5C", params: BenchmarkParameters { include: None, exclude: None, runs: None, backends: Some("Cranelift,Llvm") } })]"#);
        insta::assert_compact_debug_snapshot!(get_build_commands(r#"@rust-timer build 5832462aa1d9373b24ace96ad2c50b7a18af995D include=hello backends=Llvm"#),
            @r#"[Ok(BuildCommand { sha: "5832462aa1d9373b24ace96ad2c50b7a18af995D", params: BenchmarkParameters { include: Some("hello"), exclude: None, runs: None, backends: Some("Llvm") } })]"#);
        insta::assert_compact_debug_snapshot!(get_build_commands(r#"@rust-timer build 5832462aa1d9373b24ace96ad2c50b7a18af995E runs=10 backends=Llvm"#),
            @r#"[Ok(BuildCommand { sha: "5832462aa1d9373b24ace96ad2c50b7a18af995E", params: BenchmarkParameters { include: None, exclude: None, runs: Some(10), backends: Some("Llvm") } })]"#);
    }

    #[test]
    fn queue_command_with_backends() {
        insta::assert_compact_debug_snapshot!(parse_queue_command("@rust-timer queue backends=Llvm"),
            @r#"Some(Ok(QueueCommand { params: BenchmarkParameters { include: None, exclude: None, runs: None, backends: Some("Llvm") } }))"#);
        insta::assert_compact_debug_snapshot!(parse_queue_command("@rust-timer queue backends=Cranelift"),
            @r#"Some(Ok(QueueCommand { params: BenchmarkParameters { include: None, exclude: None, runs: None, backends: Some("Cranelift") } }))"#);
        insta::assert_compact_debug_snapshot!(parse_queue_command("@rust-timer queue backends=Cranelift,Llvm"),
            @r#"Some(Ok(QueueCommand { params: BenchmarkParameters { include: None, exclude: None, runs: None, backends: Some("Cranelift,Llvm") } }))"#);
        insta::assert_compact_debug_snapshot!(parse_queue_command("@rust-timer queue"),
            @"Some(Ok(QueueCommand { params: BenchmarkParameters { include: None, exclude: None, runs: None, backends: None } }))");
        insta::assert_compact_debug_snapshot!(parse_queue_command("@rust-timer queue include=hello backends=Llvm"),
            @r#"Some(Ok(QueueCommand { params: BenchmarkParameters { include: Some("hello"), exclude: None, runs: None, backends: Some("Llvm") } }))"#);
        insta::assert_compact_debug_snapshot!(parse_queue_command("@rust-timer queue include=hello exclude=ripgrep runs=3 backends=Llvm"),
            @r#"Some(Ok(QueueCommand { params: BenchmarkParameters { include: Some("hello"), exclude: Some("ripgrep"), runs: Some(3), backends: Some("Llvm") } }))"#);
    }

    #[test]
    fn no_empty_arguments_thank_you() {
        insta::assert_compact_debug_snapshot!(parse_queue_command("@rust-timer queue include="),
            @"Some(Ok(QueueCommand { params: BenchmarkParameters { include: None, exclude: None, runs: None, backends: None } }))");
        insta::assert_compact_debug_snapshot!(parse_queue_command("@rust-timer queue exclude="),
            @"Some(Ok(QueueCommand { params: BenchmarkParameters { include: None, exclude: None, runs: None, backends: None } }))");
        insta::assert_compact_debug_snapshot!(parse_queue_command("@rust-timer queue runs="),
            @"Some(Ok(QueueCommand { params: BenchmarkParameters { include: None, exclude: None, runs: None, backends: None } }))");
        insta::assert_compact_debug_snapshot!(parse_queue_command("@rust-timer queue backends="),
            @"Some(Ok(QueueCommand { params: BenchmarkParameters { include: None, exclude: None, runs: None, backends: None } }))");
    }
}
