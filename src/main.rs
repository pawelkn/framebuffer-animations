use clap::Parser;
use framebuffer::{Framebuffer, FramebufferError, KdMode};
use std::error::Error;
use std::fs::File;
use std::thread;
use std::time::{Duration, Instant};
use gif::{Decoder, DecodingError, ColorOutput};

#[derive(clap::Parser, Debug, Default)]
#[command(about, version, long_about = None)]
/// Framebuffer GIF animation player
struct Args {
    /// Framebuffer device file
    #[arg(short, long, default_value_t = String::from("/dev/fb0"))]
    device: String,

    /// Interval step for displaying GIF frames (milliseconds)
    #[arg(short, long, default_value_t = 5)]
    interval: u64,

    /// Play the file just one time
    #[arg(short, long, default_value_t = false)]
    once: bool,

    /// GIF file to be played
    gif_file: String,
}

/// Information about the framebuffer
struct FramebufferInfo {
    width: usize,
    height: usize,
    channels: usize
}

/// Retrieves information about the framebuffer.
fn get_framebuffer_info(fb: &Framebuffer) -> FramebufferInfo {
    let width = fb.var_screen_info.xres as usize;
    let height = fb.var_screen_info.yres as usize;
    let channels = fb.var_screen_info.bits_per_pixel as usize / 8;
    FramebufferInfo { width, height, channels }
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
fn process_gif_frame(gif_frame: &gif::Frame, gif_palette: &[u8], fb_frame: &mut [u8], fb_info: &FramebufferInfo) {
    let buffer = &gif_frame.buffer;
    let lines = buffer.chunks(gif_frame.width as usize);

    for (y, line) in lines.enumerate() {
        let y = y + gif_frame.top as usize;
        if y >= fb_info.height {
            break;
        }

        for (x, pixel) in line.iter().enumerate() {
            let x = x + gif_frame.left as usize;
            if x >= fb_info.width {
                break;
            }

            if let Some(transparent) = gif_frame.transparent {
                if *pixel == transparent {
                    continue;
                }
            }

            let i = (y * fb_info.width + x) * fb_info.channels;
            let j = *pixel as usize * 3;

            fb_frame[i] = gif_palette[j + 2];
            fb_frame[i + 1] = gif_palette[j + 1];
            fb_frame[i + 2] = gif_palette[j];
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    // Parse command line arguments
    let args = Args::parse();

    // Set keyboard display mode to graphics
    set_keyboard_display_mode(KdMode::Graphics)?;

    // Initialize framebuffer
    let mut fb = Framebuffer::new(&args.device)?;
    let fb_info = get_framebuffer_info(&fb);

    // Create framebuffer frame buffer
    let mut fb_frame = vec![0; (fb_info.channels * fb_info.width * fb_info.height) as usize];
    let mut frame_prepare_time = Instant::now();

    // Decode GIF file
    let mut decoder = get_gif_decoder(&args.gif_file)?;
    let global_palette = decoder.global_palette().unwrap_or_default();
    let global_palette = global_palette.to_vec();

    loop {
        // Process each frame of the GIF file
        while let Some(gif_frame) = decoder.read_next_frame()? {
            let gif_palette = gif_frame.palette.as_ref().unwrap_or(&global_palette);

            process_gif_frame(gif_frame, gif_palette, &mut fb_frame, &fb_info);
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

