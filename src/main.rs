#![cfg_attr(debug_assertions, allow(dead_code, unused_variables,))]
use std::{collections::VecDeque, sync::Arc, time::Duration};

use egui::{
    pos2, text::LayoutJob, Align2, Area, Color32, Frame, Galley, KeyboardShortcut, Margin,
    Modifiers, Rect, Sense, TextFormat, Vec2,
};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

#[derive(Debug)]
struct Occupied {
    map: Vec<f32>,
    line_height: f32,
    max_rows: usize,
}

impl Occupied {
    fn new(ctx: &egui::Context, max_height: f32) -> Self {
        let fid = TEXT_STYLE.resolve(&ctx.style());
        let line_height = ctx.fonts(|f| f.row_height(&fid));

        // let max_rows = (max_height.floor() as usize / line_height.ceil() as usize);
        let max_rows = 10;
        dbg!(Self {
            line_height,
            max_rows,
            map: vec![0.0; max_rows],
        })
    }

    fn place(&mut self, length: f32) -> (f32, usize, Option<f32>) {
        assert!(length >= 0.0, "length cannot be negative");

        let mut rng = fastrand::Rng::new();

        if self.map.iter().all(|&c| c != 0.0) {
            let mut v: Vec<_> = self.map.iter_mut().enumerate().collect();
            v.sort_unstable_by(|(_, l), (_, r)| r.total_cmp(l));
            let (index, row) = v.pop().unwrap();
            let old = *row;
            *row += length;
            return (index as f32 * self.line_height, index, Some(old));
        }

        let mut list = self.map.iter_mut().enumerate().collect::<Vec<_>>();

        loop {
            rng.shuffle(&mut list);
            if let Some((index, row)) = list.first_mut().filter(|(_, c)| **c == 0.0) {
                **row += length;
                return (*index as f32 * self.line_height, *index, None);
            }
        }
    }

    fn evict(&mut self, index: usize, width: f32) {
        let row = self.map[index];
        self.map[index] = (row - width).max(0.0);
    }
}

trait IterExt: Iterator + Sized {
    fn choose(mut self, rng: &mut fastrand::Rng) -> Option<Self::Item> {
        fn rand_index(rng: &mut fastrand::Rng, bound: usize) -> usize {
            if bound < (u32::MAX as usize) {
                rng.u32(0..bound as u32) as _
            } else {
                rng.usize(0..bound)
            }
        }

        let (mut lower, mut upper) = self.size_hint();
        let mut pos = 0;
        let mut out = None;
        if upper == Some(lower) {
            if lower == 0 {
                return out;
            }
            return self.nth(rand_index(rng, lower));
        }

        loop {
            if lower > 1 {
                let index = rand_index(rng, lower + pos);
                let skipped = if index < lower {
                    out = self.nth(index);
                    lower - index + 1
                } else {
                    lower
                };
                if upper == Some(lower) {
                    return out;
                }
                pos += lower;
                if skipped > 0 {
                    self.nth(skipped - 1);
                }
            } else {
                let next = match self.next() {
                    Some(next) => next,
                    None => return out,
                };
                pos += 1;
                if rng.f64() / pos as f64 > 0.5 {
                    out.replace(next);
                }
            }

            let hint = self.size_hint();
            lower = hint.0;
            upper = hint.1;
        }
    }
}

impl<I> IterExt for I where I: Iterator + Sized {}

struct PreparedMessage {
    galley: Arc<Galley>,
    pos: f32,
    row: f32,
    index: usize,
    length: f32,
    alive: bool,
}

impl PreparedMessage {
    fn prepare(msg: Message, occupied: &mut Occupied, ctx: &egui::Context) -> Self {
        let rect = ctx.available_rect();
        let (width, height) = (rect.width(), rect.height());

        let fid = TEXT_STYLE.resolve(&ctx.style());
        let style = ctx.style().visuals.strong_text_color();
        let galley = ctx.fonts(|f| {
            let w = f.glyph_width(&fid, ' ');
            f.layout_job({
                let mut job = LayoutJob::simple(msg.name, fid.clone(), msg.color, f32::INFINITY);
                job.append(
                    " ",
                    w,
                    TextFormat::simple(fid.clone(), Color32::TRANSPARENT),
                );
                job.append(&msg.data, 0.0, TextFormat::simple(fid.clone(), style));
                job
            })
        });

        let length = galley.size().x;
        assert!(length >= 0.0, "length cannot be zero");

        let (row, index, offset) = occupied.place(length);

        Self {
            pos: width + offset.unwrap_or_default(),
            row,
            length,
            index,
            galley,
            alive: true,
        }
    }
}

