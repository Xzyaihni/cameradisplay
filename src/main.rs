use std::time::Instant;

use image::{Rgb, DynamicImage};

use nokhwa::{
    Camera,
    pixel_format::RgbFormat,
    utils::{
        RequestedFormat,
        RequestedFormatType,
        CameraIndex,
        Resolution,
        CameraControl,
        KnownCameraControl,
        ControlValueSetter,
        ControlValueDescription
    }
};

use sdl2::{
    keyboard::Keycode,
    pixels::PixelFormatEnum,
    surface::Surface,
    event::{WindowEvent, Event},
    render::WindowCanvas
};


fn set_closest_aspect(window: &mut WindowCanvas, aspect: f64) -> bool
{
    let window = window.window_mut();
    let (width, height) = window.size();

    let height_scaled = height as f64 * aspect;

    let (new_width, new_height) = if height_scaled > width as f64
    {
        (width, (width as f64 / aspect) as u32)
    } else
    {
        ((height as f64 * aspect) as u32, height)
    };

    if new_width == width && new_height == height
    {
        return false;
    }

    if let Err(err) = window.set_size(new_width, new_height)
    {
        eprintln!("window resize error: {err}");
    }

    true
}

struct Averager<const WINDOW_SIZE: usize>
{
    window: [f64; WINDOW_SIZE],
    index: usize
}

impl<const WINDOW_SIZE: usize> Averager<WINDOW_SIZE>
{
    pub fn new() -> Self
    {
        Self{window: [0.0; WINDOW_SIZE], index: 0}
    }

    pub fn add(&mut self, value: f64) -> f64
    {
        self.window[self.index] = value;

        self.index += 1;
        if self.index == self.window.len()
        {
            self.index = 0;
        }

        self.window.iter().copied().sum::<f64>() / WINDOW_SIZE as f64
    }
}

#[allow(dead_code)]
struct ControlInfo
{
    pub min: i64,
    pub max: i64,
    pub value: i64,
    pub step: i64,
    pub default: i64
}

struct ControlController
{
    control: Option<CameraControl>,
    current: i64,
    which: KnownCameraControl
}

impl ControlController
{
    pub fn new(camera: &Camera, which: KnownCameraControl) -> Self
    {
        let control = camera.camera_control(which).ok();

        let mut this = Self{control, current: 0, which};

        this.current = this.current_raw();

        this
    }

    fn info(&self) -> ControlInfo
    {
        if let ControlValueDescription::IntegerRange{
            min,
            max,
            value,
            step,
            default
        } = self.control.as_ref().unwrap().description().clone()
        {
            ControlInfo{min, max, value, step, default}
        } else
        {
            panic!("control must be an integer range")
        }
    }

    pub fn clamp(&self, value: i64) -> i64
    {
        let info = self.info();
        value.clamp(info.min, info.max)
    }

    fn current_raw(&self) -> i64
    {
        self.info().value
    }

    pub fn current(&self) -> i64
    {
        self.current
    }

    pub fn reset(&mut self, camera: &mut Camera)
    {
        let value = self.info().default;
        self.set(camera, value)
    }

    pub fn set_max(&mut self, camera: &mut Camera)
    {
        let value = self.info().max;
        self.set(camera, value)
    }

