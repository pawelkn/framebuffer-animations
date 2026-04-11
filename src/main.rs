use framebuffer::{Bitfield, Framebuffer, FramebufferError, KdMode};
use gif::{ColorOutput, Decoder, DecodingError};
use pico_args::Arguments;
use std::error::Error;
use std::process;
use std::thread;
use std::time::{Duration, Instant};
use std::io::Cursor;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use ctrlc;

const HELP: &str = "\
Framebuffer GIF animation player

USAGE:
  fba [OPTIONS] --number NUMBER [INPUT]

FLAGS:
  -h, --help                Prints help information

OPTIONS:
  -d, --device DEVICE       Framebuffer device file [default: /dev/fb0]
  -i, --interval NUMBER     Interval step for displaying GIF frames (milliseconds) [default: 10]
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
    line_length: isize,
    red: Bitfield,
    green: Bitfield,
    blue: Bitfield,
    transp: Bitfield,
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
        interval: pargs.opt_value_from_fn(["-i", "--interval"], parse_interval)?.unwrap_or(10),
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
fn get_framebuffer_info(fb: &Framebuffer) -> Result<FramebufferInfo, Box<dyn Error>> {
    let width = fb.var_screen_info.xres as isize;
    let height = fb.var_screen_info.yres as isize;
    let channels = fb.var_screen_info.bits_per_pixel as isize / 8;
    let line_length = fb.fix_screen_info.line_length as isize;

    if !matches!(channels, 2 | 3 | 4) {
        return Err(format!("Unsupported framebuffer format: {} bits per pixel", fb.var_screen_info.bits_per_pixel).into());
    }

    Ok(FramebufferInfo {
        width,
        height,
        channels,
        line_length,
        red: fb.var_screen_info.red.clone(),
        green: fb.var_screen_info.green.clone(),
        blue: fb.var_screen_info.blue.clone(),
        transp: fb.var_screen_info.transp.clone(),
    })
}

fn reverse_bits(value: u32, bits: u32) -> u32 {
    let mut value = value;
    let mut reversed = 0;
    for _ in 0..bits {
        reversed = (reversed << 1) | (value & 1);
        value >>= 1;
    }
    reversed
}

fn scale_channel(value: u8, bitfield: &Bitfield) -> u32 {
    if bitfield.length == 0 {
        return 0;
    }

    let max = (1u32 << bitfield.length) - 1;
    let scaled = ((value as u32 * max + 127) / 255) as u32;
    let scaled = if bitfield.msb_right == 0 {
        scaled
    } else {
        reverse_bits(scaled, bitfield.length)
    };

    scaled << bitfield.offset
}

fn write_pixel_to_fb(fb_frame: &mut [u8], i: usize, fb_info: &FramebufferInfo, red: u8, green: u8, blue: u8) {
    let mut pixel = scale_channel(red, &fb_info.red)
        | scale_channel(green, &fb_info.green)
        | scale_channel(blue, &fb_info.blue);

    if fb_info.transp.length > 0 {
        let alpha = (1u32 << fb_info.transp.length) - 1;
        pixel |= alpha << fb_info.transp.offset;
    }

    let bytes = pixel.to_le_bytes();
    for byte_index in 0..(fb_info.channels as usize) {
        fb_frame[i + byte_index] = bytes[byte_index];
    }
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
fn get_gif_decoder_from_cursor(cursor: Cursor<&[u8]>) -> Result<Decoder<Cursor<&[u8]>>, DecodingError> {
    let mut decode_options = gif::DecodeOptions::new();
    decode_options.set_color_output(ColorOutput::Indexed);
    decode_options.read_info(cursor)
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

            let i = (y * fb_info.line_length + x * fb_info.channels) as usize;
            let j = *pixel as usize * 3;

            write_pixel_to_fb(
                fb_frame,
                i,
                &fb_info,
                gif_palette[j],
                gif_palette[j + 1],
                gif_palette[j + 2],
            );
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
    let fb_info = get_framebuffer_info(&fb)?;

    // Create framebuffer frame buffer
    let mut fb_frame = vec![0; fb.frame.len() as usize];
    let mut frame_prepare_time = Instant::now();

    // Decode GIF file
    let gif_data = std::fs::read(&args.gif_file)?;
    let mut decoder = get_gif_decoder_from_cursor(Cursor::new(&gif_data))?;
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

    // Handle Ctrl-C signal for graceful shutdown
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
        println!("Received SIGINT, shutting down...");
    }).expect("Error setting Ctrl-C handler");


    loop {
        decoder = get_gif_decoder_from_cursor(Cursor::new(&gif_data))?; // resets the read head of gif

        // Process each frame of the GIF file
        while let Some(gif_frame) = decoder.read_next_frame()? {
            let gif_palette = gif_frame.palette.as_ref().unwrap_or(&global_palette);

            process_gif_frame(gif_frame, gif_palette, &mut fb_frame, &fb_info, &offset);
            fb.write_frame(&fb_frame);

            let delay = args.interval * gif_frame.delay as u64;
            postpone_next_frame(delay, &frame_prepare_time.elapsed());
            frame_prepare_time = Instant::now();
        }

        // Stop after one the GIF file loop, if specified or Ctrl-C signal is received
        if args.once || !running.load(Ordering::SeqCst) {
            break;
        }

        // gif_cursor is dropped here; next iteration creates a new one from the same Vec
    }

    // Set keyboard display mode back to text
    set_keyboard_display_mode(KdMode::Text)?;

    Ok(())
}
