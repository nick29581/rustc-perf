use std::{str::FromStr, sync::Arc};

use database::{
    metric::Metric,
    selector::{BenchmarkQuery, CompileBenchmarkQuery},
    ArtifactId, Connection,
};
use ratatui::{
    crossterm::event::{self, Event, KeyCode},
    prelude::*,
    widgets::{Block, Paragraph, Row, Table, TableState},
};
use tabled::{Table as TTable, Tabled};

static ALL_METRICS: &[Metric] = &[
    Metric::InstructionsUser,
    Metric::Cycles,
    Metric::WallTime,
    Metric::MaxRSS,
    Metric::LinkedArtifactSize,
    Metric::AssemblyFileSize,
    Metric::BranchMisses,
    Metric::CacheMisses,
    Metric::CodegenUnitLlvmIrCount,
    Metric::CodegenUnitSize,
    Metric::ContextSwitches,
    Metric::CpuClock,
    Metric::CpuClockUser,
    Metric::CrateMetadataSize,
    Metric::CyclesUser,
    Metric::DepGraphSize,
    Metric::DocByteSize,
    Metric::DwoFileSize,
    Metric::Faults,
    Metric::FaultsUser,
    Metric::LlvmBitcodeSize,
    Metric::LlvmIrSize,
    Metric::ObjectFileSize,
    Metric::QueryCacheSize,
    Metric::TaskClock,
    Metric::TaskClockUser,
    Metric::WorkProductIndexSize,
];

