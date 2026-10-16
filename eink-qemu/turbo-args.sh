#!/bin/sh
# One argument per line, shared by native/Node launchers and smoke tests.
# Keep the browser's turboProperties in wasm/web/runtime.js in sync.
set -eu
# The X4 Pro and Picco cards sit on the S3's SDMMC host, not on SPI.
if [ "$1" != x4pro ] && [ "$1" != picco ]; then
    for property in read-access-us read-seq-access-us read-repeat-us read-next-us \
        write-busy-us write-random-busy-us write-repeat-busy-us write-block-busy-us \
        write-stop-busy-us write-stop-decrement-us write-stop-random-extra-us; do
        printf '%s\n' -global "ssi-sd.$property=0"
    done
fi
for property in transaction-overhead-us transaction-overhead-ns buffer-overhead-ns; do
    printf '%s\n' -global "driver=ssi.esp32s3.gpspi,property=$property,value=0"
done
printf '%s\n' -global driver=ssi.esp32s3.gpspi,property=zero-wire-time,value=on
case "$1" in
    x4pro|picco) printf '%s\n' -global dwc_sdmmc.timing=off ;;
esac
# Retain spacing so the guest can service each RX interrupt.
printf '%s\n' -global esp32_wifi.rx-frame-us=500
case "$1" in
    x3)
        for property in frame-us refresh-overhead-us; do
            printf '%s\n' -global "uc8279.$property=0"
        done
        # Polling drivers must observe BUSY before it completes.
        for property in pon-ms pof-ms busy-ms; do
            printf '%s\n' -global "uc8279.$property=1"
        done
        ;;
    x4|picco)
        printf '%s\n' -global ssd1677.busy-ms=1
        ;;
    x4pro)
        printf '%s\n' -global uc8179.frame-us=1
        for property in pon-ms pof-ms busy-ms; do
            printf '%s\n' -global "uc8179.$property=1"
        done
        ;;
    *) echo "unsupported machine: $1" >&2; exit 2 ;;
esac
