#!/bin/bash
WD=$(dirname $0)
cp /usr/bin/browser /usr/bin/browser.orig
cp $WD/browser /usr/bin/browser
chmod +x /usr/bin/browser
