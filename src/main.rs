use std::error::Error;
use std::io::{self, stdin, stdout, Read, Write};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{atomic::{AtomicU8, Ordering::*}, mpsc::{channel, Receiver, TryRecvError}};
use std::thread;
use std::time::Duration;

use clap::{ArgAction, Parser, builder::{PossibleValuesParser, TypedValueParser}};
use serialport::{DataBits, FlowControl, Parity, SerialPort, SerialPortBuilder, StopBits};
use termion::{screen::IntoAlternateScreen, raw::{IntoRawMode, RawTerminal}};
use termion::screen::{AlternateScreen, ToMainScreen};

// clap 4 PossibleValueParser builder.
macro_rules! pvp {
    ($t:ty, $vals:expr) => {
        PossibleValuesParser::new($vals).map(|s| <$t>::from_str(s.as_ref()).unwrap())
    };
}

#[derive(Debug, Parser)]
#[clap(
    author,
    name = env!("CARGO_BIN_NAME"),
    disable_help_flag = true,
    after_help = "
    Escape commands begin with <Enter> and end with one of the following sequences:
    ~~ - send the '~' character
    ~. - terminate the connection
",
    version
)]
struct SC {

    /// Get the path of a binary to push up
    #[clap(long, short)]
    binfile: Option<String>,

    /// Show push in terminal
    #[clap(long, short)]
    verbose: bool,

    /// Help flag
    #[clap(long, action = ArgAction::Help)]
    help: Option<bool>,

    /// Short help flag
    #[clap(short = 'h', action = ArgAction::HelpShort)]
    short_help: Option<bool>,

    /// Set the device path to a serial port
    device: String,

    /// Set the baud rate to connect at
    #[clap(
        name = "baud rate",
        default_value = "9600",
        long_help = r"Set the baud rate to connect at

Common values: 300, 1200, 2400, 4800, 9600, 19200, 38400, 57600, 115200, 230400, 460800, 500000, 576000, 921600, 1000000, 1152000, 1500000, 2000000, 2500000, 3000000, 3500000, 4000000
"
    )]
    baud_rate: u32,

    /// Set the number of bits used per character
    #[clap(
        name = "data bits",
        default_value = "8",
        value_parser = pvp!(u8, &["5", "6", "7", "8"]),
    )]
    data_bits: u8,
    /// Set the parity checking mode
    #[clap(
        name = "parity",
        default_value = "N",
        ignore_case = true,
        value_parser = pvp!(String, &["N","O","E"]),
        long_help = r"Set the parity checking mode

Possible values:
    - N, n => None
    - O, o => Odd
    - E, e => Even
"
    )]
    parity: String,
    /// Set the number of stop bits transmitted after every character
    #[clap(
        name = "stop bits",
        default_value = "1",
        value_parser = pvp!(u8, &["1", "2"]),
    )]
    stop_bits: u8,
    /// Set the flow control mode
    #[clap(
        name = "flow control",
        default_value = "N",
        ignore_case = true,
        value_parser = pvp!(String, &["N","H","S"]),
        long_help = r"Set the flow control mode

Possible values:
    - N, n => None
    - H, h => Hardware    # uses XON/XOFF bytes
    - S, s => Software    # uses RTS/CTS signals
"
    )]
    flow_control: String,
}

enum EscapeState {
    // Wait for Enter
    WaitForEnter,
    // Wait for escape character
    WaitForEC,
    // Ready to process command
    ProcessCMD,
}

enum NextStep {
    LoopContinue,
    LoopBreak,
    Data(Box<([u8; 512], usize)>),
    Upload,
    None,
}

