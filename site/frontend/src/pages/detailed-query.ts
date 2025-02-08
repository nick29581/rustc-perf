import {createUrlWithAppendedParams, getUrlParams} from "../utils/navigation";
import {postMsgpack} from "../utils/requests";
import {SELF_PROFILE_DATA_URL} from "../urls";
import {
  chromeProfileUrl,
  processedSelfProfileRelativeUrl,
} from "../self-profile";
import {openTraceInPerfetto} from "../perfetto";

function normalize_value(value) {
  return value;
}

function fmt_delta(to, delta, is_integral_delta) {
  let from = to - delta;
  let pct = ((to - from) / from) * 100;
  if (from == to) {
    pct = 0;
  }
  let classes;
  if (pct > 1) {
    classes = "positive";
  } else if (pct < -1) {
    classes = "negative";
  } else {
    classes = "neutral";
  }
  // some arbitrary "small" value
  // ignore this because it's not too interesting likely
  if (Math.abs(delta) <= 0.05) {
    classes = "neutral";
  }
  let text;
  if (is_integral_delta) {
    text = delta.toString();
  } else {
    text = delta.toFixed(3);
  }
  if (pct != Infinity && pct != -Infinity) {
    text += `(${pct.toFixed(1)}%)`.padStart(10, " ");
  } else {
    text += `-`.padStart(10, " ");
  }
  return `<span class="${classes}" title="${from.toFixed(3)} to ${to.toFixed(
    3
  )} ≈ ${delta.toFixed(3)}">${text}</span>`;
}

function raw_self_profile_link(
  commit: string,
  benchmark: string,
  scenario: string
): string {
  const url = `/perf/download-raw-self-profile?commit=${commit}&benchmark=${benchmark}&scenario=${scenario}`;
  return `<a href="${url}">raw</a>`;
}

function processed_link(
  commit: string,
  benchmark: string,
  scenario: string,
  type: string
): string {
  const url = processedSelfProfileRelativeUrl(
    commit,
    benchmark,
    scenario,
    type
  );
  return `<a href="${url}">${type}</a>`;
}

// FIXME: remove this hack and use the PerfettoLink component once this page is
// converted to Vue.
function perfetto_profiler_link(
  id: string,
  commit: string,
  benchmark: string,
  scenario: string
): string {
  let link = chromeProfileUrl(commit, benchmark, scenario);
  let traceTitle = `${benchmark}-${scenario} (${commit})`;
  document.addEventListener("click", (event) => {
    if ((event.target as HTMLElement).id === id) {
      openTraceInPerfetto(link, traceTitle);
    }
  });
  return `<a href="#" id="${id}">Perfetto</a>`;
}

function firefox_profiler_link(
  commit: string,
  benchmark: string,
  scenario: string
): string {
  let crox_url = encodeURIComponent(
    chromeProfileUrl(commit, benchmark, scenario)
  );
  let ff_url = `https://profiler.firefox.com/from-url/${crox_url}/marker-chart/?v=5`;
  return `<a href="${ff_url}">Firefox profiler</a>`;
}

function createDownloadLinks(
  state: Selector,
  commit: string,
  label: string
): string {
  const raw = raw_self_profile_link(
    state.commit,
    state.benchmark,
    state.scenario
  );
  const flamegraph_link = processed_link(
    commit,
    state.benchmark,
    state.scenario,
    "flamegraph"
  );
  const crox_link = processed_link(
    commit,
    state.benchmark,
    state.scenario,
    "crox"
  );
  const codegen = processed_link(
    commit,
    state.benchmark,
    state.scenario,
    "codegen-schedule"
  );
  const perfetto = perfetto_profiler_link(
    `perfetto-${label}`,
    commit,
    state.benchmark,
    state.scenario
  );
  const firefox = firefox_profiler_link(
    state.base_commit,
    state.benchmark,
    state.scenario
  );

  return `Download/view ${raw}, ${flamegraph_link}, ${crox_link}, ${codegen} (${perfetto}, ${firefox}) results for ${commit.substring(
    0,
    10
  )} (${label} commit)`;
}

