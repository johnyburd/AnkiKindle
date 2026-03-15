# Anki Kindle

This project is a minimal implementation of an [Anki](https://apps.ankiweb.net/) app for the Kindle.

## Features

* Card Review
* Sync decks and media to and from [AnkiWeb ](https://ankiweb.net/).
* Custom Study 5 extra cards

## Tested Devices
* [Kindle PW5 (11th gen)](https://wiki.mobileread.com/wiki/Kindle_Serial_Numbers)

# Installation
* Install [KUAL](https://wiki.mobileread.com/wiki/KUAL)
* ...

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
```
rustup target add arm-unknown-linux-gnueabihf
cargo build --target arm-unknown-linux-gnueabihf --release
```
