use framebuffer::{Framebuffer, FramebufferError, KdMode};
use gif::{ColorOutput, Decoder, DecodingError};
use pico_args::Arguments;
use std::error::Error;
use std::fs::File;
use std::process;
use std::thread;
use std::time::{Duration, Instant};

const HELP: &str = "\
Framebuffer GIF animation player

USAGE:
  fba [OPTIONS] --number NUMBER [INPUT]

FLAGS:
  -h, --help                Prints help information

OPTIONS:
  -d, --device DEVICE       Framebuffer device file [default: /dev/fb0]
  -i, --interval NUMBER     Interval step for displaying GIF frames (milliseconds) [default: 5]
  -o, --once                Play the file just one time
  -c, --center              Center the GIF

ARGS:
  <FILE>                    GIF file to be played
";

/// Command line arguments
#[derive(Debug)]
struct Args {
    device: String,
    interval: u64,
    once: bool,
    center: bool,
    gif_file: String,
}

/// Information about the framebuffer
struct FramebufferInfo {
    width: isize,
    height: isize,
    channels: isize,
    alignment: isize,
}

struct Offset {
    x: isize,
    y: isize,
}

/// Parses command line arguments
fn parse_args() -> Result<Args, pico_args::Error> {
    let mut pargs = Arguments::from_env();

    // Help has a higher priority and should be handled separately.
    if pargs.contains(["-h", "--help"]) {
        print!("{}", HELP);
        process::exit(0);
    }

    let args = Args {
        device: pargs.opt_value_from_str(["-d", "--device"])?.unwrap_or("/dev/fb0".to_string()),
        interval: pargs.opt_value_from_fn(["-i", "--interval"], parse_interval)?.unwrap_or(5),
        once: pargs.contains(["-o", "--once"]),
        center: pargs.contains(["-c", "--center"]),
        gif_file: pargs.free_from_str()?,
    };

    // It's up to the caller what to do with the remaining arguments.
    let remaining = pargs.finish();
    if !remaining.is_empty() {
        eprintln!("Warning: unused arguments left: {:?}.", remaining);
    }

    Ok(args)
}

/// Parses the interval argument.
fn parse_interval(s: &str) -> Result<u64, &'static str> {
    s.parse().map_err(|_| "not a number")
}

/// Retrieves information about the framebuffer.
fn get_framebuffer_info(fb: &Framebuffer) -> FramebufferInfo {
    let width = fb.var_screen_info.xres as isize;
    let height = fb.var_screen_info.yres as isize;
    let channels = fb.var_screen_info.bits_per_pixel as isize / 8;
    let alignment = fb.fix_screen_info.line_length as isize - fb.var_screen_info.xres as isize * channels;
    FramebufferInfo { width, height, channels, alignment }
}

/// Sets the keyboard display mode to either graphics or text mode.
fn set_keyboard_display_mode(kd_mode: KdMode) -> Result<i32, FramebufferError> {
    let console_devices = ["/dev/tty0", "/dev/tty", "/dev/console"];
    for console_device in console_devices {
        let kd_mode = match &kd_mode {
            KdMode::Graphics => KdMode::Graphics,
            KdMode::Text => KdMode::Text,
        };
        if let Ok(result) = Framebuffer::set_kd_mode_ex(console_device, kd_mode) {
            return Ok(result);
        }
    }
    Framebuffer::set_kd_mode(kd_mode)
}

/// Delays the execution of the next frame in a GIF animation based on the specified delay and
/// the elapsed time since the previous frame was prepared.
fn postpone_next_frame(delay: u64, elapsed: &Duration) {
    let elapsed_time = elapsed.as_millis() as u64;
    if elapsed_time < delay {
        let remaining_time = delay - elapsed_time;
        thread::sleep(Duration::from_millis(remaining_time));
    }
}

/// Creates a `gif::Decoder` instance to decode a GIF file.
fn get_gif_decoder(gif_file: &str) -> Result<Decoder<File>, DecodingError> {
    let file = File::open(gif_file)?;
    let mut decode_options = gif::DecodeOptions::new();
    decode_options.set_color_output(ColorOutput::Indexed);
    decode_options.read_info(file)
}

/// Processes a single frame of a GIF image and updates the framebuffer frame buffer accordingly.
fn process_gif_frame(gif_frame: &gif::Frame, gif_palette: &[u8], fb_frame: &mut [u8], fb_info: &FramebufferInfo, offset: &Offset) {
    let buffer = &gif_frame.buffer;
    let lines = buffer.chunks(gif_frame.width as usize);

    for (y, line) in lines.enumerate() {
        let y = y as isize + offset.y + gif_frame.top as isize;
        if y < 0 {
            continue;
        }

        if y >= fb_info.height {
            break;
        }

        for (x, pixel) in line.iter().enumerate() {
            let x = x as isize + offset.x + gif_frame.left as isize;
            if x < 0 {
                continue;
            }
            if x >= fb_info.width {
                break;
            }

            if let Some(transparent) = gif_frame.transparent {
                if *pixel == transparent {
                    continue;
                }
            }

            let i = ((y * fb_info.width + x) * fb_info.channels + y * fb_info.alignment) as usize;
            let j = *pixel as usize * 3;

            fb_frame[i] = gif_palette[j + 2];
            fb_frame[i + 1] = gif_palette[j + 1];
            fb_frame[i + 2] = gif_palette[j];
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    // Parse command line arguments
    let args = parse_args()?;

    // Set keyboard display mode to graphics
    set_keyboard_display_mode(KdMode::Graphics)?;

    // Initialize framebuffer
    let mut fb = Framebuffer::new(&args.device)?;
    let fb_info = get_framebuffer_info(&fb);

    // Create framebuffer frame buffer
    let mut fb_frame = vec![0; (fb.frame.len()) as usize];
    let mut frame_prepare_time = Instant::now();

    // Decode GIF file
    let mut decoder = get_gif_decoder(&args.gif_file)?;
    let global_palette = decoder.global_palette().unwrap_or_default();
    let global_palette = global_palette.to_vec();

    // Calulcate Offset
    let offset = if args.center {
        Offset {
            x: (fb_info.width - decoder.width() as isize) / 2,
            y: (fb_info.height - decoder.height() as isize) / 2,
        }
    } else {
        Offset { x: 0, y: 0 }
    };

    loop {
        // Process each frame of the GIF file
        while let Some(gif_frame) = decoder.read_next_frame()? {
            let gif_palette = gif_frame.palette.as_ref().unwrap_or(&global_palette);

            process_gif_frame(gif_frame, gif_palette, &mut fb_frame, &fb_info, &offset);
            fb.write_frame(&fb_frame);

            let delay = args.interval * gif_frame.delay as u64;
            postpone_next_frame(delay, &frame_prepare_time.elapsed());
            frame_prepare_time = Instant::now();
        }

        // Stop after one the GIF file loop, if specified
        if args.once {
            break;
        }

        // Reinitialize the decoder to the beginning of the GIF file
        decoder = get_gif_decoder(&args.gif_file)?;
    }

    // Set keyboard display mode back to text
    set_keyboard_display_mode(KdMode::Text)?;

    Ok(())
}
