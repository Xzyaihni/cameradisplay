use std::time::Instant;

use nokhwa::{
    Camera,
    pixel_format::RgbFormat,
    utils::{RequestedFormat, RequestedFormatType, CameraIndex, Resolution}
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
        (height_scaled as u32, height)
    } else
    {
        (width, (width as f64 / aspect) as u32)
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

fn main()
{
    let ctx = sdl2::init().unwrap();

    let video = ctx.video().unwrap();

    let camera_format = RequestedFormat::new::<RgbFormat>(RequestedFormatType::AbsoluteHighestResolution);
    let mut camera = Camera::new(CameraIndex::Index(0), camera_format).unwrap();

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

    let mut averager: Averager<5> = Averager::new();

    let mut title_delay = 0;
    let mut resized = false;
    let mut last_frame = Instant::now();
    loop
    {
        for event in events.poll_iter()
        {
            match event
            {
                Event::Quit{..} => return,
                Event::Window{win_event: WindowEvent::SizeChanged(_, _), ..} =>
                {
                    resized = true;
                },
                Event::KeyDown{keycode: Some(Keycode::SPACE), ..} =>
                {
                    if let Err(err) = canvas.window_mut().set_size(width, height)
                    {
                        eprintln!("error setting window size: {err}");
                    }
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

        let image = match frame.decode_image::<RgbFormat>()
        {
            Ok(x) => x,
            Err(err) =>
            {
                eprintln!("error decoding the frame: {err}");
                continue;
            }
        };

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
            let mut title = format!("{width}x{height} {current_average:.1} ms ({fps:.1} fps)");

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
}
