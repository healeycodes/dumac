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
17714352	/tmp/
dumac -h /tmp/
8.4G	/tmp/
```

Output follows `du`: block counts in 512-byte units by default (or `BLOCKSIZE` if set), and these flags change the unit, as with `du`:

| Flag | Output |
| --- | --- |
| `-h` | human-readable, powers of 1024 (`4.0K`, `104K`, `1.0G`) |
| `--si` | human-readable, powers of 1000 (`4.1k`, `41M`) |
| `-k`, `-m`, `-g` | 1 KiB, 1 MiB or 1 GiB blocks |
| `-B size` | blocks of `size` bytes; accepts `k`, `m` or `g` suffixes, e.g. `-B 4k` |

`-s` is accepted for `du` compatibility (`du -sh` works as is). dumac always prints one total per path, so it has no effect. Flags can be combined (`-sh`, `-hk`), and the last size flag wins. Use `--` to end option parsing before a path that starts with a dash.

Multiple paths and quoted glob patterns are supported (`*`, `?`, `[...]`, `**`). Hardlinks shared between paths are counted once.

```bash
dumac ~/Code ~/Downloads
dumac 'target/**/build'
```

The worker pool defaults to the number of performance cores (more threads mostly contend on kernel locks). Set `DUMAC_THREADS=n` to override.

On APFS, files cloned with `cp -c` (or by Xcode, Finder duplicates, etc.) share storage, but `du` and `dumac` count every clone in full. Pass `--dedupe-clones` to count the blocks shared within a clone group once. It's an estimate when clones have been edited in different places, and it applies to files found inside directories, not to file arguments.

```bash
dumac --dedupe-clones ~/Library/Developer
```

Empty directories are skipped without being opened, using the entry count APFS keeps for each directory. This is only done on writable APFS volumes and never for mount points: the sealed system volume reports firmlinks such as `/Users` as empty, so it is always walked fully.

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
