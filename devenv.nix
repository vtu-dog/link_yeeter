{ pkgs, lib, ... }:

{
  packages = with pkgs;
    lib.optionals stdenv.isDarwin
    ([ darwin.apple_sdk.frameworks.SystemConfiguration ])
    ++ [ git ffmpeg-headless yt-dlp ];

  languages.rust.enable = true;
  dotenv.disableHint = true;
}