fn main() {
    let sc_args: SC = SC::parse();

    if sc_args.help.is_some() || sc_args.short_help.is_some() {
        println!();
        return;
    }

    let arg_record = match parse_arguments_into_serialport(&sc_args) {
        Ok(a) => a,
        Err(e) => {
            eprint!("Could not open serial port: {}\n\r", e);
            return;
        }
    };

    let path = PathBuf::from(arg_record.device);
    if !path.exists() {
        eprint!("waiting for device\n\r");
        while !path.exists() {
            thread::sleep(Duration::from_millis(100u64));
        }
    }

    let mut serial_port;
    match arg_record.serial.open() {
        Ok(sp) => serial_port = sp,
        Err(err) if err.kind() == serialport::ErrorKind::Io(io::ErrorKind::NotFound) => {
            eprint!("Device not found: {}\n\r", sc_args.device);
            return;
        }
        Err(err) => {
            eprint!("Error opening port, please report this: {:?}\n\r", err);
            return;
        }
    };

    let mut stdin = stdin();
    let mut screen = stdout().into_raw_mode().unwrap().into_alternate_screen().unwrap();

    write_start_screen_msg(&mut screen);

    let (tx, rx) = channel::<([u8; 512], usize)>();

    // read from terminal stdin
    let _terminal_stdin = thread::spawn(move || loop {
        let mut data = [0; 512];
        let n = stdin.read(&mut data[..]).unwrap();
        tx.send((data, n)).unwrap();
    });

    let upload = arg_record.binfile.is_some();
    let mut escape_state: EscapeState = EscapeState::WaitForEnter;
    loop {
        match read_from_serial_port(&mut serial_port, &mut screen, upload) {
            NextStep::None => (),
            NextStep::LoopBreak => break,
            NextStep::Upload if upload => {
                if sc_args.verbose {
                    screen.write_all(b"UPLOADING... ").unwrap();
                }
                let binfile = arg_record.binfile.as_ref().unwrap();
                upload_to_serial_port(binfile, &mut serial_port)
                    .unwrap_or_else(|e| eprint!("{}upload failed: {}", ToMainScreen, e));
                if sc_args.verbose {
                    screen.write_all(b"DONE.\r\n").unwrap();
                }
                continue;
            }
            _ => unreachable!(),
        }

        let data: [u8; 512];
        let n: usize;
        match read_from_stdin_thread(&rx) {
            NextStep::LoopContinue => continue,
            NextStep::LoopBreak => break,
            NextStep::Data(d) => {
                data = d.0;
                n = d.1;
            }
            _ => unreachable!(),
        }

        if n == 1 {
            match escape_state_machine(&data[0], &mut escape_state) {
                NextStep::LoopContinue => continue,
                NextStep::LoopBreak => break,
                _ => {}
            }
        }

        if let NextStep::LoopBreak = write_to_serial_port(&mut serial_port, &data[..n]) {
            break;
        }
    }
}

fn upload_to_serial_port(
    binfile: &str,
    serial_port: &mut Box<dyn SerialPort>,
) -> Result<(), Box<dyn Error>> {
    let data: Vec<u8> = std::fs::read(binfile)?;
    let ndata = data.len();
    assert!(ndata <= u32::MAX as usize);
    for i in 0..4 {
        let b = ((ndata >> (i * 8)) & 0xff) as u8;
        let n = serial_port.write(&[b])?;
        assert_eq!(1, n);
    }
    serial_port.write_all(&data)?;
    Ok(())
}

fn read_from_serial_port(
    serial_port: &mut Box<dyn SerialPort>,
    screen: &mut AlternateScreen<RawTerminal<io::Stdout>>,
    upload: bool,
) -> NextStep {
    static ETX_COUNT: AtomicU8 = AtomicU8::new(0);
    let mut serial_bytes = [0; 512];
    #[allow(clippy::needless_return)]
    match serial_port.read(&mut serial_bytes[..]) {
        Ok(n) => {
            if upload {
                let mut front = 0;
                let mut etx_count = ETX_COUNT.load(Acquire);
                for (i, &b) in serial_bytes[..n].iter().enumerate() {
                    if b == 3 {
                        etx_count += 1;
                        if etx_count >= 3 {
                            ETX_COUNT.store(0, Release);
                            return NextStep::Upload;
                        }
                        screen.write_all(&serial_bytes[front..i]).unwrap();
                        front = i + 1;
                    } else {
                        etx_count = 0;
                    }
                };
                ETX_COUNT.store(etx_count, Release);
                screen.write_all(&serial_bytes[front..n]).unwrap();
            } else {
                screen.write_all(&serial_bytes[..n]).unwrap();
            }
            screen.flush().unwrap();
            return NextStep::None;
        }
        Err(err) if err.kind() == io::ErrorKind::TimedOut => {
            return NextStep::None;
        }
        Err(err) if err.kind() == io::ErrorKind::BrokenPipe => {
            eprint!("{}Device disconnected\n\r", ToMainScreen);
            return NextStep::LoopBreak;
        }
        Err(err) => {
            eprint!("{}{}\n\r", ToMainScreen, err);
            return NextStep::LoopBreak;
        }
    }
}