    pub fn set(&mut self, camera: &mut Camera, value: i64)
    {
        if self.control.is_none()
        {
            return;
        }

        let value = self.clamp(value);

        if value == self.current
        {
            return;
        }

        self.current = value;

        let value = ControlValueSetter::Integer(value);
        if let Err(err) = camera.set_camera_control(self.which, value)
        {
            eprintln!("error setting control: {err}");
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum GammaMode
{
    Manual{fullbright: bool, current: i64},
    Auto
}

fn main()
{
    let ctx = sdl2::init().unwrap();

    let video = ctx.video().unwrap();

    let camera_format = RequestedFormat::new::<RgbFormat>(RequestedFormatType::AbsoluteHighestResolution);
    let mut camera = (0..10).filter_map(|i| Camera::new(CameraIndex::Index(i), camera_format).ok())
        .next()
        .unwrap_or_else(|| panic!("couldnt find a camera"));

    let request_framerate = 10;

    if let Err(err) = camera.set_frame_rate(request_framerate)
    {
        eprintln!("error setting framerate to {request_framerate}: {err}");
    }

    let mut gamma_control = ControlController::new(&camera, KnownCameraControl::Gamma);
    let mut brightness_control = ControlController::new(&camera, KnownCameraControl::Brightness);

    let Resolution{width_x: width, height_y: height} = camera.camera_format().resolution();

    let aspect = width as f64 / height as f64;

    let window = video.window("cam", width, height)
        .always_on_top()
        .resizable()
        .build()
        .unwrap();

    let mut canvas = window.into_canvas().build().unwrap();

    canvas.clear();
    canvas.present();

    let mut events = ctx.event_pump().unwrap();

    camera.open_stream().unwrap();

    let mut gamma_mode = GammaMode::Manual{fullbright: false, current: gamma_control.current()};

    let mut averager: Averager<5> = Averager::new();
    let target_brightness = 15.0;
    let brightness_range = 10.0;

    let mut mirrored = false;

    let mut title_delay = 0;

    let mut resized = false;
    let mut last_frame = Instant::now();

    'window_loop: loop
    {
        for event in events.poll_iter()
        {
            match event
            {
                Event::Quit{..} => break 'window_loop,
                Event::Window{win_event: WindowEvent::SizeChanged(_, _), ..} =>
                {
                    resized = true;
                },
                Event::KeyDown{keycode: Some(code), ..} =>
                {
                    match code
                    {
                        Keycode::SPACE =>
                        {
                            if let Err(err) = canvas.window_mut().set_size(width, height)
                            {
                                eprintln!("error setting window size: {err}");
                            }
                        },
                        Keycode::M =>
                        {
                            mirrored = !mirrored;
                        },
                        Keycode::G =>
                        {
                            gamma_control.reset(&mut camera);
                            brightness_control.reset(&mut camera);

                            gamma_mode = match gamma_mode
                            {
                                GammaMode::Manual{..} => GammaMode::Auto,
                                GammaMode::Auto => GammaMode::Manual{fullbright: false, current: gamma_control.current()}
                            };
                        },
                        Keycode::F =>
                        {
                            if let GammaMode::Manual{ref mut fullbright, current} = gamma_mode
                            {
                                *fullbright = !*fullbright;

                                if *fullbright
                                {
                                    gamma_control.set_max(&mut camera);
                                    brightness_control.set_max(&mut camera);
                                } else
                                {
                                    gamma_control.set(&mut camera, current);
                                    brightness_control.reset(&mut camera);
                                }
                            }
                        },
                        Keycode::Up | Keycode::Down =>
                        {
                            if let GammaMode::Manual{ref mut current, ..} = gamma_mode
                            {
                                let new_current = if let Keycode::Up = code
                                {
                                    *current + 1
                                } else
                                {
                                    *current - 1
                                };

                                gamma_control.set(&mut camera, new_current);
                                *current = gamma_control.current();
                            }
                        },
                        _ => ()
                    }

                    title_delay = 0;
                },
                _ => ()
            }
        }

        if resized
        {
            if !set_closest_aspect(&mut canvas, aspect)
            {
                resized = false;
            }
        }

        let frame = match camera.frame()
        {
            Ok(x) => x,
            Err(err) =>
            {
                eprintln!("error getting a frame: {err}");
                continue;
            }
        };

        let mut image = match frame.decode_image::<RgbFormat>()
        {
            Ok(x) => x,
            Err(err) =>
            {
                eprintln!("error decoding the frame: {err}");
                continue;
            }
        };

        if mirrored
        {
            image = DynamicImage::from(image).fliph().to_rgb8();
        }

        if gamma_mode == GammaMode::Auto
        {
            let average_brightness = {
                let total = (image.width() * image.height()) as f64;

                let luminance = image.pixels().map(|Rgb([r, g, b])|
                {
                    let d = |&x|
                    {
                        let value = x as f64 / u8::MAX as f64;

                        if value < 0.04045
                        {
                            value / 12.92
                        } else
                        {
                            ((value + 0.055) / 1.055).powf(2.4)
                        }
                    };

                    d(r) * 0.2126 + d(g) * 0.7152 + d(b) * 0.0722
                }).sum::<f64>() / total;

                if luminance <= 0.008856
                {
                    luminance * 903.3
                } else
                {
                    luminance.cbrt() * 116.0 - 16.0
                }
            };

            let brightness_diff = target_brightness - average_brightness;

            if brightness_diff.abs() > brightness_range
            {
                let current_gamma = gamma_control.current();
                let new_gamma = if brightness_diff < 0.0
                {
                    current_gamma - 1
                } else
                {
                    current_gamma + 1
                };

                gamma_control.set(&mut camera, new_gamma);
            }
        }

        let surface_rect;
        let is_same_size;

        {
            let mut surface = canvas.window().surface(&events).unwrap();
            surface_rect = surface.rect();

            let width = image.width();
            let height = image.height();

            let mut data = image.into_raw();

            let surface_image = Surface::from_data(
                &mut data,
                width,
                height,
                width * 3,
                PixelFormatEnum::RGB24
            ).unwrap();

            is_same_size = surface_rect.width() == width && surface_rect.height() == height;

            if is_same_size
            {
                surface_image.blit(None, &mut surface, None).unwrap();
            } else
            {
                surface_image.blit_scaled(None, &mut surface, surface_rect).unwrap();
            }

            surface.update_window().unwrap();
        }

        let frametime = last_frame.elapsed().as_secs_f64() * 1000.0;
        let current_average = averager.add(frametime);

        title_delay -= 1;
        if title_delay <= 0
        {
            let width = surface_rect.width();
            let height = surface_rect.height();

            let fps = 1000.0 / current_average;
            let gamma = gamma_control.current();

            let gamma_tag = match gamma_mode
            {
                GammaMode::Auto => "AUTO",
                GammaMode::Manual{fullbright: true, ..} => "FULLBRIGHT",
                GammaMode::Manual{..} => ""
            };

            let gamma_tag = if gamma_tag.is_empty()
            {
                String::new()
            } else
            {
                format!("[{gamma_tag}] ")
            };

            let mut title = format!("{width}x{height}, {fps:.1} fps, {gamma_tag}{gamma} gamma");

            if is_same_size
            {
                title = "[EXACT SIZE] ".to_owned() + &title;
            }

            if let Err(err) = canvas.window_mut().set_title(&title)
            {
                eprintln!("error updating title: {err}");
            }

            title_delay = 10;
        }

        last_frame = Instant::now();
    }

    gamma_control.reset(&mut camera);
    brightness_control.reset(&mut camera);
}
