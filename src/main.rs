use std::{
    future::Future,
    sync::{Arc, Mutex, MutexGuard},
    time::Instant,
};

use chrono::prelude::*;
use clap::Parser;
use eyre::Result;
use sqlx::SqlitePool;
use tokio::runtime::Runtime;

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

#[derive(Clone)]
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
            let messages = sqlx::query_as::<_, (String, i64, Option<String>)>(
                r#"
                    SELECT
                        m.text, m.date, h.id
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
            .map(|(text, timestamp, sender)| Message {
                text,
                date: time(timestamp),
                sender: sender.map(Sender::SomeoneElse).unwrap_or(Sender::Me),
            })
            .collect::<Vec<_>>();

            println!("Loaded {} messages", messages.len());

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
        egui::SidePanel::left("my_left_panel").show(ctx, |ui| match &*self.chats.get() {
            State::Empty => {
                ui.heading("no chats found");
            }
            State::Fetching => {
                ui.heading("loading...");
            }
            State::Ready(chats) => {
                for chat in chats {
                    if ui
                        .add(egui::Button::new(format!(
                            "{} - {:?}",
                            chat.name, chat.last_active
                        )))
                        .clicked()
                    {
                        self.selected_chat = Some(chat.clone());
                        self.load_messages(chat.name.clone());
                    }
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
                    State::Ready(messages) => {
                        let start = Instant::now();
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            for msg in messages {
                                ui.group(|ui| {
                                    ui.label(&msg.text);
                                });
                            }
                        });
                        dbg!(start.elapsed());
                    }
                }
            } else {
                ui.heading("select a chat on the left");
            }
        });
    }
}
