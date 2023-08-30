#![allow(non_snake_case)]
#![feature(array_chunks)]
#![feature(file_create_new)]
#![feature(iter_array_chunks)]
#![feature(array_windows)]
#![feature(iter_collect_into)]
#![feature(iter_advance_by)]

use std::{net::SocketAddr, path::PathBuf};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use async_channel::{Receiver, Sender};
use axum::{extract::{
    TypedHeader,
    ws::{Message, WebSocket, WebSocketUpgrade},
}, http::StatusCode, response::IntoResponse, Router, routing::{get, get_service}};
use axum::extract::State;
use futures_util::{SinkExt, StreamExt};
use futures_util::stream::{SplitSink, SplitStream};
use rtlsdr_rs::{DEFAULT_BUF_LENGTH, RtlSdr};
use rustfft::num_complex::Complex32;
use rustfft::num_traits::Zero;
use serde::Serialize;
use tokio::select;
use tower_http::{
    services::ServeDir,
    trace::{DefaultMakeSpan, TraceLayer},
};
use tracing::{debug, info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use crate::iter::IterExt;

mod iter;

struct ControllerState {
    cmd_tx: Sender<SdrCommand>,
    next_id: AtomicUsize,
    buffers: Mutex<HashMap<usize, Sender<Data>>>,
}

struct BufferHandle<'cs> {
    controller: &'cs ControllerState,
    id: usize,
}

impl Drop for BufferHandle<'_> {
    fn drop(&mut self) {
        info!("dropping buf handle for {}", self.id);
        self.controller.free_buffer(self.id);
    }
}

impl ControllerState {
    fn alloc_buffer(&self) -> (BufferHandle<'_>, Receiver<Data>) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = async_channel::bounded(100);
        self.buffers.lock().unwrap().insert(id, tx);
        (BufferHandle {
            controller: self,
            id,
        }, rx)
    }

    fn send_to_all(&self, data: Data) {
        let lock = self.buffers.lock().unwrap();
        for (id, tx) in lock.iter() {
            if let Err(_) = tx.send_blocking(data.clone()) {
                warn!("error while sending to {id}");
            }
        }
    }

    fn free_buffer(&self, buf_id: usize) {
        let mut lock = self.buffers.lock().unwrap();
        lock.remove(&buf_id);
    }
}

#[tokio::main]
async fn main() {
    // console_subscriber::init();
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG")
                .unwrap_or_else(|_| "tower_http=info,websdr=debug".into()),
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let assets_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets");

    let (cmd_tx, cmd_rx) = async_channel::bounded(4);
    let state = Arc::new(ControllerState {
        cmd_tx,
        next_id: Default::default(),
        buffers: Mutex::new(Default::default()),
    });

    tokio::task::spawn_blocking({
        let state = state.clone();
        move || {
            let term = AtomicBool::new(false);
            loop {
                info!("Starting SDR worker thread...");
                if let Err(e) = sdr_worker(&state, &cmd_rx, &term) {
                    warn!("SDR worker thread error: {:?}", e);
                }
                std::thread::sleep(Duration::from_secs(10));
            }
        }
    });

    // build our application with some routes
    let app = Router::new()
        .fallback_service(
            get_service(ServeDir::new(assets_dir).append_index_html_on_directories(true))
                .handle_error(|error: std::io::Error| async move {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("Unhandled internal error: {}", error),
                    )
                }),
        )
        // routes are matched from bottom to top, so we have to put `nest` at the
        // top since it matches all routes
        .route("/ws", get(ws_handler))
        // logging so we can see whats going on
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::default().include_headers(true)),
        )
        .with_state(state);


    // run it with hyper
    let addr = SocketAddr::from(([0, 0, 0, 0], 3000));
    debug!("listening on {}", addr);
    axum::Server::bind(&addr)
        .serve(app.into_make_service())
        .await
        .unwrap();
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    user_agent: Option<TypedHeader<headers::UserAgent>>,
    State(state): State<Arc<ControllerState>>,
) -> impl IntoResponse {
    if let Some(TypedHeader(user_agent)) = user_agent {
        info!("`{}` connected", user_agent.as_str());
    }

    ws.on_upgrade(move |socket| async move {
        let (sink, stream) = socket.split();
        let (stats_tx, stats_rx) = async_channel::bounded(1);
        let (_handle, rx) = state.alloc_buffer();
        tokio::join!(handle_commands(stream, stats_tx, state.clone()), handle_data(sink, stats_rx, rx));
        info!("Close");
    })
}

