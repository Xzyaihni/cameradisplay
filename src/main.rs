use std::{
    thread,
    convert,
    time::Duration,
    sync::{
        Arc,
        Mutex,
        mpsc::{self, TryRecvError}
    },
    time::Instant
};

use image::{Rgb, DynamicImage, RgbImage};

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
    rect::Rect,
    keyboard::{Mod, Keycode},
    pixels::PixelFormatEnum,
    event::{WindowEvent, Event},
    render::{Texture, WindowCanvas}
};


const UPDATE_FPS: u32 = 60;

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

#[derive(Debug, Clone)]
enum ProgramMessage
{
    Render(Box<RgbImage>),
    SetWindowSize(u32, u32),
    SetClosestAspect(f64),
    SetTitle(String)
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

enum CropControl
{
    ZoomXPlus,
    ZoomXMinus,
    ZoomYPlus,
    ZoomYMinus,
    ShiftXPlus,
    ShiftXMinus,
    ShiftYPlus,
    ShiftYMinus,
    Length
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct CropInfo
{
    pub scale_x: f32,
    pub scale_y: f32,
    pub pos_x: f32,
    pub pos_y: f32
}

impl CropInfo
{
    pub fn new() -> Self
    {
        Self{
            scale_x: 1.0,
            scale_y: 1.0,
            pos_x: 0.5,
            pos_y: 0.5
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
    let camera_format = RequestedFormat::new::<RgbFormat>(RequestedFormatType::AbsoluteHighestResolution);
    let mut camera = (0..10).filter_map(|i| Camera::new(CameraIndex::Index(i), camera_format).ok())
        .next()
        .unwrap_or_else(|| panic!("couldnt find a camera"));

    let update_fps = (camera.frame_rate() * 2).max(UPDATE_FPS);

    let mut gamma_control = ControlController::new(&camera, KnownCameraControl::Gamma);
    let mut brightness_control = ControlController::new(&camera, KnownCameraControl::Brightness);

    let Resolution{width_x: width, height_y: height} = camera.camera_format().resolution();

    let aspect = width as f64 / height as f64;

    camera.open_stream().unwrap();

    let mut gamma_mode = GammaMode::Manual{fullbright: false, current: gamma_control.current()};

    let mut averager: Averager<5> = Averager::new();
    let target_brightness = 15.0;
    let brightness_range = 10.0;

    let mut mirrored = false;

    let mut title_delay = 0;

    let mut resized = false;
    let mut last_frame = Instant::now();

    let (tx, rx) = mpsc::channel();
    let (response_tx, response_rx) = mpsc::channel();

    let slow_events = Arc::new(Mutex::new(Vec::new()));

    let input_thread = {
        let slow_events = slow_events.clone();

        thread::spawn(move ||
        {
            let ctx = sdl2::init().unwrap();

            let video = ctx.video().unwrap();
            video.enable_screen_saver();

            let window = video.window("cam", width, height)
                .always_on_top()
                .resizable()
                .build()
                .unwrap();

            let mut canvas = window.into_canvas().build().unwrap();

            canvas.clear();
            canvas.present();

            let mut events = ctx.event_pump().unwrap();

            let texture_creator = canvas.texture_creator();
            let mut camera_texture: Option<Texture> = None;

            let mut crop_info = CropInfo::new();
            let mut crop_controls = [false; CropControl::Length as usize];

            fn crop_control_of(
                code: Keycode,
                keymod: Mod,
                ignore_shift: bool,
                mut f: impl FnMut(CropControl) -> bool
            ) -> bool
            {
                let is_shift = keymod == Mod::LSHIFTMOD;

                match code
                {
                    Keycode::Minus =>
                    {
                        if ignore_shift
                        {
                            f(CropControl::ZoomXMinus);
                            f(CropControl::ZoomYMinus)
                        } else if is_shift
                        {
                            f(CropControl::ZoomYMinus)
                        } else
                        {
                            f(CropControl::ZoomXMinus)
                        }
                    },
                    Keycode::Equals =>
                    {
                        if ignore_shift
                        {
                            f(CropControl::ZoomXPlus);
                            f(CropControl::ZoomYPlus)
                        } else if is_shift
                        {
                            f(CropControl::ZoomYPlus)
                        } else
                        {
                            f(CropControl::ZoomXPlus)
                        }
                    },
                    Keycode::Up if is_shift || ignore_shift => f(CropControl::ShiftYPlus),
                    Keycode::Down if is_shift || ignore_shift => f(CropControl::ShiftYMinus),
                    Keycode::Right if is_shift || ignore_shift => f(CropControl::ShiftXPlus),
                    Keycode::Left if is_shift || ignore_shift => f(CropControl::ShiftXMinus),
                    _ => false
                }
            }

            loop
            {
                let received: Option<ProgramMessage> = match rx.try_recv()
                {
                    Ok(x) => Some(x),
                    Err(TryRecvError::Empty) => None,
                    _ => return
                };

                for event in events.poll_iter()
                {
                    match event
                    {
                        Event::KeyUp{keycode: Some(code), keymod, ..} =>
                        {
                            let handled = crop_control_of(code, keymod, true, |c|
                            {
                                crop_controls[c as usize] = false;

                                true
                            });

                            if handled
                            {
                                continue;
                            }
                        },
                        Event::KeyDown{keycode: Some(code), keymod, ..} =>
                        {
                            let handled = crop_control_of(code, keymod, false, |c|
                            {
                                crop_controls[c as usize] = true;

                                true
                            });

                            if handled
                            {
                                continue;
                            }
                        },
                        _ => ()
                    }

                    slow_events.lock().unwrap().push(event);
                }

                {
                    let dt: f32 = 1.0 / update_fps as f32;

                    let zoom_speed = 0.25;

                    let zoom_in_factor = 1.0 - dt * zoom_speed;
                    let zoom_out_factor = 1.0 + dt * zoom_speed;

                    let c = |x| crop_controls[x as usize];

                    let low_zoom = 0.01;

                    if c(CropControl::ZoomXPlus)
                    {
                        crop_info.scale_x = (crop_info.scale_x * zoom_in_factor).clamp(low_zoom, 1.0);
                    }

                    if c(CropControl::ZoomYPlus)
                    {
                        crop_info.scale_y = (crop_info.scale_y * zoom_in_factor).clamp(low_zoom, 1.0);
                    }

                    if c(CropControl::ZoomXMinus)
                    {
                        crop_info.scale_x = (crop_info.scale_x * zoom_out_factor).clamp(low_zoom, 1.0);
                    }

                    if c(CropControl::ZoomYMinus)
                    {
                        crop_info.scale_y = (crop_info.scale_y * zoom_out_factor).clamp(low_zoom, 1.0);
                    }

                    let change_pos = |value: &mut f32, amount: f32, zoom: f32|
                    {
                        let half_zoom = zoom * 0.5;

                        let low = half_zoom;
                        let high = 1.0 - half_zoom;

                        *value = (*value + amount * zoom).clamp(low, high);
                    };

                    let move_speed = 0.8 * dt;

                    if c(CropControl::ShiftXPlus)
                    {
                        change_pos(&mut crop_info.pos_x, move_speed, crop_info.scale_x);
                    }

                    if c(CropControl::ShiftXMinus)
                    {
                        change_pos(&mut crop_info.pos_x, -move_speed, crop_info.scale_x);
                    }

                    if c(CropControl::ShiftYMinus)
                    {
                        change_pos(&mut crop_info.pos_y, move_speed, crop_info.scale_y);
                    }

                    if c(CropControl::ShiftYPlus)
                    {
                        change_pos(&mut crop_info.pos_y, -move_speed, crop_info.scale_y);
                    }

                    if crop_controls.iter().copied().any(convert::identity)
                    {
                        let idk = ();
                        // set_closest_aspect(&mut canvas, aspect * crop_info.aspect());
                    }
                }

                if let Some(received) = received
                {
                    match received
                    {
                        ProgramMessage::Render(image) =>
                        {
                            let original_width = image.width();
                            let original_height = image.height();

                            let mut data = image.into_raw();

                            if camera_texture.is_none()
                            {
                                camera_texture = Some(texture_creator.create_texture_streaming(
                                    PixelFormatEnum::RGB24,
                                    original_width,
                                    original_height
                                ).unwrap());
                            }

                            {
                                let camera_texture = camera_texture.as_mut().unwrap();

                                camera_texture.update(
                                    None,
                                    &mut data,
                                    (original_width * 3) as usize
                                ).unwrap();
                            }

                            let camera_texture = camera_texture.as_ref().unwrap();

                            let src = if crop_info == CropInfo::new()
                            {
                                None
                            } else
                            {
                                let width = (original_width as f32 * crop_info.scale_x) as u32;
                                let height = (original_height as f32 * crop_info.scale_y) as u32;

                                let pos_of = |p: f32, s: f32, original: u32| -> i32
                                {
                                    ((p - s * 0.5) * original as f32) as i32
                                };

                                let cropped_rect = Rect::new(
                                    pos_of(crop_info.pos_x, crop_info.scale_x, original_width),
                                    pos_of(crop_info.pos_y, crop_info.scale_y, original_height),
                                    width,
                                    height
                                );

                                Some(cropped_rect)
                            };

                            canvas.copy(camera_texture, src, None).unwrap();
                            canvas.present();
                        },
                        ProgramMessage::SetWindowSize(width, height) =>
                        {
                            if let Err(err) = canvas.window_mut().set_size(width, height)
                            {
                                eprintln!("error setting window size: {err}");
                            }
                        },
                        ProgramMessage::SetClosestAspect(aspect) =>
                        {
                            let has_resized = !set_closest_aspect(&mut canvas, aspect);
                            response_tx.send(has_resized).unwrap();
                        },
                        ProgramMessage::SetTitle(title) =>
                        {
                            if let Err(err) = canvas.window_mut().set_title(&title)
                            {
                                eprintln!("error updating title: {err}");
                            }
                        }
                    }
                }

                thread::sleep(Duration::from_millis(1000 / update_fps as u64));
            }
        })
    };

    'window_loop: loop
    {
        for event in slow_events.lock().unwrap().drain(..)
        {
            match event
            {
                Event::Quit{..} => break 'window_loop,
                Event::Window{win_event: WindowEvent::SizeChanged(_, _), ..} =>
                {
                    resized = true;
                },
                Event::KeyDown{keycode: Some(code), keymod, ..} =>
                {
                    match code
                    {
                        Keycode::SPACE =>
                        {
                            tx.send(ProgramMessage::SetWindowSize(width, height)).unwrap();
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
                        Keycode::Up | Keycode::Down if keymod != Mod::LSHIFTMOD =>
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
            tx.send(ProgramMessage::SetClosestAspect(aspect)).unwrap();
            if response_rx.recv().unwrap()
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

        let frametime = last_frame.elapsed().as_secs_f64() * 1000.0;
        let current_average = averager.add(frametime);

        tx.send(ProgramMessage::Render(Box::new(image))).unwrap();

        title_delay -= 1;
        if title_delay <= 0
        {
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

            let title = format!("{fps:.1} fps, {gamma_tag}{gamma} gamma");

            tx.send(ProgramMessage::SetTitle(title)).unwrap();

            title_delay = 10;
        }

        last_frame = Instant::now();
    }

    gamma_control.reset(&mut camera);
    brightness_control.reset(&mut camera);

    drop(tx);

    input_thread.join().unwrap();
}
