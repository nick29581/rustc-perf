# The Compile-time Benchmark Suite

This file describes the programs in the compile-time benchmark suite and explains why they
were included.

The suite changes over time. Sometimes the code for a benchmark is updated, in
which case a small suffix will be added (starting with "-2", then "-3", and so
on.)

There are three categories of compile-time benchmarks, **Primary**, **Secondary**, and
**Stable**.

## Primary

These are real programs that are important in some way, and worth tracking.
They mostly consist of real-world crates.

- **bitmaps-3.1.0**: A bitmaps implementation. Stresses the compiler's trait
  handling by implementing a trait `Bits` for the type `BitsImpl<N>` for every
  `N` value from 1 to 1024.
- **bitmaps-3.2.1**: A bitmaps implementation. Stresses the compiler's trait
  handling by implementing a trait `Bits` for the type `BitsImpl<N>` for every
  `N` value from 1 to 1024.
- **cargo-0.60.0**: The Rust package manager. A large program, and an important
  part of the Rust ecosystem.
- **clap-3.1.6**: A command line argument parser library. A crate used by many
  Rust programs.
- **cranelift-codegen-0.82.1**: The largest crate from a code generator. Used by
  wasmtime. Stresses obligation processing.
- **diesel-1.4.8**: A type safe SQL query builder. Utilizes the type system to
  ensure a lot of invariants. Stresses anything related to resolving
  trait bounds, by having a lot of trait impls for a large number of different
  types.
- **exa-0.10.1**: An `ls` replacement. A widely-used utility, and a binary
  crate.
- **helloworld**: A trivial program. Gives a lower bound on compile time.
- **html5ever-0.26.0**: An HTML parser. Stresses macro parsing code.
- **hyper-0.14.18**: A fairly large crate. Utilizes async/await, and used by
  many Rust programs. The crate uses cargo features to enable large portions of its
  structure and is built with `--features=client,http1,http2,server,stream`.
- **image-0.24.1**: Basic image processing functions and methods for 
  converting to and from various image formats. Used often in graphics 
  programming.
- **libc-0.2.124**: An interface to `libc`. Contains many declarations of
  types, constants, and functions, but relatively little normal code. Stresses
  the parser. A very widely-used crate.
- **nalgebra-0.33.0**: A linear algebra library. It exercises the new trait solver
  in different ways than the old solver.
- **regex-1.5.5**: A regular expression parser. Used by many Rust programs.
- **ripgrep-13.0.0**: A line-oriented search tool. A widely-used utility, and a
  binary crate.
- **serde-1.0.136**: A serialization/deserialization crate. Used by many other
  Rust programs.
- **serde_derive-1.0.136**: A proc-macro sub-crate used by `serde`. Used by
  many other Rust programs. Stresses declarative macro expansion somewhat.
- **stm32f4-0.14.0**: A crate that has many thousands of blanket impl blocks.
  It uses cargo features to enable large portions of its structure and is
  built with `--features=stm32f410` to have faster benchmarking times.
- **syn-1.0.89**: A library for parsing Rust code. An important part of the Rust
  ecosystem.
- **typenum-1.17.0**: A library that encodes integer computation within the trait system. Serves as
  a stress test for the trait solver, but at the same time it is also a very popular crate.
- **unicode-normalization-0.1.19**: Unicode character composition and decomposition
  utilities. Uses huge `match` statements that stress the compiler in unusual
  ways.
- **webrender-2022**: A web renderer. A large, complex crate used by Firefox
  and Servo. Webrender isn't released regularly so this is a development
  version (revision da1df33). The `-2022` suffix distinguishes it from earlier
  Webrender versions that used to be used in this benchmark suite.

## Secondary

These are either artificial programs or real crates that stress one particular aspect of the
compiler in interesting ways.

