# link_yeeter

Get rid of video link clutter

- [Overview](#overview)
- [Running the project](#running-the-project)
- [Additional info](#additional-info)

## Overview

`link_yeeter` is a Telegram Messenger bot which detects video links in messages and reposts them with the video attached. Very convenient, especially in channels!

Unfortunately, videos over 50MB cannot be downloaded due to Telegram API limitations.

**When adding the bot to a group / channel, remember to give it permissions to post and delete messages!**

## Running the project

You'll need to rename `.env_example` to `.env` and populate it with the following keys:

- **TELOXIDE_TOKEN**, which is your bot's HTTP token to access Telegram API; you can create it via [@BotFather](https://t.me/BotFather) (detailed instructions [here](https://core.telegram.org/bots#6-botfather)). Rememeber to keep it safe!
- **WHITELIST**, which is a list of netlocs the bot is allowed to download from; example: "site1.com,site2.net,site3.edu"
- **MAX_FILESIZE**, which is the maximum file size the bot is allowed to download (in megabytes)
- **MAINTAINER**, which is your Telegram handle; set this to your username if you want users to be able to contact you easily

If you want to run the project locally, you'll need [yt-dlp](https://github.com/yt-dlp/yt-dlp) and [ffmpeg](https://www.ffmpeg.org) ([ffprobe](https://ffmpeg.org/ffprobe.html) included) in your `PATH`.

## Additional info

The project was tested using Rust 1.70.0 (Stable) on macOS 13.4 Ventura.

Any more questions? Feature suggestions? Contact me [on Telegram](https://t.me/Vyaatu)! Pull requests / GitHub issues are greatly appreciated as well!