function populate_data(data, state: Selector) {
  let txt = `${state.commit.substring(0, 10)}: Self profile results for ${
    state.benchmark
  } run ${state.scenario}`;
  if (state.base_commit) {
    let self_href = `/detailed-query.html?sort_idx=${state.sort_idx}&commit=${state.commit}&scenario=${state.scenario}&benchmark=${state.benchmark}`;
    let base_href = `/detailed-query.html?sort_idx=${state.sort_idx}&commit=${state.base_commit}&scenario=${state.scenario}&benchmark=${state.benchmark}`;
    txt += `<br>diff vs base ${state.base_commit.substring(
      0,
      10
    )}, <a href="${base_href}">query info for just base commit</a>`;
    txt += `<br><a href="${self_href}">query info for just this commit</a>`;
  }
  document.querySelector("#title").innerHTML = txt;

  txt = "";
  if (state.base_commit) {
    txt += createDownloadLinks(state, state.base_commit, "base");
    txt += "<br>";
  }
  txt += createDownloadLinks(state, state.commit, "new");

  // FIXME: use the Cachegrind Vue components once this page is refactored to Vue
  let profile = (b) =>
    b.endsWith("-opt")
      ? "Opt"
      : b.endsWith("-doc")
      ? "Doc"
      : b.endsWith("-debug")
      ? "Debug"
      : b.endsWith("-check")
      ? "Check"
      : "???";
  let bench_name = (b) => b.replace(/-[^-]*$/, "");
  let scenario_filter = (s) =>
    s == "full"
      ? "Full"
      : s == "incr-full"
      ? "IncrFull"
      : s == "incr-unchanged"
      ? "IncrUnchanged"
      : s.startsWith("incr-patched")
      ? "IncrPatched"
      : "???";
  if (state.base_commit) {
    txt += "<br>";
    txt += `Diff: <a
                        href="/perf/processed-self-profile?commit=${state.commit}&base_commit=${state.base_commit}&benchmark=${state.benchmark}&scenario=${state.scenario}&type=codegen-schedule"
                        >codegen-schedule</a>`;
    txt +=
      "<br>Local profile (base): <code>" +
      `./target/release/collector profile_local cachegrind
                    +${state.base_commit} --include ${bench_name(
        state.benchmark
      )} --profiles
                ${profile(state.benchmark)} --scenarios ${scenario_filter(
        state.scenario
      )}</code>`;
  }
  txt +=
    "<br>Local profile (new): <code>" +
    `./target/release/collector profile_local cachegrind
                +${state.commit} --include ${bench_name(
      state.benchmark
    )} --profiles
                ${profile(state.benchmark)} --scenarios ${scenario_filter(
      state.scenario
    )}</code>`;
  if (state.base_commit) {
    txt +=
      "<br>Local profile (diff): <code>" +
      `./target/release/collector profile_local cachegrind
                +${state.base_commit} --rustc2 +${
        state.commit
      } --include ${bench_name(state.benchmark)} --profiles
                ${profile(state.benchmark)} --scenarios ${scenario_filter(
        state.scenario
      )}</code>`;
  }
  document.querySelector("#raw-urls").innerHTML = txt;
  let sort_idx = state.sort_idx;
  if (!data.base_profile_delta) {
    document.body.classList.add("hide-delta");
  }
  let header = document.getElementById("table-header");
  for (let th of header.querySelectorAll("th") as any as HTMLElement[]) {
    // Skip non-sortable columns
    if (!th.attributes["data-sort-idx"]) {
      continue;
    }
    let idx = th.attributes["data-sort-idx"].value;
    if (idx == sort_idx) {
      th.setAttribute("data-sorted-by", "desc");
    } else if (idx == -sort_idx) {
      th.setAttribute("data-sorted-by", "asc");
    }
    let sortedBy = th.attributes["data-sorted-by"];
    let clickState = Object.assign({}, state);
    if (sortedBy && sortedBy.value == "desc") {
      clickState.sort_idx = -idx;
    } else if (sortedBy && sortedBy.value == "asc") {
      clickState.sort_idx = idx;
    } else {
      // start with descending
      if (th.attributes["data-default-sort-dir"].value == "1") {
        clickState.sort_idx = idx;
      } else {
        clickState.sort_idx = -idx;
      }
    }
    let inner = th.innerHTML;
    th.innerHTML = `<a href="${createUrlWithAppendedParams(
      clickState
    ).toString()}">${inner}</a>`;
  }

  if (!state.scenario.includes("incr-")) {
    // No need to show incremental columns if not showing
    // incremental data.
    document.body.classList.add("hide-incr");
  }

  let table = document.getElementById("primary-table");
  let idx = 0;
  for (let element of [data.profile.totals, ...data.profile.query_data]) {
    let cur = to_object(element);
    let delta = null;
    if (data.base_profile_delta) {
      if (idx == 0) {
        delta = data.base_profile_delta.totals;
      } else {
        delta = data.base_profile_delta.query_data[idx - 1];
      }
    }
    let row = document.createElement("tr");

    function td(row, content, is_delta = false) {
      let td = document.createElement("td");
      td.innerHTML = content;
      if (is_delta) {
        td.classList.add("delta");
      }
      row.appendChild(td);
      return td;
    }

    td(row, cur.label);
    if (cur.percent_total_time < 0) {
      td(row, "-").setAttribute(
        "title",
        "No wall-time stat collected for this run"
      );
    } else {
      let t = td(row, cur.percent_total_time.toFixed(2) + "%");
      if (idx == 0) {
        t.innerText += "*";
        t.setAttribute("title", "% of cpu-time stat");
      }
    }
    td(row, normalize_value(cur.self_time));
    if (delta) {
      td(
        row,
        fmt_delta(
          normalize_value(cur.self_time),
          normalize_value(delta.self_time),
          true
        ),
        true
      );
    } else {
      td(row, "-", true);
    }
    td(row, cur.invocation_count);
    if (delta) {
      td(
        row,
        fmt_delta(cur.invocation_count, delta.invocation_count, true),
        true
      );
    } else {
      td(row, "-", true);
    }
    td(row, normalize_value(cur.incremental_load_time)).classList.add("incr");
    if (delta) {
      td(
        row,
        fmt_delta(
          normalize_value(cur.incremental_load_time),
          normalize_value(delta.incremental_load_time),
          true
        ),
        true
      ).classList.add("incr");
    } else {
      td(row, "-", true).classList.add("incr");
    }
    table.appendChild(row);
    idx += 1;
  }

  let artifactTable = document.getElementById("artifact-body");

  function td(row, content) {
    let td = document.createElement("td");
    td.innerHTML = content;
    row.appendChild(td);
    return td;
  }

  for (let [idx, element] of data.profile.artifact_sizes.entries()) {
    let row = document.createElement("tr");
    const label = td(row, element.label);
    label.style.textAlign = "center";
    td(row, element.bytes);
    if (
      data.base_profile_delta &&
      data.base_profile_delta.artifact_sizes[idx]
    ) {
      td(row, data.base_profile_delta.artifact_sizes[idx].bytes);
    }
    artifactTable.appendChild(row);
    idx += 1;
  }
}

