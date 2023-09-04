use clap::Parser;
use framebuffer::{Framebuffer, FramebufferError, KdMode};
use std::error::Error;
use std::fs::File;
use std::thread;
use std::time::{Duration, Instant};

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

fn set_keyboard_display_mode(kd_mode: KdMode) -> Result<i32, FramebufferError> {
    let console_devices = ["/dev/tty0", "/dev/tty", "/dev/console"];
    for console_device in console_devices {
        let kd_mode = match &kd_mode {
            KdMode::Graphics => KdMode::Graphics,
            KdMode::Text => KdMode::Text,
        };
        match Framebuffer::set_kd_mode_ex(console_device, kd_mode) {
            Ok(result) => return Ok(result),
            Err(_) => continue,
        }
    }
    Framebuffer::set_kd_mode(kd_mode)
}

fn postpone_next_frame(delay: u64, elapsed: &Duration) {
    let elapsed_time = elapsed.as_millis() as u64;
    if elapsed_time < delay {
        thread::sleep(Duration::from_millis(
            delay - elapsed_time,
        ));
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();

    set_keyboard_display_mode(KdMode::Graphics)?;

    let mut fb = Framebuffer::new(&args.device)?;
    let fb_width = fb.var_screen_info.xres as usize;
    let fb_height = fb.var_screen_info.yres as usize;
    let fb_channels = fb.var_screen_info.bits_per_pixel as usize / 8;

    let mut fb_frame = vec![0; (fb_channels * fb_width * fb_height) as usize];
    let mut frame_prepare_time = Instant::now();

    loop {
        let mut decoder = gif::DecodeOptions::new();
        decoder.set_color_output(gif::ColorOutput::Indexed);

        let file = File::open(&args.gif_file)?;
        let mut decoder = decoder.read_info(file)?;

        let global_palette = decoder.global_palette().unwrap_or_default();
        let global_palette = global_palette.to_vec();

        while let Some(gif_frame) = decoder.read_next_frame()? {
            let palette = match &gif_frame.palette {
                Some(palette) => palette,
                None => &global_palette,
            };

            let buffer = &gif_frame.buffer;
            let lines = buffer.chunks(gif_frame.width as usize);

            for (y, line) in lines.enumerate() {
                for (x, pixel) in line.iter().enumerate() {
                    if let Some(transparent) = gif_frame.transparent {
                        if *pixel == transparent {
                            continue;
                        }
                    }

                    let x = x + gif_frame.left as usize;
                    let y = y + gif_frame.top as usize;
                    if x >= fb_width || y >= fb_height {
                        continue;
                    }

                    let i = (y * fb_width + x) * fb_channels;
                    let j = *pixel as usize * 3;

                    fb_frame[i] = palette[j + 2];
                    fb_frame[i + 1] = palette[j + 1];
                    fb_frame[i + 2] = palette[j];
                }
            }

            fb.write_frame(&fb_frame);

            let delay = args.interval * gif_frame.delay as u64;
            postpone_next_frame(delay, &frame_prepare_time.elapsed());

            frame_prepare_time = Instant::now();
        }

        if args.once {
            set_keyboard_display_mode(KdMode::Text)?;
            return Ok(());
        }
    }
}
