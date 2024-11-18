# Scip - Serial Console Interfacing Program
A command line tool to communicate with a serial console written in [Rust](https://rust-lang.org)

## Installation
```bash
cargo install serial-console
```

## Usage
```
USAGE:
    scip [--binfile <FILE>] <DEVICE> [ARGS]

ARGS:
    --binfile <FILE>  Provide a file that can be uploaded by a remote escape sequence
    --verbose         Print messages during file upload
    <DEVICE>          Set the device path to a serial port
    <baud rate>       Set the baud rate to connect at [default: 9600]
    <data bits>       Set the number of bits used per character [default: 8] [possible values:
                      5, 6, 7, 8]
    <parity>          Set the parity checking mode [default: N] [possible values: N, O, E]
    <stop bits>       Set the number of stop bits transmitted after every character [default: 1]
                      [possible values: 1, 2]
    <flow control>    Set the flow control mode [default: N] [possible values: N, H, S]

Escape commands begin with <Enter> and end with one of the following sequences:
    ~~ - send the '~' character
    ~. - terminate the connection
```

For more verbose help information and parameter suggestions add the `--help` option:
```bash
scip --help
```

## Binfile support

This version of `scip` supports the upload protocol used by
<https://github.com/rust-embedded/rust-raspberrypi-OS-tutorials/>
for uploading kernel images to a Raspberry Pi: this is
intended as a replacement for the Ruby `minipush` in the
tutorials. See the tutorial source code for details.

## Examples
```bash
scip /dev/ttyUSB0 115200
scip /dev/ttyUSB1 19200 6 E 2 H
scip --binfile kernel8.img /dev/ttyUSB0 921600 8 N 1 N
```

## License
MIT
