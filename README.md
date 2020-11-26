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
You'll need to set two environment variables:
- **TOKEN**, which is your bot's HTTP token to access Telegram API; you can create it via [@BotFather](https://t.me/BotFather) (detailed instructions [here](https://core.telegram.org/bots#6-botfather)). Rememeber to keep it safe!
- **WHITELIST**, which is a list of netlocs the bot is allowed to download from; example: "site1.com,site2.net,site3.edu"

If you want to run the project locally, you'll need `youtube-dl` and `ffmpeg` in your `PATH`.

If you'd rather host the app on Heroku, you can install the dependencies by adding [veeraya/heroku-buildpack-ffmpeg-latest](https://github.com/veeraya/heroku-buildpack-ffmpeg-latest) to your buildpacks.

## Additional info
The project was tested using Rust 1.48.0 (Stable) on macOS 11.0.1 Big Sur.

Any more questions? Feature suggestions? Contact me [on Telegram](https://t.me/Vyaatu)! Pull requests / GitHub issues are greatly appreciated as well!
