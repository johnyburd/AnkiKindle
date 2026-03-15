#!/bin/bash
WD=$(dirname $0)
export ANKI_COLLECTION_PATH=/mnt/us/documents/collection.anki2
export ANKI_PASSWORD=CHANGEME
export ANKI_USERNAME=CHANGEME@example.com
export BIND_ADDR=127.0.0.1:80
killall anki-kindle
$WD/anki-kindle $* &> /mnt/us/anki.log &
