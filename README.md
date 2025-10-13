# dl-nzb

A fast, modern NZB downloader written in Rust. Downloads from Usenet with parallel connections, automatic repair and extraction, and a clean terminal interface.

## Why dl-nzb?

Most Usenet downloaders are either slow, bloated with unnecessary features, or haven't been updated in years. dl-nzb is built from scratch in Rust to be fast and efficient without the overhead. It does one thing well: download your NZB files as quickly as your connection allows.

The entire downloader is a 2.5MB statically-linked binary with zero runtime dependencies. PAR2 repair and RAR extraction are built directly into the binary using optimized C++ libraries (par2cmdline-turbo), so you don't need to install external tools. Just copy the binary to your server and run it. It uses async I/O and connection pooling to maximize throughput, and it's smart about memory usage so you can run it on servers with limited RAM.

## Features

Downloads with parallel connections and automatic retries. Built-in PAR2 verification and repair using par2cmdline-turbo (no external par2 command needed). Built-in RAR extraction (no external unrar needed). Handles obfuscated filenames. Shows real-time progress with speed and ETA. All configurable through a simple TOML file or command-line options.

## Installation

Build from source with Cargo:

```bash
git clone https://github.com/zephleggett/dl-nzb.git
cd dl-nzb
cargo build --release
cp target/release/dl-nzb /usr/local/bin/
```

## Quick Start

On first run, dl-nzb creates a config file. Edit it with your Usenet server credentials:

- Linux: `~/.config/dl-nzb/config.toml`
- macOS: `~/Library/Application Support/dl-nzb/config.toml`
- Windows: `%APPDATA%\dl-nzb\config.toml`

```toml
[usenet]
server = "news.example.com"
port = 563
username = "your-username"
password = "your-password"
ssl = true
connections = 30
```

Then download any NZB:

```bash
dl-nzb file.nzb
```

That's it. Files download to a `downloads/` folder in your current directory by default.

## Usage Examples

Download an NZB file:
```bash
dl-nzb movie.nzb
```

Download to a specific directory:
```bash
dl-nzb -o /path/to/downloads movie.nzb
```

Download multiple files:
```bash
dl-nzb file1.nzb file2.nzb file3.nzb
```

Use more connections for faster speeds:
```bash
dl-nzb -c 50 movie.nzb
```

List NZB contents without downloading:
```bash
dl-nzb -l movie.nzb
```

Skip automatic extraction and repair:
```bash
dl-nzb --no-extract-rar --no-par2 movie.nzb
```

Download and clean up archive files automatically:
```bash
dl-nzb --delete-rar-after-extract --delete-par2 movie.nzb
```

Test your server connection:
```bash
dl-nzb test
```

View your configuration:
```bash
dl-nzb config
```

## Configuration

The config file lives at `~/.config/dl-nzb/config.toml`. You can also use a local `dl-nzb.toml` file in your working directory for project-specific settings.

Here are the main options:

```toml
[usenet]
server = "news.example.com"  # Your Usenet provider
port = 563                    # 563 for SSL, 119 for plain
username = "user"
password = "pass"
ssl = true
connections = 30              # More = faster (check your provider's limits)
timeout = 45
retry_attempts = 2

[download]
dir = "downloads"             # Where files go
create_subfolders = true      # Create a folder for each NZB

[post_processing]
auto_par2_repair = true       # Verify and repair with PAR2
auto_extract_rar = true       # Extract RAR archives
delete_rar_after_extract = false
delete_par2_after_repair = false
deobfuscate_file_names = true # Rename obfuscated files

[memory]
max_segments_in_memory = 100
io_buffer_size = 8388608      # 8MB - good for most systems
max_concurrent_files = 10
```

All settings can be overridden via environment variables with the `DL_NZB_` prefix:

```bash
DL_NZB_USENET_CONNECTIONS=50 dl-nzb movie.nzb
```

## Command-Line Options

Run `dl-nzb --help` for the full list. Here are the most useful ones:

```
-o, --output-dir <DIR>         Output directory
-c, --connections <NUM>        Number of connections
-l, --list                     List NZB contents without downloading
-q, --quiet                    Quiet mode
-v, --verbose                  Verbose output (use -vv for trace)
--no-par2                      Skip PAR2 repair
--no-extract-rar               Skip RAR extraction
--delete-rar-after-extract     Clean up RAR files after extraction
--delete-par2                  Clean up PAR2 files after repair
--no-directories               Don't create subfolders
--keep-partial                 Keep partial files on error
--memory-limit <MB>            Memory limit for segment buffering
--buffer-size <KB>             I/O buffer size (default 4096)
--max-concurrent-files <NUM>   Max concurrent file downloads
--log-level <LEVEL>            error, warn, info, debug, trace
--server <HOST>                Override Usenet server
--port <PORT>                  Override server port
-u, --user <USER>              Override username
-p, --password <PASS>          Override password (use - for stdin)
```

## Requirements

Just a Usenet provider with NNTP access. That's it.

PAR2 repair and RAR extraction are compiled directly into the binary, so there's nothing else to install. The binary is statically linked and includes all the necessary code from par2cmdline-turbo for verification and repair.

## Performance

dl-nzb is designed to max out your connection speed. It uses async I/O, connection pooling, and streaming writes to disk. On a gigabit connection with 30-50 connections, you should see speeds close to your line capacity.

Memory usage is configurable but defaults are tuned for typical systems (around 100-200MB during downloads). Increase `max_segments_in_memory` if you have RAM to spare and want slightly better performance.

## Troubleshooting

**Connection errors**: Test your server settings with `dl-nzb test`. Make sure your credentials are correct and your provider allows the port you're using (usually 563 for SSL).

**Slow downloads**: Increase connections with `-c 50` or edit `connections` in your config. Check that SSL is enabled if your provider supports it (it's usually faster).

**Missing segments**: Some Usenet posts are incomplete or expired. dl-nzb will download what's available and use PAR2 to repair if possible. If repair fails, the files are incomplete on Usenet.

**Out of memory**: Lower `max_segments_in_memory` or `max_concurrent_files` in your config.

## License

GPL 2.0

## Contributing

Pull requests welcome. This is a side project to get started in rust, but I'll review contributions when I can.
