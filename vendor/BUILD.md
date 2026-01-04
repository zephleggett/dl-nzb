# Building par2cmdline-turbo

The `vendor/par2cmdline-turbo` directory contains the source for the high-performance PAR2 tool.

## Quick Start: Pre-built Binaries

The easiest approach is to download pre-built binaries from the [releases page](https://github.com/animetosho/par2cmdline-turbo/releases).

| Platform | Binary |
|----------|--------|
| Windows x64 | `par2.exe` |
| Linux x64 | `par2` |
| macOS x64 | `par2` |
| macOS ARM64 | `par2` |

Place the binary in the same directory as `dl-nzb`.

---

## Building from Source

### Prerequisites

All platforms need a C++ compiler with C++11 support.

| Platform | Requirements |
|----------|--------------|
| Windows | MSYS2/MinGW or Visual Studio 2019+ |
| Linux | GCC 10+ or Clang, autotools, make |
| macOS | Xcode Command Line Tools, autotools |

### Linux / macOS

```bash
cd vendor/par2cmdline-turbo

# Install autotools if needed (Ubuntu/Debian)
# sudo apt install autoconf automake libtool

# Generate configure script
./automake.sh

# Configure and build
./configure
make -j$(nproc)

# Binary is now at: ./par2
```

### Windows (MSYS2/MinGW)

```bash
# Install MSYS2 from https://www.msys2.org/
# Open MSYS2 MINGW64 terminal

# Install build tools
pacman -S mingw-w64-x86_64-gcc autoconf automake make

cd /path/to/vendor/par2cmdline-turbo

./automake.sh
./configure
make -j$(nproc)

# Binary is now at: ./par2.exe
```

### Windows (Visual Studio)

```powershell
cd vendor/par2cmdline-turbo

# Open solution in Visual Studio
start par2cmdline.sln

# Or build via command line (requires VS Developer Command Prompt)
msbuild par2cmdline.vcxproj /p:Configuration=Release /p:Platform=x64
```

---

## Installation

After building, copy the `par2` binary to one of these locations:

1. **Same directory as dl-nzb executable** (recommended)
2. **System PATH** 
   - Linux/macOS: `/usr/local/bin/`
   - Windows: Add to `%PATH%`

The `dl-nzb` application will automatically find the binary.