/// Compare 2 artifacts and print the result.
pub async fn compare_artifacts(
    mut conn: Box<dyn Connection>,
    metric: Option<Metric>,
    base: Option<String>,
    modified: Option<String>,
) -> anyhow::Result<()> {
    let index = database::Index::load(&mut *conn).await;

    let metric = match metric {
        Some(v) => v,
        None => {
            let metric_str = inquire::Select::new(
                "Choose metric:",
                ALL_METRICS.iter().map(|m| m.as_str()).collect::<Vec<_>>(),
            )
            .prompt()?;
            Metric::from_str(metric_str).map_err(|e| anyhow::anyhow!(e))?
        }
    };

    let mut aids = index.artifacts().map(str::to_string).collect::<Vec<_>>();
    if aids.len() < 2 {
        return Err(anyhow::anyhow!(
            "There are not enough artifacts to compare, at least two are needed"
        ));
    }

    let select_artifact_id = |name: &str, aids: &Vec<String>| {
        anyhow::Ok(
            inquire::Select::new(
                &format!("Choose {} artifact to compare:", name),
                aids.clone(),
            )
            .prompt()?,
        )
    };

    let base = match base {
        Some(v) => v,
        None => select_artifact_id("base", &aids)?.to_string(),
    };
    aids.retain(|id| id != &base);
    let modified = if aids.len() == 1 {
        let new_modified = aids[0].clone();
        println!(
            "Only 1 artifact remains, automatically selecting: {}",
            new_modified
        );

        new_modified
    } else {
        match modified {
            Some(v) => v,
            None => select_artifact_id("modified", &aids)?.to_string(),
        }
    };

    let query = CompileBenchmarkQuery::default().metric(database::selector::Selector::One(metric));
    let resp = query
        .execute(
            &mut *conn,
            &index,
            Arc::new(vec![ArtifactId::Tag(base), ArtifactId::Tag(modified)]),
        )
        .await
        .unwrap();

    let tuple_pstats = resp
        .iter()
        .map(|resp| {
            let points = resp.series.points.clone().collect::<Vec<_>>();
            (points[0], points[1])
        })
        .collect::<Vec<_>>();

    #[derive(Tabled)]
    struct Regression {
        count: usize,
        #[tabled(display_with = "display_range")]
        range: (Option<f64>, Option<f64>),
        #[tabled(display_with = "display_mean")]
        mean: Option<f64>,
    }

    fn format_value(value: Option<f64>) -> String {
        match value {
            Some(value) => format!("{:+.2}%", value),
            None => "-".to_string(),
        }
    }

    fn display_range(&(min, max): &(Option<f64>, Option<f64>)) -> String {
        format!("[{}, {}]", &format_value(min), &format_value(max))
    }

    fn display_mean(value: &Option<f64>) -> String {
        match value {
            Some(value) => format!("{:+.2}%", value),
            None => "-".to_string(),
        }
    }

    impl From<&Vec<f64>> for Regression {
        fn from(value: &Vec<f64>) -> Self {
            let min = value.iter().copied().min_by(|a, b| a.total_cmp(b));
            let max = value.iter().copied().max_by(|a, b| a.total_cmp(b));
            let count = value.len();

            Regression {
                range: (min, max),
                count,
                mean: if count == 0 {
                    None
                } else {
                    Some(value.iter().sum::<f64>() / count as f64)
                },
            }
        }
    }

    let change = tuple_pstats
        .iter()
        .filter_map(|&(a, b)| match (a, b) {
            (Some(a), Some(b)) => {
                if a == 0.0 {
                    None
                } else {
                    Some((b - a) / a * 100.0)
                }
            }
            (_, _) => None,
        })
        .collect::<Vec<_>>();
    let negative_change = change
        .iter()
        .copied()
        .filter(|&c| c < 0.0)
        .collect::<Vec<_>>();
    let positive_change = change
        .iter()
        .copied()
        .filter(|&c| c > 0.0)
        .collect::<Vec<_>>();

    #[derive(Tabled)]
    struct NamedRegression {
        name: String,
        #[tabled(inline)]
        regression: Regression,
    }

    let regressions = [negative_change, positive_change, change]
        .into_iter()
        .map(|c| Regression::from(&c))
        .zip(["❌", "✅", "✅, ❌"])
        .map(|(c, label)| NamedRegression {
            name: label.to_string(),
            regression: c,
        })
        .collect::<Vec<_>>();

    let regressions_table = TTable::new(regressions).to_string();

    #[derive(Default)]
    struct State {
        table_state: TableState,
    }

    let mut terminal = ratatui::init();
    let mut state = State::default();
    state.table_state.select(Some(0));
    loop {
        terminal
            .draw(|frame: &mut Frame| {
                let layout = Layout::default()
                    .direction(Direction::Vertical)
                    .margin(1)
                    .constraints(
                        [
                            Constraint::Min(regressions_table.lines().count() as u16),
                            Constraint::Percentage(100),
                        ]
                        .as_ref(),
                    )
                    .split(frame.area());
                frame.render_widget(
                    Paragraph::new(Text::raw(regressions_table.clone()))
                        .block(Block::bordered().title("Regression")),
                    layout[0],
                );
                let header = Row::new(vec![
                    "Benchmark",
                    "Profile",
                    "Scenario",
                    "Backend",
                    "Change",
                ]);
                let widths = [30, 10, 40, 20, 20];
                let mut rows = vec![];
                for x in resp.iter() {
                    let points = x.series.points.clone().collect::<Vec<_>>();
                    let change = match (points[0], points[1]) {
                        (Some(base), Some(modified)) => {
                            if base == 0.0 {
                                None
                            } else {
                                Some((modified - base) / base * 100.0)
                            }
                        }
                        (_, _) => None,
                    };
                    let profile = x.test_case.profile.to_string();
                    let scenario = x.test_case.scenario.to_string();
                    let backend = x.test_case.backend.to_string();
                    rows.push(vec![
                        x.test_case.benchmark.to_string(),
                        profile,
                        scenario,
                        backend,
                        change
                            .map(|c| format!("{:+.2}%", c))
                            .unwrap_or("-".to_string()),
                    ]);
                }

                rows.sort_by(|a, b| {
                    let a = a[4].trim_end_matches("%").parse::<f64>().unwrap_or(0.0);
                    let b = b[4].trim_end_matches("%").parse::<f64>().unwrap_or(0.0);
                    b.abs().partial_cmp(&a.abs()).unwrap()
                });

                frame.render_stateful_widget(
                    Table::new(
                        rows.into_iter()
                            .map(|v| {
                                let change =
                                    v[4].trim_end_matches("%").parse::<f64>().unwrap_or(0.0);
                                let mut cells =
                                    v.into_iter().map(|c| Span::from(c)).collect::<Vec<_>>();
                                if change < 0.0 {
                                    cells[4] = cells[4].clone().green();
                                } else {
                                    cells[4] = cells[4].clone().red();
                                }

                                Row::new(cells)
                            })
                            .collect::<Vec<Row>>(),
                        widths,
                    )
                    .header(header)
                    .block(Block::bordered().title("Benchmarks"))
                    .column_spacing(1)
                    .highlight_symbol("> ")
                    .highlight_style(Style::new().bg(Color::DarkGray).fg(Color::Black)),
                    layout[1],
                    &mut state.table_state,
                );
            })
            .expect("failed to draw frame");

        match event::read()? {
            Event::Key(key_event) => match key_event.code {
                KeyCode::Char('q') | KeyCode::Esc => break,
                KeyCode::Down => {
                    state.table_state.select_next();
                }
                KeyCode::Up => {
                    state.table_state.select_previous();
                }
                _ => {}
            },
            _ => {}
        }
    }
    ratatui::restore();

    Ok(())
}