struct Application {
    events: UnboundedReceiver<Message>,
    queue: VecDeque<PreparedMessage>,
}

impl eframe::App for Application {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        thread_local! {
            static HAS_MOUSE_PASSTHROUGH: std::cell::RefCell<bool> = const { std::cell::RefCell::new(false) };
        }

        thread_local! {
            static OCCUPIED: std::cell::RefCell<Occupied> = const { std::cell::RefCell::new(Occupied{
                map: vec![],
                line_height: 0.0,
                max_rows: 0
            })};
        }

        static OCCUPIED_INIT: std::sync::Once = std::sync::Once::new();

        OCCUPIED_INIT.call_once(|| {
            OCCUPIED.with(|f| *f.borrow_mut() = Occupied::new(ctx, frame.info().window_info.size.x))
        });

        if ctx.input(|i| i.pointer.primary_down() && i.modifiers.shift_only()) {
            frame.drag_window();
        }

        if ctx.input_mut(|i| {
            i.consume_shortcut(&KeyboardShortcut::new(Modifiers::NONE, egui::Key::F2))
        }) {
            let passthrough = HAS_MOUSE_PASSTHROUGH.with(|d| {
                let mut d = d.borrow_mut();
                *d = !*d;
                *d
            });
            frame.set_mouse_passthrough(passthrough);
        }

        while let Ok(msg) = self.events.try_recv() {
            let msg = OCCUPIED
                .with(|occupied| PreparedMessage::prepare(msg, &mut *occupied.borrow_mut(), ctx));

            self.queue.push_back(msg);
        }

        let display = |ui: &mut egui::Ui| {
            let resp = ui.allocate_rect(ui.available_rect_before_wrap(), Sense::click_and_drag());

            ui.allocate_ui_at_rect(resp.rect, |ui| {
                // if ui.button("generate random message").clicked() {
                //     const NAMES: &[&str] = &["museun", "shaken_bot", "test_user"];
                //     const COLORS: &[Color32] = &[Color32::RED, Color32::GREEN, Color32::YELLOW];
                //     const LINES: &[&str] = &[
                //         "Lorem ipsum dolor sit amet, consectetur adipiscing elit, \
                //         sed do eiusmod tempor incididunt ut labore et dolore magna aliqua.",
                //         "Pulvinar elementum integer enim neque volutpat ac.",
                //         "Velit aliquet sagittis id consectetur purus ut faucibus.",
                //         "Massa tincidunt dui ut ornare.",
                //         "Pellentesque id nibh tortor id aliquet.",
                //         "Commodo nulla facilisi nullam vehicula ipsum.",
                //         "Malesuada fames ac turpis egestas integer eget.",
                //         "Quis blandit turpis cursus in hac habitasse platea dictumst.",
                //         "Consectetur adipiscing elit ut aliquam purus sit amet.",
                //         "Platea dictumst vestibulum rhoncus est pellentesque\
                //          elit ullamcorper dignissim",
                //     ];

                //     let (name, color) = fastrand::choice(NAMES.iter().zip(COLORS)).unwrap();

                //     let msg = fastrand::choice(LINES.iter()).unwrap();

                //     let msg = OCCUPIED.with(|occupied| {
                //         PreparedMessage::prepare(
                //             Message {
                //                 color: *color,
                //                 name: name.to_string(),
                //                 data: msg.to_string(),
                //             },
                //             &mut *occupied.borrow_mut(),
                //             ctx,
                //         )
                //     });

                //     self.queue.push_back(msg);
                // }
                // ui.separator();

                let dt = ui.input(|i| i.unstable_dt).min(0.1);

                let fid = TEXT_STYLE.resolve(ui.style());
                for (i, msg) in self.queue.iter_mut().enumerate() {
                    let delta = dt * 1e2;
                    msg.pos -= delta;
                    if msg.pos + msg.galley.size().x <= 0.0 {
                        msg.alive = false;
                        ctx.fonts(|f| {
                            OCCUPIED
                                .with(|occupied| occupied.borrow_mut().evict(msg.index, msg.length))
                        });
                        continue;
                    }

                    let anchor = Align2::LEFT_TOP.anchor_rect(Rect::from_min_size(
                        pos2(msg.pos, msg.row),
                        msg.galley.size(),
                    ));
                    ui.put(anchor, |ui: &mut egui::Ui| ui.label(msg.galley.clone()));
                }

                ctx.request_repaint();
                self.queue.retain(|d| d.alive);
            })
        };

