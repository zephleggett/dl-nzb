# dl-nzb

NZB downloader written in Rust. Downloads from Usenet with parallel connections, PAR2 repair, and RAR extraction.

PAR2 uses [par2-rs](https://github.com/zephleggett/par2-rs) (pure Rust with SIMD). RAR extraction built in.

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
dl-nzb -l file.nzb                # list contents only
dl-nzb -f file.nzb                # force re-download
dl-nzb test                        # test server connection
dl-nzb config                      # show config location and values
dl-nzb --json file.nzb            # JSON output for scripting
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
max_concurrent_files = 100

[tuning]
pipeline_size = 50            # segments per batch
connection_wait_timeout = 300 # seconds
max_concurrent_connections = 20 # parallel connection creation (raise for faster ramp)
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
dl-nzb [OPTIONS] [FILES]...
dl-nzb <COMMAND>

Commands:
  test     Test server connection
  config   Show configuration
  version  Show version information

Options:
  -o, --output <DIR>    Output directory
  -l, --list            List NZB contents
  -q, --quiet           Suppress output
  -v, --verbose         Verbose (-vv for debug)
  -f, --force           Force re-download (overwrite existing)
  --json                JSON output for scripting
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

GPL-2.0
