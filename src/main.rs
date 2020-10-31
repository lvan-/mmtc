#![feature(box_patterns)]
#![forbid(unsafe_code)]

mod config;
mod fail;
mod layout;
mod mpd;

use anyhow::{Context, Error, Result};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use tokio::{
    sync::mpsc,
    time::{sleep_until, Duration, Instant},
};
use tui::{backend::CrosstermBackend, widgets::ListState, Terminal};

use std::{
    io::{stdout, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr},
    process::exit,
};

use crate::{
    config::Config,
    mpd::{Status, Track},
};

fn cleanup() -> Result<()> {
    disable_raw_mode().context("Failed to clean up terminal")?;
    execute!(stdout(), LeaveAlternateScreen, DisableMouseCapture)
        .context("Failed to clean up terminal")?;
    Ok(())
}

fn die<T>(e: impl Into<Error>) -> T {
    eprintln!("{:?}", cleanup().map_or_else(|x| x, |_| e.into()));
    exit(1);
}

#[derive(Debug)]
enum Command {
    Quit,
    UpdateFrame,
    UpdateQueue(Vec<Track>),
    UpdateStatus(Status),
    Down,
    Up,
}

#[tokio::main]
async fn main() {
    let res = run().await;
    if let Err(e) = cleanup().and_then(|_| res) {
        eprintln!("{:?}", e);
        exit(1);
    }
    exit(0);
}

async fn run() -> Result<()> {
    let cfg: Config = ron::from_str(&std::fs::read_to_string("mmtc.ron").unwrap()).unwrap();
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 6600);

    let mut idle_cl = mpd::init(addr).await?;
    let mut status_cl = mpd::init(addr).await?;

    let mut queue = mpd::queue(&mut idle_cl).await?;
    let mut status = mpd::status(&mut status_cl).await?;
    let mut selected = status.song.map_or(0, |song| song.pos);
    let mut liststate = ListState::default();
    liststate.select(Some(selected));

    let update_interval = Duration::from_secs_f64(1.0 / cfg.ups);

    let (tx, mut rx) = mpsc::channel(32);
    let tx1 = tx.clone();
    let tx2 = tx.clone();
    let tx3 = tx.clone();

    tokio::spawn(async move {
        let tx = tx1;
        loop {
            mpd::idle_playlist(&mut idle_cl)
                .await
                .context("Failed to idle")
                .unwrap_or_else(die);
            tx.send(Command::UpdateQueue(
                mpd::queue(&mut idle_cl)
                    .await
                    .context("Failed to query queue information")
                    .unwrap_or_else(die),
            ))
            .await
            .unwrap_or_else(die);
        }
    });

    tokio::spawn(async move {
        let tx = tx2;
        loop {
            let deadline = Instant::now() + update_interval;
            tx.send(Command::UpdateStatus(
                mpd::status(&mut status_cl)
                    .await
                    .context("Failed to query status")
                    .unwrap_or_else(die),
            ))
            .await
            .unwrap_or_else(die);
            sleep_until(deadline).await;
        }
    });

    let mut stdout = stdout();
    enable_raw_mode().context("Failed to initialize terminal")?;
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)
        .context("Failed to initialize terminal")?;
    let mut term =
        Terminal::new(CrosstermBackend::new(stdout)).context("Failed to initialize terminal")?;

    tokio::spawn(async move {
        let tx = tx3;
        while let Ok(ev) = event::read() {
            match ev {
                Event::Key(KeyEvent { code, .. }) => match code {
                    KeyCode::Char('q') | KeyCode::Esc => {
                        tx.send(Command::Quit).await.unwrap_or_else(die);
                    }
                    KeyCode::Char('j') | KeyCode::Down => {
                        tx.send(Command::Down).await.unwrap_or_else(die);
                    }
                    KeyCode::Char('k') | KeyCode::Up => {
                        tx.send(Command::Up).await.unwrap_or_else(die);
                    }
                    _ => (),
                },
                Event::Resize(..) => tx.send(Command::UpdateFrame).await.unwrap_or_else(die),
                _ => (),
            }
        }
    });

    while let Some(cmd) = rx.recv().await {
        match cmd {
            Command::Quit => break,
            Command::UpdateFrame => term
                .draw(|frame| {
                    layout::render(
                        frame,
                        frame.size(),
                        &cfg.layout,
                        &queue,
                        &status,
                        selected,
                        &liststate,
                    );
                })
                .context("Failed to draw to terminal")?,
            Command::UpdateQueue(new_queue) => {
                queue = new_queue;
                selected = status.song.map_or(0, |song| song.pos);
                liststate = ListState::default();
                liststate.select(Some(selected));
                tx.send(Command::UpdateFrame).await?;
            }
            Command::UpdateStatus(new_status) => {
                status = new_status;
                tx.send(Command::UpdateFrame).await?;
            }
            Command::Down => {
                let len = queue.len();
                if selected >= len {
                    selected = status.song.map_or(0, |song| song.pos);
                } else if selected == len - 1 {
                    if cfg.cycle {
                        selected = 0
                    }
                } else {
                    selected += 1
                }
                liststate.select(Some(selected));
                tx.send(Command::UpdateFrame).await?;
            }
            Command::Up => {
                let len = queue.len();
                if selected >= len {
                    selected = status.song.map_or(0, |song| song.pos);
                } else if selected == 0 {
                    if cfg.cycle {
                        selected = len - 1
                    }
                } else {
                    selected -= 1
                }
                liststate.select(Some(selected));
                tx.send(Command::UpdateFrame).await?;
            }
        }
    }

    Ok(())
}