#[derive(Serialize)]
#[serde(untagged)]
#[serde(rename_all = "snake_case")]
enum Stats {
    Frequency(u32),
}

#[derive(Clone)]
enum Data {
    Fft(bytes::Bytes),
    Audio(bytes::Bytes),
}

async fn handle_data(
    mut socket: SplitSink<WebSocket, Message>,
    mut stats_rx: Receiver<Stats>,
    mut data_rx: Receiver<Data>,
) {
    let (audio_tx, audio_rx) = async_channel::bounded::<bytes::Bytes>(10);
    let (fft_tx, fft_rx) = async_channel::bounded::<bytes::Bytes>(10);
    let (frames_tx, mut frames_rx) = async_channel::bounded::<Vec<u8>>(10);

    let audio_task = {
        let tx = frames_tx.clone();
        async move {
            let mut interval = tokio::time::interval(Duration::from_millis(20));
            while let Ok(audio) = audio_rx.recv().await {
                let mut data: Vec<u8> = Vec::with_capacity(1 + audio.len());
                data.push(0x02);
                data.extend_from_slice(audio.as_ref());
                if tx.send(data).await.is_err() {
                    warn!("Cannot send audio frame to websocket task");
                    break;
                }
                interval.tick().await;
            }
        }
    };

    let fft_task = async move {
        while let Ok(fft) = fft_rx.recv().await {
            let mut data: Vec<u8> = Vec::with_capacity(1 + fft.len());
            data.push(0x01);
            data.extend_from_slice(fft.as_ref());
            if frames_tx.send(data).await.is_err() {
                warn!("Cannot send FFT data to websocket task");
            }
        }
    };

    tokio::spawn(fft_task);
    tokio::spawn(audio_task);

    // FIXME: there is a risk of deadlock when websocket connection is not stable

    loop {
        select! {
            biased;
            item = stats_rx.next() => {
                let Some(stats) = item else {
                    break;
                };
                if let Err(e) = socket.send(Message::Text(serde_json::to_string(&stats).unwrap())).await {
                    info!("client disconnected {:?}", e);
                    return;
                }
            },
            item = frames_rx.next() => {
                let data: Vec<u8> = item.unwrap();
                if let Err(e) = socket.send(Message::Binary(data)).await {
                    info!("client disconnected {:?}", e);
                    return;
                }
            },
            item = data_rx.next() => {
                let data: Data = item.unwrap();
                let res = match data {
                    Data::Fft(fft) => fft_tx.send(fft).await,
                    Data::Audio(audio) => audio_tx.send(audio).await,
                };
                if let Err(e) = res {
                    info!("client disconnected {:?}", e);
                    return;
                }
            }
        }
    }
}

async fn handle_commands(mut stream: SplitStream<WebSocket>, stats_tx: Sender<Stats>, state: Arc<ControllerState>) {
    while let Some(msg) = stream.next().await {
        match msg {
            Ok(msg) => match msg {
                Message::Text(payload) => {
                    let command = match serde_json::from_str(&payload) {
                        Ok(command) => command,
                        Err(e) => {
                            warn!("Invalid command: {:?}", e);
                            continue;
                        }
                    };
                    match &command {
                        SdrCommand::ChangeFrequency { frequency } => {
                            stats_tx.send(Stats::Frequency(*frequency)).await.unwrap();
                        }
                    }
                    state.cmd_tx.send(command).await.unwrap();
                }
                other => {
                    debug!("message: {:?}", &other);
                }
            },
            Err(e) => {
                info!("client disconnected: {:?}", e);
                return;
            }
        }
    }
}

#[derive(Debug)]
#[derive(serde::Deserialize)]
#[serde(rename_all = "snake_case", tag = "command")]
enum SdrCommand {
    ChangeFrequency { frequency: u32 },
}

const FFT_LEN: usize = 4096;

