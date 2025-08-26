use std::error::Error;
use std::io::{self, stdin, stdout, Read, Write};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::thread;
use std::time::Duration;

use clap::{Parser, builder::{PossibleValuesParser, TypedValueParser}};
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
    after_help = "\
Escape commands begin with <Enter> and end with one of the following sequences:
    ~~ - send the '~' character
    ~. - terminate the connection
",
    mut_arg(
        "help",
        |a| a.help("Print help information,\nPrint verbose help information with --help")
    ),
    name = env!("CARGO_BIN_NAME"),
    version
)]
struct SC {
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

    help: bool,
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
    None,
}

fn main() {
    let sc_args: SC = SC::parse();

    let (path, port_builder) = match parse_arguments_into_serialport(&sc_args) {
        Ok(a) => a,
        Err(e) => {
            eprint!("Could not open serial port: {}\n\r", e);
            return;
        }
    };

    let path = PathBuf::from(path);
    if !path.exists() {
        eprint!("waiting for device\n\r");
        while !path.exists() {
            thread::sleep(Duration::from_millis(100u64));
        }
    }

    let mut serial_port;
    match port_builder.open() {
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

    let mut escape_state: EscapeState = EscapeState::WaitForEnter;
    loop {
        if let NextStep::LoopBreak = read_from_serial_port(&mut serial_port, &mut screen) {
            break;
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

fn read_from_serial_port(
    serial_port: &mut Box<dyn SerialPort>,
    screen: &mut AlternateScreen<RawTerminal<io::Stdout>>,
) -> NextStep {
    let mut serial_bytes = [0; 512];
    match serial_port.read(&mut serial_bytes[..]) {
        Ok(n) => {
            if n > 0 {
                screen.write_all(&serial_bytes[..n]).unwrap();
                screen.flush().unwrap();
            }
        }
        Err(err) if err.kind() == io::ErrorKind::TimedOut => {}
        Err(err) if err.kind() == io::ErrorKind::BrokenPipe => {
            eprint!("{}Device disconnected\n\r", ToMainScreen);
            return NextStep::LoopBreak;
        }
        Err(err) => {
            eprint!("{}{}\n\r", ToMainScreen, err);
            return NextStep::LoopBreak;
        }
    }
    NextStep::None
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

fn parse_arguments_into_serialport(sc_args: &SC) -> Result<(String, SerialPortBuilder), Box<dyn Error>> {
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
    Ok((path.into(), p))
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
