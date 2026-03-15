# Anki Kindle

This project is a minimal implementation of an [Anki](https://apps.ankiweb.net/) app for the Kindle.

## Features

* Card Review
* Sync decks and media to and from [AnkiWeb ](https://ankiweb.net/).
* Custom Study 5 extra cards
* Works offline

## Tested Devices
* [Kindle PW5 (11th gen)](https://wiki.mobileread.com/wiki/Kindle_Serial_Numbers)

# Installation
* Install [KUAL](https://wiki.mobileread.com/wiki/KUAL)
* Download the [latest release](https://github.com/johnyburd/AnkiKindle/releases/latest/download/anki-kindle.zip) and extract it in your KUAL extensions directory.
* Set your AnkiWeb username and password in AnkiKindle/bin/launch.sh if using sync.
   * Otherwise be sure to copy over your Anki collection manually.
* Run "Install Custom Browser" (this step is only required once)
* Run "Launch Anki Server" (this step is required after each boot)
* Navigate to "http://kindle.local" in the kindle browser. Bookmark it for easy access.

# Development
## Build for Dev
```
export ANKI_COLLECTION_PATH="./collection.anki2"
export ANKI_USERNAME="username"
export ANKI_PASSWORD="password"
# listens on http://127.0.0.1:8080
cargo run
```
## Build for Kindle
Download kindle toolchain: [github.com/koreader/koxtoolchain/releases](https://github.com/koreader/koxtoolchain/releases) and extract so `x-tools` is in the project root
```
rustup target add arm-unknown-linux-gnueabihf
cargo build --target arm-unknown-linux-gnueabihf --release
```
