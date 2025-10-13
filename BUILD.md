# Build Instructions

## Standard Release Build

For distribution (portable across similar architectures):

```bash
cargo build --release
```

This produces a **2.5MB binary** optimized for size with full LTO.

## Performance-Optimized Builds

### Native CPU (Maximum Speed)

Build optimized for YOUR specific CPU (not portable):

```bash
cargo release-native
# or
RUSTFLAGS="-C target-cpu=native" cargo build --release
```

This enables ALL CPU features available on your machine (AVX2, AVX-512, NEON, etc).

### Fast Development Builds

For quick iteration during development:

```bash
cargo build --profile release-fast
```

Uses thin LTO and opt-level 3 for faster compilation with good performance.

## Architecture-Specific Builds

The project automatically optimizes for:

- **Apple Silicon (M1/M2/M3)**: `target-cpu=apple-m1`
- **Modern Intel/AMD (x86_64)**: `target-cpu=x86-64-v3` (AVX2)
- **Intel Macs**: `target-cpu=haswell` (AVX2)

## Cross-Compilation Examples

### Linux (from macOS)

```bash
rustup target add x86_64-unknown-linux-gnu
cargo build --release --target x86_64-unknown-linux-gnu
```

### Windows (from macOS/Linux)

```bash
rustup target add x86_64-pc-windows-gnu
cargo build --release --target x86_64-pc-windows-gnu
```

### ARM64 Linux

```bash
rustup target add aarch64-unknown-linux-gnu
cargo build --release --target aarch64-unknown-linux-gnu
```

## Build Profiles

- **release**: Optimized for size (2.6MB), portable
- **release-fast**: Optimized for speed, faster compilation
- **release-native**: Maximum performance for your CPU (not portable)

## Optimization Flags Explained

- `opt-level = "z"`: Optimize for binary size
- `opt-level = 3`: Optimize for maximum speed
- `lto = "fat"`: Full link-time optimization across all crates
- `lto = "thin"`: Faster LTO with good results
- `target-cpu=native`: Use all features of your CPU
- `target-cpu=apple-m1`: Apple Silicon optimizations
- `target-cpu=x86-64-v3`: Modern x86_64 (AVX2, FMA, BMI)
- `codegen-units = 1`: More optimization, slower compile

## Benchmarking

To measure performance improvements:

```bash
# Standard build
cargo build --release
time ./target/release/dl-nzb file.nzb

# Native CPU build
RUSTFLAGS="-C target-cpu=native" cargo build --release
time ./target/release/dl-nzb file.nzb
```

Expect 5-15% performance improvement with native CPU flags for compute-intensive operations like yEnc decoding and PAR2 verification.

## Link-Time Optimization (LTO)

The release build uses full LTO by default. This:
- Reduces binary size by ~20%
- Improves runtime performance by ~10-15%
- Increases compile time significantly

For faster iteration, use `release-fast` profile with thin LTO.