fn read_from_stdin_thread(rx: &Receiver<([u8; 512], usize)>) -> NextStep {
    match rx.try_recv() {
        Ok(data) => NextStep::Data(Box::new(data)),
        Err(TryRecvError::Empty) => NextStep::LoopContinue,
        Err(TryRecvError::Disconnected) => {
            eprint!("{}Error: Stdin reading thread stopped.\n\r", ToMainScreen);
            NextStep::LoopBreak
        }
    }
}

fn escape_state_machine(character: &u8, escape_state: &mut EscapeState) -> NextStep {
    match escape_state {
        EscapeState::WaitForEnter => {
            if *character == b'\r' || *character == b'\n' {
                *escape_state = EscapeState::WaitForEC;
            }
        }
        EscapeState::WaitForEC => match *character {
            b'~' => {
                *escape_state = EscapeState::ProcessCMD;
                return NextStep::LoopContinue;
            }
            b'\r' => {
                *escape_state = EscapeState::WaitForEC;
            }
            _ => {
                *escape_state = EscapeState::WaitForEnter;
            }
        },
        EscapeState::ProcessCMD => match *character {
            b'.' => {
                return NextStep::LoopBreak;
            }
            b'\r' => {
                *escape_state = EscapeState::WaitForEC;
            }
            _ => {
                *escape_state = EscapeState::WaitForEnter;
            }
        },
    }
    NextStep::None
}

fn write_to_serial_port(serial_port: &mut Box<dyn SerialPort>, data: &[u8]) -> NextStep {
    // try to write terminal input to serial port
    match serial_port.write(data) {
        Ok(_) => {}
        Err(err) if err.kind() == io::ErrorKind::TimedOut => {}
        Err(err) => {
            eprint!("{}{}\n\r", ToMainScreen, err);
            return NextStep::LoopBreak;
        }
    }
    NextStep::None
}

struct ArgRecord {
    binfile: Option<String>,
    device: String,
    serial: SerialPortBuilder,
}

fn parse_arguments_into_serialport(sc_args: &SC) -> Result<ArgRecord, Box<dyn Error>> {
    fn match_data_bits(data_bits: u8) -> Result<DataBits, &'static str> {
        match data_bits {
            8 => Ok(DataBits::Eight),
            7 => Ok(DataBits::Seven),
            6 => Ok(DataBits::Six),
            5 => Ok(DataBits::Five),
            _ => Err("unknown data bits"),
        }
    }
    fn match_parity(parity: &str) -> Result<Parity, &'static str> {
        match parity {
            "N" | "n" => Ok(Parity::None),
            "O" | "o" => Ok(Parity::Odd),
            "E" | "e" => Ok(Parity::Even),
            _ => Err("unknown parity"),
        }
    }
    fn match_stop_bits(stop_bits: u8) -> Result<StopBits, &'static str> {
        match stop_bits {
            1 => Ok(StopBits::One),
            2 => Ok(StopBits::Two),
            _ => Err("unknown stop bits"),
        }
    }
    fn match_flow_control(flow_control: &str) -> Result<FlowControl, &'static str> {
        match flow_control {
            "N" | "n" => Ok(FlowControl::None),
            "H" | "h" => Ok(FlowControl::Hardware),
            "S" | "s" => Ok(FlowControl::Software),
            _ => Err("unknown flow control"),
        }
    }
    let path: &str = &sc_args.device;
    let baud_rate: u32 = sc_args.baud_rate;
    let data_bits: DataBits = match_data_bits(sc_args.data_bits)?;
    let parity: Parity = match_parity(sc_args.parity.as_str())?;
    let stop_bits: StopBits = match_stop_bits(sc_args.stop_bits)?;
    let flow_control: FlowControl = match_flow_control(sc_args.flow_control.as_str())?;
    let timeout: Duration = Duration::from_millis(10);

    let p = serialport::new(path, baud_rate)
        .data_bits(data_bits)
        .parity(parity)
        .stop_bits(stop_bits)
        .flow_control(flow_control)
        .timeout(timeout);
    let arg_record = ArgRecord {
        binfile: sc_args.binfile.clone(),
        device: sc_args.device.clone(),
        serial: p,
    };
    Ok(arg_record)
}

fn write_start_screen_msg(screen: &mut impl Write) {
    write!(
        screen,
        "{}{}Welcome to {}.{}To exit type <Enter> + ~ + .\r\nor unplug the serial port.{}",
        termion::clear::All,
        termion::cursor::Goto(1, 1),
        env!("CARGO_BIN_NAME"),
        termion::cursor::Goto(1, 2),
        termion::cursor::Goto(1, 4)
    )
    .unwrap();
    screen.flush().unwrap();
}
