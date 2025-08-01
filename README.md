# dumac
> My blog post: [Maybe the Fastest Disk Usage Program on macOS](https://healeycodes.com/maybe-the-fastest-disk-usage-program-on-macos)
<br>

A very fast `du -sh` clone for macOS. Maybe the fastest.

`dumac` calculates the size of a given directory.

It's a parallelized version of `du -sh` and uses highly efficient macOS syscalls ([getattrlistbulk](https://man.freebsd.org/cgi/man.cgi?query=getattrlistbulk&sektion=2&manpath=macOS+13.6.5) – avaliable since Mac OS X 10.10).

```bash
dumac /tmp/
11.5K   /tmp/
```

<br>

## Benchmarks

It has been benchmarked (and beats) against most of the tools that come up when you search "disk usage CLI for macOS" but the fairest comparisons are `du` and `diskus` because they don't generate other output.

My benchmark is a directory with 12 levels, 100 small files per level, with a branching factor of two — 4095 directories, 409500 files.

It's ran with a warm disk cache as I found that warm disk cache performance strongly correlates with cold disk cache performance on macOS with modern Apple hardware.

```
hyperfine --warmup 3 --min-runs 3 'du -sh temp/deep' 'diskus temp/deep' './target/release/dumac temp/deep'
Benchmark 1: du -sh temp/deep
  Time (mean ± σ):      3.330 s ±  0.220 s    [User: 0.040 s, System: 1.339 s]
  Range (min … max):    3.115 s …  3.554 s    3 runs

Benchmark 2: diskus temp/deep
  Time (mean ± σ):      1.342 s ±  0.068 s    [User: 0.438 s, System: 7.728 s]
  Range (min … max):    1.272 s …  1.408 s    3 runs

Benchmark 3: ./target/release/dumac temp/deep
  Time (mean ± σ):     521.0 ms ±  24.1 ms    [User: 114.4 ms, System: 2424.5 ms]
  Range (min … max):   493.2 ms … 560.6 ms    6 runs

Summary
  ./target/release/dumac temp/deep ran
    2.58 ± 0.18 times faster than diskus temp/deep
    6.39 ± 0.52 times faster than du -sh temp/deep
```

<br>

To setup the benchmark:

```bash
pip install -r requirements.txt
python setup_benchmark.py
```

<br>

```bash
brew install hyperfine
brew install diskus
cargo build --release
hyperfine --warmup 3 --min-runs 3 'du -sh temp/deep' 'diskus temp/deep' './target/release/dumac temp/deep'
```

<br>

`cargo flamegraph` of the "deep" benchmark; 91% of time spent on the optimal syscalls with 9% of scheduling overhead.

<img src="https://github.com/healeycodes/dumac/blob/main/flamegraph.svg" alt="cargo flamegraph of the benchmark.">


<br>

## Tests

```
cargo test
```
