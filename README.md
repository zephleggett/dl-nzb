# dl-nzb

NZB downloader written in Rust. Downloads from Usenet with parallel connections, PAR2 repair, and RAR extraction.

Single binary, no external dependencies. PAR2 uses [par2-rs](https://github.com/zephleggett/par2-rs) (pure Rust with SIMD). RAR extraction built in.

## Install

Download from [releases](https://github.com/zephleggett/dl-nzb/releases) or build from source:

```bash
git clone https://github.com/zephleggett/dl-nzb.git
cd dl-nzb
cargo build --release
cp target/release/dl-nzb /usr/local/bin/
```

## Setup

First run creates a config file. Add your Usenet credentials:

```bash
dl-nzb config  # shows config path
```

Config locations:
- Linux: `~/.config/dl-nzb/config.toml`
- macOS: `~/Library/Application Support/dl-nzb/config.toml`
- Windows: `%APPDATA%\dl-nzb\config.toml`

Minimal config:
```toml
[usenet]
server = "news.example.com"
port = 563
username = "your-username"
password = "your-password"
ssl = true
connections = 20
```

## Usage

```bash
dl-nzb file.nzb                    # download
dl-nzb -o /path/to/dir file.nzb   # custom output dir
dl-nzb -c 50 file.nzb             # more connections
dl-nzb -l file.nzb                # list contents only
dl-nzb test                        # test server connection
dl-nzb --json file.nzb            # JSON output for scripting
```

Skip post-processing:
```bash
dl-nzb --no-par2 --no-extract-rar file.nzb
```

Clean up after extraction:
```bash
dl-nzb --delete-rar-after-extract --delete-par2 file.nzb
```

## Config Reference

```toml
[usenet]
server = "news.example.com"
port = 563                    # 563 for SSL, 119 for plain
username = "user"
password = "pass"
ssl = true
verify_ssl_certs = true
connections = 20              # check your provider's limit
timeout = 30
retry_attempts = 2
retry_delay = 500

[download]
dir = "downloads"
create_subfolders = true      # folder per NZB
force_redownload = false

[post_processing]
auto_par2_repair = true
auto_extract_rar = true
delete_rar_after_extract = false
delete_par2_after_repair = false
deobfuscate_file_names = true

[memory]
max_segments_in_memory = 800
io_buffer_size = 8388608      # 8MB
max_concurrent_files = 100

[tuning]
pipeline_size = 50            # segments per batch
connection_wait_timeout = 300 # seconds
large_file_threshold = 10485760  # 10MB, for progress display

[logging]
level = "info"
format = "pretty"
```

Environment variables override config with `DL_NZB_` prefix:
```bash
DL_NZB_USENET_SERVER=news.example.com dl-nzb file.nzb
```

## CLI Options

```
dl-nzb [OPTIONS] <FILES>...
dl-nzb <COMMAND>

Commands:
  test    Test server connection
  config  Show config location

Options:
  -o, --output-dir <DIR>       Output directory
  -c, --connections <NUM>      Connection count
  -l, --list                   List NZB contents
  -q, --quiet                  Suppress output
  -v, --verbose                Verbose (-vv for trace)
  --json                       JSON output
  --no-par2                    Skip PAR2 repair
  --no-extract-rar             Skip RAR extraction
  --delete-rar-after-extract   Delete RARs after extract
  --delete-par2                Delete PAR2 after repair
  --no-directories             No subfolders
  --force                      Re-download existing files
  --keep-partial               Keep partial files on error
  --print-names                Print filenames to stdout
  --server <HOST>              Override server
  --port <PORT>                Override port
  -u, --user <USER>            Override username
  -p, --password <PASS>        Override password
```

## JSON Output

With `--json`, outputs structured data for scripting:

```bash
dl-nzb --json -l file.nzb      # list as JSON
dl-nzb --json file.nzb         # download results as JSON
dl-nzb --json test             # test results as JSON
```

## Requirements

Usenet provider with NNTP access. Nothing else to install.

## License

GPL-3.0
