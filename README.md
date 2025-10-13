# dl-nzb

A macOS CLI tool written in Rust to parse and download NZB files from Usenet servers.

## Features

- 🚀 **Fast concurrent downloads** with configurable connection limits
- 📊 **Progress tracking** with real-time progress bars
- 🔧 **Automatic yEnc decoding** for binary content
- ⚙️ **Configurable settings** with TOML configuration files
- 🔍 **NZB file analysis** to preview contents before downloading
- 🧩 **Automatic file assembly** from downloaded segments
- 🔐 **Secure authentication** with Usenet servers

## Installation

### Prerequisites

- Rust 1.70+ (install from [rustup.rs](https://rustup.rs/))
- macOS 10.15+ (Catalina or later)

### Build from source

```bash
git clone <repository-url>
cd dl-nzb
cargo build --release
```

The binary will be available at `target/release/dl-nzb`.

### Install globally

```bash
cargo install --path .
```

## Configuration

The tool uses a single configuration file located at `~/.config/dl-nzb/config.toml`.

**On first run, this file will be automatically created with default values.**

The configuration file looks like this:

```toml
download_dir = "downloads"
temp_dir = "temp"

[usenet]
server = "your.usenet.server"
username = "your_username"
password = "your_password"
port = 119
ssl = false
connections = 10
```

**You must edit this file with your actual Usenet server credentials before downloading.**

### Editing the configuration

Open the config file in your favorite editor:

```bash
# macOS
open ~/.config/dl-nzb/config.toml

# Or use any text editor
nano ~/.config/dl-nzb/config.toml
vim ~/.config/dl-nzb/config.toml
```

### View configuration

To see your current configuration and the file location:

```bash
dl-nzb config
```

## Usage

### View help

```bash
dl-nzb --help
```

### Test connection to Usenet server

```bash
dl-nzb test
```

### List contents of an NZB file (without downloading)

```bash
dl-nzb -l "nzb-file.nzb"
```

This will show:
- Total files and size
- Number of segments
- Individual file information
- Main files vs PAR2 files

### Download files from an NZB

```bash
dl-nzb download "nzb-file.nzb"
```

#### Download options

```bash
# Download to a specific directory
dl-nzb -o ~/Movies "file.nzb"

# Use more connections for faster downloads
dl-nzb -c 50 "file.nzb"

# Skip post-processing (PAR2 repair and extraction)
dl-nzb --no-par2 --no-extract "file.nzb"

# Override config server settings temporarily
dl-nzb --server news.example.com --user myuser --password mypass "file.nzb"
```

## How it works

1. **NZB Parsing**: The tool parses the XML structure of NZB files to extract file and segment information
2. **NNTP Connection**: Establishes authenticated connections to the Usenet server
3. **Concurrent Downloads**: Downloads multiple segments simultaneously using configurable connection limits
4. **yEnc Decoding**: Automatically decodes yEnc-encoded binary data
5. **File Assembly**: Reassembles downloaded segments in the correct order to recreate original files
6. **Progress Tracking**: Shows real-time progress with detailed statistics

## File Structure

```
src/
├── main.rs          # CLI interface and command handling
├── config.rs        # Configuration management
├── nzb.rs          # NZB file parsing and data structures
├── nntp.rs         # NNTP protocol implementation
└── downloader.rs   # Download orchestration and progress tracking
```

## Dependencies

- **tokio**: Async runtime for concurrent operations
- **clap**: Command-line argument parsing
- **quick-xml**: Fast XML parsing for NZB files
- **serde**: Serialization/deserialization
- **indicatif**: Progress bars and status indicators
- **anyhow**: Error handling
- **reqwest**: HTTP client (for future features)

## Troubleshooting

### Connection Issues

If you encounter connection problems:

1. Check your internet connection
2. Verify Usenet server credentials
3. Try reducing the number of connections: `dl-nzb download -c 5 "file.nzb"`
4. Check if your ISP blocks Usenet traffic

### Download Failures

If segments fail to download:

- The tool will continue downloading other segments
- Failed segments are reported in the summary
- Some files may still be usable even with missing segments

### Performance

For optimal performance:

- Use SSD storage for download and temp directories
- Adjust connection count based on your internet speed and server limits
- Monitor system resources during large downloads

## License

This project is licensed under the MIT License - see the LICENSE file for details.

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.

## Disclaimer

This tool is for educational purposes and legitimate use only. Ensure you comply with your local laws and the terms of service of your Usenet provider.
