# link_yeeter

Get rid of video link clutter

- [Overview](#overview)
- [Running the project](#running-the-project)
  - [Required](#required)
  - [Optional](#optional)
- [Additional info](#additional-info)

## Overview

`link_yeeter` is a Telegram Messenger bot which detects video links in messages and reposts them with the video attached.

Unfortunately, videos over 50MB cannot be downloaded due to Telegram API limitations.

## Running the project

You'll need to rename `.env_example` to `.env` and populate it with the following keys:

### Required

- **TELOXIDE_TOKEN** - your bot's HTTP token for the Telegram API; create one via [@BotFather](https://t.me/BotFather) (detailed instructions [here](https://core.telegram.org/bots#6-botfather)). Remember to keep it safe!
- **ALLOWLIST** - comma-separated list of domains the bot can download from, e.g. `site1.com,site2.net,site3.edu`
- **MAX_FILESIZE** - maximum file size in MB the bot will process before sending

### Optional

- **MAINTAINER** - your Telegram handle, shown to users in error messages
- **FALLBACK_FILESIZE** - maximum file size in MB allowed in fallback mode; defaults to `MAX_FILESIZE * 5`
- **LOG_FORMAT** - log output format; set to `json` for JSON output (useful for piping to tools like [hl](https://github.com/pamburus/hl)), otherwise uses tree-structured output

If you want to run the project locally, you'll need [yt-dlp](https://github.com/yt-dlp/yt-dlp) and [ffmpeg](https://www.ffmpeg.org) ([ffprobe](https://ffmpeg.org/ffprobe.html) included) in your `PATH`.

Alternatively, you can use Docker (`compose-example.yaml` doubles as an env file).

## Additional info

The project was tested using Rust 1.93.0 (Stable) on macOS 26.2 Tahoe (arm64).

Any more questions? Feature suggestions? Contact me [on Telegram](https://t.me/Vyaatu)! Pull requests / GitHub issues are greatly appreciated as well!
