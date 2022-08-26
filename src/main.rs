use std::{
    future::Future,
    sync::{Arc, Mutex, MutexGuard},
};

use chrono::prelude::*;
use clap::Parser;
use egui::{Color32, Frame, Rgba, Rounding, Stroke, Ui};
use eyre::Result;
use sqlx::SqlitePool;
use tokio::runtime::Runtime;

lazy_static::lazy_static! {
    static ref BLUE: Color32 = Rgba::from_srgba_premultiplied(65, 136, 247, 255).into();
    static ref GREY: Color32 = Rgba::from_srgba_premultiplied(59, 59, 61, 255).into();
}

#[derive(Parser)]
/// View historical iMessage chats based on a `chat.db` file
struct Options {
    /// Path to the database file to load - do not use the main chat.db file
    /// directly, make a copy before feeding it to this program.
    database_file: String,
}

fn main() -> Result<()> {
    let options = Options::parse();

    let native_options = eframe::NativeOptions::default();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    let db = rt.block_on(async { SqlitePool::connect(&options.database_file).await })?;

    let app = MyEguiApp::new(rt, db);
    app.initial_load();

    eframe::run_native(
        "iMessage Reader",
        native_options,
        Box::new(|_cc| Box::new(app)),
    );

    Ok(())
}

#[derive(Clone)]
struct Handle<T> {
    lock: Arc<Mutex<State<T>>>,
}

enum State<T> {
    Empty,
    Fetching,
    Ready(T),
}

impl<T> Handle<T> {
    fn new() -> Self {
        Self {
            lock: Arc::new(Mutex::new(State::Empty)),
        }
    }

    fn set(&self, state: State<T>) {
        *self.lock.lock().unwrap() = state;
    }

    fn get(&self) -> MutexGuard<State<T>> {
        self.lock.lock().unwrap()
    }
}

#[derive(Clone)]
struct Chat {
    name: String,
    // last_message: String,
    last_active: DateTime<Utc>,
}

#[derive(Clone, PartialEq, Debug)]
enum Sender {
    Me,
    SomeoneElse(String),
}

#[derive(Clone)]
struct Message {
    text: String,
    sender: Sender,
    date: DateTime<Utc>,
}

struct MyEguiApp {
    rt: Runtime,
    db: SqlitePool,
    chats: Handle<Vec<Chat>>,
    selected_chat: Option<Chat>,
    selected_chat_messages: Handle<Vec<Message>>,
}

/// Turn Apple's ridiculous time format into a chrono datetime
fn time(raw: i64) -> DateTime<Utc> {
    let epoch_correction = NaiveDate::from_ymd(2001, 1, 1).and_hms(0, 0, 0).timestamp();
    let seconds = (raw / 1000000000) + epoch_correction;

    NaiveDateTime::from_timestamp(seconds, 0)
        .and_local_timezone(Utc)
        .unwrap()
}

impl MyEguiApp {
    fn new(rt: Runtime, db: SqlitePool) -> Self {
        Self {
            rt,
            db,
            chats: Handle::new(),
            selected_chat: None,
            selected_chat_messages: Handle::new(),
        }
    }

    fn load<T>(&self, handle: Handle<T>, f: impl 'static + Send + Future<Output = Result<T>>)
    where
        T: 'static + Send + Sync,
    {
        handle.set(State::Fetching);

        self.rt.spawn(async move {
            match f.await {
                Ok(val) => handle.set(State::Ready(val)),
                Err(e) => eprintln!("{e}"),
            }
        });
    }

    fn load_messages(&self, chat_id: String) {
        let db = self.db.clone();

        self.load(self.selected_chat_messages.clone(), async move {
            let messages = sqlx::query_as::<_, (String, i64, String, bool)>(
                r#"
                    SELECT
                        m.text, m.date, h.id, m.is_from_me
                    from message m
                    join chat_message_join cmj
                        on m.ROWID = cmj.message_id
                    join chat c
                        on cmj.chat_id = c.ROWID
                    left join handle h
                        on m.handle_id = h.ROWID
                    where c.chat_identifier=$1
                    order by date
                    ;
                "#,
            )
            .bind(chat_id)
            .fetch_all(&db)
            .await?
            .into_iter()
            .map(|(text, timestamp, sender, is_from_me)| Message {
                text,
                date: time(timestamp),
                sender: if is_from_me {
                    Sender::Me
                } else {
                    Sender::SomeoneElse(sender)
                },
            })
            .collect::<Vec<_>>();

            Ok(messages)
        });
    }

    fn initial_load(&self) {
        let db = self.db.clone();

        self.load(self.chats.clone(), async move {
            let chats = sqlx::query_as::<_, (i64, String)>(
                r#"SELECT
            max(m.date), c.chat_identifier
        from message m
        join chat_message_join cmj
            on m.ROWID = cmj.message_id
        join chat c
            on cmj.chat_id = c.ROWID
        group by c.chat_identifier
        order by max(m.date) desc
        ;
        "#,
            )
            .fetch_all(&db)
            .await?
            .into_iter()
            .map(|(timestamp, name)| Chat {
                name,
                last_active: time(timestamp),
            })
            .collect();

            Ok(chats)
        });
    }
}

impl eframe::App for MyEguiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::SidePanel::left("my_left_panel").show(ctx, |ui| {
            let guard = self.chats.get();

            match &*guard {
                State::Empty => {
                    ui.heading("no chats found");
                }
                State::Fetching => {
                    ui.heading("loading...");
                }
                State::Ready(chats) => {
                    let chats = chats.to_owned();
                    drop(guard);
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        for chat in chats {
                            let mut frame = Frame::group(ui.style());

                            if let Some(c) = &self.selected_chat {
                                if c.name == chat.name {
                                    frame = frame.fill(*BLUE);
                                }
                            }

                            let response = frame
                                .show(ui, |ui| {
                                    ui.visuals_mut().override_text_color = Some(Color32::WHITE);
                                    ui.label(&chat.name);
                                    ui.label(format!("{:?}", chat.last_active));
                                })
                                .response;

                            if response.interact(egui::Sense::click()).clicked() {
                                self.selected_chat = Some(chat.clone());
                                self.load_messages(chat.name.clone());
                            }
                        }
                    });
                }
            }
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(chat) = &self.selected_chat {
                ui.heading(&chat.name);

                match &*self.selected_chat_messages.get() {
                    State::Empty => {
                        ui.label("no messages found");
                    }
                    State::Fetching => {
                        ui.label("loading...");
                    }
                    State::Ready(messages) => render_messages(ui, messages),
                }
            } else {
                ui.heading("select a chat on the left");
            }
        });
    }
}

fn render_messages(ui: &mut Ui, messages: &[Message]) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        for msg in messages {
            let (layout, bg) = if msg.sender == Sender::Me {
                (egui::Layout::right_to_left(egui::Align::TOP), *BLUE)
            } else {
                (egui::Layout::left_to_right(egui::Align::TOP), *GREY)
            };

            ui.with_layout(layout, |ui| {
                ui.visuals_mut().override_text_color = Some(Color32::WHITE);

                Frame::group(ui.style())
                    .fill(bg)
                    .stroke(Stroke::none())
                    .rounding(Rounding::same(2.0 * 3.14))
                    .show(ui, |ui| {
                        ui.set_max_width(250.0);
                        ui.style_mut().wrap = Some(true);
                        ui.label(&msg.text);
                    });
            });
        }
    });
}
