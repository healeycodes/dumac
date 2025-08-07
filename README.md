# dumac
> My blog posts:
> - [Maybe the Fastest Disk Usage Program on macOS](https://healeycodes.com/maybe-the-fastest-disk-usage-program-on-macos)
> - [Optimizing My Disk Usage Program](https://healeycodes.com/optimizing-my-disk-usage-program)
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
hyperfine --warmup 3 --min-runs 5 'du -sh temp/deep' 'diskus temp/deep' './target/release/dumac temp/deep'
Benchmark 1: du -sh temp/deep
  Time (mean ± σ):      3.186 s ±  0.198 s    [User: 0.040 s, System: 1.391 s]
  Range (min … max):    2.851 s …  3.367 s    5 runs

Benchmark 2: diskus temp/deep
  Time (mean ± σ):      1.834 s ±  0.157 s    [User: 0.482 s, System: 10.333 s]
  Range (min … max):    1.622 s …  2.046 s    5 runs

Benchmark 3: ./target/release/dumac temp/deep
  Time (mean ± σ):     563.1 ms ±  22.6 ms    [User: 57.4 ms, System: 2213.7 ms]
  Range (min … max):   545.1 ms … 595.5 ms    5 runs

Summary
  ./target/release/dumac temp/deep ran
    3.26 ± 0.31 times faster than diskus temp/deep
    5.66 ± 0.42 times faster than du -sh temp/deep
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

## Tests

```
cargo test
```