        // egui::CentralPanel::default().show(ctx, display);
        Area::new("main-border")
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .movable(false)
            .show(ctx, |ui| {
                Frame::none()
                    .outer_margin(Margin::same(1.0))
                    .show(ui, display);
            });
    }

    fn save(&mut self, _storage: &mut dyn eframe::Storage) {}

    fn persist_egui_memory(&self) -> bool {
        true
    }

    fn persist_native_window(&self) -> bool {
        true
    }

    // fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {

    // }
}

#[derive(Debug)]
struct Message {
    color: Color32,
    name: String,
    data: String,
}

async fn start_connection(
    sender: UnboundedSender<Message>,
    repaint: impl Fn() + Send + Sync + 'static,
) -> anyhow::Result<()> {
    use tokio::io::AsyncWriteExt as _;

    'outer: loop {
        use tokio::io::AsyncBufReadExt as _;
        eprintln!("connecting to twitch");
        let mut stream = tokio::net::TcpStream::connect(twitch_message::TWITCH_IRC_ADDRESS).await?;
        let (read, mut write) = stream.split();
        let mut lines = tokio::io::BufReader::new(read).lines();

        macro_rules! reconnect {
            ($args:expr) => {{
                eprintln!("{args}", args = $args);
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue 'outer;
            }};
        }

        macro_rules! write_msg {
            ($msg:expr) => {{
                if let Err(err) = write.write_all($msg.to_string().as_bytes()).await {
                    reconnect!("cannot write: {err}");
                }
                if let Err(err) = write.flush().await {
                    reconnect!("cannot flush stream: {err}");
                }
            }};
        }

        let (name, pass) = twitch_message::ANONYMOUS_LOGIN;
        write_msg!(twitch_message::encode::register(
            name,
            pass,
            twitch_message::encode::ALL_CAPABILITIES
        ));

        let mut waiting = false;

        'inner: loop {
            match match tokio::time::timeout(Duration::from_secs(30), lines.next_line()).await {
                Ok(event) => event,
                Err(..) if !waiting => {
                    let token = std::iter::repeat_with(fastrand::alphanumeric)
                        .take(10)
                        .collect::<String>();
                    eprintln!("sending ping");
                    write_msg!(twitch_message::encode::ping(&token));
                    waiting = true;
                    continue 'inner;
                }

                Err(..) => reconnect!("connecting timed out"),
            } {
                Ok(Some(line)) => {
                    waiting = false;

                    let msg = match twitch_message::parse(&line) {
                        Ok(msg) => msg,
                        Err(err) => reconnect!(format_args!("cannot parse line: {err}")),
                    };

                    use twitch_message::messages::TwitchMessage as M;
                    match msg.message.as_enum() {
                        M::Reconnect(_) => reconnect!("twitch wants us to reconnect"),
                        M::IrcReady(_) => write_msg!(twitch_message::encode::join(TWITCH_CHANNEL)),
                        M::Privmsg(msg) => {
                            eprintln!("{}: {}", msg.sender, msg.data);
                            let out = Message {
                                color: msg
                                    .color()
                                    .map(|c| Color32::from_rgb(c.red(), c.green(), c.blue()))
                                    .unwrap_or(Color32::WHITE),
                                name: msg.sender.as_str().to_string(),
                                data: msg.data.to_string(),
                            };
                            let _ = sender.send(out);
                            (repaint)()
                        }
                        M::Ping(msg) => write_msg!(twitch_message::encode::pong(&*msg.token)),
                        M::Join(_) => {}
                        _ => {}
                    }
                }

                Ok(..) => {
                    continue 'inner;
                }

                Err(err) => reconnect!("cannot read line: {err}"),
            }
        }
    }
}

const TWITCH_CHANNEL: &str = "#museun";
const TEXT_STYLE: egui::TextStyle = egui::TextStyle::Heading;

#[tokio::main]
async fn main() {
    eframe::run_native(
        env!("CARGO_PKG_NAME"),
        eframe::NativeOptions {
            transparent: true,
            decorated: false,
            always_on_top: true,
            mouse_passthrough: false,
            ..Default::default()
        },
        Box::new(|cc| {
            cc.egui_ctx.set_pixels_per_point(2.0);
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            let repaint = {
                let ctx = cc.egui_ctx.clone();
                move || ctx.request_repaint()
            };
            tokio::spawn(async move { start_connection(tx, repaint).await });

            Box::new(Application {
                events: rx,
                queue: VecDeque::new(),
            })
        }),
    )
    .unwrap()
}