// https://stackoverflow.com/questions/6234773
function escapeHtml(unsafe) {
  return unsafe
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#039;");
}

function to_object(element) {
  if (!element.length) {
    element = [
      // escape html, to prevent rendering queries like <unknown> as tags
      escapeHtml(element.label),
      element.self_time,
      element.percent_total_time,
      element.number_of_cache_misses,
      element.number_of_cache_hits,
      element.invocation_count,
      element.blocked_time,
      element.incremental_load_time,
    ];
  }
  let [
    label,
    self_time,
    percent_total_time,
    _cache_misses,
    _cache_hits,
    invocation_count,
    _blocked_time,
    incremental_load_time,
  ] = element;
  return {
    label,
    self_time,
    percent_total_time,
    invocation_count,
    incremental_load_time,
  };
}

interface Selector {
  commit: string;
  base_commit: string | null;
  benchmark: string;
  scenario: string;
  sort_idx: string | number;
}

async function loadData() {
  const params = getUrlParams();
  const {commit, base_commit, benchmark, scenario, sort_idx} = params;
  const selector: Selector = {
    commit,
    base_commit: base_commit ?? null,
    benchmark,
    scenario,
    sort_idx: sort_idx ?? "-2",
  };

  const response = await postMsgpack(SELF_PROFILE_DATA_URL, selector);
  populate_data(response, selector);
}

loadData();