- **await-call-tree**: A tree of async fns that await each other, creating a
  large type composed of many repeated `impl Future` types. Such types caused
  [poor performance](https://github.com/rust-lang/rust/issues/65147) in the
  past.
- **coercions**: Contains a static array with 65,536 string literals, which
  caused [poor performance](https://github.com/rust-lang/rust/issues/32278) in
  the past.
- **ctfe-stress-5**: A stress test for compile-time function evaluation.
- **deeply-nested-multi**: A small program containing multiple examples 
  ([one](https://github.com/rust-lang/rust/issues/38528),
  [two](https://github.com/rust-lang/rust/issues/72408),
  [three](https://github.com/rust-lang/rust/issues/75992))
  of code that caused exponential behavior in the past.
- **deep-vector**: A test containing a single large vector of zeroes, which
  caused [poor performance](https://github.com/rust-lang/rust/issues/20936) in
  the past. Stresses macro expansion and type inference.
- **derive**: A large number of simple structs with a `#[derive]` attribute for common built-in traits such as Copy and Debug.
- **externs**: A large number of extern functions has caused [slowdowns in the past](https://github.com/rust-lang/rust/pull/78448).
- **helloworld-tiny**: A trivial program optimized with flags that should reduce binary size.
  Gives a lower bound on compiled binary size.
- **issue-46449**: A small program that caused [poor
  performance](https://github.com/rust-lang/rust/issues/46449) in the past.
- **issue-58319**: A small program that caused [poor
  performance](https://github.com/rust-lang/rust/issues/58319) in the past.
- **issue-88862**: A MCVE of a program that had a
  [severe performance regression](https://github.com/rust-lang/rust/issues/88862)
  when trying to normalize large opaque types with late-bound regions.
- **many-assoc-items**: Contains a struct with many associated items, which
  caused [quadratic behavior](https://github.com/rust-lang/rust/issues/68957)
  in the past.
- **match-stress**: Contains examples 
  (one involving [a huge enum](https://github.com/rust-lang/rust/issues/7462),
  one involving
  [`exhaustive_patterns`](https://github.com/rust-lang/rust/pull/79394)) of
  `match` code that caused bad performance in the past.
- **projection-caching**: A small program that causes extremely, deeply nested
  types which stress the trait system's projection cache. Removing that cache
  resulted in hours long compilations for some programs using futures,
  actix-web and other libraries with similarly nested type combinators.
- **regression-31157**: A small program that caused a [large performance
  regression](https://github.com/rust-lang/rust/issues/31157) from the past.
- **ripgrep-13.0.0-tiny**: A line-oriented search tool, optimized with flags that should reduce
  binary size.
- **token-stream-stress**: A proc-macro crate. Constructs a long token stream
  much like the `quote` crate does, which caused [quadratic
  behavior](https://github.com/rust-lang/rust/issues/65080) in the past.
- **tt-muncher**: Calls a quadratic TT muncher macro (based on `quote::quote!`)
  with a long input, which stresses macro expansion.
- **tuple-stress**: Contains a single array of 65,535 nested `(i32, (f64, f64,
  f64))` tuples. The data was extracted and reduced from a [program dealing
  with grid coordinates](https://github.com/urschrei/ostn15_phf) that was
  causing rustc to [run out of
  memory](https://github.com/rust-lang/rust/issues/36799).
- **ucd**: A Unicode crate. Contains large statics that
  [stress](https://github.com/rust-lang/rust/issues/53643) the borrow checker's
  implementation of NLL.
- **unify-linearly**: Contains many variables that all have equality relations
  between them, which caused [exponential
  behavior](https://github.com/rust-lang/rust/pull/32062) in the past.
- **unused-warnings**: Contains many unused imports, which caused [quadratic
  behavior](https://github.com/rust-lang/rust/issues/43572) in the past.
- **wf-projection-stress-65510**: A stress test which showcases [quadratic
  behavior](https://github.com/rust-lang/rust/issues/65510) (in the number of
  associated type bounds).
- **wg-grammar**: A parser generator.
  [Stresses](https://github.com/rust-lang/rust/issues/58178) the borrow
  checker's implementation of NLL.

## Stable

These are benchmarks used in the
[dashboard](https://perf.rust-lang.org/dashboard.html). They provide the
longest continuous data set for compiler performance. As a result, they are
quite old (e.g. 2017 or earlier), and not necessarily reflective of typical
Rust code being written today.

- **encoding**: An old crate providing character encoding support. Contains
  some large tables.
- **futures**: v0.1.0 of the popular `futures` crate, which was used by many
  Rust programs. Newer versions of this crate (e.g. v0.3.21 from February 2021)
  contain very little code, instead relying on sub-crates. This makes them less
  interesting as benchmarks, because we only measure final crate compilation.
  This is why there is no futures crate among the primary benchmarks.
- **html5ever**: See above. This is an older version (v0.5.4) of the crate.
- **inflate**: An old implementation of the DEFLATE algorithm. Contains
  a very large function containing many locals and basic blocks, which stresses
  obligation processing.
- **regex**: See above. This is an older version of the crate.
- **piston-image**: See above. This is an older version of the `image` crate.
- **style-servo**: An old version of Servo's `style` crate. A large crate, and
  one used by old versions of Firefox. Built with `--features=gecko`.
- **syn**: See above. This is an older version (0.11.11) of the crate.
- **tokio-webpush-simple**: A simple web server built with a very old version
  of tokio. Uses futures a lot, but doesn't use `async`/`await`.

# How to update/add/remove benchmarks

## Add a new benchmark

- Decide on which category it belongs to. Probably primary if it's a real-world
  crate, and secondary if it's a stress test or intended to catch specific
  regressions.
- If it's a third-party crate:
  - If you are keen: talk with a maintainer of the crate to see if there is
    anything we should be aware of when using this crate as a compile-time
    benchmark.
  - Look at [crates.io](https://crates.io) to find the latest (non-prerelease) version.
  - Download it with `collector download -c $CATEGORY -a $ARTIFACT crate $NAME $VERSION`.
    The `$CATEGORY` is probably `primary`. `$ARTIFACT` is either `library` or `binary`, depending
    on what kind of artifact does the benchmark build.
- It makes it easier for reviewers if you split things into two commits.
- In the first commit, just add the code for the entire benchmark.
  - Do this by doing `git add` on the new directory.
  - There is no need to remove seemingly unnecessary files such as
    documentation or CI configuration.
- In the second commit, do everything else.
  - Add `[workspace]` to the very bottom of the benchmark's `Cargo.toml`, if
    doesn't already have a `[workspace]` section. This means commands like
    `cargo build` will work within the benchmark directory.
  - Add any necessary stuff to the `perf-config.json` file.
    - If the benchmark is a sub-crate within a top-level crate, you'll need a
      `"cargo_toml"` entry.
    - If you get a "non-wrapped rustc" error when running it, you'll need a
      `"touch_file"` entry.
    - See [`collector/src/benchmark/mod.rs`](https://github.com/rust-lang/rustc-perf/blob/12cb796f8a932a891b385ba23a36d78a2867ace1/collector/src/benchmark/mod.rs#L24-L27) for a complete reference.
  - Consider adding one or more `N-*.patch` files for the `IncrPatched`
    scenario.
    - If it's a primary benchmark, you should definitely do this.
    - These usually consist of a patch that adds a single
      `println!("testing");` statement somewhere.
    - Creating the patch against what you've committed so far might be useful.
      Use `git diff` from the repository root, or `git diff --relative` within
      the benchmark directory. Copy the output into the `N-*.patch` file.
    - Do a test run with an `IncrPatched` scenario to make sure the patch
      applies correctly, e.g. `target/release/collector bench_local +nightly
      --id Test --profiles=Check --scenarios=IncrPatched
      --include=$NEW_BENCHMARK`
  - Add the new entry to `collector/compile-benchmarks/README.md`.
  - Add a new licensing entry to `collector/compile-benchmarks/REUSE.toml` (see existing entries
  for inspiration).
    - If the benchmark is artificial, use the `MIT OR Apache-2.0` license and set Rust Project
    developers as the copyright owners (see e.g. `await-call-tree` as an example).
    - If the benchmark is a third-party crate, make sure to use its license. Try to find the
    copyright owner in the crate's `COPYRIGHT` or `README` files. If you cannot find it, consider
    using the copyright owner `<crate-name> contributors`.
  - `git add` the `Cargo.lock` file, if it's not already part of the
    benchmark's committed code.
    - If the benchmark has a `.gitignore` file that contains `Cargo.lock`,
      you'll need to comment out that line so that `Cargo.lock` gets uploaded
      in the PR.
- Consider the benchmarking time for the benchmark.
  - First, measure the entire compilation time with something like this, by
    doing this within the benchmark directory is good:
    ```
    CARGO_INCREMENTAL=0 cargo check ; cargo clean
    CARGO_INCREMENTAL=0 cargo build ; cargo clean
    CARGO_INCREMENTAL=0 cargo build --release ; cargo clean
    ```
  - Second, compare the final crate time with these commands:
    ```
    target/release/collector bench_local +nightly --id Test \
      --profiles=Check,Debug,Opt --scenarios=Full --include=$NEW,helloworld
    target/release/site results.db
    ```
    (See [here](../../site/README.md) for instructions on how to build the website).
    Then switch to wall-times, compare `Test` against itself, and toggle the
    "Show non-relevant results"/"Display raw data" check boxes to make sure it
    hasn't changed drastically.
    - E.g. `futures` was changed so it's just a facade for a bunch of
      sub-crates, and the final crate time became very similar to `helloworld`,
      which wasn't interesting.
- File a PR, including the two sets of timing measurements in the description.

## Remove a benchmark

- It makes it easier for reviewers if you split things into two commits.
- In the first commit just remove the old code.
  - Do this with `git rm -r` on the directory.
- In the second commit do everything else.
  - Remove the entry from `collector/compile-benchmarks/README.md`.
  - `git grep` for occurrences of the old benchmark name (e.g. in
    `.github/workflows/ci.yml` or `ci/check-*.sh`) and see if anything needs
    changing... usually not.
- File a PR.

## Update a benchmark

- Do this in two steps.
  - First add the new version of the benchmark. Compare the benchmarking time
    of the two versions to make sure nothing outrageous has happened. Once the
    PR is merged, make sure it's running correctly.
  - Second, remove the old version of the benchmark.
  Doing it in two steps ensures we have continuity of profiling coverage in the
  case where something goes wrong.
- Compare the benchmarking time of the two versions.
- When adding the new version, for `perf-config.json` and the `N-*.patch`
  files, use the corresponding files for the old version as a starting point.

# Benchmark update policy

## Background

rustc-perf is a "living benchmark suite" that is regularly changed. Some
benchmarks in rustc-perf are verbatim copies of third-party crates. We
periodically do a mass update of these benchmarks.

Benefits of this approach:
- We ensure we are measuring compilation of the crates most people are using.
  This is most relevant for popular crates.
- We get coverage of newer language features.

Costs of this approach:
- It takes time and effort.
- We lose some data continuity.
  - But the stable set of benchmarks used for the dashboard are not affected,
    and they provide the greatest continuity.
- If the code hasn't changed much, it won't have much effect.

## Update policy

- The third-party crates should be updated every three years. This is a
  reasonable refresh period that is neither too short or too long. It happens
  to match the Rust edition cycle, but this is just coincidence.
- All third-party crates that have had at least one new release should be
  updated, even if not much code has changed. This avoids having to make
  decisions about whether a crate has changed enough.
- When doing this mass update, there may be some benchmarks that are deemed no
  longer interesting and removed. For example, in the 2022 update we found that
  the `futures` crate was no longer interesting because all the functionality
  had been split into sub-crates that rustc-perf doesn't measure. Likewise,
  there may be some new benchmarks that are added.
- New versions should be added before old versions are removed, to ensure
  continuity of profiling coverage.
- The ad hoc addition and removal of individual benchmarks can continue
  independently of this update cycle, as per the judgment of the rustc-perf
  maintainers.

History:
- The first mass update of third-party crates occurred in [March/April
  2022](https://hackmd.io/d9uE7qgtTWKDLivy0uoVQw).