fn sdr_worker(state: &ControllerState, rx: &Receiver<SdrCommand>, terminated: &AtomicBool) -> rtlsdr_rs::error::Result<()> {
    let mut sdr = RtlSdr::open(0)?;
    sdr.set_tuner_gain(rtlsdr_rs::TunerGain::Auto)?;
    sdr.set_bias_tee(false)?;
    sdr.reset_buffer()?;
    sdr.set_center_freq(95_000_000)?;
    sdr.set_sample_rate(1_200_000)?;

    info!("Tuned to {} Hz.", sdr.get_center_freq());
    // info!("Buffer size: {}ms", 1000.0 * 0.5 * DEFAULT_BUF_LENGTH as f32 / radio_config.capture_rate as f32);
    info!("Sampling at {} S/s", sdr.get_sample_rate());

    info!("Reading samples in sync mode...");

    let fft = rustfft::FftPlanner::<f32>::new().plan_fft_forward(FFT_LEN);
    let mut scratch: Vec<Complex32> = vec![Complex32::zero(); fft.get_inplace_scratch_len()];
    let mut incoming_samples: Vec<Complex32> = Vec::new();
    let mut buffer: Vec<Complex32> = vec![Complex32::zero(); fft.len()];

    let window = make_hamming_window(FFT_LEN, 1.0);

    let mut audio_samples = Vec::new();

    loop {
        if terminated.load(Ordering::Relaxed) {
            break;
        }

        while incoming_samples.len() >= FFT_LEN {
            buffer.clear();
            buffer.extend(incoming_samples[..FFT_LEN].iter().copied().zip(window.iter().copied()).map(|(a, b)| a * b));
            fft.process_with_scratch(&mut buffer[..FFT_LEN], &mut scratch);
            let frame: Vec<_> = buffer[..FFT_LEN].iter().copied().map(|it| ((it.norm() as f32) * 4.0) as u8).collect();
            state.send_to_all(Data::Fft(bytes::Bytes::from(frame)));
            let samples = incoming_samples.drain(..FFT_LEN);

            {
                let Fs = 1_200_000;
                let Fbw = 200_000;
                let Fa = 22050;

                samples
                    .into_iter()
                    .step_by(Fs / Fbw)
                    .array_windows()
                    .map(|[s0, s1]| (s1 * s0.conj()).arg())
                    .step_by(Fbw / Fa)
                    .collect_into(&mut audio_samples);
            };
        }

        let fs = 22050;
        let n = fs / 10 / 5;

        while audio_samples.len() >= n {
            let audio = audio_samples
                .drain(..n)
                .map(|it| ((it * i16::MAX as f32) as i16).to_le_bytes())
                .flatten()
                .collect();
            state.send_to_all(Data::Audio(audio));
        }

        let mut new_frequency = None;
        while let Ok(command)= rx.try_recv() {
            match command {
                SdrCommand::ChangeFrequency { frequency } => {
                    new_frequency = Some(frequency);
                }
            }
        }

        if let Some(frequency) = new_frequency {
            info!("Changing central frequency to {}", frequency);
            if let Err(e) = sdr.reset_buffer() {
                warn!("reset_buffer: {:?}", e);
            }
            if let Err(e) = sdr.set_center_freq(frequency) {
                warn!("set_center_freq: {:?}", e);
            }
        }

        let mut frame = [0u8; DEFAULT_BUF_LENGTH];
        let n = sdr.read_sync(&mut frame)?;
        assert_eq!(n, DEFAULT_BUF_LENGTH);
        let samples = &frame[..n];
        incoming_samples.extend(samples.array_chunks::<2>()
            .map(|&[re, im]| Complex32::new(
                (re as i32 - 127) as f32 / 128.0,
                (im as i32 - 127) as f32 / 128.0,
            ))
        );
    }
    sdr.close()?;
    Ok(())
}

fn make_hamming_window(size: usize, scale: f32) -> Box<[f32]> {
    let mut window = Vec::with_capacity(size);
    for i in 0..size {
        window.push(scale * (0.54 - 0.46 * f32::cos(2.0 * std::f32::consts::PI * (i as f32) / (size as f32 - 1.0))));
    }
    window.into_boxed_slice()
}