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
  Time (mean ± σ):      2.484 s ±  0.013 s    [User: 0.039 s, System: 0.886 s]
  Range (min … max):    2.470 s …  2.496 s    3 runs

Benchmark 2: diskus temp/deep
  Time (mean ± σ):      1.803 s ±  0.148 s    [User: 0.396 s, System: 9.408 s]
  Range (min … max):    1.636 s …  1.917 s    3 runs

Benchmark 3: ./target/release/dumac temp/deep
  Time (mean ± σ):     541.9 ms ±  14.0 ms    [User: 110.6 ms, System: 2589.1 ms]
  Range (min … max):   520.3 ms … 559.4 ms    5 runs

Summary
  ./target/release/dumac temp/deep ran
    3.33 ± 0.29 times faster than diskus temp/deep
    4.58 ± 0.12 times faster than du -sh temp/deep
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
