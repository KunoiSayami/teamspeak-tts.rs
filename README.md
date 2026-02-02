# TeamSpeak TTS

A Rust-based TeamSpeak bot that integrates Azure Text-to-Speech services, allowing users to send TTS messages through a web interface that are played directly in TeamSpeak voice channels.

TTS service is powered by [Azure AI Services](https://azure.microsoft.com/en-us/products/ai-services/ai-speech/).

## Features

* **Web UI** - Browser-based interface accessible from anywhere via WebSocket connection
* **Intelligent Caching** - LevelDB-based cache system for frequently used phrases, reducing API calls and latency
* **Multi-language Support** - Supports all Azure TTS voices and languages
* **Follow Mode** - Bot can automatically follow a specific user between channels
* **API Key Load Balancing** - Support for multiple API keys with automatic rotation and failover
* **Server Password Support** - Connect to password-protected TeamSpeak servers
* **Cross-platform** - Supports Windows, Linux, macOS, Android, and iOS client identities

## Requirements

* Rust 1.85+ (2024 edition)
* An Azure subscription with Speech Services enabled

## Building

```bash
cargo build --release
```

### Feature Flags

| Flag | Description |
|------|-------------|
| `full` (default) | Enables all features (`spin-sleep` + `rustls`) |
| `rustls` | Uses rustls for TLS (recommended) |
| `spin-sleep` | Uses spin_sleep for more precise audio timing |
| `measure-time` | Enables debug timing measurements |

## Usage

```bash
# Run with default config file (config.toml)
./teamspeak-tts

# Run with custom config file
./teamspeak-tts /path/to/config.toml

# Command line options
./teamspeak-tts [OPTIONS] [CONFIG]

Options:
  -v, --verbose...          Increase log level (can be used multiple times)
      --log-commands        Enable logging for TeamSpeak commands
      --server <SERVER>     Override teamspeak server address
      --web <BIND_ADDRESS>  Override web server bind address
      --leveldb <FOLDER>    Override leveldb cache location
```

### Verbosity Levels

| Level | Description |
|-------|-------------|
| `-v` | Reduce symphonia OGG warnings |
| `-vv` | Reduce DNS resolver logs |
| `-vvv` | Reduce tsproto/reqwest/axum logs |
| `-vvvv` | Reduce h2/tungstenite/resend logs |
| `-vvvvv` | Enable TeamSpeak command logging |
| `-vvvvvv` | Enable packet logging |
| `-vvvvvvv` | Enable UDP packet logging |

## Configuration

In order to use this bot, you need an Azure subscription. Microsoft provides [0.5 million characters free per month](https://azure.microsoft.com/en-us/pricing/details/cognitive-services/speech-services/), which is sufficient for most use cases.

### Getting Azure TTS Credentials

1. Create an Azure account at [azure.microsoft.com](https://azure.microsoft.com/)
2. Create a Speech Services resource in the Azure Portal
3. Copy your API key and endpoint URL from the resource's "Keys and Endpoint" page
4. The endpoint format is: `https://<region>.tts.speech.microsoft.com/cognitiveservices/v1`

### Configuration File

```toml
# config.toml

# LevelDB folder path for caching TTS audio (optional, default: "tts.db")
leveldb = "tts.db"

[teamspeak]
# TeamSpeak identity key (base64 encoded)
# Generate one using: ts3client_runscript.sh identity create
key = "your_identity_key_here"

# TeamSpeak server address (can include port, e.g., "example.com:9987")
server = "your.teamspeak.server.com"

# Bot nickname (optional, default: "tts")
nickname = "TTS Bot"

# Default channel ID to join (optional, default: 0 = server default)
channel = 0

# Server password (optional, for password-protected servers)
password = ""

# Follow user by client database ID (optional)
# If the user moves channels, the bot will follow them
# If multiple clients match, one is chosen randomly
#follow = 123

[tts]
# Azure TTS endpoint URL
# Replace <region> with your Azure region (e.g., eastus, westeurope)
endpoint = "https://<region>.tts.speech.microsoft.com/cognitiveservices/v1"

# Azure TTS API key - single key
Ocp-Apim-Subscription-Key = "your_api_key_here"

# Or use multiple keys for load balancing (randomly selected)
# Ocp-Apim-Subscription-Key = ["key1", "key2", "key3"]

[web]
# Web server bind address
listen = "127.0.0.1"
port = 11400
```

### Configuration Options Reference

| Section | Key | Required | Default | Description |
|---------|-----|----------|---------|-------------|
| (root) | `leveldb` | No | `tts.db` | Path to LevelDB cache folder |
| `teamspeak` | `key` / `identity` | Yes | - | TeamSpeak identity key |
| `teamspeak` | `server` | Yes | - | TeamSpeak server address |
| `teamspeak` | `nickname` | No | `tts` | Bot display name |
| `teamspeak` | `channel` | No | `0` | Default channel ID to join |
| `teamspeak` | `password` | No | `""` | Server password |
| `teamspeak` | `follow` | No | - | Client database ID to follow |
| `tts` | `endpoint` | Yes | - | Azure TTS API endpoint |
| `tts` | `Ocp-Apim-Subscription-Key` | Yes | - | Azure API key(s) |
| `web` | `listen` | Yes | - | Web server bind IP |
| `web` | `port` | Yes | - | Web server port |

## Web Interface

Once running, access the web interface at `http://<listen>:<port>/` (default: http://127.0.0.1:11400/).

The web interface communicates via WebSocket and allows you to:
- Enter text to be spoken
- Select language and voice
- Choose voice gender and variant

## Architecture

```
┌─────────────┐     ┌──────────────┐     ┌─────────────┐
│  Web UI     │────▶│  Web Server  │────▶│  TTS Queue  │
│ (Browser)   │ WS  │   (Axum)     │     │ (Middleware)│
└─────────────┘     └──────────────┘     └──────┬──────┘
                                                │
                    ┌──────────────┐            │
                    │   LevelDB    │◀───────────┤
                    │   (Cache)    │            │
                    └──────────────┘            ▼
                                         ┌─────────────┐
┌─────────────┐     ┌──────────────┐     │  Azure TTS  │
│  TeamSpeak  │◀────│  Connection  │◀────│    API      │
│   Server    │     │   Handler    │     └─────────────┘
└─────────────┘     └──────────────┘
```

## Troubleshooting

### Bot connects but no audio plays
- Ensure the bot has permission to speak in the channel
- Check that the Azure TTS API key is valid
- Verify the endpoint URL matches your Azure region

### "KeyStore is empty" error
- All configured API keys have been invalidated or exhausted
- Check your Azure subscription status and API key validity
- Add new valid API keys to the configuration

### Bot gets kicked from server
- The bot will automatically handle channel kicks and continue operating
- Server kicks will terminate the bot gracefully
- Check server permissions if kicks occur frequently

### Cache not working
- Ensure the `leveldb` path is writable
- Very short phrases (< 30 characters) or long phrases (> 75 characters) are not cached by design

## Dependencies

This project uses the following major dependencies:

| Crate | Purpose |
|-------|---------|
| [tsclientlib](https://github.com/ReSpeak/tsclientlib) | TeamSpeak client library |
| [axum](https://github.com/tokio-rs/axum) | Web framework for the API |
| [reqwest](https://github.com/seanmonstar/reqwest) | HTTP client for Azure API |
| [symphonia](https://github.com/pdeljanov/Symphonia) | Audio decoding (OGG/Opus) |
| [rusty-leveldb](https://github.com/dermesser/leveldb-rs) | Cache storage |
| [tokio](https://github.com/tokio-rs/tokio) | Async runtime |

## Open Source License

[![](https://www.gnu.org/graphics/agplv3-155x51.png)](https://www.gnu.org/licenses/agpl-3.0.txt)

Copyright (C) 2024-2026 KunoiSayami

This program is free software: you can redistribute it and/or modify it under the terms of the GNU Affero General Public License as published by the Free Software Foundation, either version 3 of the License, or any later version.

This program is distributed in the hope that it will be useful, but WITHOUT ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the GNU Affero General Public License for more details.

You should have received a copy of the GNU Affero General Public License along with this program. If not, see <https://www.gnu.org/licenses/>.


### tsclientlib
The MIT License (MIT)

Copyright (c) 2017-2020 tsclientlib contributors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.